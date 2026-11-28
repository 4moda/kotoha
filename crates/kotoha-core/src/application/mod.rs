//! Application Layer: Use Cases & Pipeline Orchestration (depends ONLY on `domain`).

pub mod glossary_manager;
pub mod pipeline;
pub mod session;
pub mod suppressible_sink;
pub mod translator_manager;

pub use glossary_manager::GlossaryManager;
pub use pipeline::{
    PipelineConfig, PipelineService, PipelineStats, PipelineStatsHandle, ResamplerFactory,
    TRANSLATION_SKIPPED_REASON, TRANSLATOR_CRASHED_REASON,
};
pub use session::{SeqStatus, SessionAction, SourceSessionManager, SourceStats};
pub use suppressible_sink::SuppressibleSubtitleSink;
pub use translator_manager::TranslatorManager;
