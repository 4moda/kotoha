//! Mock adapters and Phase 1 translator factory for testing and CLI/E2E verification.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::domain::{
    CoreError, LanguageCode, SpeakerKey, SttEngine, SttStream, SubtitleEntry, SubtitleSink,
    TranscriptEvent, TranslationProviderConfig, TranslationRequest, TranslationResult, Translator,
    TranslatorFactory, UtteranceId,
};
use crate::infrastructure::audio::AudioPreprocessor;

/// Deterministic mock [`SttEngine`] for Phase 1 testing and CLI/E2E verification.
///
/// Generates interim hypotheses from non-silent 16 kHz chunks and finalizes utterances on flush
/// (silence / talking stop / user removal / source reset) or after `finalize_every_frames` active
/// audio frames.
#[derive(Debug)]
pub struct MockSttEngine {
    next_utterance_id: Arc<AtomicU64>,
    rms_threshold: f32,
    finalize_every_frames: usize,
}

impl Default for MockSttEngine {
    fn default() -> Self {
        Self::new(0.005, 4)
    }
}

impl MockSttEngine {
    /// Creates a configurable [`MockSttEngine`].
    #[must_use]
    pub fn new(rms_threshold: f32, finalize_every_frames: usize) -> Self {
        Self {
            next_utterance_id: Arc::new(AtomicU64::new(1)),
            rms_threshold,
            finalize_every_frames: finalize_every_frames.max(1),
        }
    }
}

impl SttEngine for MockSttEngine {
    fn name(&self) -> &'static str {
        "mock-stt"
    }

    fn create_stream(
        &self,
        speaker_key: SpeakerKey,
        src_lang: &LanguageCode,
    ) -> Result<Box<dyn SttStream>, CoreError> {
        Ok(Box::new(MockSttStream {
            speaker_key,
            src_lang: src_lang.clone(),
            next_utterance_id: Arc::clone(&self.next_utterance_id),
            rms_threshold: self.rms_threshold,
            finalize_every_frames: self.finalize_every_frames,
            active_utterance: None,
            active_frames: 0,
            accumulated_samples: 0,
        }))
    }
}

struct MockSttStream {
    speaker_key: SpeakerKey,
    src_lang: LanguageCode,
    next_utterance_id: Arc<AtomicU64>,
    rms_threshold: f32,
    finalize_every_frames: usize,
    active_utterance: Option<UtteranceId>,
    active_frames: usize,
    accumulated_samples: usize,
}

impl SttStream for MockSttStream {
    fn accept_audio(
        &mut self,
        samples_16k_mono: &[f32],
        timestamp_ms: u64,
    ) -> Result<Vec<TranscriptEvent>, CoreError> {
        if samples_16k_mono.is_empty() {
            return Ok(Vec::new());
        }

        let rms = AudioPreprocessor::compute_rms(samples_16k_mono);
        if rms < self.rms_threshold {
            return self.flush(timestamp_ms);
        }

        let utt_id = *self.active_utterance.get_or_insert_with(|| {
            UtteranceId(self.next_utterance_id.fetch_add(1, Ordering::Relaxed))
        });
        self.active_frames += 1;
        self.accumulated_samples += samples_16k_mono.len();

        let text = format!(
            "[mock-stt:{}:{}] speech frames={} samples={}",
            self.src_lang.as_str(),
            self.speaker_key.user_id,
            self.active_frames,
            self.accumulated_samples
        );

        let is_final = self.active_frames >= self.finalize_every_frames;
        let event = TranscriptEvent {
            utterance_id: utt_id,
            speaker_key: self.speaker_key.clone(),
            text,
            is_final,
            timestamp_ms,
        };

        if is_final {
            self.active_utterance = None;
            self.active_frames = 0;
            self.accumulated_samples = 0;
        }

        Ok(vec![event])
    }

    fn flush(&mut self, timestamp_ms: u64) -> Result<Vec<TranscriptEvent>, CoreError> {
        let Some(utt_id) = self.active_utterance.take() else {
            return Ok(Vec::new());
        };
        let frames = self.active_frames;
        let samples = self.accumulated_samples;
        self.active_frames = 0;
        self.accumulated_samples = 0;

        Ok(vec![TranscriptEvent {
            utterance_id: utt_id,
            speaker_key: self.speaker_key.clone(),
            text: format!(
                "[mock-stt:{}:{}] speech frames={frames} samples={samples}",
                self.src_lang.as_str(),
                self.speaker_key.user_id
            ),
            is_final: true,
            timestamp_ms,
        }])
    }
}

/// Deterministic mock [`Translator`] for Phase 1 pipeline verification and E2E testing.
#[derive(Debug, Clone)]
pub struct MockTranslator {
    provider_id: String,
    prefix: String,
}

impl Default for MockTranslator {
    fn default() -> Self {
        Self::new("mock", "[ja] ")
    }
}

impl MockTranslator {
    /// Creates a [`MockTranslator`] with an explicit `provider_id` and `prefix`.
    #[must_use]
    pub fn new(provider_id: impl Into<String>, prefix: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            prefix: prefix.into(),
        }
    }
}

#[async_trait]
impl Translator for MockTranslator {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    async fn translate(&self, req: TranslationRequest) -> Result<TranslationResult, CoreError> {
        let started = Instant::now();
        let translated_text = format!("{}{}", self.prefix, req.text);
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(0);
        Ok(TranslationResult {
            utterance_id: req.utterance_id,
            original_text: req.text,
            translated_text,
            provider_id: self.provider_id.clone(),
            latency_ms,
        })
    }
}

/// Phase 1 [`TranslatorFactory`] for mock translation.
///
/// Supports [`TranslationProviderConfig::Mock`] and reports Phase 2 providers as
/// [`CoreError::UnsupportedInPhase1`].
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultTranslatorFactory;

impl TranslatorFactory for DefaultTranslatorFactory {
    fn create(&self, config: &TranslationProviderConfig) -> Result<Arc<dyn Translator>, CoreError> {
        match config {
            TranslationProviderConfig::Mock { prefix } => {
                Ok(Arc::new(MockTranslator::new("mock", prefix.clone())))
            }
            TranslationProviderConfig::LocalModel { model_id, .. } => {
                if model_id.trim().is_empty() {
                    return Err(CoreError::NotConfigured(
                        "local model_id must not be empty".to_owned(),
                    ));
                }
                Err(CoreError::UnsupportedInPhase1(format!(
                    "local CTranslate2 model '{model_id}' requires kotoha-translate-local (Phase 2)"
                )))
            }
            TranslationProviderConfig::OpenAiCompatible {
                base_url, model, ..
            } => {
                if base_url.trim().is_empty() || model.trim().is_empty() {
                    return Err(CoreError::NotConfigured(
                        "OpenAiCompatible requires non-empty base_url and model".to_owned(),
                    ));
                }
                Err(CoreError::UnsupportedInPhase1(format!(
                    "OpenAI-compatible provider '{model}' at '{base_url}' requires kotoha-translate-http (Phase 2)"
                )))
            }
            TranslationProviderConfig::DeepL { .. } => Err(CoreError::UnsupportedInPhase1(
                "DeepL provider requires kotoha-translate-http (Phase 2)".to_owned(),
            )),
            TranslationProviderConfig::GoogleCloud { .. } => Err(CoreError::UnsupportedInPhase1(
                "Google Cloud Translation provider requires kotoha-translate-http (Phase 2)"
                    .to_owned(),
            )),
            TranslationProviderConfig::GasWebhook { .. } => Err(CoreError::UnsupportedInPhase1(
                "GAS Webhook provider requires kotoha-translate-http (Phase 2)".to_owned(),
            )),
        }
    }
}

/// Thread-safe in-memory [`SubtitleSink`] recording all emitted subtitle entries and cleared sources.
#[derive(Debug, Default, Clone)]
pub struct CollectingSubtitleSink {
    entries: Arc<Mutex<Vec<SubtitleEntry>>>,
    cleared_sources: Arc<Mutex<Vec<String>>>,
}

impl CollectingSubtitleSink {
    /// Creates an empty [`CollectingSubtitleSink`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a cloned snapshot of all emitted [`SubtitleEntry`] items in arrival order.
    pub async fn entries(&self) -> Vec<SubtitleEntry> {
        self.entries.lock().await.clone()
    }

    /// Returns a cloned snapshot of all `clear_source` source names received.
    pub async fn cleared_sources(&self) -> Vec<String> {
        self.cleared_sources.lock().await.clone()
    }
}

#[async_trait]
impl SubtitleSink for CollectingSubtitleSink {
    async fn emit(&self, entry: SubtitleEntry) -> Result<(), CoreError> {
        self.entries.lock().await.push(entry);
        Ok(())
    }

    async fn clear_source(&self, source: &str) -> Result<(), CoreError> {
        self.cleared_sources.lock().await.push(source.to_owned());
        Ok(())
    }
}
