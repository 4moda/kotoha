//! Application pipeline orchestrating input events, preprocessing, per-speaker STT,
//! pre-filtering, context injection, bounded parallel translation, and 3-stage subtitle output.
//!
//! # Error policy
//!
//! A single failing adapter call never stops the pipeline. Every error is classified with
//! [`CoreError::scope`], bounded by the call site with [`ErrorScope::at_most`], and recovered from
//! at that scope:
//!
//! | Call site | Scope ceiling | Recovery |
//! |---|---|---|
//! | `ResampleFn` | [`ErrorScope::Chunk`] | drop the chunk |
//! | `SttEngine::create_stream` / `SttStream::accept_audio` | [`ErrorScope::Speaker`] | `Chunk`: drop the chunk; otherwise drop the speaker's stream (recreated on the next chunk) |
//! | `SttStream::flush` | [`ErrorScope::Speaker`] | drop the speaker's stream |
//! | `Translator::translate` (error or panic) | [`ErrorScope::Utterance`] | emit [`SubtitleStage::TranslationFailed`]; the context window does not record the turn |
//! | `SubtitleSink::emit` / `clear_source` | [`ErrorScope::Utterance`] | lose that update only |
//! | translation task panic (`JoinError`) | [`ErrorScope::Utterance`] | count it and continue |
//!
//! Only pipeline-owned invariants (e.g. the translation semaphore being closed) are
//! [`ErrorScope::Fatal`] and end [`PipelineService::run`]. Non-fatal errors are counted in
//! [`PipelineStats`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{mpsc, Mutex, RwLock, Semaphore};
use tokio::task::{AbortHandle, JoinError, JoinSet};

use crate::application::session::{SessionAction, SourceSessionManager};
use crate::application::translator_manager::TranslatorManager;
use crate::domain::{
    AudioChunk, AudioInputEvent, ContextWindow, CoreError, ErrorScope, HistoryEntry, LanguageCode,
    MergedGlossary, PreFilter, SpeakerKey, SttEngine, SttStream, SubtitleEntry, SubtitleSink,
    SubtitleStage, TranscriptEvent, TranslationRequest, DEFAULT_HOTWORDS_SCORE,
};

/// Audio normalization callback converting raw interleaved [`AudioChunk`] into 16 kHz mono `f32`.
///
/// Defined as a pure function/closure boundary so `application` does not depend on concrete
/// DSP or infrastructure adapters directly.
pub type ResampleFn = Arc<dyn Fn(&AudioChunk) -> Result<Vec<f32>, CoreError> + Send + Sync>;

/// User-safe reason attached to [`SubtitleStage::TranslationFailed`] when the translator panicked.
const TRANSLATOR_CRASHED_REASON: &str = "translation engine crashed";

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

/// Point-in-time snapshot of the non-fatal errors the pipeline recovered from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PipelineStats {
    /// Utterances whose translation failed (error or translator panic) and were reported with
    /// [`SubtitleStage::TranslationFailed`].
    pub translation_failures: u64,
    /// STT errors from `create_stream`, `accept_audio`, or `flush`.
    pub stt_errors: u64,
    /// Speaker STT streams dropped because of an error (recreated on the next chunk).
    pub stt_stream_resets: u64,
    /// Audio chunks lost to an error (resample failure or STT error while handling the chunk).
    pub dropped_chunks: u64,
    /// Failed `SubtitleSink::emit` / `clear_source` calls.
    pub sink_errors: u64,
    /// Panicked tasks (translator panics and translation task panics).
    pub task_panics: u64,
}

#[derive(Debug, Default)]
struct StatsCounters {
    translation_failures: AtomicU64,
    stt_errors: AtomicU64,
    stt_stream_resets: AtomicU64,
    dropped_chunks: AtomicU64,
    sink_errors: AtomicU64,
    task_panics: AtomicU64,
}

/// Cheaply clonable, lock-free handle to the pipeline's error counters.
///
/// Obtain it with [`PipelineService::stats_handle`] before moving the service into a task, then
/// read [`PipelineStatsHandle::snapshot`] while [`PipelineService::run`] is executing.
#[derive(Debug, Clone, Default)]
pub struct PipelineStatsHandle(Arc<StatsCounters>);

impl PipelineStatsHandle {
    /// Returns the current counter values.
    #[must_use]
    pub fn snapshot(&self) -> PipelineStats {
        let c = &self.0;
        PipelineStats {
            translation_failures: c.translation_failures.load(Ordering::Relaxed),
            stt_errors: c.stt_errors.load(Ordering::Relaxed),
            stt_stream_resets: c.stt_stream_resets.load(Ordering::Relaxed),
            dropped_chunks: c.dropped_chunks.load(Ordering::Relaxed),
            sink_errors: c.sink_errors.load(Ordering::Relaxed),
            task_panics: c.task_panics.load(Ordering::Relaxed),
        }
    }

    fn incr(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn translation_failure(&self) {
        Self::incr(&self.0.translation_failures);
    }

    fn stt_error(&self) {
        Self::incr(&self.0.stt_errors);
    }

    fn stt_stream_reset(&self) {
        Self::incr(&self.0.stt_stream_resets);
    }

    fn dropped_chunk(&self) {
        Self::incr(&self.0.dropped_chunks);
    }

    fn sink_error(&self) {
        Self::incr(&self.0.sink_errors);
    }

    fn task_panic(&self) {
        Self::incr(&self.0.task_panics);
    }

    /// Records the outcome of a `SubtitleSink` call; sink errors never exceed
    /// [`ErrorScope::Utterance`], so they are counted and swallowed.
    fn record_sink(&self, result: &Result<(), CoreError>) {
        if result.is_err() {
            self.sink_error();
        }
    }
}

struct SourcePipelineState {
    epoch: u64,
    context: Arc<Mutex<ContextWindow>>,
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
    speaker_streams: HashMap<SpeakerKey, std::sync::Mutex<Box<dyn SttStream>>>,
    source_states: Arc<RwLock<HashMap<String, SourcePipelineState>>>,
    translation_semaphore: Arc<Semaphore>,
    clock_ms: AtomicU64,
    stats: PipelineStatsHandle,
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
            source_states: Arc::new(RwLock::new(HashMap::new())),
            translation_semaphore: Arc::new(Semaphore::new(concurrency)),
            clock_ms: AtomicU64::new(0),
            stats: PipelineStatsHandle::default(),
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
        let contexts: Vec<Arc<Mutex<ContextWindow>>> = {
            let states = self.source_states.read().await;
            states.values().map(|s| Arc::clone(&s.context)).collect()
        };
        for ctx in contexts {
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

    /// Returns a snapshot of the non-fatal error counters.
    #[must_use]
    pub fn stats(&self) -> PipelineStats {
        self.stats.snapshot()
    }

    /// Returns a shared handle to the error counters that stays readable after the service has
    /// been moved into a task running [`Self::run`].
    #[must_use]
    pub fn stats_handle(&self) -> PipelineStatsHandle {
        self.stats.clone()
    }

    /// Runs the pipeline loop consuming events from `rx` until the channel closes, then awaits all
    /// in-flight translation tasks.
    ///
    /// Returns `Ok(())` when the input channel closes. Returns `Err` only for
    /// [`ErrorScope::Fatal`] errors; every other error is recovered from (see the module docs).
    pub async fn run(&mut self, mut rx: mpsc::Receiver<AudioInputEvent>) -> Result<(), CoreError> {
        let mut translation_tasks = JoinSet::new();

        while let Some(event) = rx.recv().await {
            self.process_event(event, &mut translation_tasks).await?;
            // Reap completed translation tasks promptly without blocking ingestion.
            while let Some(res) = translation_tasks.try_join_next() {
                self.handle_translation_task_result(res)?;
            }
        }

        while let Some(res) = translation_tasks.join_next().await {
            self.handle_translation_task_result(res)?;
        }

        Ok(())
    }

    fn handle_translation_task_result(
        &self,
        res: Result<Result<(), CoreError>, JoinError>,
    ) -> Result<(), CoreError> {
        match res {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => {
                if err.scope().is_fatal() {
                    return Err(err);
                }
                // Translation tasks report utterance failures themselves; anything else that
                // escapes is still bounded to that utterance.
                self.stats.translation_failure();
                Ok(())
            }
            Err(join_err) => {
                // A panicking task loses at most its own utterance; cancellation is benign.
                if join_err.is_panic() {
                    self.stats.task_panic();
                }
                Ok(())
            }
        }
    }

    /// Processes a single [`AudioInputEvent`], spawning translation work onto `translation_tasks`.
    ///
    /// Non-fatal adapter errors are recovered from internally (see the module docs); `Err` is
    /// returned only for [`ErrorScope::Fatal`] conditions.
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
                    .await;
            }
            SessionAction::ClearSpeakers {
                source,
                removed_speakers,
            } => {
                for key in removed_speakers {
                    self.flush_and_drop_speaker(&key, now_ms, translation_tasks)
                        .await;
                }
                advance_source_epoch_and_clear(&self.source_states, &source).await;
                self.stats
                    .record_sink(&self.subtitle_sink.clear_source(&source).await);
            }
            SessionAction::Connected { .. }
            | SessionAction::None
            | SessionAction::AudioSequenceChecked(_) => {}
        }

        if let AudioInputEvent::AudioFrame { conn_id, chunk } = event {
            if self
                .session_manager
                .is_owner(conn_id, &chunk.speaker_key.source)
            {
                let Ok(samples_16k) = (self.resample_fn)(&chunk) else {
                    // Resample failures are bounded to the chunk.
                    self.stats.dropped_chunk();
                    return Ok(());
                };
                if !samples_16k.is_empty() {
                    let events = self.accept_audio(&chunk.speaker_key, &samples_16k, now_ms);
                    self.dispatch_transcript_events(events, translation_tasks)
                        .await;
                }
            }
        } else if let AudioInputEvent::TalkingStateChanged {
            conn_id,
            speaker_key,
            state,
        } = event
        {
            if self.session_manager.is_owner(conn_id, &speaker_key.source) && !state.is_speaking() {
                let events = self.flush_speaker(&speaker_key, now_ms);
                self.dispatch_transcript_events(events, translation_tasks)
                    .await;
            }
        }

        Ok(())
    }

    /// Feeds audio into the speaker's STT stream, recovering from STT errors.
    ///
    /// A [`ErrorScope::Chunk`] error drops only the chunk; anything wider drops the speaker's
    /// stream so that the next chunk starts from a fresh one.
    fn accept_audio(
        &mut self,
        speaker_key: &SpeakerKey,
        samples_16k: &[f32],
        now_ms: u64,
    ) -> Vec<TranscriptEvent> {
        let result = self
            .get_or_create_stream(speaker_key)
            .and_then(|stream| stream.accept_audio(samples_16k, now_ms));
        match result {
            Ok(events) => events,
            Err(err) => {
                self.stats.stt_error();
                self.stats.dropped_chunk();
                if err.scope().at_most(ErrorScope::Speaker) > ErrorScope::Chunk
                    && self.speaker_streams.remove(speaker_key).is_some()
                {
                    self.stats.stt_stream_reset();
                }
                Vec::new()
            }
        }
    }

    /// Flushes the speaker's STT stream (if any) on end of speech; on error the stream is dropped.
    fn flush_speaker(&mut self, speaker_key: &SpeakerKey, now_ms: u64) -> Vec<TranscriptEvent> {
        let Some(stream_cell) = self.speaker_streams.get_mut(speaker_key) else {
            return Vec::new();
        };
        let result = stream_cell
            .get_mut()
            .map_err(|_| CoreError::Stt("speaker STT stream mutex poisoned".to_owned()))
            .and_then(|stream| stream.flush(now_ms));
        result.unwrap_or_else(|_| {
            self.stats.stt_error();
            self.speaker_streams.remove(speaker_key);
            self.stats.stt_stream_reset();
            Vec::new()
        })
    }

    async fn flush_and_drop_speaker(
        &mut self,
        speaker_key: &SpeakerKey,
        timestamp_ms: u64,
        translation_tasks: &mut JoinSet<Result<(), CoreError>>,
    ) {
        let Some(mut stream_cell) = self.speaker_streams.remove(speaker_key) else {
            return;
        };
        let result = stream_cell
            .get_mut()
            .map_err(|_| CoreError::Stt("speaker STT stream mutex poisoned".to_owned()))
            .and_then(|stream| stream.flush(timestamp_ms));
        match result {
            Ok(events) => {
                self.dispatch_transcript_events(events, translation_tasks)
                    .await;
            }
            // The stream is being dropped anyway; only the pending utterance is lost.
            Err(_) => self.stats.stt_error(),
        }
    }

    fn get_or_create_stream(
        &mut self,
        speaker_key: &SpeakerKey,
    ) -> Result<&mut Box<dyn SttStream>, CoreError> {
        use std::collections::hash_map::Entry;
        let cell = match self.speaker_streams.entry(speaker_key.clone()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let stream = self
                    .stt_engine
                    .create_stream(speaker_key.clone(), &self.config.src_lang)?;
                entry.insert(std::sync::Mutex::new(stream))
            }
        };
        cell.get_mut()
            .map_err(|_| CoreError::Stt("speaker STT stream mutex poisoned".to_owned()))
    }

    async fn dispatch_transcript_events(
        &self,
        events: Vec<TranscriptEvent>,
        translation_tasks: &mut JoinSet<Result<(), CoreError>>,
    ) {
        for event in events {
            let speaker_name = self
                .session_manager
                .speaker_display_name(&event.speaker_key);
            let speaker_color = event.speaker_key.color_hex().to_owned();

            if !event.is_final {
                let result = self
                    .subtitle_sink
                    .emit(SubtitleEntry {
                        utterance_id: event.utterance_id,
                        speaker_key: event.speaker_key,
                        speaker_name,
                        speaker_color,
                        src_text: event.text,
                        translated_text: None,
                        stage: SubtitleStage::Interim,
                        failure_reason: None,
                        timestamp_ms: event.timestamp_ms,
                    })
                    .await;
                self.stats.record_sink(&result);
                continue;
            }

            // Phase B: Final STT always updates the card first.
            let result = self
                .subtitle_sink
                .emit(SubtitleEntry {
                    utterance_id: event.utterance_id,
                    speaker_key: event.speaker_key.clone(),
                    speaker_name: speaker_name.clone(),
                    speaker_color: speaker_color.clone(),
                    src_text: event.text.clone(),
                    translated_text: None,
                    stage: SubtitleStage::FinalPendingTranslation,
                    failure_reason: None,
                    timestamp_ms: event.timestamp_ms,
                })
                .await;
            self.stats.record_sink(&result);

            if !self.pre_filter.should_translate(&event.text) {
                continue;
            }

            let (source_epoch, context) = source_state(
                &self.source_states,
                &event.speaker_key.source,
                self.config.context_window_capacity,
                &self.active_glossary,
            )
            .await;
            let context_snapshot = context.lock().await.snapshot();

            let job = TranslationJob {
                req: TranslationRequest {
                    utterance_id: event.utterance_id,
                    speaker_key: event.speaker_key,
                    speaker_name: speaker_name.clone(),
                    text: event.text,
                    src_lang: self.config.src_lang.clone(),
                    tgt_lang: self.config.tgt_lang.clone(),
                    context: context_snapshot,
                },
                speaker_name,
                speaker_color,
                timestamp_ms: event.timestamp_ms,
                source_epoch,
                context,
            };
            let deps = TranslationDeps {
                semaphore: Arc::clone(&self.translation_semaphore),
                translator_manager: Arc::clone(&self.translator_manager),
                sink: Arc::clone(&self.subtitle_sink),
                source_states: Arc::clone(&self.source_states),
                stats: self.stats.clone(),
            };
            translation_tasks.spawn(run_translation_job(job, deps));
        }
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

/// One finalized utterance waiting for translation.
struct TranslationJob {
    req: TranslationRequest,
    speaker_name: String,
    speaker_color: String,
    timestamp_ms: u64,
    source_epoch: u64,
    context: Arc<Mutex<ContextWindow>>,
}

/// Shared collaborators needed to run a [`TranslationJob`] outside the event loop.
struct TranslationDeps {
    semaphore: Arc<Semaphore>,
    translator_manager: Arc<TranslatorManager>,
    sink: Arc<dyn SubtitleSink>,
    source_states: Arc<RwLock<HashMap<String, SourcePipelineState>>>,
    stats: PipelineStatsHandle,
}

/// Aborts the wrapped task when dropped, so a cancelled job does not leave its translator call
/// running in the background.
struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Translates one utterance and emits Phase C ([`SubtitleStage::Translated`] or
/// [`SubtitleStage::TranslationFailed`]).
///
/// Translator errors and panics are bounded to [`ErrorScope::Utterance`]: whatever variant the
/// adapter returns, only this utterance fails. Returns `Err` only for pipeline invariants.
async fn run_translation_job(job: TranslationJob, deps: TranslationDeps) -> Result<(), CoreError> {
    let TranslationJob {
        req,
        speaker_name,
        speaker_color,
        timestamp_ms,
        source_epoch,
        context,
    } = job;

    let _permit = deps
        .semaphore
        .acquire_owned()
        .await
        .map_err(|_| CoreError::ChannelClosed("translation semaphore closed"))?;

    if is_source_epoch_stale(&deps.source_states, &req.speaker_key.source, source_epoch).await {
        return Ok(());
    }

    // Run the adapter call in its own task so that a panicking translator only fails this
    // utterance instead of losing the card in a `JoinError` without an utterance id.
    let translator_manager = Arc::clone(&deps.translator_manager);
    let translate_req = req.clone();
    let handle = tokio::spawn(async move { translator_manager.translate(translate_req).await });
    let _abort_guard = AbortOnDrop(handle.abort_handle());
    let outcome = match handle.await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(err)) => Err(err.translation_failure_reason()),
        Err(join_err) => {
            if join_err.is_panic() {
                deps.stats.task_panic();
            }
            Err(TRANSLATOR_CRASHED_REASON)
        }
    };

    if is_source_epoch_stale(&deps.source_states, &req.speaker_key.source, source_epoch).await {
        return Ok(());
    }

    let entry = match outcome {
        Ok(result) => {
            // Only successful turns enter the context window.
            context.lock().await.push_turn(HistoryEntry {
                speaker_name: speaker_name.clone(),
                original_text: req.text.clone(),
                translated_text: result.translated_text.clone(),
            });
            // Phase C: Translated subtitle update correlated by utterance_id.
            SubtitleEntry {
                utterance_id: result.utterance_id,
                speaker_key: req.speaker_key,
                speaker_name,
                speaker_color,
                src_text: req.text,
                translated_text: Some(result.translated_text),
                stage: SubtitleStage::Translated,
                failure_reason: None,
                timestamp_ms,
            }
        }
        Err(reason) => {
            deps.stats.translation_failure();
            SubtitleEntry {
                utterance_id: req.utterance_id,
                speaker_key: req.speaker_key,
                speaker_name,
                speaker_color,
                src_text: req.text,
                translated_text: None,
                stage: SubtitleStage::TranslationFailed,
                failure_reason: Some(reason.to_owned()),
                timestamp_ms,
            }
        }
    };
    deps.stats.record_sink(&deps.sink.emit(entry).await);
    Ok(())
}

async fn source_state(
    source_states: &RwLock<HashMap<String, SourcePipelineState>>,
    source: &str,
    context_window_capacity: usize,
    active_glossary: &MergedGlossary,
) -> (u64, Arc<Mutex<ContextWindow>>) {
    {
        let states = source_states.read().await;
        if let Some(state) = states.get(source) {
            return (state.epoch, Arc::clone(&state.context));
        }
    }

    let mut states = source_states.write().await;
    let state = states.entry(source.to_owned()).or_insert_with(|| {
        let mut window = ContextWindow::new(context_window_capacity);
        window.set_glossary(active_glossary.clone());
        SourcePipelineState {
            epoch: 0,
            context: Arc::new(Mutex::new(window)),
        }
    });
    let out = (state.epoch, Arc::clone(&state.context));
    drop(states);
    out
}

async fn advance_source_epoch_and_clear(
    source_states: &RwLock<HashMap<String, SourcePipelineState>>,
    source: &str,
) {
    let context_to_clear = {
        let mut states = source_states.write().await;
        if let Some(state) = states.get_mut(source) {
            state.epoch = state.epoch.saturating_add(1);
            Some(Arc::clone(&state.context))
        } else {
            None
        }
    };
    if let Some(ctx) = context_to_clear {
        ctx.lock().await.clear_history();
    }
}

async fn is_source_epoch_stale(
    source_states: &RwLock<HashMap<String, SourcePipelineState>>,
    source: &str,
    expected_epoch: u64,
) -> bool {
    let states = source_states.read().await;
    states
        .get(source)
        .is_some_and(|state| state.epoch != expected_epoch)
}
