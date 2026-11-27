//! Hexagonal Port traits defined by the domain layer and implemented by adapters.

use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::entities::{
    LanguageCode, SpeakerKey, SubtitleEntry, TranscriptEvent, TranslationProviderConfig,
    TranslationRequest, TranslationResult,
};
use crate::domain::error::CoreError;

/// Per-speaker stateful streaming speech-to-text recognizer.
pub trait SttStream: Send {
    /// Feeds 16 kHz mono `f32` PCM samples and returns zero or more transcript hypotheses.
    fn accept_audio(
        &mut self,
        samples_16k_mono: &[f32],
        timestamp_ms: u64,
    ) -> Result<Vec<TranscriptEvent>, CoreError>;

    /// Flushes any remaining buffered speech on silence, user removal, or source reset.
    fn flush(&mut self, timestamp_ms: u64) -> Result<Vec<TranscriptEvent>, CoreError>;
}

/// Factory for creating per-speaker [`SttStream`] instances.
pub trait SttEngine: Send + Sync {
    /// Engine name identifier (e.g., `"mock-stt"`, `"sherpa-onnx"`).
    fn name(&self) -> &str;

    /// Creates a new independent stream bound to `speaker_key` and `src_lang`.
    fn create_stream(
        &self,
        speaker_key: SpeakerKey,
        src_lang: &LanguageCode,
    ) -> Result<Box<dyn SttStream>, CoreError>;
}

/// Asynchronous translation provider port.
#[async_trait]
pub trait Translator: Send + Sync {
    /// Unique identifier for the active translation provider or model.
    fn provider_id(&self) -> &str;

    /// Translates the text in `req` into `req.tgt_lang`.
    async fn translate(&self, req: TranslationRequest) -> Result<TranslationResult, CoreError>;
}

/// Factory constructing a [`Translator`] instance from a runtime [`TranslationProviderConfig`].
pub trait TranslatorFactory: Send + Sync {
    /// Validates `config` and instantiates a corresponding [`Translator`].
    fn create(&self, config: &TranslationProviderConfig) -> Result<Arc<dyn Translator>, CoreError>;
}

/// Outbound sink receiving 3-stage subtitle updates and source clear events.
#[async_trait]
pub trait SubtitleSink: Send + Sync {
    /// Emits an interim, finalized, or translated subtitle update.
    async fn emit(&self, entry: SubtitleEntry) -> Result<(), CoreError>;

    /// Clears all displayed subtitles originating from `source` (e.g., on `SourceReset`).
    async fn clear_source(&self, _source: &str) -> Result<(), CoreError> {
        Ok(())
    }
}
