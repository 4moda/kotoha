//! Typed error definitions for the Kotoha IPC v1 protocol and endpoint resolution.

use thiserror::Error;

/// Errors produced while decoding, encoding, validating, or resolving endpoints for Kotoha IPC v1.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtocolError {
    /// Input buffer is shorter than required for a header or frame.
    #[error("unexpected end of input: expected at least {expected} bytes, got {actual}")]
    UnexpectedEof {
        /// Minimum number of bytes expected.
        expected: usize,
        /// Actual number of bytes available.
        actual: usize,
    },

    /// Header magic bytes did not match `b"VT"` (`[0x56, 0x54]`).
    #[error("invalid protocol magic: expected [0x56, 0x54], got [{0:#04x}, {1:#04x}]")]
    InvalidMagic(u8, u8),

    /// Protocol version byte is unsupported (only version `1` is supported).
    #[error("unsupported protocol version: {0}")]
    UnsupportedVersion(u8),

    /// Message type byte is outside `0x00..=0x06`.
    #[error("unknown message type: {0:#04x}")]
    UnknownMessageType(u8),

    /// Payload length exceeds [`crate::MAX_PAYLOAD_LEN`] (`65536` bytes).
    #[error("payload too large: {size} bytes (max {max})")]
    PayloadTooLarge {
        /// Actual payload length declared or provided.
        size: usize,
        /// Maximum allowed payload length.
        max: usize,
    },

    /// Payload length does not match the constraints of the message type, or trailing bytes remain.
    #[error("invalid payload length for {message}: {detail}")]
    InvalidPayloadLength {
        /// Message type name.
        message: &'static str,
        /// Human-readable reason describing the length mismatch.
        detail: String,
    },

    /// A string field exceeded its allowed byte length or failed minimum length constraints.
    #[error("invalid string length for {field}: {len} bytes (expected {min}..={max})")]
    InvalidStringLength {
        /// Field name.
        field: &'static str,
        /// Actual byte length.
        len: usize,
        /// Minimum allowed byte length.
        min: usize,
        /// Maximum allowed byte length.
        max: usize,
    },

    /// A string field contained invalid UTF-8 bytes.
    #[error("invalid UTF-8 in {field}")]
    InvalidUtf8 {
        /// Field name.
        field: &'static str,
    },

    /// A string field contained an embedded NUL (`0x00`) byte.
    #[error("string field {field} contains forbidden NUL byte")]
    ContainsNulByte {
        /// Field name.
        field: &'static str,
    },

    /// Audio sample rate is outside `8000..=192000` Hz.
    #[error("invalid sample rate: {0} Hz (expected 8000..=192000)")]
    InvalidSampleRate(u32),

    /// Audio channel count is outside `1..=8`.
    #[error("invalid channel count: {0} (expected 1..=8)")]
    InvalidChannelCount(u16),

    /// `AudioFrame` flags contained non-zero reserved bits (`flags & 0xFFFE != 0`).
    #[error("invalid audio frame flags: {0:#06x} (reserved bits 1..=15 must be zero)")]
    InvalidFlags(u16),

    /// `AudioFrame` contained a non-finite (`NaN` / `Inf`) or out-of-range (`< -1.0` or `> 1.0`) sample.
    #[error(
        "invalid PCM sample at index {index}: {value} (must be finite and within [-1.0, 1.0])"
    )]
    InvalidPcmSample {
        /// Index of the offending sample in `pcm_samples`.
        index: usize,
        /// String representation of the offending float value.
        value: String,
    },

    /// `TalkingState` state byte is not in `0..=4`.
    #[error("invalid talking state value: {0} (expected 0..=4)")]
    InvalidTalkingState(u8),

    /// IPC endpoint string is invalid (e.g., empty pipe name after stripping `\\.\pipe\`).
    #[error("invalid IPC endpoint: {0}")]
    InvalidEndpoint(String),
}
