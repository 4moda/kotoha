//! Typed errors for `kotoha-setup`.

use std::path::PathBuf;

use thiserror::Error;

/// Unified error type across Mumble plugin installation, model downloading, and settings persistence.
#[derive(Debug, Error)]
pub enum SetupError {
    /// Filesystem or OS I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// HTTP / transport / status error while downloading assets.
    #[error("network error: {0}")]
    Network(String),

    /// Downloaded or existing asset failed SHA-256 integrity verification.
    #[error("SHA-256 checksum mismatch for '{path}': expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// Path of the file whose checksum mismatched.
        path: PathBuf,
        /// Expected lowercase hex SHA-256 digest.
        expected: String,
        /// Computed lowercase hex SHA-256 digest.
        actual: String,
    },

    /// Invalid configuration, path, manifest digest, or corrupted settings payload.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// Target plugin file is locked by a running host application (e.g., Mumble).
    #[error("plugin file locked at '{}': {guidance}", path.display())]
    PluginLocked {
        /// Path to the locked plugin file.
        path: PathBuf,
        /// Human-readable guidance for resolving the lock.
        guidance: String,
    },

    /// Current or requested operating system / platform is unsupported.
    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(String),

    /// Security policy violation (e.g., plaintext HTTP on non-loopback, SSRF metadata IP, downgrade redirect, or secret leakage).
    #[error("security policy violation: {0}")]
    SecurityViolation(String),
}
