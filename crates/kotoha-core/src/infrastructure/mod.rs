//! Infrastructure Layer: concrete adapters for IPC, audio DSP, and Phase 1 mocks.

pub mod audio;
pub mod ipc_server;
pub mod mock_adapters;

pub use audio::{AudioPreprocessor, TARGET_SAMPLE_RATE};
pub use ipc_server::{connect_local_socket, IpcServerAdapter};
pub use mock_adapters::{
    CollectingSubtitleSink, DefaultTranslatorFactory, MockSttEngine, MockTranslator,
};
