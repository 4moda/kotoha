//! Cross-platform IPC endpoint resolution for Kotoha IPC v1.
//!
//! Resolution precedence (Section 3.1 of `DESIGN.md`):
//! 1. `KOTOHA_IPC_ENDPOINT` environment variable if set and non-empty.
//! 2. Windows default: `\\.\pipe\kotoha_v1`.
//! 3. Linux/macOS default: `$XDG_RUNTIME_DIR/kotoha_v1.sock` if `$XDG_RUNTIME_DIR` is non-empty.
//! 4. Linux/macOS fallback: a per-user private directory
//!    `${TMPDIR:-/tmp}/kotoha-<euid>/kotoha_v1.sock`. `$TMPDIR` is only used when it is an
//!    absolute path other than `/` and the resulting endpoint is at most
//!    [`MAX_UNIX_ENDPOINT_LEN`] bytes; otherwise `/tmp/kotoha-<euid>/kotoha_v1.sock` is used.
//!    The server creates the directory with mode `0700` and refuses to use it unless it is a
//!    real directory owned by the effective UID without group/other permission bits.
//!
//! There is intentionally no fallback directly inside a world-writable directory (such as the
//! former `/tmp/kotoha_v1.sock`), because another local user could pre-create (squat) that path.
//!
//! The C++ client (`plugins/common/src/ipc_client.cpp`, `resolve_ipc_endpoint`) implements the
//! identical rules; `tests/fixtures/endpoint_resolution_cases.tsv` is checked by both test suites.

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

/// Base directory for the per-user private directory when `$TMPDIR` is unset or unusable.
pub const DEFAULT_UNIX_TMP_BASE: &str = "/tmp";

/// Name prefix of the per-user private directory (`kotoha-<euid>`).
pub const PRIVATE_DIR_PREFIX: &str = "kotoha-";

/// Maximum byte length of a derived private-directory Unix endpoint (macOS `sun_path` is 104
/// bytes including NUL; `DESIGN.md` mandates staying at or below 90 bytes).
pub const MAX_UNIX_ENDPOINT_LEN: usize = 90;

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

/// Explicit environment inputs for pure, testable endpoint resolution.
///
/// `None` and `Some("")` are treated identically (unset).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EndpointEnv<'a> {
    /// Value of `KOTOHA_IPC_ENDPOINT`.
    pub endpoint_override: Option<&'a str>,
    /// Value of `XDG_RUNTIME_DIR` (ignored on Windows).
    pub xdg_runtime_dir: Option<&'a str>,
    /// Value of `TMPDIR` (ignored on Windows).
    pub tmpdir: Option<&'a str>,
    /// Effective UID of the process (ignored on Windows).
    pub euid: u32,
}

/// Result of endpoint resolution, including the per-user private directory when the endpoint
/// was derived from the private-directory fallback rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEndpoint {
    /// The endpoint path (Unix) or pipe path (Windows).
    pub endpoint: String,
    /// The per-user private directory (`.../kotoha-<euid>`) that the server must create with
    /// mode `0700` and validate before binding. `None` for overrides, `$XDG_RUNTIME_DIR` and
    /// Windows.
    pub private_dir: Option<String>,
}

/// Returns the effective UID of the current process (always `0` on non-Unix targets).
#[must_use]
pub fn current_euid() -> u32 {
    #[cfg(unix)]
    {
        rustix::process::geteuid().as_raw()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Resolves the IPC endpoint using the current process environment and target OS.
#[must_use]
pub fn resolve_endpoint() -> String {
    resolve_endpoint_details().endpoint
}

/// Resolves the IPC endpoint (and the private directory, if any) from the current process
/// environment and target OS.
#[must_use]
pub fn resolve_endpoint_details() -> ResolvedEndpoint {
    let env_override = env::var(ENV_IPC_ENDPOINT).ok();
    let xdg_runtime_dir = env::var("XDG_RUNTIME_DIR").ok();
    let tmpdir = env::var("TMPDIR").ok();
    resolve_endpoint_details_with(
        TargetOs::current(),
        &EndpointEnv {
            endpoint_override: env_override.as_deref(),
            xdg_runtime_dir: xdg_runtime_dir.as_deref(),
            tmpdir: tmpdir.as_deref(),
            euid: current_euid(),
        },
    )
}

/// Pure, testable endpoint resolution with explicit OS and environment inputs.
#[must_use]
pub fn resolve_endpoint_with(os: TargetOs, env: &EndpointEnv<'_>) -> String {
    resolve_endpoint_details_with(os, env).endpoint
}

/// Pure, testable endpoint resolution that also reports the private directory.
#[must_use]
pub fn resolve_endpoint_details_with(os: TargetOs, env: &EndpointEnv<'_>) -> ResolvedEndpoint {
    if let Some(val) = env.endpoint_override.filter(|s| !s.is_empty()) {
        return ResolvedEndpoint {
            endpoint: val.to_owned(),
            private_dir: None,
        };
    }

    match os {
        TargetOs::Windows => ResolvedEndpoint {
            endpoint: DEFAULT_WINDOWS_ENDPOINT.to_owned(),
            private_dir: None,
        },
        TargetOs::Unix => {
            if let Some(xdg) = env.xdg_runtime_dir.filter(|s| !s.is_empty()) {
                return ResolvedEndpoint {
                    endpoint: join_unix_socket_path(xdg),
                    private_dir: None,
                };
            }
            let private_dir = private_dir_for(env.tmpdir, env.euid);
            ResolvedEndpoint {
                endpoint: format!("{private_dir}/{DEFAULT_UNIX_SOCKET_NAME}"),
                private_dir: Some(private_dir),
            }
        }
    }
}

/// Computes the per-user private directory `<base>/kotoha-<euid>` for the fallback rule.
fn private_dir_for(tmpdir: Option<&str>, euid: u32) -> String {
    let leaf = format!("{PRIVATE_DIR_PREFIX}{euid}");
    let fallback = format!("{DEFAULT_UNIX_TMP_BASE}/{leaf}");
    let Some(base) = tmpdir
        .filter(|s| s.starts_with('/'))
        .map(|s| s.trim_end_matches('/'))
        .filter(|s| !s.is_empty())
    else {
        return fallback;
    };
    let dir = format!("{base}/{leaf}");
    // `+ 1` accounts for the separator before the socket file name.
    if dir.len() + 1 + DEFAULT_UNIX_SOCKET_NAME.len() > MAX_UNIX_ENDPOINT_LEN {
        fallback
    } else {
        dir
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
    let endpoint = resolve_endpoint_with(
        TargetOs::Windows,
        &EndpointEnv {
            endpoint_override: env_override.as_deref(),
            ..EndpointEnv::default()
        },
    );
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
