//! Streaming `sherpa-onnx` STT infrastructure adapter for Kotoha.
//!
//! Implements [`kotoha_core::domain::ports::SttEngine`] and
//! [`kotoha_core::domain::ports::SttStream`] while isolating optional native C/C++ bindings behind
//! the `native-sherpa` feature flag so unit and integration tests run instantaneously in pure Rust.

pub mod backend;
pub mod config;
pub mod engine;
pub mod hotwords;

#[cfg(feature = "native-sherpa")]
pub use backend::native::{build_native_config, NativeSherpaBackend};
pub use backend::{RecognizerBackend, RecognizerHypothesis, RecognizerStreamBackend};
pub use config::{
    compute_device_to_provider, validate_hotwords_score, SherpaEndpointConfig, SherpaModelFamily,
    SherpaModelingUnit, SherpaSttConfig, DEFAULT_FEATURE_DIM, DEFAULT_MAX_ACTIVE_PATHS,
    DEFAULT_NUM_THREADS, DEFAULT_RULE1_MIN_TRAILING_SILENCE, DEFAULT_RULE2_MIN_TRAILING_SILENCE,
    DEFAULT_RULE3_MIN_UTTERANCE_LENGTH, DEFAULT_SAMPLE_RATE,
};
pub use engine::{SherpaSttEngine, SherpaSttStream};
pub use hotwords::{
    format_hotwords_text, format_hotwords_text_with_scores, sanitize_hotwords, HotwordsRegistry,
    HotwordsSnapshot,
};
