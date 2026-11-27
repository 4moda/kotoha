//! Application Layer: Use Cases & Pipeline Orchestration (depends ONLY on `domain`).

pub mod glossary_manager;
pub mod pipeline;
pub mod session;
pub mod translator_manager;

pub use glossary_manager::GlossaryManager;
pub use pipeline::{PipelineConfig, PipelineService, ResampleFn};
pub use session::{SeqStatus, SessionAction, SourceSessionManager, SourceStats};
pub use translator_manager::TranslatorManager;
