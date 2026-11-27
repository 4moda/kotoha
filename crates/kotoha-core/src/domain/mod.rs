//! Innermost Domain Layer: entities, value objects, pure rules, and port traits.
//!
//! Contains zero I/O or transport dependencies (`tokio::net`, `interprocess`, HTTP, ML runtimes).

pub mod entities;
pub mod error;
pub mod logic;
pub mod ports;

pub use entities::{
    default_model_catalog, AudioChunk, AudioInputEvent, ComputeDevice, GlossaryEntry,
    GlossaryProfile, GlossarySettings, HistoryEntry, LanguageCode, MergedGlossary, ModelBackend,
    ModelProfile, PreprocessedAudio, SpeakerKey, SttDecodingMethod, SttHotwordsConfig,
    SubtitleEntry, SubtitleStage, TalkingState, TranscriptEvent, TranslateContext,
    TranslationProviderConfig, TranslationRequest, TranslationResult, UtteranceId,
    DEFAULT_HOTWORDS_SCORE,
};
pub use error::{CoreError, ErrorScope};
pub use logic::{
    protect_terms, restore_terms, ContextWindow, GlossaryPlaceholderGuard, PreFilter,
    DEFAULT_CONTEXT_WINDOW_CAPACITY,
};
pub use ports::{
    GlossaryRepository, SttEngine, SttStream, SubtitleSink, Translator, TranslatorFactory,
};
