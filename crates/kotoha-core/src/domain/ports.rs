//! Hexagonal Port traits defined by the domain layer and implemented by adapters.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::entities::{
    AudioChunk, GlossaryProfile, LanguageCode, SpeakerKey, SttHotwordsConfig, SubtitleEntry,
    TranscriptEvent, TranslationProviderConfig, TranslationRequest, TranslationResult,
};
use crate::domain::error::CoreError;
use crate::domain::secret::{Secret, SecretRef};

/// Per-speaker stateful audio normalizer: raw interleaved [`AudioChunk`] to 16 kHz mono `f32`.
///
/// One instance belongs to exactly one speaker worker, so implementations may keep filter
/// history across chunks. It runs on the worker's blocking thread. A change of sample rate or
/// channel count between chunks is handled by the implementation (history is reset).
pub trait Resampler: Send {
    /// Converts one chunk; the result may be shorter than the nominal length while the filter
    /// fills, but never repeats or drops samples over a long run.
    fn process(&mut self, chunk: &AudioChunk) -> Result<Vec<f32>, CoreError>;
}

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

    /// Returns `true` if this engine supports contextual STT hotword biasing.
    fn hotwords_supported(&self) -> bool {
        false
    }

    /// Hot-reloads contextual STT hotwords for subsequent streams (no-op by default).
    fn update_hotwords(&self, _config: &SttHotwordsConfig) -> Result<(), CoreError> {
        Ok(())
    }

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

/// Port resolving a persisted [`SecretRef`] into a [`Secret`] value.
///
/// Implementations: in-memory (tests), environment variables (CLI / E2E) and, as a follow-up,
/// the OS keychain. Implementations must never include secret values in errors or `Debug`.
pub trait SecretStore: Send + Sync + std::fmt::Debug {
    /// Resolves `secret_ref`, returning [`CoreError::SecretMissing`] if it is not set.
    fn resolve(&self, secret_ref: &SecretRef) -> Result<Secret, CoreError>;

    /// Returns `true` if `secret_ref` resolves to a non-blank value (never exposes the value).
    fn is_configured(&self, secret_ref: &SecretRef) -> bool {
        self.resolve(secret_ref).is_ok_and(|s| !s.is_blank())
    }

    /// Resolves `secret_ref`, mapping "not set" to `Ok(None)` (for optional secrets).
    fn resolve_if_set(&self, secret_ref: &SecretRef) -> Result<Option<Secret>, CoreError> {
        match self.resolve(secret_ref) {
            Ok(secret) => Ok(Some(secret)),
            Err(CoreError::SecretMissing(_)) => Ok(None),
            Err(err) => Err(err),
        }
    }
}

/// Write side of a [`SecretStore`] (set / delete). Not implemented by read-only stores such as the
/// environment-variable store.
pub trait SecretWriter: SecretStore {
    /// Stores `value` under `secret_ref`, replacing any previous value.
    fn set(&self, secret_ref: &SecretRef, value: Secret) -> Result<(), CoreError>;

    /// Deletes the value under `secret_ref` (idempotent when absent).
    fn delete(&self, secret_ref: &SecretRef) -> Result<(), CoreError>;
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

/// Repository loading custom [`GlossaryProfile`] definitions from storage.
pub trait GlossaryRepository: Send + Sync {
    /// Loads and validates a single [`GlossaryProfile`] file at `path`.
    fn load_file(&self, path: &Path) -> Result<GlossaryProfile, CoreError>;

    /// Loads all glossary profile files from `dir` in deterministic sorted filename order.
    fn load_dir(&self, dir: &Path) -> Result<Vec<GlossaryProfile>, CoreError>;
}
