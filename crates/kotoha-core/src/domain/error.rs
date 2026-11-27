//! Typed domain, application, and adapter errors for `kotoha-core`.

use kotoha_protocol::ProtocolError;
use thiserror::Error;

/// Unified error type across the `kotoha-core` domain, application, and infrastructure layers.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Invalid language code syntax.
    #[error("invalid language code: '{0}'")]
    InvalidLanguageCode(String),

    /// Invalid audio input parameters.
    #[error("invalid audio parameters: {0}")]
    InvalidAudio(String),

    /// Protocol codec or framing error.
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),

    /// IPC transport error.
    #[error("IPC transport error: {0}")]
    Ipc(String),

    /// Speech-to-text engine error.
    #[error("STT error: {0}")]
    Stt(String),

    /// Translation provider error.
    #[error("translation error: {0}")]
    Translation(String),

    /// Requested model profile was not found in the catalog.
    #[error("unknown model profile: '{0}'")]
    UnknownModelProfile(String),

    /// Provider is not configured with required credentials or endpoints.
    #[error("provider not configured: {0}")]
    NotConfigured(String),

    /// Provider backend requires Phase 2 engine crates not yet attached.
    #[error("provider unsupported in Phase 1 core: {0}")]
    UnsupportedInPhase1(String),

    /// Subtitle sink emission failed.
    #[error("subtitle sink error: {0}")]
    Sink(String),

    /// Channel or runtime synchronization failure.
    #[error("pipeline channel closed: {0}")]
    ChannelClosed(&'static str),
}
