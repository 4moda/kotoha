//! Infrastructure Layer: concrete adapters for IPC, audio DSP, and Phase 1 mocks.

pub mod audio;
pub mod glossary_toml;
pub mod ipc_server;
pub mod mock_adapters;
pub mod secret_store;

pub use audio::{AudioPreprocessor, StreamResampler, TARGET_SAMPLE_RATE};
pub use glossary_toml::TomlGlossaryRepository;
pub use ipc_server::{
    connect_local_socket, parse_config_number, IpcDiagnostic, IpcServerAdapter, IpcServerConfig,
    ENV_IPC_HELLO_TIMEOUT_MS, ENV_IPC_IDLE_TIMEOUT_MS, ENV_IPC_MAX_CONNECTIONS,
};
pub use mock_adapters::{
    CollectingSubtitleSink, DefaultTranslatorFactory, MockSttEngine, MockTranslator,
};
pub use secret_store::{EnvSecretStore, InMemorySecretStore, ENV_SECRET_PREFIX};
