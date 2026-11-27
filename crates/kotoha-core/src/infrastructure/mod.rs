//! Infrastructure Layer: concrete adapters for IPC, audio DSP, and Phase 1 mocks.

pub mod audio;
pub mod glossary_toml;
pub mod ipc_server;
pub mod mock_adapters;

pub use audio::{AudioPreprocessor, TARGET_SAMPLE_RATE};
pub use glossary_toml::TomlGlossaryRepository;
pub use ipc_server::{connect_local_socket, IpcServerAdapter, IpcServerConfig};
pub use mock_adapters::{
    CollectingSubtitleSink, DefaultTranslatorFactory, MockSttEngine, MockTranslator,
};
