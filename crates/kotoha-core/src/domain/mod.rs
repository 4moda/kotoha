//! Innermost Domain Layer: entities, value objects, pure rules, and port traits.
//!
//! Contains zero I/O or transport dependencies (`tokio::net`, `interprocess`, HTTP, ML runtimes).

pub mod entities;
pub mod error;
pub mod logic;
pub mod ports;

pub use entities::{
    default_model_catalog, AudioChunk, AudioInputEvent, ComputeDevice, GlossaryEntry, HistoryEntry,
    LanguageCode, ModelBackend, ModelProfile, PreprocessedAudio, SpeakerKey, SubtitleEntry,
    SubtitleStage, TalkingState, TranscriptEvent, TranslateContext, TranslationProviderConfig,
    TranslationRequest, TranslationResult, UtteranceId,
};
pub use error::CoreError;
pub use logic::{ContextWindow, PreFilter, DEFAULT_CONTEXT_WINDOW_CAPACITY};
pub use ports::{SttEngine, SttStream, SubtitleSink, Translator, TranslatorFactory};
