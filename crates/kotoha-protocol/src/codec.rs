//! Binary framing, constants, message types, encoding, and strict validation for Kotoha IPC v1.

use crate::error::ProtocolError;

/// Fixed 2-byte protocol magic (`"VT"`).
pub const PROTOCOL_MAGIC: [u8; 2] = [0x56, 0x54];

/// Protocol version supported by this crate (`1`).
pub const PROTOCOL_VERSION: u8 = 1;

/// Fixed frame header size in bytes (`magic` + `version` + `msg_type` + `payload_len`).
pub const FRAME_HEADER_LEN: usize = 8;

/// Maximum payload size in bytes (`65536`).
pub const MAX_PAYLOAD_LEN: usize = 65_536;

/// Maximum total frame size in bytes (`FRAME_HEADER_LEN + MAX_PAYLOAD_LEN = 65544`).
pub const MAX_FRAME_LEN: usize = FRAME_HEADER_LEN + MAX_PAYLOAD_LEN;

/// Minimum allowed byte length for `Hello::app_name`.
pub const MIN_APP_NAME_LEN: usize = 1;

/// Maximum allowed byte length for `Hello::app_name`.
pub const MAX_APP_NAME_LEN: usize = 64;

/// Maximum allowed byte length for `Hello::plugin_version`.
pub const MAX_PLUGIN_VERSION_LEN: usize = 64;

/// Fixed byte length of the `AudioFrame` subheader (`user_id` + `seq` + `sample_rate` + `channels` + `flags`).
pub const AUDIO_FRAME_HEADER_LEN: usize = 16;

/// Minimum allowed audio sample rate in Hz (`8000`).
pub const MIN_SAMPLE_RATE: u32 = 8_000;

/// Maximum allowed audio sample rate in Hz (`192000`).
pub const MAX_SAMPLE_RATE: u32 = 192_000;

/// Minimum allowed audio channel count (`1`).
pub const MIN_CHANNELS: u16 = 1;

/// Maximum allowed audio channel count (`8`).
pub const MAX_CHANNELS: u16 = 8;

/// `AudioFrame` flag bit indicating the speaker is the local user (`0x0001`).
pub const AUDIO_FLAG_IS_LOCAL_USER: u16 = 0x0001;

/// Bitmask of all valid `AudioFrame` flags (`0x0001`).
pub const AUDIO_FLAGS_VALID_MASK: u16 = AUDIO_FLAG_IS_LOCAL_USER;

/// Wire message type discriminant (`0x00..=0x06`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MessageType {
    /// `0x00`: Client handshake identifying app name and plugin version.
    Hello = 0x00,
    /// `0x01`: Interleaved `f32` PCM audio frame.
    AudioFrame = 0x01,
    /// `0x02`: User talking state transition notification.
    TalkingState = 0x02,
    /// `0x03`: User display name registration or update.
    UserInfo = 0x03,
    /// `0x04`: Periodic keepalive heartbeat.
    Heartbeat = 0x04,
    /// `0x05`: User disconnected / left channel notification.
    UserRemoved = 0x05,
    /// `0x06`: Voice chat server disconnected; reset all source state.
    SourceReset = 0x06,
}

impl MessageType {
    /// Returns the wire `u8` tag for this message type.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Returns a human-readable name for diagnostics and error messages.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Hello => "Hello",
            Self::AudioFrame => "AudioFrame",
            Self::TalkingState => "TalkingState",
            Self::UserInfo => "UserInfo",
            Self::Heartbeat => "Heartbeat",
            Self::UserRemoved => "UserRemoved",
            Self::SourceReset => "SourceReset",
        }
    }
}

impl TryFrom<u8> for MessageType {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(Self::Hello),
            0x01 => Ok(Self::AudioFrame),
            0x02 => Ok(Self::TalkingState),
            0x03 => Ok(Self::UserInfo),
            0x04 => Ok(Self::Heartbeat),
            0x05 => Ok(Self::UserRemoved),
            0x06 => Ok(Self::SourceReset),
            other => Err(ProtocolError::UnknownMessageType(other)),
        }
    }
}

/// Talking state values matching `Mumble_TalkingState` (`0..=4`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TalkingStateKind {
    /// `0`: Not talking.
    Passive = 0,
    /// `1`: Normal talking.
    Talking = 1,
    /// `2`: Whispering.
    Whispering = 2,
    /// `3`: Shouting.
    Shouting = 3,
    /// `4`: Talking while muted.
    TalkingMuted = 4,
}

impl TalkingStateKind {
    /// Returns the wire `u8` discriminant for this talking state.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for TalkingStateKind {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Passive),
            1 => Ok(Self::Talking),
            2 => Ok(Self::Whispering),
            3 => Ok(Self::Shouting),
            4 => Ok(Self::TalkingMuted),
            other => Err(ProtocolError::InvalidTalkingState(other)),
        }
    }
}

/// Decoded 8-byte IPC v1 frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Protocol version (always [`PROTOCOL_VERSION`] = `1` for valid headers).
    pub version: u8,
    /// Message type discriminant.
    pub msg_type: MessageType,
    /// Payload length in bytes (`0..=65536`).
    pub payload_len: u32,
}

impl FrameHeader {
    /// Decodes and validates an 8-byte little-endian frame header.
    ///
    /// Accepts slices longer than 8 bytes and inspects only `&bytes[..8]`, making it convenient
    /// for streaming readers that inspect a fixed 8-byte buffer.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() < FRAME_HEADER_LEN {
            return Err(ProtocolError::UnexpectedEof {
                expected: FRAME_HEADER_LEN,
                actual: bytes.len(),
            });
        }

        if bytes[0..2] != PROTOCOL_MAGIC {
            return Err(ProtocolError::InvalidMagic(bytes[0], bytes[1]));
        }

        let version = bytes[2];
        if version != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion(version));
        }

        let msg_type = MessageType::try_from(bytes[3])?;
        let payload_len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        if (payload_len as usize) > MAX_PAYLOAD_LEN {
            return Err(ProtocolError::PayloadTooLarge {
                size: payload_len as usize,
                max: MAX_PAYLOAD_LEN,
            });
        }

        Ok(Self {
            version,
            msg_type,
            payload_len,
        })
    }

    /// Encodes this frame header into an 8-byte little-endian array after validation.
    pub const fn encode(&self) -> Result<[u8; FRAME_HEADER_LEN], ProtocolError> {
        if self.version != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion(self.version));
        }
        if (self.payload_len as usize) > MAX_PAYLOAD_LEN {
            return Err(ProtocolError::PayloadTooLarge {
                size: self.payload_len as usize,
                max: MAX_PAYLOAD_LEN,
            });
        }

        let len_bytes = self.payload_len.to_le_bytes();
        Ok([
            PROTOCOL_MAGIC[0],
            PROTOCOL_MAGIC[1],
            self.version,
            self.msg_type.as_u8(),
            len_bytes[0],
            len_bytes[1],
            len_bytes[2],
            len_bytes[3],
        ])
    }
}

/// Returns the maximum number of interleaved `f32` PCM samples that fit into a single
/// `AudioFrame` for the given channel count while staying channel-aligned and `<= 65536` bytes.
///
/// Formula from Section 3.4 of `DESIGN.md`:
/// `((65536 - 16) / (4 * channels)) * channels`
///
/// Returns `Err(ProtocolError::InvalidChannelCount)` if `channels` is not in `1..=8`.
pub fn max_samples_per_frame(channels: u16) -> Result<usize, ProtocolError> {
    validate_channels(channels)?;
    let ch = usize::from(channels);
    let max_sample_bytes = MAX_PAYLOAD_LEN - AUDIO_FRAME_HEADER_LEN;
    let frames = max_sample_bytes / (4 * ch);
    Ok(frames * ch)
}

/// Decoded IPC v1 protocol message (`0x00..=0x06`).
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// `0x00`: Client identification handshake.
    Hello {
        /// Source application identifier (1..=64 bytes, valid UTF-8, no NUL).
        app_name: String,
        /// Plugin version string (0..=64 bytes, valid UTF-8, no NUL).
        plugin_version: String,
    },
    /// `0x01`: Interleaved `f32` PCM audio frame.
    AudioFrame {
        /// Speaker ID unique within the source plugin.
        user_id: u32,
        /// Monotonically increasing sequence counter across the plugin instance.
        seq: u32,
        /// Sample rate in Hz (`8000..=192000`).
        sample_rate: u32,
        /// Channel count (`1..=8`).
        channels: u16,
        /// Bitflags (`0x0001` = `IS_LOCAL_USER`, bits `1..=15` must be 0).
        flags: u16,
        /// Interleaved IEEE 754 `f32` PCM samples, each finite and within `[-1.0, 1.0]`.
        pcm_samples: Vec<f32>,
    },
    /// `0x02`: User talking state transition.
    TalkingState {
        /// Target speaker ID.
        user_id: u32,
        /// Talking state enum (`0..=4`).
        state: TalkingStateKind,
    },
    /// `0x03`: User display name registration or update.
    UserInfo {
        /// Target speaker ID.
        user_id: u32,
        /// Display name (valid UTF-8, no NUL; empty allowed as fallback trigger).
        name: String,
    },
    /// `0x04`: Keepalive heartbeat.
    Heartbeat,
    /// `0x05`: User removed from channel/server.
    UserRemoved {
        /// Removed speaker ID.
        user_id: u32,
    },
    /// `0x06`: Voice chat source disconnected/reset.
    SourceReset,
}

impl Message {
    /// Returns the wire [`MessageType`] corresponding to this message variant.
    #[must_use]
    pub const fn msg_type(&self) -> MessageType {
        match self {
            Self::Hello { .. } => MessageType::Hello,
            Self::AudioFrame { .. } => MessageType::AudioFrame,
            Self::TalkingState { .. } => MessageType::TalkingState,
            Self::UserInfo { .. } => MessageType::UserInfo,
            Self::Heartbeat => MessageType::Heartbeat,
            Self::UserRemoved { .. } => MessageType::UserRemoved,
            Self::SourceReset => MessageType::SourceReset,
        }
    }

    /// Validates all fields of this message against the IPC v1 specification.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::Hello {
                app_name,
                plugin_version,
            } => {
                validate_bounded_str("app_name", app_name, MIN_APP_NAME_LEN, MAX_APP_NAME_LEN)?;
                validate_bounded_str("plugin_version", plugin_version, 0, MAX_PLUGIN_VERSION_LEN)?;
            }
            Self::AudioFrame {
                user_id: _,
                seq: _,
                sample_rate,
                channels,
                flags,
                pcm_samples,
            } => {
                validate_sample_rate(*sample_rate)?;
                validate_channels(*channels)?;
                validate_flags(*flags)?;

                let ch = usize::from(*channels);
                if pcm_samples.len() % ch != 0 {
                    return Err(ProtocolError::InvalidPayloadLength {
                        message: "AudioFrame",
                        detail: format!(
                            "sample count {} is not divisible by channel count {ch}",
                            pcm_samples.len()
                        ),
                    });
                }

                let payload_len = AUDIO_FRAME_HEADER_LEN + pcm_samples.len() * 4;
                if payload_len > MAX_PAYLOAD_LEN {
                    return Err(ProtocolError::PayloadTooLarge {
                        size: payload_len,
                        max: MAX_PAYLOAD_LEN,
                    });
                }

                validate_pcm_samples(pcm_samples)?;
            }
            Self::TalkingState { .. }
            | Self::Heartbeat
            | Self::UserRemoved { .. }
            | Self::SourceReset => {}
            Self::UserInfo { user_id: _, name } => {
                validate_no_nul("name", name)?;
                let payload_len = 4 + name.len();
                if payload_len > MAX_PAYLOAD_LEN {
                    return Err(ProtocolError::PayloadTooLarge {
                        size: payload_len,
                        max: MAX_PAYLOAD_LEN,
                    });
                }
            }
        }
        Ok(())
    }

    /// Encodes only the message payload (without the 8-byte header) after validating invariants.
    pub fn encode_payload(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;

        let mut buf = Vec::new();
        match self {
            Self::Hello {
                app_name,
                plugin_version,
            } => {
                let app_len = u16::try_from(app_name.len()).map_err(|_| {
                    ProtocolError::InvalidStringLength {
                        field: "app_name",
                        len: app_name.len(),
                        min: MIN_APP_NAME_LEN,
                        max: MAX_APP_NAME_LEN,
                    }
                })?;
                let ver_len = u16::try_from(plugin_version.len()).map_err(|_| {
                    ProtocolError::InvalidStringLength {
                        field: "plugin_version",
                        len: plugin_version.len(),
                        min: 0,
                        max: MAX_PLUGIN_VERSION_LEN,
                    }
                })?;
                buf.reserve(4 + app_name.len() + plugin_version.len());
                buf.extend_from_slice(&app_len.to_le_bytes());
                buf.extend_from_slice(app_name.as_bytes());
                buf.extend_from_slice(&ver_len.to_le_bytes());
                buf.extend_from_slice(plugin_version.as_bytes());
            }
            Self::AudioFrame {
                user_id,
                seq,
                sample_rate,
                channels,
                flags,
                pcm_samples,
            } => {
                buf.reserve(AUDIO_FRAME_HEADER_LEN + pcm_samples.len() * 4);
                buf.extend_from_slice(&user_id.to_le_bytes());
                buf.extend_from_slice(&seq.to_le_bytes());
                buf.extend_from_slice(&sample_rate.to_le_bytes());
                buf.extend_from_slice(&channels.to_le_bytes());
                buf.extend_from_slice(&flags.to_le_bytes());
                for &sample in pcm_samples {
                    buf.extend_from_slice(&sample.to_le_bytes());
                }
            }
            Self::TalkingState { user_id, state } => {
                buf.reserve(5);
                buf.extend_from_slice(&user_id.to_le_bytes());
                buf.push(state.as_u8());
            }
            Self::UserInfo { user_id, name } => {
                buf.reserve(4 + name.len());
                buf.extend_from_slice(&user_id.to_le_bytes());
                buf.extend_from_slice(name.as_bytes());
            }
            Self::Heartbeat | Self::SourceReset => {}
            Self::UserRemoved { user_id } => {
                buf.extend_from_slice(&user_id.to_le_bytes());
            }
        }
        Ok(buf)
    }

    /// Decodes and validates a message payload given a previously decoded [`MessageType`].
    pub fn decode_payload(msg_type: MessageType, payload: &[u8]) -> Result<Self, ProtocolError> {
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err(ProtocolError::PayloadTooLarge {
                size: payload.len(),
                max: MAX_PAYLOAD_LEN,
            });
        }

        match msg_type {
            MessageType::Hello => decode_hello_payload(payload),
            MessageType::AudioFrame => decode_audio_frame_payload(payload),
            MessageType::TalkingState => decode_talking_state_payload(payload),
            MessageType::UserInfo => decode_user_info_payload(payload),
            MessageType::Heartbeat => {
                if !payload.is_empty() {
                    return Err(ProtocolError::InvalidPayloadLength {
                        message: "Heartbeat",
                        detail: format!("expected 0 bytes, got {}", payload.len()),
                    });
                }
                Ok(Self::Heartbeat)
            }
            MessageType::UserRemoved => {
                if payload.len() != 4 {
                    return Err(ProtocolError::InvalidPayloadLength {
                        message: "UserRemoved",
                        detail: format!("expected 4 bytes, got {}", payload.len()),
                    });
                }
                let user_id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                Ok(Self::UserRemoved { user_id })
            }
            MessageType::SourceReset => {
                if !payload.is_empty() {
                    return Err(ProtocolError::InvalidPayloadLength {
                        message: "SourceReset",
                        detail: format!("expected 0 bytes, got {}", payload.len()),
                    });
                }
                Ok(Self::SourceReset)
            }
        }
    }
}

/// Encodes a [`Message`] into a complete framed byte buffer (8-byte LE header + payload).
pub fn encode_frame(msg: &Message) -> Result<Vec<u8>, ProtocolError> {
    let payload = msg.encode_payload()?;
    let payload_len = u32::try_from(payload.len()).map_err(|_| ProtocolError::PayloadTooLarge {
        size: payload.len(),
        max: MAX_PAYLOAD_LEN,
    })?;
    let header = FrameHeader {
        version: PROTOCOL_VERSION,
        msg_type: msg.msg_type(),
        payload_len,
    };
    let header_bytes = header.encode()?;

    let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
    frame.extend_from_slice(&header_bytes);
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Decodes a complete single frame from the front of `bytes`, returning `(message, bytes_consumed)`.
///
/// `bytes_consumed` is always `FRAME_HEADER_LEN + header.payload_len as usize`. Trailing bytes
/// after the frame are left unread so streaming callers can advance a slice/buffer cursor; use
/// [`decode_frame_exact`] when trailing bytes should be rejected.
pub fn decode_frame(bytes: &[u8]) -> Result<(Message, usize), ProtocolError> {
    let header = FrameHeader::decode(bytes)?;
    let payload_len = header.payload_len as usize;
    let total_len = FRAME_HEADER_LEN + payload_len;
    if bytes.len() < total_len {
        return Err(ProtocolError::UnexpectedEof {
            expected: total_len,
            actual: bytes.len(),
        });
    }
    let payload = &bytes[FRAME_HEADER_LEN..total_len];
    let msg = Message::decode_payload(header.msg_type, payload)?;
    Ok((msg, total_len))
}

/// Decodes an exact single frame from `bytes`, rejecting both truncated buffers and trailing bytes.
pub fn decode_frame_exact(bytes: &[u8]) -> Result<Message, ProtocolError> {
    let (msg, consumed) = decode_frame(bytes)?;
    if consumed != bytes.len() {
        return Err(ProtocolError::InvalidPayloadLength {
            message: msg.msg_type().name(),
            detail: format!(
                "expected exact frame length {consumed} bytes, got {} bytes",
                bytes.len()
            ),
        });
    }
    Ok(msg)
}

fn decode_hello_payload(payload: &[u8]) -> Result<Message, ProtocolError> {
    if payload.len() < 5 {
        return Err(ProtocolError::InvalidPayloadLength {
            message: "Hello",
            detail: format!("expected at least 5 bytes, got {}", payload.len()),
        });
    }

    let app_name_len = usize::from(u16::from_le_bytes([payload[0], payload[1]]));
    if !(MIN_APP_NAME_LEN..=MAX_APP_NAME_LEN).contains(&app_name_len) {
        return Err(ProtocolError::InvalidStringLength {
            field: "app_name",
            len: app_name_len,
            min: MIN_APP_NAME_LEN,
            max: MAX_APP_NAME_LEN,
        });
    }

    let ver_len_offset = 2 + app_name_len;
    if payload.len() < ver_len_offset + 2 {
        return Err(ProtocolError::InvalidPayloadLength {
            message: "Hello",
            detail: format!(
                "truncated before plugin_version_len: payload has {} bytes, need at least {}",
                payload.len(),
                ver_len_offset + 2
            ),
        });
    }

    let app_name = decode_utf8_no_nul("app_name", &payload[2..ver_len_offset])?;

    let plugin_version_len = usize::from(u16::from_le_bytes([
        payload[ver_len_offset],
        payload[ver_len_offset + 1],
    ]));
    if plugin_version_len > MAX_PLUGIN_VERSION_LEN {
        return Err(ProtocolError::InvalidStringLength {
            field: "plugin_version",
            len: plugin_version_len,
            min: 0,
            max: MAX_PLUGIN_VERSION_LEN,
        });
    }

    let expected_len = 4 + app_name_len + plugin_version_len;
    if payload.len() != expected_len {
        return Err(ProtocolError::InvalidPayloadLength {
            message: "Hello",
            detail: format!(
                "expected {expected_len} bytes (4 + {app_name_len} + {plugin_version_len}), got {}",
                payload.len()
            ),
        });
    }

    let plugin_version =
        decode_utf8_no_nul("plugin_version", &payload[ver_len_offset + 2..expected_len])?;

    Ok(Message::Hello {
        app_name,
        plugin_version,
    })
}

fn decode_audio_frame_payload(payload: &[u8]) -> Result<Message, ProtocolError> {
    if payload.len() < AUDIO_FRAME_HEADER_LEN {
        return Err(ProtocolError::InvalidPayloadLength {
            message: "AudioFrame",
            detail: format!(
                "expected at least {AUDIO_FRAME_HEADER_LEN} bytes, got {}",
                payload.len()
            ),
        });
    }

    let user_id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let seq = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let sample_rate = u32::from_le_bytes([payload[8], payload[9], payload[10], payload[11]]);
    let channels = u16::from_le_bytes([payload[12], payload[13]]);
    let flags = u16::from_le_bytes([payload[14], payload[15]]);

    validate_channels(channels)?;
    validate_sample_rate(sample_rate)?;
    validate_flags(flags)?;

    let pcm_bytes = &payload[AUDIO_FRAME_HEADER_LEN..];
    let frame_stride = 4 * usize::from(channels);
    if pcm_bytes.len() % frame_stride != 0 {
        return Err(ProtocolError::InvalidPayloadLength {
            message: "AudioFrame",
            detail: format!(
                "PCM byte length {} is not a multiple of 4 * channels ({frame_stride})",
                pcm_bytes.len()
            ),
        });
    }

    let sample_count = pcm_bytes.len() / 4;
    let mut pcm_samples = Vec::with_capacity(sample_count);
    for (index, chunk) in pcm_bytes.chunks_exact(4).enumerate() {
        let sample = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        if !is_valid_pcm_sample(sample) {
            return Err(ProtocolError::InvalidPcmSample {
                index,
                value: sample.to_string(),
            });
        }
        pcm_samples.push(sample);
    }

    Ok(Message::AudioFrame {
        user_id,
        seq,
        sample_rate,
        channels,
        flags,
        pcm_samples,
    })
}

fn decode_talking_state_payload(payload: &[u8]) -> Result<Message, ProtocolError> {
    if payload.len() != 5 {
        return Err(ProtocolError::InvalidPayloadLength {
            message: "TalkingState",
            detail: format!("expected 5 bytes, got {}", payload.len()),
        });
    }

    let user_id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let state = TalkingStateKind::try_from(payload[4])?;

    Ok(Message::TalkingState { user_id, state })
}

fn decode_user_info_payload(payload: &[u8]) -> Result<Message, ProtocolError> {
    if payload.len() < 4 {
        return Err(ProtocolError::InvalidPayloadLength {
            message: "UserInfo",
            detail: format!("expected at least 4 bytes, got {}", payload.len()),
        });
    }

    let user_id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let name = decode_utf8_no_nul("name", &payload[4..])?;

    Ok(Message::UserInfo { user_id, name })
}

fn validate_bounded_str(
    field: &'static str,
    value: &str,
    min: usize,
    max: usize,
) -> Result<(), ProtocolError> {
    if !(min..=max).contains(&value.len()) {
        return Err(ProtocolError::InvalidStringLength {
            field,
            len: value.len(),
            min,
            max,
        });
    }
    validate_no_nul(field, value)
}

fn validate_no_nul(field: &'static str, value: &str) -> Result<(), ProtocolError> {
    if value.as_bytes().contains(&0) {
        return Err(ProtocolError::ContainsNulByte { field });
    }
    Ok(())
}

fn decode_utf8_no_nul(field: &'static str, bytes: &[u8]) -> Result<String, ProtocolError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ProtocolError::InvalidUtf8 { field })?;
    validate_no_nul(field, text)?;
    Ok(text.to_owned())
}

const fn validate_sample_rate(sample_rate: u32) -> Result<(), ProtocolError> {
    if sample_rate < MIN_SAMPLE_RATE || sample_rate > MAX_SAMPLE_RATE {
        return Err(ProtocolError::InvalidSampleRate(sample_rate));
    }
    Ok(())
}

const fn validate_channels(channels: u16) -> Result<(), ProtocolError> {
    if channels < MIN_CHANNELS || channels > MAX_CHANNELS {
        return Err(ProtocolError::InvalidChannelCount(channels));
    }
    Ok(())
}

const fn validate_flags(flags: u16) -> Result<(), ProtocolError> {
    if (flags & !AUDIO_FLAGS_VALID_MASK) != 0 {
        return Err(ProtocolError::InvalidFlags(flags));
    }
    Ok(())
}

#[inline]
fn is_valid_pcm_sample(sample: f32) -> bool {
    sample.is_finite() && (-1.0..=1.0).contains(&sample)
}

fn validate_pcm_samples(samples: &[f32]) -> Result<(), ProtocolError> {
    for (index, &sample) in samples.iter().enumerate() {
        if !is_valid_pcm_sample(sample) {
            return Err(ProtocolError::InvalidPcmSample {
                index,
                value: sample.to_string(),
            });
        }
    }
    Ok(())
}
