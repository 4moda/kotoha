//! Cross-platform E2E test harness for orchestrating `kotoha_mock_client`,
//! `kotoha-ipc-monitor`, unique IPC endpoints, typed NDJSON event streaming, and domain
//! assertions.

use std::collections::{BTreeSet, HashSet};
use std::env;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[cfg(not(windows))]
const UNIX_MAX_SOCKET_PATH_BYTES: usize = 80;
const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_CLIENT_TIMEOUT: Duration = Duration::from_secs(15);

/// Environment variable overriding automatic discovery of the C++ `kotoha_mock_client` binary.
pub const ENV_MOCK_CLIENT_BIN: &str = "KOTOHA_MOCK_CLIENT_BIN";

/// Environment variable overriding automatic discovery of the Rust `kotoha-ipc-monitor` binary.
pub const ENV_IPC_MONITOR_BIN: &str = "KOTOHA_IPC_MONITOR_BIN";

static ENDPOINT_SEQ: AtomicU64 = AtomicU64::new(1);

/// Error returned by the Kotoha E2E test harness.
#[derive(Debug)]
pub enum E2eError {
    /// Failed to locate a required binary or directory on disk / `PATH`.
    BinaryNotFound {
        /// Human-readable binary label.
        label: &'static str,
        /// Paths checked during discovery.
        searched: Vec<PathBuf>,
    },
    /// Failed to generate or validate an OS-compatible IPC endpoint.
    InvalidEndpoint(String),
    /// Child process spawn, wait, or I/O failure.
    Io {
        /// Contextual description of the failed operation.
        context: String,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// Timed out waiting for a child process, socket listener, or expected IPC event.
    Timeout(String),
    /// Child process exited with an unexpected status or malformed NDJSON output.
    ProcessFailed(String),
    /// Domain assertion failed with detailed diagnostic context.
    Assertion(String),
}

impl fmt::Display for E2eError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BinaryNotFound { label, searched } => {
                let paths = searched
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "could not locate {label} binary (searched: [{paths}])")
            }
            Self::InvalidEndpoint(msg) => write!(f, "invalid IPC endpoint: {msg}"),
            Self::Io { context, source } => write!(f, "{context}: {source}"),
            Self::Timeout(msg) => write!(f, "timed out: {msg}"),
            Self::ProcessFailed(msg) => write!(f, "process failure: {msg}"),
            Self::Assertion(msg) => write!(f, "E2E assertion failed:\n{msg}"),
        }
    }
}

impl std::error::Error for E2eError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Discovered paths to the native C++ mock client and Rust IPC monitor binaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct E2eBinaries {
    /// Path to the built `kotoha_mock_client` executable.
    pub mock_client: PathBuf,
    /// Path to the built `kotoha-ipc-monitor` executable.
    pub ipc_monitor: PathBuf,
}

impl E2eBinaries {
    /// Discovers `kotoha_mock_client` and `kotoha-ipc-monitor` binaries across Linux, macOS, and
    /// Windows build directories.
    ///
    /// Precedence:
    /// 1. Explicit `KOTOHA_MOCK_CLIENT_BIN` / `KOTOHA_IPC_MONITOR_BIN` environment overrides.
    /// 2. Existing build artifacts in `plugins/build/tests/{Release,RelWithDebInfo,Debug,}` and
    ///    `target/{release,debug}`, preferring the most recently modified binary and breaking ties
    ///    in favor of `Release`/`release` builds.
    /// 3. Fallback search on `PATH`.
    pub fn discover() -> Result<Self, E2eError> {
        let root = workspace_root();
        let mock_client = discover_binary(
            ENV_MOCK_CLIENT_BIN,
            "kotoha_mock_client",
            &mock_client_candidates(&root),
        )?;
        let ipc_monitor = discover_binary(
            ENV_IPC_MONITOR_BIN,
            "kotoha-ipc-monitor",
            &ipc_monitor_candidates(&root),
        )?;
        Ok(Self {
            mock_client,
            ipc_monitor,
        })
    }

    /// Discovers only the C++ `kotoha_mock_client` binary (useful for tests hosting an in-process
    /// [`kotoha_core::infrastructure::IpcServerAdapter`]).
    pub fn discover_mock_client() -> Result<PathBuf, E2eError> {
        let root = workspace_root();
        discover_binary(
            ENV_MOCK_CLIENT_BIN,
            "kotoha_mock_client",
            &mock_client_candidates(&root),
        )
    }
}

fn workspace_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| manifest_dir.to_path_buf(), Path::to_path_buf)
}

fn exe_filename(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }
}

fn mock_client_candidates(root: &Path) -> Vec<PathBuf> {
    let exe = exe_filename("kotoha_mock_client");
    let tests_dir = root.join("plugins").join("build").join("tests");
    let build_dir = root.join("plugins").join("build");
    vec![
        tests_dir.join("Release").join(&exe),
        tests_dir.join(&exe),
        tests_dir.join("RelWithDebInfo").join(&exe),
        tests_dir.join("Debug").join(&exe),
        build_dir.join("Release").join(&exe),
        build_dir.join("Debug").join(&exe),
        build_dir.join(&exe),
    ]
}

fn ipc_monitor_candidates(root: &Path) -> Vec<PathBuf> {
    let exe = exe_filename("kotoha-ipc-monitor");
    let target_dir =
        env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    let mut candidates = vec![
        target_dir.join("release").join(&exe),
        target_dir.join("debug").join(&exe),
    ];
    if let Some(target_triple_dir) = current_exe_target_dir() {
        candidates.push(target_triple_dir.join(&exe));
    }
    candidates
}

fn current_exe_target_dir() -> Option<PathBuf> {
    let current = env::current_exe().ok()?;
    let deps_dir = current.parent()?;
    deps_dir.parent().map(Path::to_path_buf)
}

fn discover_binary(
    env_var: &'static str,
    label: &'static str,
    candidates: &[PathBuf],
) -> Result<PathBuf, E2eError> {
    if let Some(raw_override) = env::var_os(env_var).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(raw_override);
        if path.is_file() {
            return Ok(path);
        }
        return Err(E2eError::BinaryNotFound {
            label,
            searched: vec![path],
        });
    }

    let mut existing: Vec<(std::time::SystemTime, PathBuf)> = candidates
        .iter()
        .filter(|path| path.is_file())
        .map(|path| {
            let mtime = path
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (mtime, path.clone())
        })
        .collect();

    if !existing.is_empty() {
        // Prefer the newest binary when both Debug and Release artifacts exist locally; stable
        // sort breaks equal-mtime ties by `candidates` order (`Release`/`release` first).
        existing.sort_by(|(a_time, _), (b_time, _)| b_time.cmp(a_time));
        if let Some((_, best)) = existing.into_iter().next() {
            return Ok(best);
        }
    }

    let exe_name = exe_filename(label);
    if let Some(path_env) = env::var_os("PATH") {
        for dir in env::split_paths(&path_env) {
            let candidate = dir.join(&exe_name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    Err(E2eError::BinaryNotFound {
        label,
        searched: candidates.to_vec(),
    })
}

/// RAII-managed unique IPC endpoint name/path for isolated parallel E2E runs.
///
/// - On Unix (Linux/macOS): creates a short `< 80`-byte path under `/tmp`
///   (`/tmp/kt_e2e_<tag>_<pid>_<seq>.sock`) to stay safely below macOS's 104-byte `sun_path` limit,
///   and automatically unlinks any stale/leftover socket file on creation and [`Drop`].
/// - On Windows: creates a unique Named Pipe path `\\.\pipe\kotoha_e2e_<tag>_<pid>_<seq>`.
#[derive(Debug)]
pub struct UniqueIpcEndpoint {
    endpoint: String,
    #[cfg(unix)]
    socket_path: PathBuf,
}

impl UniqueIpcEndpoint {
    /// Creates a new unique IPC endpoint tagged with `tag` (sanitized to ASCII alphanumerics).
    pub fn new(tag: &str) -> Result<Self, E2eError> {
        let pid = std::process::id();
        let seq = ENDPOINT_SEQ.fetch_add(1, Ordering::Relaxed);
        let clean_tag: String = tag
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(12)
            .collect();
        let tag_part = if clean_tag.is_empty() {
            "t".to_owned()
        } else {
            clean_tag
        };

        #[cfg(windows)]
        {
            let endpoint = format!(r"\\.\pipe\kotoha_e2e_{tag_part}_{pid}_{seq}");
            Ok(Self { endpoint })
        }

        #[cfg(not(windows))]
        {
            let mut candidate = format!("/tmp/kt_e2e_{tag_part}_{pid}_{seq}.sock");
            if candidate.len() >= UNIX_MAX_SOCKET_PATH_BYTES {
                candidate = format!("/tmp/kt_{pid}_{seq}.sock");
            }
            if candidate.len() >= UNIX_MAX_SOCKET_PATH_BYTES {
                return Err(E2eError::InvalidEndpoint(format!(
                    "Unix socket path '{candidate}' exceeds {UNIX_MAX_SOCKET_PATH_BYTES} bytes"
                )));
            }
            let socket_path = PathBuf::from(&candidate);
            remove_socket_if_exists(&socket_path);
            Ok(Self {
                endpoint: candidate,
                socket_path,
            })
        }
    }

    /// Returns the endpoint string suitable for `--endpoint` or `IpcServerAdapter::new`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.endpoint
    }

    /// Returns the socket path on Unix targets (`None` on Windows Named Pipes).
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn unix_path(&self) -> Option<&Path> {
        #[cfg(unix)]
        {
            Some(self.socket_path.as_path())
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
}

impl Drop for UniqueIpcEndpoint {
    fn drop(&mut self) {
        #[cfg(unix)]
        remove_socket_if_exists(&self.socket_path);
    }
}

#[cfg(unix)]
fn remove_socket_if_exists(path: &Path) {
    if std::fs::symlink_metadata(path).is_ok() {
        let _ = std::fs::remove_file(path);
    }
}

/// Captured stdout, stderr, and exit status from running `kotoha_mock_client`.
#[derive(Debug, Clone)]
pub struct MockClientOutput {
    /// Exit status returned by the child process.
    pub status: ExitStatus,
    /// Captured UTF-8 stdout.
    pub stdout: String,
    /// Captured UTF-8 stderr.
    pub stderr: String,
}

/// Strongly typed builder for invoking the C++ `kotoha_mock_client` binary.
#[derive(Debug, Clone)]
pub struct MockClientArgs {
    endpoint: String,
    source: Option<String>,
    version: Option<String>,
    user_id: Option<u32>,
    user_name: Option<String>,
    frames: Option<u32>,
    duration_ms: Option<u32>,
    wav: Option<PathBuf>,
    tail_silence_ms: Option<u32>,
    with_heartbeat: bool,
    connect_timeout_ms: Option<u32>,
    wait_stdin_eof: bool,
}

/// Backwards-compatible alias for [`MockClientArgs`].
pub type MockClientCommand = MockClientArgs;

impl MockClientArgs {
    /// Creates a new `kotoha_mock_client` invocation targeting `endpoint`.
    #[must_use]
    pub fn new(endpoint: &UniqueIpcEndpoint) -> Self {
        Self::for_endpoint(endpoint.as_str())
    }

    /// Creates a new `kotoha_mock_client` invocation targeting a raw IPC endpoint string.
    #[must_use]
    pub fn for_endpoint(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            source: None,
            version: None,
            user_id: None,
            user_name: None,
            frames: None,
            duration_ms: None,
            wav: None,
            tail_silence_ms: None,
            with_heartbeat: false,
            connect_timeout_ms: None,
            wait_stdin_eof: false,
        }
    }

    /// Sets `--source <name>` (e.g., `"mumble"`, `"ts3"`).
    #[must_use]
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Sets `--version <ver>`.
    #[must_use]
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Sets `--user-id <id>` and `--user-name <name>`.
    #[must_use]
    pub fn user(mut self, user_id: u32, user_name: impl Into<String>) -> Self {
        self.user_id = Some(user_id);
        self.user_name = Some(user_name.into());
        self
    }

    /// Sets `--frames <n>`.
    #[must_use]
    pub const fn frames(mut self, frames: u32) -> Self {
        self.frames = Some(frames);
        self
    }

    /// Sets `--duration-ms <ms>`.
    #[must_use]
    pub const fn duration_ms(mut self, duration_ms: u32) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }

    /// Sets `--wav <path>` to stream a real WAV file over IPC.
    #[must_use]
    pub fn wav(mut self, path: impl AsRef<Path>) -> Self {
        self.wav = Some(path.as_ref().to_path_buf());
        self
    }

    /// Sets `--tail-silence-ms <ms>` appended after WAV playback for endpointing.
    #[must_use]
    pub const fn tail_silence_ms(mut self, tail_silence_ms: u32) -> Self {
        self.tail_silence_ms = Some(tail_silence_ms);
        self
    }

    /// Enables `--with-heartbeat`.
    #[must_use]
    pub const fn with_heartbeat(mut self) -> Self {
        self.with_heartbeat = true;
        self
    }

    /// Sets `--connect-timeout-ms <ms>`.
    #[must_use]
    pub const fn connect_timeout_ms(mut self, connect_timeout_ms: u32) -> Self {
        self.connect_timeout_ms = Some(connect_timeout_ms);
        self
    }

    /// Enables `--wait-stdin-eof` so the client keeps the IPC session open until stdin closes.
    #[must_use]
    pub const fn wait_stdin_eof(mut self) -> Self {
        self.wait_stdin_eof = true;
        self
    }

    /// Converts configured options into CLI argument strings for `kotoha_mock_client`.
    #[must_use]
    pub fn to_cli_args(&self) -> Vec<String> {
        let mut args = vec!["--endpoint".to_owned(), self.endpoint.clone()];
        if let Some(source) = &self.source {
            args.push("--source".to_owned());
            args.push(source.clone());
        }
        if let Some(version) = &self.version {
            args.push("--version".to_owned());
            args.push(version.clone());
        }
        if let Some(user_id) = self.user_id {
            args.push("--user-id".to_owned());
            args.push(user_id.to_string());
        }
        if let Some(user_name) = &self.user_name {
            args.push("--user-name".to_owned());
            args.push(user_name.clone());
        }
        if let Some(frames) = self.frames {
            args.push("--frames".to_owned());
            args.push(frames.to_string());
        }
        if let Some(duration_ms) = self.duration_ms {
            args.push("--duration-ms".to_owned());
            args.push(duration_ms.to_string());
        }
        if let Some(wav) = &self.wav {
            args.push("--wav".to_owned());
            args.push(wav.to_string_lossy().into_owned());
        }
        if let Some(tail_silence_ms) = self.tail_silence_ms {
            args.push("--tail-silence-ms".to_owned());
            args.push(tail_silence_ms.to_string());
        }
        if self.with_heartbeat {
            args.push("--with-heartbeat".to_owned());
        }
        if let Some(connect_timeout_ms) = self.connect_timeout_ms {
            args.push("--connect-timeout-ms".to_owned());
            args.push(connect_timeout_ms.to_string());
        }
        if self.wait_stdin_eof {
            args.push("--wait-stdin-eof".to_owned());
        }
        args
    }

    /// Spawns `kotoha_mock_client` asynchronously without waiting for exit.
    ///
    /// Pipes `stdin` when [`Self::wait_stdin_eof`] is enabled so the caller can drop
    /// `child.stdin.take()` to signal a clean disconnect.
    pub fn spawn(&self, client_bin: &Path) -> Result<Child, E2eError> {
        let mut cmd = Command::new(client_bin);
        cmd.args(self.to_cli_args())
            .stdin(if self.wait_stdin_eof {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.spawn().map_err(|source| E2eError::Io {
            context: format!("failed to spawn {}", client_bin.display()),
            source,
        })
    }

    /// Runs `kotoha_mock_client` to completion with a default 15s timeout and asserts exit code 0.
    pub async fn run(&self, client_bin: &Path) -> Result<MockClientOutput, E2eError> {
        self.run_to_completion(client_bin, DEFAULT_CLIENT_TIMEOUT)
            .await
    }

    /// Runs `kotoha_mock_client` to completion within `timeout` and asserts exit code 0.
    pub async fn run_to_completion(
        &self,
        client_bin: &Path,
        timeout: Duration,
    ) -> Result<MockClientOutput, E2eError> {
        let child = self.spawn(client_bin)?;
        await_mock_client(child, timeout).await
    }
}

/// Awaits a previously spawned `kotoha_mock_client` child process with timeout and exit-code check.
pub async fn await_mock_client(
    mut child: Child,
    timeout: Duration,
) -> Result<MockClientOutput, E2eError> {
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let stdout_task = tokio::spawn(read_pipe_to_string(stdout_pipe));
    let stderr_task = tokio::spawn(read_pipe_to_string(stderr_pipe));

    let status = if let Ok(wait_res) = tokio::time::timeout(timeout, child.wait()).await {
        wait_res.map_err(|source| E2eError::Io {
            context: "failed waiting for kotoha_mock_client".to_owned(),
            source,
        })?
    } else {
        let _ = child.kill().await;
        let _ = child.wait().await;
        let stdout = join_pipe(stdout_task).await;
        let stderr = join_pipe(stderr_task).await;
        return Err(E2eError::Timeout(format!(
            "kotoha_mock_client exceeded {timeout:?}\nstdout: {stdout}\nstderr: {stderr}"
        )));
    };

    let stdout = join_pipe(stdout_task).await;
    let stderr = join_pipe(stderr_task).await;

    if !status.success() {
        return Err(E2eError::ProcessFailed(format!(
            "kotoha_mock_client failed with status {status}\nstdout: {stdout}\nstderr: {stderr}"
        )));
    }

    Ok(MockClientOutput {
        status,
        stdout,
        stderr,
    })
}

async fn read_pipe_to_string<R>(pipe: Option<R>) -> String
where
    R: tokio::io::AsyncRead + Unpin,
{
    let Some(mut reader) = pipe else {
        return String::new();
    };
    let mut buf = Vec::new();
    let _ = reader.read_to_end(&mut buf).await;
    String::from_utf8_lossy(&buf).into_owned()
}

async fn join_pipe(task: JoinHandle<String>) -> String {
    task.await.unwrap_or_default()
}

/// Strongly typed representation of NDJSON events emitted by `kotoha-ipc-monitor --json`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum MonitorEvent {
    /// Initial `Hello` handshake received from a connected plugin client.
    Hello {
        /// Connection ID assigned by `IpcServerAdapter`.
        conn_id: u64,
        /// Source application name (`Hello.app_name`).
        source: String,
        /// Plugin version string (`Hello.plugin_version`).
        plugin_version: String,
        /// Previous owning connection ID superseded by this connection, if any.
        superseded_conn_id: Option<u64>,
    },
    /// Client disconnected from the IPC server.
    Disconnected {
        /// Connection ID that disconnected.
        conn_id: u64,
        /// Source application name.
        source: String,
        /// Number of speakers cleared on disconnect.
        cleared_speakers: usize,
    },
    /// Client issued `SourceReset`.
    SourceReset {
        /// Connection ID issuing reset.
        conn_id: u64,
        /// Source application name.
        source: String,
        /// Number of speakers cleared by reset.
        cleared_speakers: usize,
    },
    /// Speaker display name registration or update.
    UserInfo {
        /// Source application name.
        source: String,
        /// Speaker ID within `source`.
        user_id: u32,
        /// Display name.
        name: String,
    },
    /// Speaker left the voice channel/server.
    UserRemoved {
        /// Source application name.
        source: String,
        /// Speaker ID within `source`.
        user_id: u32,
    },
    /// Speaker talking state change.
    TalkingState {
        /// Source application name.
        source: String,
        /// Speaker ID within `source`.
        user_id: u32,
        /// Talking state string (`"passive"`, `"talking"`, `"whispering"`, `"shouting"`, `"talking_muted"`).
        state: String,
    },
    /// Preprocessed PCM audio frame metrics.
    AudioFrame {
        /// Source application name.
        source: String,
        /// Speaker ID within `source`.
        user_id: u32,
        /// Monotonic frame sequence number.
        seq: u32,
        /// Original sample rate in Hz.
        sample_rate: u32,
        /// Original channel count.
        channels: u16,
        /// Whether frame originated from the local user.
        is_local_user: bool,
        /// Raw interleaved sample count (`samples.len()`).
        samples: usize,
        /// Resampled 16 kHz mono sample count.
        samples_16k_mono: usize,
        /// Root-mean-square amplitude of the raw audio samples.
        rms: f32,
        /// Sequence status (`"init"`, `"ok"`, `"dropped"`, `"anomaly"`, `"unknown"`).
        seq_status: String,
        /// Frames dropped immediately before this frame.
        dropped_frames: u32,
        /// Cumulative dropped frames for this source connection.
        total_dropped: u64,
    },
    /// Keepalive heartbeat from an active source connection.
    Heartbeat {
        /// Connection ID.
        conn_id: u64,
        /// Source application name.
        source: String,
    },
}

impl MonitorEvent {
    /// Returns the source name associated with this event.
    #[must_use]
    pub fn source(&self) -> &str {
        match self {
            Self::Hello { source, .. }
            | Self::Disconnected { source, .. }
            | Self::SourceReset { source, .. }
            | Self::UserInfo { source, .. }
            | Self::UserRemoved { source, .. }
            | Self::TalkingState { source, .. }
            | Self::AudioFrame { source, .. }
            | Self::Heartbeat { source, .. } => source,
        }
    }
}

/// Managed `kotoha-ipc-monitor --json` child session streaming typed [`MonitorEvent`]s.
///
/// Automatically terminates and reaps the monitor process on [`Drop`] if not already stopped via
/// [`MonitorSession::finish_and_collect`].
pub struct MonitorSession {
    child: Option<Child>,
    endpoint: String,
    event_rx: mpsc::Receiver<Result<MonitorEvent, E2eError>>,
    reader_task: Option<JoinHandle<()>>,
    stderr_task: Option<JoinHandle<String>>,
    collected: Vec<MonitorEvent>,
}

impl MonitorSession {
    /// Spawns `kotoha-ipc-monitor --endpoint <endpoint> --json [--max-events <n>]` and waits for
    /// IPC listener readiness.
    pub async fn start(
        monitor_bin: &Path,
        endpoint: &UniqueIpcEndpoint,
        max_events: Option<usize>,
    ) -> Result<Self, E2eError> {
        let mut cmd = Command::new(monitor_bin);
        cmd.arg("--endpoint").arg(endpoint.as_str()).arg("--json");
        if let Some(limit) = max_events {
            cmd.arg("--max-events").arg(limit.to_string());
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|source| E2eError::Io {
            context: format!("failed to spawn {}", monitor_bin.display()),
            source,
        })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            E2eError::ProcessFailed("kotoha-ipc-monitor stdout was not piped".to_owned())
        })?;
        let stderr = child.stderr.take();

        let (event_tx, event_rx) = mpsc::channel(256);
        let reader_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut line_no = 0usize;
            loop {
                match lines.next_line().await {
                    Ok(Some(raw)) => {
                        line_no += 1;
                        let trimmed = raw.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<MonitorEvent>(trimmed) {
                            Ok(event) => {
                                if event_tx.send(Ok(event)).await.is_err() {
                                    break;
                                }
                            }
                            Err(err) => {
                                let _ = event_tx
                                    .send(Err(E2eError::ProcessFailed(format!(
                                        "invalid NDJSON on line {line_no}: {trimmed:?} ({err})"
                                    ))))
                                    .await;
                                break;
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(source) => {
                        let _ = event_tx
                            .send(Err(E2eError::Io {
                                context: "failed reading kotoha-ipc-monitor stdout".to_owned(),
                                source,
                            }))
                            .await;
                        break;
                    }
                }
            }
        });
        let stderr_task = tokio::spawn(read_pipe_to_string(stderr));

        let mut session = Self {
            child: Some(child),
            endpoint: endpoint.as_str().to_owned(),
            event_rx,
            reader_task: Some(reader_task),
            stderr_task: Some(stderr_task),
            collected: Vec::new(),
        };

        session
            .wait_for_listener_ready(endpoint, DEFAULT_READY_TIMEOUT)
            .await?;
        Ok(session)
    }

    async fn wait_for_listener_ready(
        &mut self,
        endpoint: &UniqueIpcEndpoint,
        timeout: Duration,
    ) -> Result<(), E2eError> {
        #[cfg(windows)]
        {
            // Avoid probing `\\.\pipe\...` directly because opening a Named Pipe instance races
            // with `ConnectNamedPipe`; wait briefly to ensure the monitor stayed up and let
            // `kotoha_mock_client`'s built-in connect retry loop attach cleanly.
            let _ = endpoint;
            let wait_for = timeout.min(Duration::from_millis(250));
            let deadline = Instant::now() + wait_for;
            while Instant::now() < deadline {
                if let Some(child) = self.child.as_mut() {
                    if let Some(status) = child.try_wait().map_err(|source| E2eError::Io {
                        context: "failed polling kotoha-ipc-monitor".to_owned(),
                        source,
                    })? {
                        let stderr = self.drain_stderr().await;
                        return Err(E2eError::ProcessFailed(format!(
                            "kotoha-ipc-monitor exited prematurely with {status}\nstderr: {stderr}"
                        )));
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok(())
        }

        #[cfg(not(windows))]
        {
            let Some(socket_path) = endpoint.unix_path() else {
                return Ok(());
            };
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if let Some(child) = self.child.as_mut() {
                    if let Some(status) = child.try_wait().map_err(|source| E2eError::Io {
                        context: "failed polling kotoha-ipc-monitor".to_owned(),
                        source,
                    })? {
                        let stderr = self.drain_stderr().await;
                        return Err(E2eError::ProcessFailed(format!(
                            "kotoha-ipc-monitor exited prematurely with {status}\nstderr: {stderr}"
                        )));
                    }
                }
                if socket_path.exists() {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }

            Err(E2eError::Timeout(format!(
                "timed out waiting for Unix socket listener at {}",
                socket_path.display()
            )))
        }
    }

    async fn drain_stderr(&mut self) -> String {
        if let Some(task) = self.stderr_task.take() {
            join_pipe(task).await
        } else {
            String::new()
        }
    }

    /// Waits until a `disconnected` event has been observed for every source in `expected_sources`,
    /// or until stdout closes.
    pub async fn wait_for_sources_disconnected(
        &mut self,
        expected_sources: &[&str],
    ) -> Result<(), E2eError> {
        self.wait_for_sources_disconnected_within(expected_sources, DEFAULT_DISCONNECT_TIMEOUT)
            .await
    }

    /// Waits up to `timeout` until a `disconnected` event has been observed for every source in
    /// `expected_sources`, or until stdout closes.
    pub async fn wait_for_sources_disconnected_within(
        &mut self,
        expected_sources: &[&str],
        timeout: Duration,
    ) -> Result<(), E2eError> {
        let required: BTreeSet<&str> = expected_sources.iter().copied().collect();
        let deadline = Instant::now() + timeout;

        loop {
            let disconnected: BTreeSet<&str> = self
                .collected
                .iter()
                .filter_map(|event| match event {
                    MonitorEvent::Disconnected { source, .. } => Some(source.as_str()),
                    _ => None,
                })
                .collect();

            if required.is_subset(&disconnected) {
                return Ok(());
            }

            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(E2eError::Timeout(format!(
                    "waiting for disconnected events for {required:?}; seen_events={}, disconnected={disconnected:?}",
                    self.collected.len()
                )));
            };

            match tokio::time::timeout(remaining, self.event_rx.recv()).await {
                Ok(Some(Ok(event))) => {
                    self.collected.push(event);
                }
                Ok(Some(Err(err))) => return Err(err),
                Ok(None) => {
                    if required.is_subset(&disconnected) {
                        return Ok(());
                    }
                    let missing: Vec<&str> = required.difference(&disconnected).copied().collect();
                    return Err(E2eError::ProcessFailed(format!(
                        "kotoha-ipc-monitor closed stdout before emitting Disconnected for {missing:?}; seen_events={}, disconnected={disconnected:?}",
                        self.collected.len()
                    )));
                }
                Err(_) => {
                    return Err(E2eError::Timeout(format!(
                        "waiting for disconnected events for {required:?}; seen_events={}, disconnected={disconnected:?}",
                        self.collected.len()
                    )));
                }
            }
        }
    }

    async fn drain_reader_and_events(&mut self) -> Result<(), E2eError> {
        if let Some(task) = self.reader_task.take() {
            let _ = task.await;
        }
        while let Ok(item) = self.event_rx.try_recv() {
            self.collected.push(item?);
        }
        Ok(())
    }

    #[cfg(windows)]
    async fn flush_windows_stdout_and_poll(
        &mut self,
        child: &mut Child,
    ) -> Result<Option<ExitStatus>, E2eError> {
        let flush_deadline = Instant::now() + Duration::from_millis(50);
        loop {
            while let Ok(item) = self.event_rx.try_recv() {
                self.collected.push(item?);
            }
            if let Some(early_status) = child.try_wait().map_err(|source| E2eError::Io {
                context: "failed polling kotoha-ipc-monitor".to_owned(),
                source,
            })? {
                return Ok(Some(early_status));
            }
            let Some(remaining) = flush_deadline.checked_duration_since(Instant::now()) else {
                return Ok(None);
            };
            let step = remaining.min(Duration::from_millis(10));
            if let Ok(Some(item)) = tokio::time::timeout(step, self.event_rx.recv()).await {
                self.collected.push(item?);
            }
        }
    }

    /// Gracefully stops `kotoha-ipc-monitor`, drains all remaining NDJSON events, and verifies clean
    /// process exit.
    pub async fn finish_and_collect(mut self) -> Result<Vec<MonitorEvent>, E2eError> {
        if let Some(mut child) = self.child.take() {
            if let Some(early_status) = child.try_wait().map_err(|source| E2eError::Io {
                context: "failed polling kotoha-ipc-monitor".to_owned(),
                source,
            })? {
                self.drain_reader_and_events().await?;
                let stderr = self.drain_stderr().await;
                if !early_status.success() {
                    return Err(E2eError::ProcessFailed(format!(
                        "kotoha-ipc-monitor exited prematurely with {early_status}\nstderr: {stderr}"
                    )));
                }
                return Ok(std::mem::take(&mut self.collected));
            }

            #[cfg(windows)]
            if let Some(early_status) = self.flush_windows_stdout_and_poll(&mut child).await? {
                self.drain_reader_and_events().await?;
                let stderr = self.drain_stderr().await;
                if !early_status.success() {
                    return Err(E2eError::ProcessFailed(format!(
                        "kotoha-ipc-monitor exited prematurely with {early_status}\nstderr: {stderr}"
                    )));
                }
                return Ok(std::mem::take(&mut self.collected));
            }

            signal_graceful_stop(&mut child);

            let status =
                if let Ok(res) = tokio::time::timeout(DEFAULT_STOP_TIMEOUT, child.wait()).await {
                    res.map_err(|source| E2eError::Io {
                        context: "failed waiting for kotoha-ipc-monitor".to_owned(),
                        source,
                    })?
                } else {
                    let _ = child.kill().await;
                    child.wait().await.map_err(|source| E2eError::Io {
                        context: "failed killing unresponsive kotoha-ipc-monitor".to_owned(),
                        source,
                    })?
                };

            self.drain_reader_and_events().await?;
            let stderr = self.drain_stderr().await;

            #[cfg(not(windows))]
            if !is_clean_unix_exit(status) {
                return Err(E2eError::ProcessFailed(format!(
                    "kotoha-ipc-monitor exited with {status}\nstderr: {stderr}"
                )));
            }
            #[cfg(windows)]
            let _ = (&status, &stderr);
        }

        Ok(std::mem::take(&mut self.collected))
    }

    /// Returns the IPC endpoint string monitored by this session.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl Drop for MonitorSession {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            let _ = child.try_wait();
        }
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
    }
}

#[cfg(not(windows))]
fn is_clean_unix_exit(status: ExitStatus) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        status.success() || status.signal() == Some(2)
    }
    #[cfg(not(unix))]
    {
        status.success()
    }
}

#[cfg(unix)]
fn signal_graceful_stop(child: &mut Child) {
    // Send SIGINT so `kotoha-ipc-monitor` exits its `tokio::signal::ctrl_c()` branch cleanly,
    // closes its listener, and removes its Unix socket file. Fall back to `start_kill()` only if
    // `kill(2)` is unavailable or PID is missing.
    if let Some(pid) = child.id() {
        let status = std::process::Command::new("kill")
            .arg("-INT")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if status.is_ok_and(|s| s.success()) {
            return;
        }
    }
    let _ = child.start_kill();
}

#[cfg(not(unix))]
fn signal_graceful_stop(child: &mut Child) {
    let _ = child.start_kill();
}

/// Validates a complete single-source IPC lifecycle in `events` and returns the assigned `conn_id`.
pub fn assert_single_source_lifecycle(
    events: &[MonitorEvent],
    source: &str,
    user_id: u32,
    user_name: &str,
    min_frames: usize,
    require_heartbeat: bool,
    min_rms: f32,
) -> Result<u64, E2eError> {
    let src_events: Vec<&MonitorEvent> = events.iter().filter(|e| e.source() == source).collect();
    if src_events.is_empty() {
        return Err(E2eError::Assertion(format!(
            "no monitor events recorded for source={source:?}"
        )));
    }

    let conn_id = assert_hello_and_speaker(&src_events, source, user_id, user_name)?;
    let frame_indices = assert_audio_frames(&src_events, source, user_id, min_frames, min_rms)?;

    if require_heartbeat
        && !src_events
            .iter()
            .any(|e| matches!(e, MonitorEvent::Heartbeat { .. }))
    {
        return Err(E2eError::Assertion(format!(
            "[{source}] expected >= 1 Heartbeat event"
        )));
    }

    let user_removed_idx = src_events
        .iter()
        .position(
            |e| matches!(e, MonitorEvent::UserRemoved { user_id: uid, .. } if *uid == user_id),
        )
        .ok_or_else(|| {
            E2eError::Assertion(format!(
                "[{source}] missing UserRemoved for user_id={user_id}"
            ))
        })?;

    if let Some(&last_audio_idx) = frame_indices.last() {
        if user_removed_idx <= last_audio_idx {
            return Err(E2eError::Assertion(format!(
                "[{source}] UserRemoved ({user_removed_idx}) occurred before last AudioFrame ({last_audio_idx})"
            )));
        }
    }

    let has_lifecycle_end = src_events.iter().any(|e| {
        matches!(
            e,
            MonitorEvent::Disconnected { .. } | MonitorEvent::SourceReset { .. }
        )
    });
    if !has_lifecycle_end {
        return Err(E2eError::Assertion(format!(
            "[{source}] missing Disconnected/SourceReset lifecycle end event"
        )));
    }

    Ok(conn_id)
}

fn assert_hello_and_speaker(
    src_events: &[&MonitorEvent],
    source: &str,
    user_id: u32,
    user_name: &str,
) -> Result<u64, E2eError> {
    let hellos: Vec<(u64, &str)> = src_events
        .iter()
        .filter_map(|e| match e {
            MonitorEvent::Hello {
                conn_id,
                plugin_version,
                ..
            } => Some((*conn_id, plugin_version.as_str())),
            _ => None,
        })
        .collect();
    if hellos.len() != 1 {
        return Err(E2eError::Assertion(format!(
            "[{source}] expected exactly 1 Hello event, got {}: {src_events:#?}",
            hellos.len()
        )));
    }
    let (conn_id, plugin_version) = hellos[0];
    if plugin_version != "0.1.0" {
        return Err(E2eError::Assertion(format!(
            "[{source}] expected plugin_version \"0.1.0\", got {plugin_version:?}"
        )));
    }
    if !matches!(src_events.first(), Some(MonitorEvent::Hello { .. })) {
        return Err(E2eError::Assertion(format!(
            "[{source}] first event must be Hello, got {:?}",
            src_events.first()
        )));
    }

    let has_user_info = src_events.iter().any(|e| {
        matches!(
            e,
            MonitorEvent::UserInfo {
                user_id: uid,
                name,
                ..
            } if *uid == user_id && name == user_name
        )
    });
    if !has_user_info {
        return Err(E2eError::Assertion(format!(
            "[{source}] missing UserInfo for user_id={user_id}, name={user_name:?}"
        )));
    }

    let talking_states: Vec<&str> = src_events
        .iter()
        .filter_map(|e| match e {
            MonitorEvent::TalkingState {
                user_id: uid,
                state,
                ..
            } if *uid == user_id => Some(state.as_str()),
            _ => None,
        })
        .collect();
    if !talking_states.contains(&"talking") || !talking_states.contains(&"passive") {
        return Err(E2eError::Assertion(format!(
            "[{source}] expected both \"talking\" and \"passive\" states for user_id={user_id}, got {talking_states:?}"
        )));
    }

    Ok(conn_id)
}

fn assert_audio_frames(
    src_events: &[&MonitorEvent],
    source: &str,
    user_id: u32,
    min_frames: usize,
    min_rms: f32,
) -> Result<Vec<usize>, E2eError> {
    let mut frame_indices = Vec::new();
    for (idx, event) in src_events.iter().enumerate() {
        let MonitorEvent::AudioFrame {
            user_id: uid,
            samples_16k_mono,
            rms,
            seq_status,
            dropped_frames,
            total_dropped,
            ..
        } = event
        else {
            continue;
        };

        if *uid != user_id {
            return Err(E2eError::Assertion(format!(
                "[{source}] unexpected audio_frame for user_id={uid} (expected {user_id})"
            )));
        }
        if *dropped_frames != 0 || *total_dropped != 0 {
            return Err(E2eError::Assertion(format!(
                "[{source}] non-zero drop counters on frame {idx}: dropped={dropped_frames}, total_dropped={total_dropped}"
            )));
        }
        if seq_status != "init" && seq_status != "ok" {
            return Err(E2eError::Assertion(format!(
                "[{source}] unexpected seq_status {seq_status:?} on frame {idx}"
            )));
        }
        if *rms <= min_rms {
            return Err(E2eError::Assertion(format!(
                "[{source}] frame {idx} RMS {rms} is <= expected threshold {min_rms}"
            )));
        }
        if *samples_16k_mono == 0 {
            return Err(E2eError::Assertion(format!(
                "[{source}] frame {idx} produced 0 resampled 16k mono samples"
            )));
        }
        frame_indices.push(idx);
    }

    if frame_indices.len() < min_frames {
        return Err(E2eError::Assertion(format!(
            "[{source}] expected >= {min_frames} AudioFrame events, got {}",
            frame_indices.len()
        )));
    }
    Ok(frame_indices)
}

/// Validates concurrent multi-source speaker isolation across two sources sharing a `MonitorSession`.
///
/// Asserts that:
/// - Only the expected sources appear in `events`.
/// - Each source completes a full drop-free lifecycle.
/// - Distinct `conn_id`s are assigned.
/// - Each source observes only its own speaker display name (`SpeakerKey` isolation).
pub fn assert_multi_source_isolation(
    events: &[MonitorEvent],
    source_a: (&str, u32, &str, usize),
    source_b: (&str, u32, &str, usize),
    require_heartbeat: bool,
    min_rms: f32,
) -> Result<(u64, u64), E2eError> {
    let observed_sources: BTreeSet<&str> = events.iter().map(MonitorEvent::source).collect();
    let expected_sources = BTreeSet::from([source_a.0, source_b.0]);
    if observed_sources != expected_sources {
        return Err(E2eError::Assertion(format!(
            "expected sources {expected_sources:?}, observed {observed_sources:?}"
        )));
    }

    let conn_a = assert_single_source_lifecycle(
        events,
        source_a.0,
        source_a.1,
        source_a.2,
        source_a.3,
        require_heartbeat,
        min_rms,
    )?;
    let conn_b = assert_single_source_lifecycle(
        events,
        source_b.0,
        source_b.1,
        source_b.2,
        source_b.3,
        require_heartbeat,
        min_rms,
    )?;

    if conn_a == conn_b {
        return Err(E2eError::Assertion(format!(
            "concurrent clients {} and {} shared conn_id={conn_a}",
            source_a.0, source_b.0
        )));
    }

    let names_a = collect_user_info_names(events, source_a.0);
    let names_b = collect_user_info_names(events, source_b.0);
    if names_a != HashSet::from([source_a.2]) {
        return Err(E2eError::Assertion(format!(
            "source {} SpeakerKey contaminated: expected {:?}, got {names_a:?}",
            source_a.0, source_a.2
        )));
    }
    if names_b != HashSet::from([source_b.2]) {
        return Err(E2eError::Assertion(format!(
            "source {} SpeakerKey contaminated: expected {:?}, got {names_b:?}",
            source_b.0, source_b.2
        )));
    }

    Ok((conn_a, conn_b))
}

fn collect_user_info_names<'a>(
    events: &'a [MonitorEvent],
    target_source: &str,
) -> HashSet<&'a str> {
    events
        .iter()
        .filter_map(|e| match e {
            MonitorEvent::UserInfo { source, name, .. } if source == target_source => {
                Some(name.as_str())
            }
            _ => None,
        })
        .collect()
}
