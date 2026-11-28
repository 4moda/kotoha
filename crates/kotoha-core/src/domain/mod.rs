//! Innermost Domain Layer: entities, value objects, pure rules, and port traits.
//!
//! Contains zero I/O or transport dependencies (`tokio::net`, `interprocess`, HTTP, ML runtimes).

pub mod entities;
pub mod error;
pub mod logic;
pub mod ports;
pub mod secret;

pub use entities::{
    default_model_catalog, url_origin, AudioChunk, AudioInputEvent, ComputeDevice,
    DisconnectReason, GlossaryEntry, GlossaryProfile, GlossarySettings, HistoryEntry, LanguageCode,
    MergedGlossary, ModelBackend, ModelProfile, PreprocessedAudio, SpeakerKey, SttDecodingMethod,
    SttHotwordsConfig, SubtitleEntry, SubtitleStage, TalkingState, TranscriptEvent,
    TranslateContext, TranslationProviderConfig, TranslationRequest, TranslationResult,
    UtteranceId, DEFAULT_HOTWORDS_SCORE,
};
pub use error::{CoreError, ErrorScope};
pub use logic::{
    protect_terms, restore_terms, ContextWindow, GlossaryPlaceholderGuard, PreFilter,
    DEFAULT_CONTEXT_WINDOW_CAPACITY,
};
pub use ports::{
    GlossaryRepository, Resampler, SecretStore, SecretWriter, SttEngine, SttStream, SubtitleSink,
    Translator, TranslatorFactory,
};
pub use secret::{ProviderSecretKind, Secret, SecretRef, MAX_SECRET_REF_LEN, REDACTED};
