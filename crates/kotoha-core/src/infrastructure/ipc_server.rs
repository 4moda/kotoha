//! Asynchronous cross-platform IPC server adapter built on `tokio` + `interprocess` + `kotoha-protocol`.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::tokio::Stream as LocalSocketStream;
#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(windows)]
use interprocess::local_socket::GenericNamespaced;
use interprocess::local_socket::ListenerOptions;
#[cfg(unix)]
use interprocess::os::unix::local_socket::ListenerOptionsExt as _;
#[cfg(windows)]
use interprocess::os::windows::{
    local_socket::ListenerOptionsExt as _, security_descriptor::SecurityDescriptor,
};
#[cfg(unix)]
use kotoha_protocol::current_euid;
#[cfg(windows)]
use kotoha_protocol::extract_windows_pipe_name;
use kotoha_protocol::{
    resolve_endpoint_details, FrameHeader, Message, ProtocolError, TalkingStateKind,
    AUDIO_FLAG_IS_LOCAL_USER, FRAME_HEADER_LEN,
};
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;
#[cfg(windows)]
use widestring::u16cstr;

use crate::domain::{
    AudioChunk, AudioInputEvent, CoreError, DisconnectReason, SpeakerKey, TalkingState,
};

/// Liveness and capacity limits for [`IpcServerAdapter`] (see `DESIGN.md` Section 3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpcServerConfig {
    /// Maximum time between accepting a connection and receiving a valid `Hello`.
    pub hello_timeout: Duration,
    /// Maximum time without receiving any frame (`Heartbeat` included) after `Hello`.
    pub idle_timeout: Duration,
    /// Maximum number of simultaneously served connections; extra connections are dropped.
    pub max_connections: usize,
}

impl Default for IpcServerConfig {
    fn default() -> Self {
        Self {
            hello_timeout: Duration::from_secs(2),
            idle_timeout: Duration::from_secs(5),
            max_connections: 16,
        }
    }
}

/// `source -> (conn_id, cancel)` for the connection that currently owns each source.
type OwnerRegistry = Arc<Mutex<HashMap<String, (u64, oneshot::Sender<()>)>>>;

/// Cross-platform asynchronous IPC v1 server accepting plugin connections over Unix Domain
/// Sockets (Linux/macOS, `0600` permissions) or Windows Named Pipes.
///
/// On Unix, an existing path at the endpoint is only removed when it is a socket owned by the
/// effective UID and no live server answers on it; anything else (regular file, directory,
/// symlink, another user's socket) makes [`IpcServerAdapter::run`] fail without touching it.
#[derive(Debug, Clone)]
pub struct IpcServerAdapter {
    endpoint: String,
    private_dir: Option<PathBuf>,
    next_conn_id: Arc<AtomicU64>,
    config: IpcServerConfig,
    owners: OwnerRegistry,
}

impl IpcServerAdapter {
    /// Creates an [`IpcServerAdapter`] bound to `endpoint`.
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self::with_config(endpoint, IpcServerConfig::default())
    }

    /// Creates an [`IpcServerAdapter`] bound to `endpoint` with explicit liveness limits.
    #[must_use]
    pub fn with_config(endpoint: impl Into<String>, config: IpcServerConfig) -> Self {
        Self {
            endpoint: endpoint.into(),
            private_dir: None,
            next_conn_id: Arc::new(AtomicU64::new(1)),
            config,
            owners: Arc::default(),
        }
    }

    /// Creates an [`IpcServerAdapter`] using [`kotoha_protocol::resolve_endpoint_details`].
    ///
    /// When the endpoint was derived from the per-user private directory fallback
    /// (`${TMPDIR:-/tmp}/kotoha-<euid>/`), that directory is enforced as described in
    /// [`IpcServerAdapter::with_private_dir`].
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_env_with_config(IpcServerConfig::default())
    }

    /// Like [`IpcServerAdapter::from_env`], but with explicit liveness limits.
    #[must_use]
    pub fn from_env_with_config(config: IpcServerConfig) -> Self {
        let resolved = resolve_endpoint_details();
        let server = Self::with_config(resolved.endpoint, config);
        match resolved.private_dir {
            Some(dir) => server.with_private_dir(dir),
            None => server,
        }
    }

    /// Marks `dir` as the per-user private directory that must contain the endpoint (Unix only;
    /// ignored on Windows). Composes with [`IpcServerAdapter::with_config`], e.g.
    /// `IpcServerAdapter::with_config(endpoint, config).with_private_dir(dir)`.
    ///
    /// Before binding, the server creates `dir` with mode `0700` if it is missing (its parent
    /// must exist) and refuses to start unless `dir` is a real directory (not a symlink) owned
    /// by the effective UID with no group/other permission bits.
    #[must_use]
    pub fn with_private_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.private_dir = Some(dir.into());
        self
    }

    /// Returns the configured IPC endpoint string.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Returns the enforced per-user private directory, if any.
    #[must_use]
    pub fn private_dir(&self) -> Option<&Path> {
        self.private_dir.as_deref()
    }

    /// Binds the IPC listener and serves incoming client connections until `shutdown_rx` signals
    /// `true`.
    ///
    /// Emits validated [`AudioInputEvent`]s into `tx`. When `ready_tx` is provided, sends `()`
    /// immediately after the listener has bound and permissions have been applied.
    pub async fn run(
        &self,
        tx: mpsc::Sender<AudioInputEvent>,
        mut shutdown_rx: watch::Receiver<bool>,
        ready_tx: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<(), CoreError> {
        #[cfg(unix)]
        let (listener, bound_identity) =
            bind_unix_listener(&self.endpoint, self.private_dir.as_deref()).await?;
        #[cfg(windows)]
        let listener = bind_windows_listener(&self.endpoint)?;

        if let Some(ready) = ready_tx {
            let _ = ready.send(());
        }

        let mut client_tasks = JoinSet::new();
        let mut accept_err: Option<CoreError> = None;

        loop {
            if *shutdown_rx.borrow_and_update() {
                break;
            }

            let client_shutdown = shutdown_rx.clone();
            tokio::select! {
                biased;
                _ = shutdown_rx.wait_for(|v| *v) => {
                    break;
                }
                accept_res = listener.accept() => {
                    match accept_res {
                        Ok(stream) => {
                            while client_tasks.try_join_next().is_some() {}
                            if client_tasks.len() >= self.config.max_connections {
                                // Over capacity: drop the new connection immediately.
                                drop(stream);
                                continue;
                            }
                            let conn_id = self.next_conn_id.fetch_add(1, Ordering::Relaxed);
                            let client_tx = tx.clone();
                            let config = self.config;
                            let owners = Arc::clone(&self.owners);
                            client_tasks.spawn(async move {
                                handle_connection(
                                    conn_id,
                                    stream,
                                    client_tx,
                                    client_shutdown,
                                    config,
                                    owners,
                                )
                                .await;
                            });
                        }
                        Err(err) => {
                            accept_err = Some(CoreError::Ipc(format!(
                                "IPC accept failed on '{}': {err}",
                                self.endpoint
                            )));
                            break;
                        }
                    }
                }
            }

            while let Some(_res) = client_tasks.try_join_next() {}
        }

        drop(listener);
        #[cfg(unix)]
        cleanup_unix_socket_on_shutdown(&self.endpoint, bound_identity);

        if accept_err.is_some() {
            client_tasks.abort_all();
        }
        while let Some(_res) = client_tasks.join_next().await {}

        accept_err.map_or(Ok(()), Err)
    }
}

/// Identity (device + inode) of the socket file this server bound, used so that shutdown cleanup
/// never removes a path that was replaced after bind.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SocketIdentity {
    dev: u64,
    ino: u64,
}

#[cfg(unix)]
impl SocketIdentity {
    fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }
}

/// Creates (mode `0700`) and validates the per-user private directory that holds the endpoint.
#[cfg(unix)]
fn ensure_private_dir(dir: &Path) -> Result<(), CoreError> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};

    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
        Err(err) => {
            return Err(CoreError::Ipc(format!(
                "failed to create private IPC directory '{}': {err}",
                dir.display()
            )));
        }
    }

    let meta = std::fs::symlink_metadata(dir).map_err(|e| {
        CoreError::Ipc(format!(
            "failed to inspect private IPC directory '{}': {e}",
            dir.display()
        ))
    })?;
    if !meta.file_type().is_dir() {
        return Err(CoreError::Ipc(format!(
            "refusing to use private IPC directory '{}': it is not a directory (symlinks are not \
             allowed)",
            dir.display()
        )));
    }
    let euid = current_euid();
    if meta.uid() != euid {
        return Err(CoreError::Ipc(format!(
            "refusing to use private IPC directory '{}': owned by uid {} instead of uid {euid}",
            dir.display(),
            meta.uid()
        )));
    }
    let mode = meta.mode() & 0o7777;
    if mode & 0o077 != 0 {
        return Err(CoreError::Ipc(format!(
            "refusing to use private IPC directory '{}': permissions {mode:04o} grant group/other \
             access (expected 0700)",
            dir.display()
        )));
    }
    Ok(())
}

/// Inspects an existing path at the endpoint and removes it only if it is a stale socket owned by
/// the effective UID. Returns an error (without deleting anything) in every other case.
#[cfg(unix)]
async fn remove_stale_unix_socket(path: &Path, endpoint: &str) -> Result<(), CoreError> {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(CoreError::Ipc(format!(
                "failed to inspect Unix socket path '{endpoint}': {err}"
            )));
        }
    };

    let file_type = meta.file_type();
    if !file_type.is_socket() {
        let kind = if file_type.is_symlink() {
            "a symlink"
        } else if file_type.is_dir() {
            "a directory"
        } else if file_type.is_file() {
            "a regular file"
        } else {
            "not a socket"
        };
        return Err(CoreError::Ipc(format!(
            "refusing to replace IPC endpoint '{endpoint}': existing path is {kind}, not a socket; \
             remove it manually or choose another endpoint"
        )));
    }

    let euid = current_euid();
    if meta.uid() != euid {
        return Err(CoreError::Ipc(format!(
            "refusing to replace IPC endpoint '{endpoint}': existing socket is owned by uid {} \
             (current uid {euid})",
            meta.uid()
        )));
    }

    // Probe whether another live server of ours is already listening before removing.
    let probe_name = path
        .to_fs_name::<GenericFilePath>()
        .map_err(|e| CoreError::Ipc(format!("invalid Unix socket path '{endpoint}': {e}")))?;
    if LocalSocketStream::connect(probe_name).await.is_ok() {
        return Err(CoreError::Ipc(format!(
            "IPC endpoint '{endpoint}' is already in use by an active server"
        )));
    }

    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(CoreError::Ipc(format!(
            "failed to remove stale socket file '{endpoint}': {err}"
        ))),
    }
}

#[cfg(unix)]
async fn bind_unix_listener(
    endpoint: &str,
    private_dir: Option<&Path>,
) -> Result<(interprocess::local_socket::tokio::Listener, SocketIdentity), CoreError> {
    use std::os::unix::fs::DirBuilderExt as _;

    let path = Path::new(endpoint);
    if let Some(dir) = private_dir {
        ensure_private_dir(dir)?;
    } else if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(|e| {
                    CoreError::Ipc(format!(
                        "failed to create parent directory '{}' for socket: {e}",
                        parent.display()
                    ))
                })?;
        }
    }

    remove_stale_unix_socket(path, endpoint).await?;

    let name = path
        .to_fs_name::<GenericFilePath>()
        .map_err(|e| CoreError::Ipc(format!("invalid Unix socket path '{endpoint}': {e}")))?;

    // Disable `interprocess` name reclamation: its drop guard unlinks the path unconditionally.
    // Cleanup is done by `cleanup_unix_socket_on_shutdown`, which verifies the file first.
    let options = ListenerOptions::new()
        .name(name)
        .reclaim_name(false)
        .try_overwrite(false);
    #[cfg(target_os = "linux")]
    let options = options.mode(0o600);

    let listener = options
        .create_tokio()
        .map_err(|e| CoreError::Ipc(format!("failed to bind Unix socket '{endpoint}': {e}")))?;

    let identity = match std::fs::symlink_metadata(path) {
        Ok(meta) => SocketIdentity::of(&meta),
        Err(err) => {
            drop(listener);
            return Err(CoreError::Ipc(format!(
                "failed to inspect bound Unix socket '{endpoint}': {err}"
            )));
        }
    };

    // Enforce 0600 permissions explicitly after bind across Linux and macOS; unlink on failure.
    if let Err(err) = apply_unix_socket_permissions(path) {
        drop(listener);
        cleanup_unix_socket_on_shutdown(endpoint, identity);
        return Err(err);
    }
    Ok((listener, identity))
}

#[cfg(unix)]
fn apply_unix_socket_permissions(path: &Path) -> Result<(), CoreError> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms).map_err(|e| {
        CoreError::Ipc(format!(
            "failed to set 0600 permissions on '{}': {e}",
            path.display()
        ))
    })
}

#[cfg(windows)]
fn bind_windows_listener(
    endpoint: &str,
) -> Result<interprocess::local_socket::tokio::Listener, CoreError> {
    let pipe_name = extract_windows_pipe_name(endpoint)?;
    let name = pipe_name
        .to_ns_name::<GenericNamespaced>()
        .map_err(|e| CoreError::Ipc(format!("invalid Windows pipe name '{pipe_name}': {e}")))?;

    // Squatting protection: `interprocess` creates the first pipe instance with
    // `FILE_FLAG_FIRST_PIPE_INSTANCE`, so binding fails if another process already owns the name.
    // Restrict Named Pipe DACL to Local System (SY), Built-in Administrators (BA), and the
    // current user SID (OW/CO) so unprivileged local users and network clients cannot connect.
    let sd = SecurityDescriptor::deserialize(u16cstr!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)"))
        .map_err(|e| {
        CoreError::Ipc(format!(
            "failed to create Named Pipe security descriptor: {e}"
        ))
    })?;

    ListenerOptions::new()
        .name(name)
        .security_descriptor(sd)
        .create_tokio()
        .map_err(|e| CoreError::Ipc(format!("failed to bind Named Pipe '{endpoint}': {e}")))
}

/// Removes the socket file on shutdown, but only if the path still refers to the very socket this
/// server bound (same device/inode, still a socket, still owned by the effective UID). A path
/// that has since been replaced by anything else is left untouched.
#[cfg(unix)]
fn cleanup_unix_socket_on_shutdown(endpoint: &str, bound: SocketIdentity) {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
    let path = Path::new(endpoint);
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    let unchanged = meta.file_type().is_socket()
        && meta.uid() == current_euid()
        && SocketIdentity::of(&meta) == bound;
    if unchanged {
        let _ = std::fs::remove_file(path);
    }
}

/// Connects a client stream to `endpoint` (useful for testing and client utilities).
pub async fn connect_local_socket(endpoint: &str) -> Result<LocalSocketStream, CoreError> {
    #[cfg(unix)]
    let name = Path::new(endpoint)
        .to_fs_name::<GenericFilePath>()
        .map_err(|e| CoreError::Ipc(format!("invalid Unix socket path '{endpoint}': {e}")))?;

    #[cfg(windows)]
    let name = {
        let pipe_name = extract_windows_pipe_name(endpoint)?;
        pipe_name
            .to_ns_name::<GenericNamespaced>()
            .map_err(|e| CoreError::Ipc(format!("invalid Windows pipe name '{pipe_name}': {e}")))?
    };

    LocalSocketStream::connect(name)
        .await
        .map_err(|e| CoreError::Ipc(format!("failed to connect to '{endpoint}': {e}")))
}

async fn handle_connection(
    conn_id: u64,
    mut stream: LocalSocketStream,
    tx: mpsc::Sender<AudioInputEvent>,
    mut shutdown_rx: watch::Receiver<bool>,
    config: IpcServerConfig,
    owners: OwnerRegistry,
) {
    let mut source_name: Option<String> = None;
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    // Resolves only when a newer connection signals supersession; a dropped sender never cancels.
    let superseded = async move {
        if cancel_rx.await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    tokio::pin!(superseded);
    let mut cancel_tx = Some(cancel_tx);
    let mut deadline = Instant::now() + config.hello_timeout;

    let reason = loop {
        let stopping = *shutdown_rx.borrow_and_update();
        if stopping {
            break DisconnectReason::Shutdown;
        }

        let msg_res = tokio::select! {
            biased;
            _ = shutdown_rx.wait_for(|v| *v) => {
                break DisconnectReason::Shutdown;
            }
            () = &mut superseded => {
                break DisconnectReason::Superseded;
            }
            // Read before the deadline so a frame that is already available always wins.
            frame = read_next_message(&mut stream) => frame,
            () = tokio::time::sleep_until(deadline) => {
                break if source_name.is_some() {
                    DisconnectReason::IdleTimeout
                } else {
                    DisconnectReason::HelloTimeout
                };
            }
        };

        let msg = match msg_res {
            Ok(Some(msg)) => msg,
            Ok(None) => break DisconnectReason::Eof,
            Err(CoreError::Protocol(err)) => break DisconnectReason::Protocol(err.to_string()),
            Err(err) => break DisconnectReason::IoError(err.to_string()),
        };

        if let Some(source) = &source_name {
            let Some(event) = message_to_event(conn_id, source, msg) else {
                // Duplicate Hello mid-session is a protocol violation -> immediate disconnect.
                break DisconnectReason::Protocol("duplicate Hello".to_owned());
            };
            if tx.send(event).await.is_err() {
                break DisconnectReason::Shutdown;
            }
            // Restart the idle window only after the send completed, so time spent blocked on a
            // slow consumer is not counted as client silence.
            deadline = Instant::now() + config.idle_timeout;
        } else if let Message::Hello {
            app_name,
            plugin_version,
        } = msg
        {
            // Reserve channel capacity BEFORE taking the registry lock, then insert, cancel the
            // previous owner and enqueue `SourceConnected` while holding the lock. This keeps the
            // channel order identical to the registry order, so a superseded connection's events
            // can never be observed after (and override) the new owner's `SourceConnected`.
            let Ok(permit) = tx.reserve().await else {
                break DisconnectReason::Shutdown;
            };
            source_name = Some(app_name.clone());
            if let Some(cancel) = cancel_tx.take() {
                let mut map = owners
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let previous = map.insert(app_name.clone(), (conn_id, cancel));
                if let Some((_, old_cancel)) = previous {
                    let _ = old_cancel.send(());
                }
                permit.send(AudioInputEvent::SourceConnected {
                    conn_id,
                    source: app_name,
                    plugin_version,
                });
                drop(map);
            }
            deadline = Instant::now() + config.idle_timeout;
        } else {
            // Any non-Hello frame before handshake is a protocol violation -> immediate disconnect.
            break DisconnectReason::Protocol("frame received before Hello".to_owned());
        }
    };

    if let Some(source) = source_name {
        {
            let mut map = owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if map.get(&source).is_some_and(|(id, _)| *id == conn_id) {
                map.remove(&source);
            }
        }
        let _ = tx
            .send(AudioInputEvent::SourceDisconnected {
                conn_id,
                source,
                reason,
            })
            .await;
    }
}

fn message_to_event(conn_id: u64, source: &str, msg: Message) -> Option<AudioInputEvent> {
    match msg {
        Message::Hello { .. } => None,
        Message::AudioFrame {
            user_id,
            seq,
            sample_rate,
            channels,
            flags,
            pcm_samples,
        } => Some(AudioInputEvent::AudioFrame {
            conn_id,
            chunk: AudioChunk {
                speaker_key: SpeakerKey::new(source.to_owned(), user_id),
                seq,
                sample_rate,
                channels,
                is_local_user: (flags & AUDIO_FLAG_IS_LOCAL_USER) != 0,
                samples: pcm_samples,
            },
        }),
        Message::TalkingState { user_id, state } => Some(AudioInputEvent::TalkingStateChanged {
            conn_id,
            speaker_key: SpeakerKey::new(source.to_owned(), user_id),
            state: map_talking_state(state),
        }),
        Message::UserInfo { user_id, name } => Some(AudioInputEvent::UserInfoUpdated {
            conn_id,
            speaker_key: SpeakerKey::new(source.to_owned(), user_id),
            name,
        }),
        Message::Heartbeat => Some(AudioInputEvent::Heartbeat {
            conn_id,
            source: source.to_owned(),
        }),
        Message::UserRemoved { user_id } => Some(AudioInputEvent::UserRemoved {
            conn_id,
            speaker_key: SpeakerKey::new(source.to_owned(), user_id),
        }),
        Message::SourceReset => Some(AudioInputEvent::SourceReset {
            conn_id,
            source: source.to_owned(),
        }),
    }
}

async fn read_next_message(stream: &mut LocalSocketStream) -> Result<Option<Message>, CoreError> {
    let mut header_buf = [0u8; FRAME_HEADER_LEN];
    let mut read = 0usize;

    while read < FRAME_HEADER_LEN {
        match stream.read(&mut header_buf[read..]).await {
            Ok(0) => {
                if read == 0 {
                    return Ok(None);
                }
                return Err(CoreError::Protocol(ProtocolError::UnexpectedEof {
                    expected: FRAME_HEADER_LEN,
                    actual: read,
                }));
            }
            Ok(n) => read += n,
            Err(err) => {
                return Err(CoreError::Ipc(format!("failed reading IPC header: {err}")));
            }
        }
    }

    let header = FrameHeader::decode(&header_buf)?;
    let payload_len = header.payload_len as usize;
    let mut payload = vec![0u8; payload_len];
    if payload_len > 0 {
        stream.read_exact(&mut payload).await.map_err(|e| {
            if e.kind() == ErrorKind::UnexpectedEof {
                CoreError::Protocol(ProtocolError::UnexpectedEof {
                    expected: payload_len,
                    actual: 0,
                })
            } else {
                CoreError::Ipc(format!("failed reading IPC payload: {e}"))
            }
        })?;
    }

    let msg = Message::decode_payload(header.msg_type, &payload)?;
    Ok(Some(msg))
}

const fn map_talking_state(kind: TalkingStateKind) -> TalkingState {
    match kind {
        TalkingStateKind::Passive => TalkingState::Passive,
        TalkingStateKind::Talking => TalkingState::Talking,
        TalkingStateKind::Whispering => TalkingState::Whispering,
        TalkingStateKind::Shouting => TalkingState::Shouting,
        TalkingStateKind::TalkingMuted => TalkingState::TalkingMuted,
    }
}
