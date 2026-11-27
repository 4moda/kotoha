//! Cross-platform IPC endpoint resolution for Kotoha IPC v1.
//!
//! Resolution precedence (Section 3.1 of `DESIGN.md`):
//! 1. `KOTOHA_IPC_ENDPOINT` environment variable if set and non-empty.
//! 2. Windows default: `\\.\pipe\kotoha_v1`.
//! 3. Linux/macOS default: `$XDG_RUNTIME_DIR/kotoha_v1.sock` if `$XDG_RUNTIME_DIR` is non-empty.
//! 4. Linux/macOS fallback: `/tmp/kotoha_v1.sock`.

use std::env;

use crate::error::ProtocolError;

/// Environment variable name that overrides the default IPC endpoint.
pub const ENV_IPC_ENDPOINT: &str = "KOTOHA_IPC_ENDPOINT";

/// Windows Named Pipe prefix (case-insensitive when normalizing).
pub const WINDOWS_PIPE_PREFIX: &str = r"\\.\pipe\";

/// Default Windows Named Pipe name (without prefix).
pub const DEFAULT_WINDOWS_PIPE_NAME: &str = "kotoha_v1";

/// Default Windows Named Pipe endpoint.
pub const DEFAULT_WINDOWS_ENDPOINT: &str = r"\\.\pipe\kotoha_v1";

/// Default Unix socket filename.
pub const DEFAULT_UNIX_SOCKET_NAME: &str = "kotoha_v1.sock";

/// Default Unix fallback socket path when `$XDG_RUNTIME_DIR` is unset or empty.
pub const DEFAULT_UNIX_FALLBACK_ENDPOINT: &str = "/tmp/kotoha_v1.sock";

/// Target operating system kind for deterministic endpoint resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetOs {
    /// Microsoft Windows (Named Pipes).
    Windows,
    /// POSIX / Unix-like OS such as Linux or macOS (Unix Domain Sockets).
    Unix,
}

impl TargetOs {
    /// Returns the compile-time host target OS category.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// Resolves the IPC endpoint using the current process environment and target OS.
#[must_use]
pub fn resolve_endpoint() -> String {
    let env_override = env::var(ENV_IPC_ENDPOINT).ok();
    let xdg_runtime_dir = env::var("XDG_RUNTIME_DIR").ok();
    resolve_endpoint_with(
        TargetOs::current(),
        env_override.as_deref(),
        xdg_runtime_dir.as_deref(),
    )
}

/// Pure, testable endpoint resolution with explicit OS and environment inputs.
#[must_use]
pub fn resolve_endpoint_with(
    os: TargetOs,
    endpoint_override: Option<&str>,
    xdg_runtime_dir: Option<&str>,
) -> String {
    if let Some(val) = endpoint_override.filter(|s| !s.is_empty()) {
        return val.to_owned();
    }

    match os {
        TargetOs::Windows => DEFAULT_WINDOWS_ENDPOINT.to_owned(),
        TargetOs::Unix => xdg_runtime_dir.filter(|s| !s.is_empty()).map_or_else(
            || DEFAULT_UNIX_FALLBACK_ENDPOINT.to_owned(),
            join_unix_socket_path,
        ),
    }
}

/// Normalizes a Windows Named Pipe endpoint for `interprocess` `GenericNamespaced` (`ToNsName`).
///
/// Accepts both the standard Windows path form (`\\.\pipe\<name>`, case-insensitive prefix)
/// and a bare pipe name (`<name>`), returning the pipe name without the `\\.\pipe\` prefix.
/// Returns [`ProtocolError::InvalidEndpoint`] if the resulting pipe name is empty or contains
/// directory separators (`\` or `/`) or NUL bytes.
pub fn extract_windows_pipe_name(endpoint: &str) -> Result<&str, ProtocolError> {
    if endpoint.is_empty() {
        return Err(ProtocolError::InvalidEndpoint(
            "endpoint must not be empty".to_owned(),
        ));
    }
    if endpoint.as_bytes().contains(&0) {
        return Err(ProtocolError::InvalidEndpoint(
            "endpoint must not contain NUL bytes".to_owned(),
        ));
    }

    let stripped =
        strip_ascii_prefix_ignore_case(endpoint, WINDOWS_PIPE_PREFIX).unwrap_or(endpoint);
    if stripped.is_empty() {
        return Err(ProtocolError::InvalidEndpoint(format!(
            "missing pipe name after prefix in '{endpoint}'"
        )));
    }
    if stripped.contains('\\') || stripped.contains('/') {
        return Err(ProtocolError::InvalidEndpoint(format!(
            "windows pipe name '{stripped}' must not contain path separators"
        )));
    }

    Ok(stripped)
}

/// Resolves the Windows pipe name from the current environment, stripping `\\.\pipe\` if present.
pub fn resolve_windows_pipe_name() -> Result<String, ProtocolError> {
    let env_override = env::var(ENV_IPC_ENDPOINT).ok();
    let endpoint = resolve_endpoint_with(TargetOs::Windows, env_override.as_deref(), None);
    extract_windows_pipe_name(&endpoint).map(str::to_owned)
}

fn join_unix_socket_path(dir: &str) -> String {
    let trimmed = dir.trim_end_matches('/');
    if trimmed.is_empty() {
        format!("/{DEFAULT_UNIX_SOCKET_NAME}")
    } else {
        format!("{trimmed}/{DEFAULT_UNIX_SOCKET_NAME}")
    }
}

const fn strip_ascii_prefix_ignore_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    if value.len() < prefix.len() || !value.is_char_boundary(prefix.len()) {
        return None;
    }
    let (head, tail) = value.split_at(prefix.len());
    if head.eq_ignore_ascii_case(prefix) {
        Some(tail)
    } else {
        None
    }
}
