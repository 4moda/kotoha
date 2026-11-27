#pragma once

#include <algorithm>
#include <array>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <limits>
#include <span>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

namespace kotoha::ipc {

inline constexpr std::array<uint8_t, 2> kProtocolMagic = {0x56, 0x54};
inline constexpr uint8_t kProtocolVersion = 1;
inline constexpr size_t kHeaderSize = 8;
inline constexpr uint32_t kMaxPayloadLen = 65536;
inline constexpr size_t kMaxFrameSize = kHeaderSize + kMaxPayloadLen;

inline constexpr size_t kHelloHeaderSize = 4;
inline constexpr size_t kMinHelloPayloadSize = 5;
inline constexpr size_t kMaxHelloAppNameLen = 64;
inline constexpr size_t kMaxHelloPluginVersionLen = 64;
inline constexpr size_t kMaxHelloPayloadSize =
    kHelloHeaderSize + kMaxHelloAppNameLen + kMaxHelloPluginVersionLen;

inline constexpr size_t kAudioFrameHeaderSize = 16;
inline constexpr uint32_t kMinSampleRate = 8000;
inline constexpr uint32_t kMaxSampleRate = 192000;
inline constexpr uint16_t kMinChannels = 1;
inline constexpr uint16_t kMaxChannels = 8;
inline constexpr uint16_t kAudioFlagIsLocalUser = 0x0001;
inline constexpr uint16_t kAudioFlagAllowedMask = kAudioFlagIsLocalUser;

inline constexpr size_t kTalkingStatePayloadSize = 5;
inline constexpr size_t kUserInfoHeaderSize = 4;
inline constexpr size_t kHeartbeatPayloadSize = 0;
inline constexpr size_t kUserRemovedPayloadSize = 4;
inline constexpr size_t kSourceResetPayloadSize = 0;

enum class MessageType : uint8_t {
    Hello = 0x00,
    AudioFrame = 0x01,
    TalkingState = 0x02,
    UserInfo = 0x03,
    Heartbeat = 0x04,
    UserRemoved = 0x05,
    SourceReset = 0x06,
};

enum class TalkingStateValue : uint8_t {
    Passive = 0,
    Talking = 1,
    Whispering = 2,
    Shouting = 3,
    TalkingMuted = 4,
};

enum class ProtocolError : uint8_t {
    Ok = 0,
    TruncatedFrame,
    InvalidMagic,
    UnsupportedVersion,
    PayloadTooLarge,
    UnknownMessageType,
    InvalidPayloadLength,
    InvalidHelloField,
    InvalidUtf8,
    ContainsNulByte,
    InvalidSampleRate,
    InvalidChannels,
    UnalignedPcmPayload,
    InvalidFlags,
    InvalidPcmSample,
    InvalidTalkingState,
    OutputBufferTooSmall,
};

[[nodiscard]] constexpr const char* protocol_error_to_string(ProtocolError err) noexcept {
    switch (err) {
        case ProtocolError::Ok:
            return "Ok";
        case ProtocolError::TruncatedFrame:
            return "TruncatedFrame";
        case ProtocolError::InvalidMagic:
            return "InvalidMagic";
        case ProtocolError::UnsupportedVersion:
            return "UnsupportedVersion";
        case ProtocolError::PayloadTooLarge:
            return "PayloadTooLarge";
        case ProtocolError::UnknownMessageType:
            return "UnknownMessageType";
        case ProtocolError::InvalidPayloadLength:
            return "InvalidPayloadLength";
        case ProtocolError::InvalidHelloField:
            return "InvalidHelloField";
        case ProtocolError::InvalidUtf8:
            return "InvalidUtf8";
        case ProtocolError::ContainsNulByte:
            return "ContainsNulByte";
        case ProtocolError::InvalidSampleRate:
            return "InvalidSampleRate";
        case ProtocolError::InvalidChannels:
            return "InvalidChannels";
        case ProtocolError::UnalignedPcmPayload:
            return "UnalignedPcmPayload";
        case ProtocolError::InvalidFlags:
            return "InvalidFlags";
        case ProtocolError::InvalidPcmSample:
            return "InvalidPcmSample";
        case ProtocolError::InvalidTalkingState:
            return "InvalidTalkingState";
        case ProtocolError::OutputBufferTooSmall:
            return "OutputBufferTooSmall";
    }
    return "Unknown";
}

[[nodiscard]] constexpr size_t max_pcm_samples_for_channels(uint16_t channels) noexcept {
    if (channels < kMinChannels || channels > kMaxChannels) {
        return 0;
    }
    const size_t max_pcm_bytes = static_cast<size_t>(kMaxPayloadLen) - kAudioFrameHeaderSize;
    const size_t bytes_per_frame = sizeof(float) * static_cast<size_t>(channels);
    return (max_pcm_bytes / bytes_per_frame) * static_cast<size_t>(channels);
}

[[nodiscard]] inline float sanitize_pcm_sample(float sample) noexcept {
    return std::isfinite(sample) ? std::clamp(sample, -1.0f, 1.0f) : 0.0f;
}

[[nodiscard]] ProtocolError validate_utf8_no_nul(std::string_view text) noexcept;

struct HelloMessage {
    std::string app_name;
    std::string plugin_version;

    [[nodiscard]] bool operator==(const HelloMessage&) const = default;
};

struct HelloView {
    std::string_view app_name;
    std::string_view plugin_version;
};

struct AudioFrameMessage {
    uint32_t user_id = 0;
    uint32_t seq = 0;
    uint32_t sample_rate = 48000;
    uint16_t channels = 1;
    uint16_t flags = 0;
    std::vector<float> pcm_samples;

    [[nodiscard]] bool operator==(const AudioFrameMessage&) const = default;
};

struct AudioFrameView {
    uint32_t user_id = 0;
    uint32_t seq = 0;
    uint32_t sample_rate = 48000;
    uint16_t channels = 1;
    uint16_t flags = 0;
    std::span<const float> pcm_samples;
};

struct TalkingStateMessage {
    uint32_t user_id = 0;
    TalkingStateValue state = TalkingStateValue::Passive;

    [[nodiscard]] bool operator==(const TalkingStateMessage&) const = default;
};

struct UserInfoMessage {
    uint32_t user_id = 0;
    std::string name;

    [[nodiscard]] bool operator==(const UserInfoMessage&) const = default;
};

struct UserInfoView {
    uint32_t user_id = 0;
    std::string_view name;
};

struct HeartbeatMessage {
    [[nodiscard]] bool operator==(const HeartbeatMessage&) const = default;
};

struct UserRemovedMessage {
    uint32_t user_id = 0;

    [[nodiscard]] bool operator==(const UserRemovedMessage&) const = default;
};

struct SourceResetMessage {
    [[nodiscard]] bool operator==(const SourceResetMessage&) const = default;
};

using Message = std::variant<HelloMessage, AudioFrameMessage, TalkingStateMessage, UserInfoMessage,
                             HeartbeatMessage, UserRemovedMessage, SourceResetMessage>;

struct FrameHeader {
    MessageType msg_type = MessageType::Hello;
    uint32_t payload_len = 0;
};

struct HeaderDecodeResult {
    ProtocolError error = ProtocolError::Ok;
    FrameHeader header{};
};

struct EncodeResult {
    ProtocolError error = ProtocolError::Ok;
    std::vector<uint8_t> bytes;
};

struct DecodeResult {
    ProtocolError error = ProtocolError::Ok;
    Message message;
    size_t bytes_consumed = 0;
};

[[nodiscard]] ProtocolError encode_hello(const HelloView& msg, std::span<uint8_t> out,
                                         size_t& bytes_written) noexcept;

[[nodiscard]] ProtocolError encode_audio_frame(const AudioFrameView& msg, std::span<uint8_t> out,
                                               size_t& bytes_written) noexcept;

[[nodiscard]] ProtocolError encode_talking_state(const TalkingStateMessage& msg,
                                                 std::span<uint8_t> out,
                                                 size_t& bytes_written) noexcept;

[[nodiscard]] ProtocolError encode_user_info(const UserInfoView& msg, std::span<uint8_t> out,
                                             size_t& bytes_written) noexcept;

[[nodiscard]] ProtocolError encode_heartbeat(std::span<uint8_t> out,
                                             size_t& bytes_written) noexcept;

[[nodiscard]] ProtocolError encode_user_removed(const UserRemovedMessage& msg,
                                                std::span<uint8_t> out,
                                                size_t& bytes_written) noexcept;

[[nodiscard]] ProtocolError encode_source_reset(std::span<uint8_t> out,
                                                size_t& bytes_written) noexcept;

[[nodiscard]] EncodeResult encode_frame(const Message& msg);
[[nodiscard]] EncodeResult encode_hello_frame(const HelloView& msg);
[[nodiscard]] EncodeResult encode_audio_frame(const AudioFrameView& msg);
[[nodiscard]] EncodeResult encode_talking_state_frame(const TalkingStateMessage& msg);
[[nodiscard]] EncodeResult encode_user_info_frame(const UserInfoView& msg);
[[nodiscard]] EncodeResult encode_heartbeat_frame();
[[nodiscard]] EncodeResult encode_user_removed_frame(const UserRemovedMessage& msg);
[[nodiscard]] EncodeResult encode_source_reset_frame();

[[nodiscard]] HeaderDecodeResult decode_header(std::span<const uint8_t> header_bytes) noexcept;

[[nodiscard]] DecodeResult decode_payload(const FrameHeader& header,
                                          std::span<const uint8_t> payload);

[[nodiscard]] DecodeResult decode_frame(std::span<const uint8_t> buffer);

}  // namespace kotoha::ipc
