//! Reusable cross-language End-to-End (E2E) test harness for Kotoha.
//!
//! Coordinates real OS-level IPC communication between the native C++ mock plugin client
//! (`kotoha_mock_client`) and Rust listeners (`kotoha-ipc-monitor` or `IpcServerAdapter`), plus
//! optional real-audio pipeline tests combining `sherpa-onnx` streaming STT, TOML glossaries, and
//! HTTP translation.

pub mod harness;

pub use harness::{
    assert_multi_source_isolation, assert_single_source_lifecycle, E2eBinaries, E2eError,
    MockClientArgs, MockClientCommand, MockClientOutput, MonitorEvent, MonitorSession,
    UniqueIpcEndpoint,
};
