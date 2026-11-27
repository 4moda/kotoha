#include "kotoha/protocol.hpp"

#include <bit>
#include <cstring>

namespace kotoha::ipc {
namespace {

static_assert(sizeof(float) == sizeof(uint32_t), "float must be 32-bit");
static_assert(std::numeric_limits<float>::is_iec559, "float must be IEEE-754 binary32");

void write_u16_le(std::span<uint8_t> out, size_t offset, uint16_t value) noexcept {
    out[offset] = static_cast<uint8_t>(value & 0xFFU);
    out[offset + 1] = static_cast<uint8_t>((value >> 8U) & 0xFFU);
}

void write_u32_le(std::span<uint8_t> out, size_t offset, uint32_t value) noexcept {
    out[offset] = static_cast<uint8_t>(value & 0xFFU);
    out[offset + 1] = static_cast<uint8_t>((value >> 8U) & 0xFFU);
    out[offset + 2] = static_cast<uint8_t>((value >> 16U) & 0xFFU);
    out[offset + 3] = static_cast<uint8_t>((value >> 24U) & 0xFFU);
}

void write_f32_le(std::span<uint8_t> out, size_t offset, float value) noexcept {
    const auto bits = std::bit_cast<uint32_t>(value);
    write_u32_le(out, offset, bits);
}

[[nodiscard]] uint16_t read_u16_le(std::span<const uint8_t> in, size_t offset) noexcept {
    return static_cast<uint16_t>(static_cast<uint16_t>(in[offset]) |
                                 (static_cast<uint16_t>(in[offset + 1]) << 8U));
}

[[nodiscard]] uint32_t read_u32_le(std::span<const uint8_t> in, size_t offset) noexcept {
    return static_cast<uint32_t>(in[offset]) | (static_cast<uint32_t>(in[offset + 1]) << 8U) |
           (static_cast<uint32_t>(in[offset + 2]) << 16U) |
           (static_cast<uint32_t>(in[offset + 3]) << 24U);
}

[[nodiscard]] float read_f32_le(std::span<const uint8_t> in, size_t offset) noexcept {
    const uint32_t bits = read_u32_le(in, offset);
    return std::bit_cast<float>(bits);
}

void write_header(std::span<uint8_t> out, MessageType type, uint32_t payload_len) noexcept {
    out[0] = kProtocolMagic[0];
    out[1] = kProtocolMagic[1];
    out[2] = kProtocolVersion;
    out[3] = static_cast<uint8_t>(type);
    write_u32_le(out, 4, payload_len);
}

[[nodiscard]] bool is_valid_message_type(uint8_t raw) noexcept {
    return raw <= static_cast<uint8_t>(MessageType::SourceReset);
}

[[nodiscard]] bool is_valid_talking_state(uint8_t raw) noexcept {
    return raw <= static_cast<uint8_t>(TalkingStateValue::TalkingMuted);
}

[[nodiscard]] bool is_continuation_byte(uint8_t byte) noexcept {
    return (byte & 0xC0U) == 0x80U;
}

}  // namespace

ProtocolError validate_utf8_no_nul(std::string_view text) noexcept {
    const size_t len = text.size();
    size_t idx = 0;

    while (idx < len) {
        const auto b0 = static_cast<uint8_t>(text[idx]);
        if (b0 == 0x00U) {
            return ProtocolError::ContainsNulByte;
        }
        if (b0 <= 0x7FU) {
            ++idx;
            continue;
        }
        if (b0 >= 0xC2U && b0 <= 0xDFU) {
            if (idx + 1 >= len) {
                return ProtocolError::InvalidUtf8;
            }
            const auto b1 = static_cast<uint8_t>(text[idx + 1]);
            if (!is_continuation_byte(b1)) {
                return ProtocolError::InvalidUtf8;
            }
            idx += 2;
            continue;
        }
        if (b0 >= 0xE0U && b0 <= 0xEFU) {
            if (idx + 2 >= len) {
                return ProtocolError::InvalidUtf8;
            }
            const auto b1 = static_cast<uint8_t>(text[idx + 1]);
            const auto b2 = static_cast<uint8_t>(text[idx + 2]);
            if (!is_continuation_byte(b1) || !is_continuation_byte(b2)) {
                return ProtocolError::InvalidUtf8;
            }
            if (b0 == 0xE0U && b1 < 0xA0U) {
                return ProtocolError::InvalidUtf8;
            }
            if (b0 == 0xEDU && b1 > 0x9FU) {
                return ProtocolError::InvalidUtf8;
            }
            idx += 3;
            continue;
        }
        if (b0 >= 0xF0U && b0 <= 0xF4U) {
            if (idx + 3 >= len) {
                return ProtocolError::InvalidUtf8;
            }
            const auto b1 = static_cast<uint8_t>(text[idx + 1]);
            const auto b2 = static_cast<uint8_t>(text[idx + 2]);
            const auto b3 = static_cast<uint8_t>(text[idx + 3]);
            if (!is_continuation_byte(b1) || !is_continuation_byte(b2) ||
                !is_continuation_byte(b3)) {
                return ProtocolError::InvalidUtf8;
            }
            if (b0 == 0xF0U && b1 < 0x90U) {
                return ProtocolError::InvalidUtf8;
            }
            if (b0 == 0xF4U && b1 > 0x8FU) {
                return ProtocolError::InvalidUtf8;
            }
            idx += 4;
            continue;
        }
        return ProtocolError::InvalidUtf8;
    }

    return ProtocolError::Ok;
}

ProtocolError encode_hello(const HelloView& msg, std::span<uint8_t> out,
                           size_t& bytes_written) noexcept {
    bytes_written = 0;

    if (msg.app_name.empty() || msg.app_name.size() > kMaxHelloAppNameLen) {
        return ProtocolError::InvalidHelloField;
    }
    if (msg.plugin_version.size() > kMaxHelloPluginVersionLen) {
        return ProtocolError::InvalidHelloField;
    }

    const ProtocolError app_err = validate_utf8_no_nul(msg.app_name);
    if (app_err != ProtocolError::Ok) {
        return app_err;
    }
    const ProtocolError ver_err = validate_utf8_no_nul(msg.plugin_version);
    if (ver_err != ProtocolError::Ok) {
        return ver_err;
    }

    const size_t payload_len = kHelloHeaderSize + msg.app_name.size() + msg.plugin_version.size();
    const size_t total_len = kHeaderSize + payload_len;
    if (out.size() < total_len) {
        return ProtocolError::OutputBufferTooSmall;
    }

    write_header(out, MessageType::Hello, static_cast<uint32_t>(payload_len));
    size_t cursor = kHeaderSize;
    write_u16_le(out, cursor, static_cast<uint16_t>(msg.app_name.size()));
    cursor += 2;
    std::memcpy(out.data() + cursor, msg.app_name.data(), msg.app_name.size());
    cursor += msg.app_name.size();
    write_u16_le(out, cursor, static_cast<uint16_t>(msg.plugin_version.size()));
    cursor += 2;
    if (!msg.plugin_version.empty()) {
        std::memcpy(out.data() + cursor, msg.plugin_version.data(), msg.plugin_version.size());
        cursor += msg.plugin_version.size();
    }

    bytes_written = cursor;
    return ProtocolError::Ok;
}

ProtocolError encode_audio_frame(const AudioFrameView& msg, std::span<uint8_t> out,
                                 size_t& bytes_written) noexcept {
    bytes_written = 0;

    if (msg.sample_rate < kMinSampleRate || msg.sample_rate > kMaxSampleRate) {
        return ProtocolError::InvalidSampleRate;
    }
    if (msg.channels < kMinChannels || msg.channels > kMaxChannels) {
        return ProtocolError::InvalidChannels;
    }
    if ((msg.flags & ~kAudioFlagAllowedMask) != 0U) {
        return ProtocolError::InvalidFlags;
    }
    if ((msg.pcm_samples.size() % static_cast<size_t>(msg.channels)) != 0U) {
        return ProtocolError::UnalignedPcmPayload;
    }
    if (msg.pcm_samples.size() > max_pcm_samples_for_channels(msg.channels)) {
        return ProtocolError::PayloadTooLarge;
    }

    const size_t payload_len = kAudioFrameHeaderSize + (msg.pcm_samples.size() * sizeof(float));
    if (payload_len > kMaxPayloadLen) {
        return ProtocolError::PayloadTooLarge;
    }

    const size_t total_len = kHeaderSize + payload_len;
    if (out.size() < total_len) {
        return ProtocolError::OutputBufferTooSmall;
    }

    write_header(out, MessageType::AudioFrame, static_cast<uint32_t>(payload_len));
    size_t cursor = kHeaderSize;
    write_u32_le(out, cursor, msg.user_id);
    cursor += 4;
    write_u32_le(out, cursor, msg.seq);
    cursor += 4;
    write_u32_le(out, cursor, msg.sample_rate);
    cursor += 4;
    write_u16_le(out, cursor, msg.channels);
    cursor += 2;
    write_u16_le(out, cursor, msg.flags);
    cursor += 2;

    for (const float raw_sample : msg.pcm_samples) {
        const float clean = sanitize_pcm_sample(raw_sample);
        write_f32_le(out, cursor, clean);
        cursor += sizeof(float);
    }

    bytes_written = cursor;
    return ProtocolError::Ok;
}

ProtocolError encode_talking_state(const TalkingStateMessage& msg, std::span<uint8_t> out,
                                   size_t& bytes_written) noexcept {
    bytes_written = 0;

    const auto state_raw = static_cast<uint8_t>(msg.state);
    if (!is_valid_talking_state(state_raw)) {
        return ProtocolError::InvalidTalkingState;
    }

    constexpr size_t kTotalLen = kHeaderSize + kTalkingStatePayloadSize;
    if (out.size() < kTotalLen) {
        return ProtocolError::OutputBufferTooSmall;
    }

    write_header(out, MessageType::TalkingState, static_cast<uint32_t>(kTalkingStatePayloadSize));
    write_u32_le(out, kHeaderSize, msg.user_id);
    out[kHeaderSize + 4] = state_raw;

    bytes_written = kTotalLen;
    return ProtocolError::Ok;
}

ProtocolError encode_user_info(const UserInfoView& msg, std::span<uint8_t> out,
                               size_t& bytes_written) noexcept {
    bytes_written = 0;

    if (msg.name.size() > static_cast<size_t>(kMaxPayloadLen) - kUserInfoHeaderSize) {
        return ProtocolError::PayloadTooLarge;
    }

    const ProtocolError name_err = validate_utf8_no_nul(msg.name);
    if (name_err != ProtocolError::Ok) {
        return name_err;
    }

    const size_t payload_len = kUserInfoHeaderSize + msg.name.size();
    const size_t total_len = kHeaderSize + payload_len;
    if (out.size() < total_len) {
        return ProtocolError::OutputBufferTooSmall;
    }

    write_header(out, MessageType::UserInfo, static_cast<uint32_t>(payload_len));
    write_u32_le(out, kHeaderSize, msg.user_id);
    if (!msg.name.empty()) {
        std::memcpy(out.data() + kHeaderSize + kUserInfoHeaderSize, msg.name.data(),
                    msg.name.size());
    }

    bytes_written = total_len;
    return ProtocolError::Ok;
}

ProtocolError encode_heartbeat(std::span<uint8_t> out, size_t& bytes_written) noexcept {
    bytes_written = 0;
    if (out.size() < kHeaderSize) {
        return ProtocolError::OutputBufferTooSmall;
    }
    write_header(out, MessageType::Heartbeat, static_cast<uint32_t>(kHeartbeatPayloadSize));
    bytes_written = kHeaderSize;
    return ProtocolError::Ok;
}

ProtocolError encode_user_removed(const UserRemovedMessage& msg, std::span<uint8_t> out,
                                  size_t& bytes_written) noexcept {
    bytes_written = 0;
    constexpr size_t kTotalLen = kHeaderSize + kUserRemovedPayloadSize;
    if (out.size() < kTotalLen) {
        return ProtocolError::OutputBufferTooSmall;
    }
    write_header(out, MessageType::UserRemoved, static_cast<uint32_t>(kUserRemovedPayloadSize));
    write_u32_le(out, kHeaderSize, msg.user_id);
    bytes_written = kTotalLen;
    return ProtocolError::Ok;
}

ProtocolError encode_source_reset(std::span<uint8_t> out, size_t& bytes_written) noexcept {
    bytes_written = 0;
    if (out.size() < kHeaderSize) {
        return ProtocolError::OutputBufferTooSmall;
    }
    write_header(out, MessageType::SourceReset, static_cast<uint32_t>(kSourceResetPayloadSize));
    bytes_written = kHeaderSize;
    return ProtocolError::Ok;
}

EncodeResult encode_hello_frame(const HelloView& msg) {
    EncodeResult res;
    res.bytes.resize(kHeaderSize + kMaxHelloPayloadSize);
    size_t written = 0;
    res.error = encode_hello(msg, res.bytes, written);
    res.bytes.resize(res.error == ProtocolError::Ok ? written : 0U);
    return res;
}

EncodeResult encode_audio_frame(const AudioFrameView& msg) {
    EncodeResult res;
    if (msg.pcm_samples.size() >
        (static_cast<size_t>(kMaxPayloadLen) - kAudioFrameHeaderSize) / sizeof(float)) {
        res.error = ProtocolError::PayloadTooLarge;
        return res;
    }
    res.bytes.resize(kHeaderSize + kAudioFrameHeaderSize +
                     (msg.pcm_samples.size() * sizeof(float)));
    size_t written = 0;
    res.error = encode_audio_frame(msg, res.bytes, written);
    res.bytes.resize(res.error == ProtocolError::Ok ? written : 0U);
    return res;
}

EncodeResult encode_talking_state_frame(const TalkingStateMessage& msg) {
    EncodeResult res;
    res.bytes.resize(kHeaderSize + kTalkingStatePayloadSize);
    size_t written = 0;
    res.error = encode_talking_state(msg, res.bytes, written);
    res.bytes.resize(res.error == ProtocolError::Ok ? written : 0U);
    return res;
}

EncodeResult encode_user_info_frame(const UserInfoView& msg) {
    EncodeResult res;
    if (msg.name.size() > static_cast<size_t>(kMaxPayloadLen) - kUserInfoHeaderSize) {
        res.error = ProtocolError::PayloadTooLarge;
        return res;
    }
    res.bytes.resize(kHeaderSize + kUserInfoHeaderSize + msg.name.size());
    size_t written = 0;
    res.error = encode_user_info(msg, res.bytes, written);
    res.bytes.resize(res.error == ProtocolError::Ok ? written : 0U);
    return res;
}

EncodeResult encode_heartbeat_frame() {
    EncodeResult res;
    res.bytes.resize(kHeaderSize);
    size_t written = 0;
    res.error = encode_heartbeat(res.bytes, written);
    res.bytes.resize(res.error == ProtocolError::Ok ? written : 0U);
    return res;
}

EncodeResult encode_user_removed_frame(const UserRemovedMessage& msg) {
    EncodeResult res;
    res.bytes.resize(kHeaderSize + kUserRemovedPayloadSize);
    size_t written = 0;
    res.error = encode_user_removed(msg, res.bytes, written);
    res.bytes.resize(res.error == ProtocolError::Ok ? written : 0U);
    return res;
}

EncodeResult encode_source_reset_frame() {
    EncodeResult res;
    res.bytes.resize(kHeaderSize);
    size_t written = 0;
    res.error = encode_source_reset(res.bytes, written);
    res.bytes.resize(res.error == ProtocolError::Ok ? written : 0U);
    return res;
}

EncodeResult encode_frame(const Message& msg) {
    return std::visit(
        [](const auto& concrete) -> EncodeResult {
            using T = std::decay_t<decltype(concrete)>;
            if constexpr (std::is_same_v<T, HelloMessage>) {
                return encode_hello_frame(HelloView{concrete.app_name, concrete.plugin_version});
            } else if constexpr (std::is_same_v<T, AudioFrameMessage>) {
                return encode_audio_frame(AudioFrameView{
                    concrete.user_id,
                    concrete.seq,
                    concrete.sample_rate,
                    concrete.channels,
                    concrete.flags,
                    concrete.pcm_samples,
                });
            } else if constexpr (std::is_same_v<T, TalkingStateMessage>) {
                return encode_talking_state_frame(concrete);
            } else if constexpr (std::is_same_v<T, UserInfoMessage>) {
                return encode_user_info_frame(UserInfoView{concrete.user_id, concrete.name});
            } else if constexpr (std::is_same_v<T, HeartbeatMessage>) {
                return encode_heartbeat_frame();
            } else if constexpr (std::is_same_v<T, UserRemovedMessage>) {
                return encode_user_removed_frame(concrete);
            } else {
                return encode_source_reset_frame();
            }
        },
        msg);
}

HeaderDecodeResult decode_header(std::span<const uint8_t> header_bytes) noexcept {
    HeaderDecodeResult res;
    if (header_bytes.size() < kHeaderSize) {
        res.error = ProtocolError::TruncatedFrame;
        return res;
    }

    if (header_bytes[0] != kProtocolMagic[0] || header_bytes[1] != kProtocolMagic[1]) {
        res.error = ProtocolError::InvalidMagic;
        return res;
    }

    if (header_bytes[2] != kProtocolVersion) {
        res.error = ProtocolError::UnsupportedVersion;
        return res;
    }

    const uint8_t raw_type = header_bytes[3];
    if (!is_valid_message_type(raw_type)) {
        res.error = ProtocolError::UnknownMessageType;
        return res;
    }

    const uint32_t payload_len = read_u32_le(header_bytes, 4);
    if (payload_len > kMaxPayloadLen) {
        res.error = ProtocolError::PayloadTooLarge;
        return res;
    }

    res.error = ProtocolError::Ok;
    res.header.msg_type = static_cast<MessageType>(raw_type);
    res.header.payload_len = payload_len;
    return res;
}

DecodeResult decode_payload(const FrameHeader& header, std::span<const uint8_t> payload) {
    DecodeResult res;

    if (header.payload_len > kMaxPayloadLen) {
        res.error = ProtocolError::PayloadTooLarge;
        return res;
    }
    if (payload.size() < header.payload_len) {
        res.error = ProtocolError::TruncatedFrame;
        return res;
    }

    const auto body = payload.first(header.payload_len);

    switch (header.msg_type) {
        case MessageType::Hello: {
            if (body.size() < kMinHelloPayloadSize || body.size() > kMaxHelloPayloadSize) {
                res.error = ProtocolError::InvalidPayloadLength;
                return res;
            }
            const uint16_t app_len = read_u16_le(body, 0);
            if (app_len < 1U || app_len > kMaxHelloAppNameLen) {
                res.error = ProtocolError::InvalidHelloField;
                return res;
            }
            if (2U + static_cast<size_t>(app_len) + 2U > body.size()) {
                res.error = ProtocolError::InvalidPayloadLength;
                return res;
            }
            const std::string_view app_name(reinterpret_cast<const char*>(body.data() + 2U),
                                            app_len);
            const ProtocolError app_err = validate_utf8_no_nul(app_name);
            if (app_err != ProtocolError::Ok) {
                res.error = app_err;
                return res;
            }

            const size_t ver_len_offset = 2U + static_cast<size_t>(app_len);
            const uint16_t ver_len = read_u16_le(body, ver_len_offset);
            if (ver_len > kMaxHelloPluginVersionLen) {
                res.error = ProtocolError::InvalidHelloField;
                return res;
            }
            if (kHelloHeaderSize + static_cast<size_t>(app_len) + static_cast<size_t>(ver_len) !=
                body.size()) {
                res.error = ProtocolError::InvalidPayloadLength;
                return res;
            }
            const std::string_view plugin_version(
                reinterpret_cast<const char*>(body.data() + ver_len_offset + 2U), ver_len);
            const ProtocolError ver_err = validate_utf8_no_nul(plugin_version);
            if (ver_err != ProtocolError::Ok) {
                res.error = ver_err;
                return res;
            }

            HelloMessage msg;
            msg.app_name.assign(app_name);
            msg.plugin_version.assign(plugin_version);
            res.message = std::move(msg);
            res.bytes_consumed = kHeaderSize + body.size();
            return res;
        }

        case MessageType::AudioFrame: {
            if (body.size() < kAudioFrameHeaderSize) {
                res.error = ProtocolError::InvalidPayloadLength;
                return res;
            }

            const uint32_t user_id = read_u32_le(body, 0);
            const uint32_t seq = read_u32_le(body, 4);
            const uint32_t sample_rate = read_u32_le(body, 8);
            const uint16_t channels = read_u16_le(body, 12);
            const uint16_t flags = read_u16_le(body, 14);

            if (sample_rate < kMinSampleRate || sample_rate > kMaxSampleRate) {
                res.error = ProtocolError::InvalidSampleRate;
                return res;
            }
            if (channels < kMinChannels || channels > kMaxChannels) {
                res.error = ProtocolError::InvalidChannels;
                return res;
            }
            if ((flags & ~kAudioFlagAllowedMask) != 0U) {
                res.error = ProtocolError::InvalidFlags;
                return res;
            }

            const size_t pcm_bytes = body.size() - kAudioFrameHeaderSize;
            const size_t frame_bytes = sizeof(float) * static_cast<size_t>(channels);
            if ((pcm_bytes % frame_bytes) != 0U) {
                res.error = ProtocolError::UnalignedPcmPayload;
                return res;
            }

            const size_t sample_count = pcm_bytes / sizeof(float);
            AudioFrameMessage msg;
            msg.user_id = user_id;
            msg.seq = seq;
            msg.sample_rate = sample_rate;
            msg.channels = channels;
            msg.flags = flags;
            msg.pcm_samples.reserve(sample_count);

            for (size_t i = 0; i < sample_count; ++i) {
                const float sample = read_f32_le(body, kAudioFrameHeaderSize + (i * sizeof(float)));
                if (!std::isfinite(sample) || sample < -1.0f || sample > 1.0f) {
                    res.error = ProtocolError::InvalidPcmSample;
                    return res;
                }
                msg.pcm_samples.push_back(sample);
            }

            res.message = std::move(msg);
            res.bytes_consumed = kHeaderSize + body.size();
            return res;
        }

        case MessageType::TalkingState: {
            if (body.size() != kTalkingStatePayloadSize) {
                res.error = ProtocolError::InvalidPayloadLength;
                return res;
            }
            const uint32_t user_id = read_u32_le(body, 0);
            const uint8_t raw_state = body[4];
            if (!is_valid_talking_state(raw_state)) {
                res.error = ProtocolError::InvalidTalkingState;
                return res;
            }
            res.message = TalkingStateMessage{user_id, static_cast<TalkingStateValue>(raw_state)};
            res.bytes_consumed = kHeaderSize + body.size();
            return res;
        }

        case MessageType::UserInfo: {
            if (body.size() < kUserInfoHeaderSize) {
                res.error = ProtocolError::InvalidPayloadLength;
                return res;
            }
            const uint32_t user_id = read_u32_le(body, 0);
            const std::string_view name(
                reinterpret_cast<const char*>(body.data() + kUserInfoHeaderSize),
                body.size() - kUserInfoHeaderSize);
            const ProtocolError name_err = validate_utf8_no_nul(name);
            if (name_err != ProtocolError::Ok) {
                res.error = name_err;
                return res;
            }
            UserInfoMessage msg;
            msg.user_id = user_id;
            msg.name.assign(name);
            res.message = std::move(msg);
            res.bytes_consumed = kHeaderSize + body.size();
            return res;
        }

        case MessageType::Heartbeat: {
            if (!body.empty()) {
                res.error = ProtocolError::InvalidPayloadLength;
                return res;
            }
            res.message = HeartbeatMessage{};
            res.bytes_consumed = kHeaderSize;
            return res;
        }

        case MessageType::UserRemoved: {
            if (body.size() != kUserRemovedPayloadSize) {
                res.error = ProtocolError::InvalidPayloadLength;
                return res;
            }
            res.message = UserRemovedMessage{read_u32_le(body, 0)};
            res.bytes_consumed = kHeaderSize + body.size();
            return res;
        }

        case MessageType::SourceReset: {
            if (!body.empty()) {
                res.error = ProtocolError::InvalidPayloadLength;
                return res;
            }
            res.message = SourceResetMessage{};
            res.bytes_consumed = kHeaderSize;
            return res;
        }
    }

    res.error = ProtocolError::UnknownMessageType;
    return res;
}

DecodeResult decode_frame(std::span<const uint8_t> buffer) {
    const HeaderDecodeResult hdr = decode_header(buffer);
    if (hdr.error != ProtocolError::Ok) {
        DecodeResult res;
        res.error = hdr.error;
        return res;
    }

    const size_t total_len = kHeaderSize + static_cast<size_t>(hdr.header.payload_len);
    if (buffer.size() < total_len) {
        DecodeResult res;
        res.error = ProtocolError::TruncatedFrame;
        return res;
    }

    return decode_payload(hdr.header, buffer.subspan(kHeaderSize, hdr.header.payload_len));
}

}  // namespace kotoha::ipc
