//! Kotoha IPC v1 binary protocol codec, validation, and cross-platform endpoint resolution.
//!
//! Matches Section 3 of `DESIGN.md` byte-for-byte in Little-Endian binary framing.

pub mod codec;
pub mod endpoint;
pub mod error;

pub use codec::{
    decode_frame, decode_frame_exact, encode_frame, max_samples_per_frame, FrameHeader, Message,
    MessageType, TalkingStateKind, AUDIO_FLAGS_VALID_MASK, AUDIO_FLAG_IS_LOCAL_USER,
    AUDIO_FRAME_HEADER_LEN, FRAME_HEADER_LEN, MAX_APP_NAME_LEN, MAX_CHANNELS, MAX_FRAME_LEN,
    MAX_PAYLOAD_LEN, MAX_PLUGIN_VERSION_LEN, MAX_SAMPLE_RATE, MIN_APP_NAME_LEN, MIN_CHANNELS,
    MIN_SAMPLE_RATE, PROTOCOL_MAGIC, PROTOCOL_VERSION,
};
pub use endpoint::{
    extract_windows_pipe_name, resolve_endpoint, resolve_endpoint_with, resolve_windows_pipe_name,
    TargetOs, DEFAULT_UNIX_FALLBACK_ENDPOINT, DEFAULT_UNIX_SOCKET_NAME, DEFAULT_WINDOWS_ENDPOINT,
    DEFAULT_WINDOWS_PIPE_NAME, ENV_IPC_ENDPOINT, WINDOWS_PIPE_PREFIX,
};
pub use error::ProtocolError;
