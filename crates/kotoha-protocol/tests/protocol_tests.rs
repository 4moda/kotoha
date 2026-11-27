//! Comprehensive unit, boundary, malformed-input, and golden-vector tests for `kotoha-protocol`.

#![allow(clippy::expect_used)]

use kotoha_protocol::{
    decode_frame, decode_frame_exact, encode_frame, extract_windows_pipe_name,
    max_samples_per_frame, resolve_endpoint, resolve_endpoint_with, FrameHeader, Message,
    MessageType, ProtocolError, TalkingStateKind, TargetOs, AUDIO_FLAG_IS_LOCAL_USER,
    AUDIO_FRAME_HEADER_LEN, DEFAULT_UNIX_FALLBACK_ENDPOINT, DEFAULT_WINDOWS_ENDPOINT,
    ENV_IPC_ENDPOINT, FRAME_HEADER_LEN, MAX_PAYLOAD_LEN, PROTOCOL_MAGIC, PROTOCOL_VERSION,
};

fn build_raw_frame(magic: [u8; 2], version: u8, msg_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
    out.extend_from_slice(&magic);
    out.push(version);
    out.push(msg_type);
    let len = u32::try_from(payload.len()).expect("test payload fits in u32");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

fn assert_round_trip(msg: &Message) {
    let encoded = encode_frame(msg).expect("encode_frame must succeed for valid message");
    let header = FrameHeader::decode(&encoded).expect("header decode must succeed");
    assert_eq!(header.version, PROTOCOL_VERSION);
    assert_eq!(header.msg_type, msg.msg_type());
    assert_eq!(
        header.payload_len as usize,
        encoded.len() - FRAME_HEADER_LEN
    );

    let (decoded, consumed) = decode_frame(&encoded).expect("decode_frame must succeed");
    assert_eq!(consumed, encoded.len());
    assert_eq!(&decoded, msg);

    let exact = decode_frame_exact(&encoded).expect("decode_frame_exact must succeed");
    assert_eq!(&exact, msg);

    let payload_decoded = Message::decode_payload(header.msg_type, &encoded[FRAME_HEADER_LEN..])
        .expect("decode_payload must succeed");
    assert_eq!(&payload_decoded, msg);
}

#[test]
fn round_trip_all_seven_messages() {
    let messages = vec![
        Message::Hello {
            app_name: "mumble".to_owned(),
            plugin_version: "0.1.0".to_owned(),
        },
        Message::Hello {
            app_name: "a".to_owned(),
            plugin_version: String::new(),
        },
        Message::AudioFrame {
            user_id: 42,
            seq: 100,
            sample_rate: 48_000,
            channels: 2,
            flags: AUDIO_FLAG_IS_LOCAL_USER,
            pcm_samples: vec![-1.0, -0.25, 0.0, 0.25, 0.5, 1.0],
        },
        Message::TalkingState {
            user_id: 7,
            state: TalkingStateKind::Passive,
        },
        Message::TalkingState {
            user_id: 7,
            state: TalkingStateKind::Talking,
        },
        Message::TalkingState {
            user_id: 7,
            state: TalkingStateKind::Whispering,
        },
        Message::TalkingState {
            user_id: 7,
            state: TalkingStateKind::Shouting,
        },
        Message::TalkingState {
            user_id: 7,
            state: TalkingStateKind::TalkingMuted,
        },
        Message::UserInfo {
            user_id: 99,
            name: "Alice (日本語)".to_owned(),
        },
        Message::UserInfo {
            user_id: 99,
            name: String::new(),
        },
        Message::Heartbeat,
        Message::UserRemoved { user_id: 123_456 },
        Message::SourceReset,
    ];

    for msg in &messages {
        assert_round_trip(msg);
    }
}

fn assert_golden(msg: &Message, expected_bytes: &[u8]) {
    assert_eq!(encode_frame(msg).expect("encode golden"), expected_bytes);
    assert_eq!(
        &decode_frame_exact(expected_bytes).expect("decode golden"),
        msg
    );
}

#[test]
fn golden_byte_vectors_match_little_endian_spec() {
    assert_golden(
        &Message::Hello {
            app_name: "mumble".to_owned(),
            plugin_version: "0.1.0".to_owned(),
        },
        &[
            0x56, 0x54, 0x01, 0x00, 15, 0, 0, 0, // header: "VT", v1, Hello, len=15 LE
            6, 0, // app_name_len = 6 LE
            b'm', b'u', b'm', b'b', b'l', b'e', // "mumble"
            5, 0, // plugin_version_len = 5 LE
            b'0', b'.', b'1', b'.', b'0', // "0.1.0"
        ],
    );

    // 0.5f32 = 0x3F000000 -> LE: [00, 00, 00, 3F], -1.0f32 = 0xBF800000 -> LE: [00, 00, 80, BF]
    assert_golden(
        &Message::AudioFrame {
            user_id: 0x0102_0304,
            seq: 0x0506_0708,
            sample_rate: 48_000,
            channels: 2,
            flags: 0x0001,
            pcm_samples: vec![0.5, -1.0],
        },
        &[
            0x56, 0x54, 0x01, 0x01, 24, 0, 0, 0, // header: "VT", v1, AudioFrame, len=24 LE
            0x04, 0x03, 0x02, 0x01, // user_id LE
            0x08, 0x07, 0x06, 0x05, // seq LE
            0x80, 0xBB, 0x00, 0x00, // sample_rate = 48000 LE
            0x02, 0x00, // channels = 2 LE
            0x01, 0x00, // flags = 0x0001 LE
            0x00, 0x00, 0x00, 0x3F, // 0.5f32 LE
            0x00, 0x00, 0x80, 0xBF, // -1.0f32 LE
        ],
    );

    assert_golden(
        &Message::TalkingState {
            user_id: 0x1122_3344,
            state: TalkingStateKind::Shouting,
        },
        &[
            0x56, 0x54, 0x01, 0x02, 5, 0, 0, 0, // header: TalkingState, len=5
            0x44, 0x33, 0x22, 0x11, // user_id LE
            0x03, // Shouting
        ],
    );

    assert_golden(
        &Message::UserInfo {
            user_id: 7,
            name: "Bob".to_owned(),
        },
        &[
            0x56, 0x54, 0x01, 0x03, 7, 0, 0, 0, // header: UserInfo, len=7
            7, 0, 0, 0, // user_id LE
            b'B', b'o', b'b', // name UTF-8
        ],
    );

    assert_golden(&Message::Heartbeat, &[0x56, 0x54, 0x01, 0x04, 0, 0, 0, 0]);

    assert_golden(
        &Message::UserRemoved {
            user_id: 0xAABB_CCDD,
        },
        &[
            0x56, 0x54, 0x01, 0x05, 4, 0, 0, 0, // header: UserRemoved, len=4
            0xDD, 0xCC, 0xBB, 0xAA, // user_id LE
        ],
    );

    assert_golden(&Message::SourceReset, &[0x56, 0x54, 0x01, 0x06, 0, 0, 0, 0]);
}

#[test]
fn streaming_decode_multiple_concatenated_frames() {
    let m1 = Message::Hello {
        app_name: "mumble".to_owned(),
        plugin_version: "0.1.0".to_owned(),
    };
    let m2 = Message::Heartbeat;
    let m3 = Message::SourceReset;

    let mut stream = Vec::new();
    stream.extend_from_slice(&encode_frame(&m1).expect("encode m1"));
    stream.extend_from_slice(&encode_frame(&m2).expect("encode m2"));
    stream.extend_from_slice(&encode_frame(&m3).expect("encode m3"));

    let mut cursor = &stream[..];
    let mut decoded = Vec::new();
    while !cursor.is_empty() {
        let (msg, consumed) = decode_frame(cursor).expect("decode stream frame");
        decoded.push(msg);
        cursor = &cursor[consumed..];
    }
    assert_eq!(decoded, vec![m1, m2, m3]);
}

#[test]
fn audio_frame_max_samples_per_frame_alignment_boundaries() {
    // Formula: ((65536 - 16) / (4 * channels)) * channels
    // Note: 16380 = 2^2 * 3^2 * 5 * 7 * 13, so 1..=7 all divide 16380 evenly; 8ch yields 16376.
    let expected = [
        (1, 16_380),
        (2, 16_380),
        (3, 16_380),
        (4, 16_380),
        (5, 16_380),
        (6, 16_380),
        (7, 16_380),
        (8, 16_376),
    ];

    for (channels, max_samples) in expected {
        let computed = max_samples_per_frame(channels).expect("valid channel count");
        assert_eq!(computed, max_samples, "channels={channels}");
        assert_eq!(computed % usize::from(channels), 0);
        let payload_len = AUDIO_FRAME_HEADER_LEN + computed * 4;
        assert!(payload_len <= MAX_PAYLOAD_LEN);
        assert!(
            AUDIO_FRAME_HEADER_LEN + (computed + usize::from(channels)) * 4 > MAX_PAYLOAD_LEN,
            "adding one more multi-channel frame must exceed MAX_PAYLOAD_LEN"
        );

        let max_frame = Message::AudioFrame {
            user_id: 1,
            seq: 0,
            sample_rate: 48_000,
            channels,
            flags: 0,
            pcm_samples: vec![0.0; computed],
        };
        assert_round_trip(&max_frame);
    }

    assert_eq!(
        max_samples_per_frame(0),
        Err(ProtocolError::InvalidChannelCount(0))
    );
    assert_eq!(
        max_samples_per_frame(9),
        Err(ProtocolError::InvalidChannelCount(9))
    );
}

#[test]
fn audio_frame_boundary_sample_rates_and_empty_frame() {
    for sample_rate in [8_000, 16_000, 48_000, 192_000] {
        let msg = Message::AudioFrame {
            user_id: 1,
            seq: u32::MAX,
            sample_rate,
            channels: 1,
            flags: 0,
            pcm_samples: Vec::new(),
        };
        assert_round_trip(&msg);
    }

    let below_min = Message::AudioFrame {
        user_id: 1,
        seq: 0,
        sample_rate: 7_999,
        channels: 1,
        flags: 0,
        pcm_samples: vec![0.0],
    };
    assert_eq!(
        encode_frame(&below_min),
        Err(ProtocolError::InvalidSampleRate(7_999))
    );

    let above_max = Message::AudioFrame {
        user_id: 1,
        seq: 0,
        sample_rate: 192_001,
        channels: 1,
        flags: 0,
        pcm_samples: vec![0.0],
    };
    assert_eq!(
        encode_frame(&above_max),
        Err(ProtocolError::InvalidSampleRate(192_001))
    );
}

#[test]
fn rejects_header_errors_and_over_64kb() {
    // Truncated header
    for len in 0..FRAME_HEADER_LEN {
        let buf = vec![0x56; len];
        assert_eq!(
            FrameHeader::decode(&buf),
            Err(ProtocolError::UnexpectedEof {
                expected: FRAME_HEADER_LEN,
                actual: len,
            })
        );
    }

    // Wrong magic
    let bad_magic = build_raw_frame([0x56, 0x55], 1, MessageType::Heartbeat.as_u8(), &[]);
    assert_eq!(
        FrameHeader::decode(&bad_magic),
        Err(ProtocolError::InvalidMagic(0x56, 0x55))
    );

    // Unsupported version
    for ver in [0u8, 2u8, 255u8] {
        let bad_ver = build_raw_frame(PROTOCOL_MAGIC, ver, MessageType::Heartbeat.as_u8(), &[]);
        assert_eq!(
            FrameHeader::decode(&bad_ver),
            Err(ProtocolError::UnsupportedVersion(ver))
        );
    }

    // Unknown msg_type
    for msg_type in [0x07u8, 0x7Fu8, 0xFFu8] {
        let bad_type = build_raw_frame(PROTOCOL_MAGIC, PROTOCOL_VERSION, msg_type, &[]);
        assert_eq!(
            FrameHeader::decode(&bad_type),
            Err(ProtocolError::UnknownMessageType(msg_type))
        );
    }

    // Payload length > 65536 (e.g. 65537) rejected at header decode before payload read
    let mut header_too_large = Vec::new();
    header_too_large.extend_from_slice(&PROTOCOL_MAGIC);
    header_too_large.push(PROTOCOL_VERSION);
    header_too_large.push(MessageType::AudioFrame.as_u8());
    let over_max_len = u32::try_from(MAX_PAYLOAD_LEN).expect("MAX_PAYLOAD_LEN fits in u32") + 1;
    header_too_large.extend_from_slice(&over_max_len.to_le_bytes());
    assert_eq!(
        FrameHeader::decode(&header_too_large),
        Err(ProtocolError::PayloadTooLarge {
            size: MAX_PAYLOAD_LEN + 1,
            max: MAX_PAYLOAD_LEN,
        })
    );

    // Exact 65536 payload succeeds for AudioFrame (8ch has 16376 samples -> 65520 bytes; 4ch has 16380 -> 65536 bytes)
    let max_4ch = Message::AudioFrame {
        user_id: 1,
        seq: 1,
        sample_rate: 48_000,
        channels: 4,
        flags: 0,
        pcm_samples: vec![0.125; 16_380],
    };
    let encoded_max = encode_frame(&max_4ch).expect("exact 65536 payload should encode");
    assert_eq!(encoded_max.len(), FRAME_HEADER_LEN + MAX_PAYLOAD_LEN);
    assert_eq!(
        decode_frame_exact(&encoded_max).expect("exact 65536 payload should decode"),
        max_4ch
    );

    // 16381 samples for 1ch = 16 + 16381 * 4 = 65540 > 65536
    let over_max = Message::AudioFrame {
        user_id: 1,
        seq: 1,
        sample_rate: 48_000,
        channels: 1,
        flags: 0,
        pcm_samples: vec![0.0; 16_381],
    };
    assert_eq!(
        encode_frame(&over_max),
        Err(ProtocolError::PayloadTooLarge {
            size: 65_540,
            max: MAX_PAYLOAD_LEN,
        })
    );
}

#[test]
fn rejects_truncated_and_trailing_frames() {
    let valid = encode_frame(&Message::UserRemoved { user_id: 42 }).expect("encode");

    // Truncated payload
    let truncated = &valid[..valid.len() - 1];
    assert_eq!(
        decode_frame(truncated),
        Err(ProtocolError::UnexpectedEof {
            expected: valid.len(),
            actual: valid.len() - 1,
        })
    );

    // Trailing bytes rejected by decode_frame_exact
    let mut with_trailing = valid.clone();
    with_trailing.push(0xAA);
    assert!(matches!(
        decode_frame_exact(&with_trailing),
        Err(ProtocolError::InvalidPayloadLength {
            message: "UserRemoved",
            ..
        })
    ));

    // Fixed-size payload length mismatches
    for bad_hb_len in [1usize, 4] {
        let frame = build_raw_frame(
            PROTOCOL_MAGIC,
            PROTOCOL_VERSION,
            MessageType::Heartbeat.as_u8(),
            &vec![0u8; bad_hb_len],
        );
        assert!(matches!(
            decode_frame_exact(&frame),
            Err(ProtocolError::InvalidPayloadLength {
                message: "Heartbeat",
                ..
            })
        ));
    }

    let bad_reset = build_raw_frame(
        PROTOCOL_MAGIC,
        PROTOCOL_VERSION,
        MessageType::SourceReset.as_u8(),
        &[0x00],
    );
    assert!(matches!(
        decode_frame_exact(&bad_reset),
        Err(ProtocolError::InvalidPayloadLength {
            message: "SourceReset",
            ..
        })
    ));

    for bad_removed in [0usize, 3, 5] {
        let frame = build_raw_frame(
            PROTOCOL_MAGIC,
            PROTOCOL_VERSION,
            MessageType::UserRemoved.as_u8(),
            &vec![0u8; bad_removed],
        );
        assert!(matches!(
            decode_frame_exact(&frame),
            Err(ProtocolError::InvalidPayloadLength {
                message: "UserRemoved",
                ..
            })
        ));
    }

    for bad_talking in [0usize, 4, 6] {
        let frame = build_raw_frame(
            PROTOCOL_MAGIC,
            PROTOCOL_VERSION,
            MessageType::TalkingState.as_u8(),
            &vec![0u8; bad_talking],
        );
        assert!(matches!(
            decode_frame_exact(&frame),
            Err(ProtocolError::InvalidPayloadLength {
                message: "TalkingState",
                ..
            })
        ));
    }
}

#[test]
fn rejects_hello_malformed_inputs() {
    // Boundary: app_name 64 bytes and plugin_version 64 bytes succeeds (total payload 132 bytes)
    let max_hello = Message::Hello {
        app_name: "a".repeat(64),
        plugin_version: "v".repeat(64),
    };
    let encoded_max = encode_frame(&max_hello).expect("max hello should encode");
    assert_eq!(encoded_max.len(), FRAME_HEADER_LEN + 132);
    assert_eq!(
        decode_frame_exact(&encoded_max).expect("max hello should decode"),
        max_hello
    );

    // Empty app_name
    assert_eq!(
        encode_frame(&Message::Hello {
            app_name: String::new(),
            plugin_version: "0.1.0".to_owned(),
        }),
        Err(ProtocolError::InvalidStringLength {
            field: "app_name",
            len: 0,
            min: 1,
            max: 64,
        })
    );

    // Too-long app_name (65 bytes)
    assert_eq!(
        encode_frame(&Message::Hello {
            app_name: "a".repeat(65),
            plugin_version: "0.1.0".to_owned(),
        }),
        Err(ProtocolError::InvalidStringLength {
            field: "app_name",
            len: 65,
            min: 1,
            max: 64,
        })
    );

    // Too-long plugin_version (65 bytes)
    assert_eq!(
        encode_frame(&Message::Hello {
            app_name: "mumble".to_owned(),
            plugin_version: "v".repeat(65),
        }),
        Err(ProtocolError::InvalidStringLength {
            field: "plugin_version",
            len: 65,
            min: 0,
            max: 64,
        })
    );

    // Raw Hello with trailing byte inside payload
    let mut payload_trailing = vec![6, 0, b'm', b'u', b'm', b'b', b'l', b'e', 1, 0, b'1', 0xFF];
    let frame_trailing = build_raw_frame(
        PROTOCOL_MAGIC,
        PROTOCOL_VERSION,
        MessageType::Hello.as_u8(),
        &payload_trailing,
    );
    assert!(matches!(
        decode_frame_exact(&frame_trailing),
        Err(ProtocolError::InvalidPayloadLength {
            message: "Hello",
            ..
        })
    ));

    // Raw Hello truncated mid-plugin_version
    payload_trailing.pop();
    payload_trailing.pop();
    let frame_truncated = build_raw_frame(
        PROTOCOL_MAGIC,
        PROTOCOL_VERSION,
        MessageType::Hello.as_u8(),
        &payload_trailing,
    );
    assert!(matches!(
        decode_frame_exact(&frame_truncated),
        Err(ProtocolError::InvalidPayloadLength {
            message: "Hello",
            ..
        })
    ));

    // Raw Hello with invalid UTF-8 in app_name
    let invalid_utf8_app = vec![2, 0, 0xFF, 0xFE, 0, 0];
    assert_eq!(
        Message::decode_payload(MessageType::Hello, &invalid_utf8_app),
        Err(ProtocolError::InvalidUtf8 { field: "app_name" })
    );

    // Raw Hello with invalid UTF-8 in plugin_version
    let invalid_utf8_ver = vec![1, 0, b'm', 2, 0, 0x80, 0x80];
    assert_eq!(
        Message::decode_payload(MessageType::Hello, &invalid_utf8_ver),
        Err(ProtocolError::InvalidUtf8 {
            field: "plugin_version"
        })
    );

    // Raw Hello with NUL byte in app_name / plugin_version
    let nul_app = vec![3, 0, b'm', 0x00, b'b', 0, 0];
    assert_eq!(
        Message::decode_payload(MessageType::Hello, &nul_app),
        Err(ProtocolError::ContainsNulByte { field: "app_name" })
    );

    let nul_ver = vec![1, 0, b'm', 2, 0, b'1', 0x00];
    assert_eq!(
        Message::decode_payload(MessageType::Hello, &nul_ver),
        Err(ProtocolError::ContainsNulByte {
            field: "plugin_version"
        })
    );
}

#[test]
fn rejects_user_info_malformed_inputs() {
    // Payload < 4 bytes
    assert!(matches!(
        Message::decode_payload(MessageType::UserInfo, &[1, 2, 3]),
        Err(ProtocolError::InvalidPayloadLength {
            message: "UserInfo",
            ..
        })
    ));

    // Invalid UTF-8
    assert_eq!(
        Message::decode_payload(MessageType::UserInfo, &[1, 0, 0, 0, 0xFF]),
        Err(ProtocolError::InvalidUtf8 { field: "name" })
    );

    // Embedded NUL byte
    assert_eq!(
        Message::decode_payload(MessageType::UserInfo, &[1, 0, 0, 0, b'a', 0x00, b'b']),
        Err(ProtocolError::ContainsNulByte { field: "name" })
    );

    assert_eq!(
        encode_frame(&Message::UserInfo {
            user_id: 1,
            name: "a\0b".to_owned(),
        }),
        Err(ProtocolError::ContainsNulByte { field: "name" })
    );

    // Boundary: UserInfo max payload (4 + 65532 = 65536) succeeds, 65533 fails
    let max_user_info = Message::UserInfo {
        user_id: 1,
        name: "x".repeat(MAX_PAYLOAD_LEN - 4),
    };
    assert_round_trip(&max_user_info);

    assert_eq!(
        encode_frame(&Message::UserInfo {
            user_id: 1,
            name: "x".repeat(MAX_PAYLOAD_LEN - 3),
        }),
        Err(ProtocolError::PayloadTooLarge {
            size: MAX_PAYLOAD_LEN + 1,
            max: MAX_PAYLOAD_LEN,
        })
    );
}

#[test]
fn rejects_audio_frame_malformed_inputs() {
    // Helper to build raw AudioFrame payload
    let make_payload = |sample_rate: u32, channels: u16, flags: u16, samples: &[f32]| -> Vec<u8> {
        let mut p = Vec::with_capacity(16 + samples.len() * 4);
        p.extend_from_slice(&1u32.to_le_bytes());
        p.extend_from_slice(&0u32.to_le_bytes());
        p.extend_from_slice(&sample_rate.to_le_bytes());
        p.extend_from_slice(&channels.to_le_bytes());
        p.extend_from_slice(&flags.to_le_bytes());
        for &s in samples {
            p.extend_from_slice(&s.to_le_bytes());
        }
        p
    };

    // Payload shorter than 16 bytes
    assert!(matches!(
        Message::decode_payload(MessageType::AudioFrame, &[0u8; 15]),
        Err(ProtocolError::InvalidPayloadLength {
            message: "AudioFrame",
            ..
        })
    ));

    // Unaligned sample bytes (e.g. 2 channels requires multiple of 8 bytes; 4 bytes is invalid)
    let unaligned_2ch = make_payload(48_000, 2, 0, &[0.5]);
    assert!(matches!(
        Message::decode_payload(MessageType::AudioFrame, &unaligned_2ch),
        Err(ProtocolError::InvalidPayloadLength {
            message: "AudioFrame",
            ..
        })
    ));

    // Trailing partial float byte (not multiple of 4)
    let mut partial_float = make_payload(48_000, 1, 0, &[0.5]);
    partial_float.push(0x00);
    assert!(matches!(
        Message::decode_payload(MessageType::AudioFrame, &partial_float),
        Err(ProtocolError::InvalidPayloadLength {
            message: "AudioFrame",
            ..
        })
    ));

    // Reserved flags bits set
    for bad_flag in [0x0002u16, 0x0003, 0x8000, 0xFFFF] {
        let p = make_payload(48_000, 1, bad_flag, &[0.0]);
        assert_eq!(
            Message::decode_payload(MessageType::AudioFrame, &p),
            Err(ProtocolError::InvalidFlags(bad_flag))
        );
    }

    // Invalid PCM samples: NaN, +Inf, -Inf, > 1.0, < -1.0
    for bad_sample in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        1.000_001,
        -1.000_001,
    ] {
        let p = make_payload(48_000, 1, 0, &[0.0, bad_sample]);
        assert!(matches!(
            Message::decode_payload(MessageType::AudioFrame, &p),
            Err(ProtocolError::InvalidPcmSample { index: 1, .. })
        ));

        let msg = Message::AudioFrame {
            user_id: 1,
            seq: 0,
            sample_rate: 48_000,
            channels: 1,
            flags: 0,
            pcm_samples: vec![0.0, bad_sample],
        };
        assert!(matches!(
            encode_frame(&msg),
            Err(ProtocolError::InvalidPcmSample { index: 1, .. })
        ));
    }

    // Negative zero (-0.0) is finite and in [-1.0, 1.0], so it must succeed
    let neg_zero = Message::AudioFrame {
        user_id: 1,
        seq: 0,
        sample_rate: 48_000,
        channels: 1,
        flags: 0,
        pcm_samples: vec![-0.0, -1.0, 1.0],
    };
    assert_round_trip(&neg_zero);
}

#[test]
fn rejects_invalid_talking_state() {
    for raw_state in 5u8..=255 {
        let mut payload = vec![1, 0, 0, 0, raw_state];
        assert_eq!(
            Message::decode_payload(MessageType::TalkingState, &payload),
            Err(ProtocolError::InvalidTalkingState(raw_state))
        );
        payload.clear();
    }
}

#[test]
fn endpoint_resolution_rules_and_windows_pipe_normalization() {
    // 1. KOTOHA_IPC_ENDPOINT non-empty override wins on both Windows and Unix
    assert_eq!(
        resolve_endpoint_with(
            TargetOs::Windows,
            Some(r"\\.\pipe\custom_kotoha"),
            Some("/run/user/1000")
        ),
        r"\\.\pipe\custom_kotoha"
    );
    assert_eq!(
        resolve_endpoint_with(
            TargetOs::Unix,
            Some("/tmp/kotoha-test-123.sock"),
            Some("/run/user/1000")
        ),
        "/tmp/kotoha-test-123.sock"
    );

    // Empty override is ignored
    assert_eq!(
        resolve_endpoint_with(TargetOs::Windows, Some(""), None),
        DEFAULT_WINDOWS_ENDPOINT
    );
    assert_eq!(
        resolve_endpoint_with(TargetOs::Unix, Some(""), Some("/run/user/1000")),
        "/run/user/1000/kotoha_v1.sock"
    );

    // 2. Unix XDG_RUNTIME_DIR trailing slash normalization and fallback
    assert_eq!(
        resolve_endpoint_with(TargetOs::Unix, None, Some("/run/user/1000/")),
        "/run/user/1000/kotoha_v1.sock"
    );
    assert_eq!(
        resolve_endpoint_with(TargetOs::Unix, None, Some("///")),
        "/kotoha_v1.sock"
    );
    assert_eq!(
        resolve_endpoint_with(TargetOs::Unix, None, Some("")),
        DEFAULT_UNIX_FALLBACK_ENDPOINT
    );
    assert_eq!(
        resolve_endpoint_with(TargetOs::Unix, None, None),
        DEFAULT_UNIX_FALLBACK_ENDPOINT
    );

    // 3. Windows pipe name extraction (standard prefix case-insensitive + bare name)
    assert_eq!(
        extract_windows_pipe_name(r"\\.\pipe\kotoha_v1"),
        Ok("kotoha_v1")
    );
    assert_eq!(
        extract_windows_pipe_name(r"\\.\PIPE\kotoha_test_99"),
        Ok("kotoha_test_99")
    );
    assert_eq!(extract_windows_pipe_name("kotoha_v1"), Ok("kotoha_v1"));

    // Invalid Windows pipe endpoints
    assert!(matches!(
        extract_windows_pipe_name(""),
        Err(ProtocolError::InvalidEndpoint(_))
    ));
    assert!(matches!(
        extract_windows_pipe_name(r"\\.\pipe\"),
        Err(ProtocolError::InvalidEndpoint(_))
    ));
    assert!(matches!(
        extract_windows_pipe_name(r"\\.\pipe\nested\name"),
        Err(ProtocolError::InvalidEndpoint(_))
    ));
    assert!(matches!(
        extract_windows_pipe_name("bad/name"),
        Err(ProtocolError::InvalidEndpoint(_))
    ));
    assert!(matches!(
        extract_windows_pipe_name("bad\0name"),
        Err(ProtocolError::InvalidEndpoint(_))
    ));

    // Also verify live resolve_endpoint() returns a non-empty string consistent with current OS
    let live = resolve_endpoint();
    assert_ne!(live, "");
    assert_eq!(ENV_IPC_ENDPOINT, "KOTOHA_IPC_ENDPOINT");
}
