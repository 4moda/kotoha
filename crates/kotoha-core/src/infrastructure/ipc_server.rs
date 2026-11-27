//! Asynchronous cross-platform IPC server adapter built on `tokio` + `interprocess` + `kotoha-protocol`.

use std::io::ErrorKind;
#[cfg(unix)]
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

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
#[cfg(windows)]
use kotoha_protocol::extract_windows_pipe_name;
use kotoha_protocol::{
    resolve_endpoint, FrameHeader, Message, ProtocolError, TalkingStateKind,
    AUDIO_FLAG_IS_LOCAL_USER, FRAME_HEADER_LEN,
};
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
#[cfg(windows)]
use widestring::u16cstr;

use crate::domain::{AudioChunk, AudioInputEvent, CoreError, SpeakerKey, TalkingState};

/// Cross-platform asynchronous IPC v1 server accepting plugin connections over Unix Domain
/// Sockets (Linux/macOS, `0600` permissions) or Windows Named Pipes.
#[derive(Debug, Clone)]
pub struct IpcServerAdapter {
    endpoint: String,
    next_conn_id: Arc<AtomicU64>,
}

impl IpcServerAdapter {
    /// Creates an [`IpcServerAdapter`] bound to `endpoint`.
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            next_conn_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Creates an [`IpcServerAdapter`] using [`kotoha_protocol::resolve_endpoint`].
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(resolve_endpoint())
    }

    /// Returns the configured IPC endpoint string.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
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
        let listener = bind_unix_listener(&self.endpoint).await?;
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
                            let conn_id = self.next_conn_id.fetch_add(1, Ordering::Relaxed);
                            let client_tx = tx.clone();
                            client_tasks.spawn(async move {
                                handle_connection(conn_id, stream, client_tx, client_shutdown).await;
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
        cleanup_unix_socket_on_shutdown(&self.endpoint);

        if accept_err.is_some() {
            client_tasks.abort_all();
        }
        while let Some(_res) = client_tasks.join_next().await {}

        accept_err.map_or(Ok(()), Err)
    }
}

#[cfg(unix)]
async fn bind_unix_listener(
    endpoint: &str,
) -> Result<interprocess::local_socket::tokio::Listener, CoreError> {
    let path = Path::new(endpoint);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CoreError::Ipc(format!(
                    "failed to create parent directory '{}' for socket: {e}",
                    parent.display()
                ))
            })?;
        }
    }

    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            // Probe whether another live server is already listening before removing a stale socket
            // or dangling symlink.
            let probe_name = path.to_fs_name::<GenericFilePath>().map_err(|e| {
                CoreError::Ipc(format!("invalid Unix socket path '{endpoint}': {e}"))
            })?;
            match LocalSocketStream::connect(probe_name).await {
                Ok(_active_stream) => {
                    return Err(CoreError::Ipc(format!(
                        "IPC endpoint '{endpoint}' is already in use by an active server"
                    )));
                }
                Err(_) => {
                    if let Err(err) = std::fs::remove_file(path) {
                        if err.kind() != ErrorKind::NotFound {
                            return Err(CoreError::Ipc(format!(
                                "failed to remove stale socket file '{endpoint}': {err}"
                            )));
                        }
                    }
                }
            }
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => {
            return Err(CoreError::Ipc(format!(
                "failed to inspect Unix socket path '{endpoint}': {err}"
            )));
        }
    }

    let name = path
        .to_fs_name::<GenericFilePath>()
        .map_err(|e| CoreError::Ipc(format!("invalid Unix socket path '{endpoint}': {e}")))?;

    let options = ListenerOptions::new().name(name);
    #[cfg(target_os = "linux")]
    let options = options.mode(0o600);

    let listener = options
        .create_tokio()
        .map_err(|e| CoreError::Ipc(format!("failed to bind Unix socket '{endpoint}': {e}")))?;

    // Enforce 0600 permissions explicitly after bind across Linux and macOS; unlink on failure.
    if let Err(err) = apply_unix_socket_permissions(path) {
        drop(listener);
        let _ = std::fs::remove_file(path);
        return Err(err);
    }
    Ok(listener)
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

#[cfg(unix)]
fn cleanup_unix_socket_on_shutdown(endpoint: &str) {
    let path = Path::new(endpoint);
    if std::fs::symlink_metadata(path).is_ok() {
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
) {
    let mut source_name: Option<String> = None;

    loop {
        if *shutdown_rx.borrow_and_update() {
            break;
        }

        let msg_res = tokio::select! {
            biased;
            _ = shutdown_rx.wait_for(|v| *v) => {
                break;
            }
            frame = read_next_message(&mut stream) => frame,
        };

        let Ok(Some(msg)) = msg_res else { break };

        if let Some(source) = &source_name {
            let Some(event) = message_to_event(conn_id, source, msg) else {
                // Duplicate Hello mid-session is a protocol violation -> immediate disconnect.
                break;
            };
            if tx.send(event).await.is_err() {
                break;
            }
        } else if let Message::Hello {
            app_name,
            plugin_version,
        } = msg
        {
            source_name = Some(app_name.clone());
            if tx
                .send(AudioInputEvent::SourceConnected {
                    conn_id,
                    source: app_name,
                    plugin_version,
                })
                .await
                .is_err()
            {
                break;
            }
        } else {
            // Any non-Hello frame before handshake is a protocol violation -> immediate disconnect.
            break;
        }
    }

    if let Some(source) = source_name {
        let _ = tx
            .send(AudioInputEvent::SourceDisconnected { conn_id, source })
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
