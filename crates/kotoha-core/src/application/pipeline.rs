//! Application pipeline orchestrating input events, preprocessing, per-speaker STT,
//! pre-filtering, context injection, bounded parallel translation, and 3-stage subtitle output.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{mpsc, Mutex, Semaphore};
use tokio::task::JoinSet;

use crate::application::session::{SessionAction, SourceSessionManager};
use crate::application::translator_manager::TranslatorManager;
use crate::domain::{
    AudioChunk, AudioInputEvent, ContextWindow, CoreError, HistoryEntry, LanguageCode,
    MergedGlossary, PreFilter, SpeakerKey, SttEngine, SttStream, SubtitleEntry, SubtitleSink,
    SubtitleStage, TranscriptEvent, TranslationRequest, DEFAULT_HOTWORDS_SCORE,
};

/// Audio normalization callback converting raw interleaved [`AudioChunk`] into 16 kHz mono `f32`.
///
/// Defined as a pure function/closure boundary so `application` does not depend on concrete
/// DSP or infrastructure adapters directly.
pub type ResampleFn = Arc<dyn Fn(&AudioChunk) -> Result<Vec<f32>, CoreError> + Send + Sync>;

/// Configuration for [`PipelineService`].
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Source language code passed to per-speaker STT streams and translation requests.
    pub src_lang: LanguageCode,
    /// Target language code passed to translation requests.
    pub tgt_lang: LanguageCode,
    /// Maximum number of concurrent translation tasks permitted by the semaphore.
    pub max_concurrent_translations: usize,
    /// Number of recent translated turns preserved per source context window.
    pub context_window_capacity: usize,
}

impl PipelineConfig {
    /// Creates a [`PipelineConfig`] with sensible defaults (`max_concurrent_translations = 4`,
    /// `context_window_capacity = 8`).
    #[must_use]
    pub const fn new(src_lang: LanguageCode, tgt_lang: LanguageCode) -> Self {
        Self {
            src_lang,
            tgt_lang,
            max_concurrent_translations: 4,
            context_window_capacity: 8,
        }
    }
}

/// Coordinates end-to-end audio-to-subtitle processing across multiple sources and speakers.
pub struct PipelineService {
    config: PipelineConfig,
    stt_engine: Arc<dyn SttEngine>,
    translator_manager: Arc<TranslatorManager>,
    subtitle_sink: Arc<dyn SubtitleSink>,
    resample_fn: ResampleFn,
    pre_filter: PreFilter,
    active_glossary: MergedGlossary,
    session_manager: SourceSessionManager,
    speaker_streams: HashMap<SpeakerKey, Box<dyn SttStream>>,
    source_contexts: HashMap<String, Arc<Mutex<ContextWindow>>>,
    translation_semaphore: Arc<Semaphore>,
    clock_ms: AtomicU64,
}

impl PipelineService {
    /// Constructs a new [`PipelineService`].
    #[must_use]
    pub fn new(
        config: PipelineConfig,
        stt_engine: Arc<dyn SttEngine>,
        translator_manager: Arc<TranslatorManager>,
        subtitle_sink: Arc<dyn SubtitleSink>,
        resample_fn: ResampleFn,
    ) -> Self {
        let concurrency = config.max_concurrent_translations.max(1);
        Self {
            config,
            stt_engine,
            translator_manager,
            subtitle_sink,
            resample_fn,
            pre_filter: PreFilter::default(),
            active_glossary: MergedGlossary::default(),
            session_manager: SourceSessionManager::new(),
            speaker_streams: HashMap::new(),
            source_contexts: HashMap::new(),
            translation_semaphore: Arc::new(Semaphore::new(concurrency)),
            clock_ms: AtomicU64::new(0),
        }
    }

    /// Replaces the default [`PreFilter`] rules.
    pub fn set_pre_filter(&mut self, pre_filter: PreFilter) {
        self.pre_filter = pre_filter;
    }

    /// Updates the active merged glossary at runtime using [`DEFAULT_HOTWORDS_SCORE`].
    ///
    /// Propagates the updated glossary to all active source [`ContextWindow`]s (and future
    /// sources) and forwards normalized hotwords to [`SttEngine::update_hotwords`].
    pub async fn set_glossary(
        &mut self,
        glossary: impl Into<MergedGlossary>,
    ) -> Result<(), CoreError> {
        self.set_glossary_with_score(glossary, DEFAULT_HOTWORDS_SCORE)
            .await
    }

    /// Updates the active merged glossary at runtime with an explicit STT hotword boosting score.
    pub async fn set_glossary_with_score(
        &mut self,
        glossary: impl Into<MergedGlossary>,
        hotwords_score: f32,
    ) -> Result<(), CoreError> {
        let merged = glossary.into();
        let hotwords_config = merged.to_stt_hotwords_config(hotwords_score);
        self.stt_engine.update_hotwords(&hotwords_config)?;
        self.active_glossary = merged.clone();
        for ctx in self.source_contexts.values() {
            ctx.lock().await.set_glossary(merged.clone());
        }
        Ok(())
    }

    /// Returns the currently active [`MergedGlossary`].
    #[must_use]
    pub const fn active_glossary(&self) -> &MergedGlossary {
        &self.active_glossary
    }

    /// Returns read-only access to the underlying [`SourceSessionManager`].
    #[must_use]
    pub const fn session_manager(&self) -> &SourceSessionManager {
        &self.session_manager
    }

    /// Returns the number of currently active per-speaker STT streams.
    #[must_use]
    pub fn active_stream_count(&self) -> usize {
        self.speaker_streams.len()
    }

    /// Runs the pipeline loop consuming events from `rx` until the channel closes, then awaits all
    /// in-flight translation tasks.
    pub async fn run(&mut self, mut rx: mpsc::Receiver<AudioInputEvent>) -> Result<(), CoreError> {
        let mut translation_tasks = JoinSet::new();

        while let Some(event) = rx.recv().await {
            self.process_event(event, &mut translation_tasks).await?;
            // Reap completed translation tasks promptly without blocking ingestion.
            while let Some(res) = translation_tasks.try_join_next() {
                if let Err(join_err) = res {
                    return Err(CoreError::Translation(format!(
                        "translation task panicked or was cancelled: {join_err}"
                    )));
                }
            }
        }

        while let Some(res) = translation_tasks.join_next().await {
            match res {
                Ok(Ok(())) => {}
                Ok(Err(err)) => return Err(err),
                Err(join_err) => {
                    return Err(CoreError::Translation(format!(
                        "translation task panicked or was cancelled: {join_err}"
                    )));
                }
            }
        }

        Ok(())
    }

    /// Processes a single [`AudioInputEvent`], spawning translation work onto `translation_tasks`.
    pub async fn process_event(
        &mut self,
        event: AudioInputEvent,
        translation_tasks: &mut JoinSet<Result<(), CoreError>>,
    ) -> Result<(), CoreError> {
        let action = self.session_manager.handle_event(&event);
        let now_ms = self.next_timestamp_ms();

        match action {
            SessionAction::RemoveSpeaker(speaker_key) => {
                self.flush_and_drop_speaker(&speaker_key, now_ms, translation_tasks)
                    .await?;
            }
            SessionAction::ClearSpeakers {
                source,
                removed_speakers,
            } => {
                for key in removed_speakers {
                    self.flush_and_drop_speaker(&key, now_ms, translation_tasks)
                        .await?;
                }
                if let Some(ctx) = self.source_contexts.get(&source) {
                    ctx.lock().await.clear_history();
                }
                self.subtitle_sink.clear_source(&source).await?;
            }
            SessionAction::Connected { .. }
            | SessionAction::None
            | SessionAction::AudioSequenceChecked(_) => {}
        }

        if let AudioInputEvent::AudioFrame(chunk) = event {
            let samples_16k = (self.resample_fn)(&chunk)?;
            if !samples_16k.is_empty() {
                let stream = self.get_or_create_stream(&chunk.speaker_key)?;
                let events = stream.accept_audio(&samples_16k, now_ms)?;
                self.handle_transcript_events(events, translation_tasks)
                    .await?;
            }
        } else if let AudioInputEvent::TalkingStateChanged { speaker_key, state } = event {
            if !state.is_speaking() {
                if let Some(stream) = self.speaker_streams.get_mut(&speaker_key) {
                    let events = stream.flush(now_ms)?;
                    self.handle_transcript_events(events, translation_tasks)
                        .await?;
                }
            }
        }

        Ok(())
    }

    async fn flush_and_drop_speaker(
        &mut self,
        speaker_key: &SpeakerKey,
        timestamp_ms: u64,
        translation_tasks: &mut JoinSet<Result<(), CoreError>>,
    ) -> Result<(), CoreError> {
        if let Some(mut stream) = self.speaker_streams.remove(speaker_key) {
            let events = stream.flush(timestamp_ms)?;
            self.handle_transcript_events(events, translation_tasks)
                .await?;
        }
        Ok(())
    }

    fn get_or_create_stream(
        &mut self,
        speaker_key: &SpeakerKey,
    ) -> Result<&mut Box<dyn SttStream>, CoreError> {
        use std::collections::hash_map::Entry;
        match self.speaker_streams.entry(speaker_key.clone()) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                let stream = self
                    .stt_engine
                    .create_stream(speaker_key.clone(), &self.config.src_lang)?;
                Ok(entry.insert(stream))
            }
        }
    }

    async fn handle_transcript_events(
        &mut self,
        events: Vec<TranscriptEvent>,
        translation_tasks: &mut JoinSet<Result<(), CoreError>>,
    ) -> Result<(), CoreError> {
        for event in events {
            let speaker_name = self
                .session_manager
                .speaker_display_name(&event.speaker_key);
            let speaker_color = event.speaker_key.color_hex().to_owned();

            if !event.is_final {
                self.subtitle_sink
                    .emit(SubtitleEntry {
                        utterance_id: event.utterance_id,
                        speaker_key: event.speaker_key,
                        speaker_name,
                        speaker_color,
                        src_text: event.text,
                        translated_text: None,
                        stage: SubtitleStage::Interim,
                        timestamp_ms: event.timestamp_ms,
                    })
                    .await?;
                continue;
            }

            // Phase B: Final STT always updates the card first.
            self.subtitle_sink
                .emit(SubtitleEntry {
                    utterance_id: event.utterance_id,
                    speaker_key: event.speaker_key.clone(),
                    speaker_name: speaker_name.clone(),
                    speaker_color: speaker_color.clone(),
                    src_text: event.text.clone(),
                    translated_text: None,
                    stage: SubtitleStage::FinalPendingTranslation,
                    timestamp_ms: event.timestamp_ms,
                })
                .await?;

            if !self.pre_filter.should_translate(&event.text) {
                continue;
            }

            let ctx_arc = self.source_context(&event.speaker_key.source);
            let context_snapshot = ctx_arc.lock().await.snapshot();

            let req = TranslationRequest {
                utterance_id: event.utterance_id,
                speaker_key: event.speaker_key.clone(),
                speaker_name: speaker_name.clone(),
                text: event.text.clone(),
                src_lang: self.config.src_lang.clone(),
                tgt_lang: self.config.tgt_lang.clone(),
                context: context_snapshot,
            };

            let semaphore = Arc::clone(&self.translation_semaphore);
            let translator_manager = Arc::clone(&self.translator_manager);
            let sink = Arc::clone(&self.subtitle_sink);
            let timestamp_ms = event.timestamp_ms;

            translation_tasks.spawn(async move {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .map_err(|_| CoreError::ChannelClosed("translation semaphore closed"))?;

                let result = translator_manager.translate(req.clone()).await?;

                {
                    let mut guard = ctx_arc.lock().await;
                    guard.push_turn(HistoryEntry {
                        speaker_name: speaker_name.clone(),
                        original_text: req.text.clone(),
                        translated_text: result.translated_text.clone(),
                    });
                }

                // Phase C: Translated subtitle update correlated by utterance_id.
                sink.emit(SubtitleEntry {
                    utterance_id: result.utterance_id,
                    speaker_key: req.speaker_key,
                    speaker_name,
                    speaker_color,
                    src_text: req.text,
                    translated_text: Some(result.translated_text),
                    stage: SubtitleStage::Translated,
                    timestamp_ms,
                })
                .await
            });
        }
        Ok(())
    }

    fn source_context(&mut self, source: &str) -> Arc<Mutex<ContextWindow>> {
        let cap = self.config.context_window_capacity;
        let active_glossary = &self.active_glossary;
        Arc::clone(
            self.source_contexts
                .entry(source.to_owned())
                .or_insert_with(|| {
                    let mut window = ContextWindow::new(cap);
                    window.set_glossary(active_glossary.clone());
                    Arc::new(Mutex::new(window))
                }),
        )
    }

    fn next_timestamp_ms(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let prev = self.clock_ms.load(Ordering::Relaxed);
        let next = now.max(prev);
        self.clock_ms.store(next, Ordering::Relaxed);
        next
    }
}
