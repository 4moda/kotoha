#pragma once

#include <algorithm>
#include <array>
#include <bit>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <fstream>
#include <limits>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

namespace kotoha::test {

struct WavAudio {
    uint32_t sample_rate = 0;
    uint16_t channels = 0;
    std::vector<float> samples;
};

struct WavReadResult {
    bool ok = false;
    WavAudio audio;
    std::string error;

    [[nodiscard]] explicit operator bool() const noexcept { return ok; }
};

namespace detail {

inline constexpr uint32_t kMinWavSampleRate = 8000U;
inline constexpr uint32_t kMaxWavSampleRate = 192000U;
inline constexpr uint16_t kMinWavChannels = 1U;
inline constexpr uint16_t kMaxWavChannels = 8U;
inline constexpr uint16_t kWavFormatPcm = 1U;
inline constexpr uint16_t kWavFormatIeeeFloat = 3U;

[[nodiscard]] inline uint16_t read_u16_le(std::span<const uint8_t> bytes, size_t offset) noexcept {
    const unsigned b0 = bytes[offset];
    const unsigned b1 = bytes[offset + 1U];
    return static_cast<uint16_t>(b0 | (b1 << 8U));
}

[[nodiscard]] inline uint32_t read_u32_le(std::span<const uint8_t> bytes, size_t offset) noexcept {
    return static_cast<uint32_t>(bytes[offset]) |
           (static_cast<uint32_t>(bytes[offset + 1U]) << 8U) |
           (static_cast<uint32_t>(bytes[offset + 2U]) << 16U) |
           (static_cast<uint32_t>(bytes[offset + 3U]) << 24U);
}

[[nodiscard]] inline int16_t read_i16_le(std::span<const uint8_t> bytes, size_t offset) noexcept {
    const uint16_t raw = read_u16_le(bytes, offset);
    return std::bit_cast<int16_t>(raw);
}

[[nodiscard]] inline float read_f32_le(std::span<const uint8_t> bytes, size_t offset) noexcept {
    const uint32_t bits = read_u32_le(bytes, offset);
    return std::bit_cast<float>(bits);
}

[[nodiscard]] inline bool tag_equals(std::span<const uint8_t> bytes, size_t offset,
                                     std::array<char, 4> tag) noexcept {
    if (offset + 4U > bytes.size()) {
        return false;
    }
    return bytes[offset] == static_cast<uint8_t>(tag[0]) &&
           bytes[offset + 1U] == static_cast<uint8_t>(tag[1]) &&
           bytes[offset + 2U] == static_cast<uint8_t>(tag[2]) &&
           bytes[offset + 3U] == static_cast<uint8_t>(tag[3]);
}

[[nodiscard]] inline float sanitize_wav_f32(float sample) noexcept {
    return std::isfinite(sample) ? std::clamp(sample, -1.0f, 1.0f) : 0.0f;
}

[[nodiscard]] inline WavReadResult make_wav_error(std::string message) {
    WavReadResult result;
    result.ok = false;
    result.error = std::move(message);
    return result;
}

}  // namespace detail

[[nodiscard]] inline WavReadResult parse_wav_bytes(std::span<const uint8_t> bytes) {
    static_assert(sizeof(float) == sizeof(uint32_t), "float must be 32-bit");
    static_assert(std::numeric_limits<float>::is_iec559, "float must be IEEE-754 binary32");

    constexpr size_t kRiffHeaderSize = 12U;
    constexpr size_t kChunkHeaderSize = 8U;

    if (bytes.size() < kRiffHeaderSize) {
        return detail::make_wav_error("WAV buffer is smaller than 12-byte RIFF/WAVE header");
    }
    if (!detail::tag_equals(bytes, 0U, {'R', 'I', 'F', 'F'})) {
        return detail::make_wav_error("Missing 'RIFF' header magic");
    }
    if (!detail::tag_equals(bytes, 8U, {'W', 'A', 'V', 'E'})) {
        return detail::make_wav_error("Missing 'WAVE' format identifier");
    }

    const uint32_t riff_declared_size = detail::read_u32_le(bytes, 4U);
    if (riff_declared_size < 4U) {
        return detail::make_wav_error("Invalid RIFF chunk size (< 4 bytes)");
    }
    if (static_cast<size_t>(riff_declared_size) > bytes.size() - 8U) {
        return detail::make_wav_error(
            "Truncated RIFF payload: file smaller than declared RIFF size");
    }
    const size_t riff_end_offset = 8U + static_cast<size_t>(riff_declared_size);

    const std::span<const uint8_t> riff_stream = bytes.first(riff_end_offset);

    bool has_fmt = false;
    bool has_data = false;
    uint16_t audio_format = 0U;
    uint16_t channels = 0U;
    uint32_t sample_rate = 0U;
    uint16_t block_align = 0U;
    uint16_t bits_per_sample = 0U;
    std::span<const uint8_t> data_payload;

    size_t cursor = kRiffHeaderSize;
    while (cursor < riff_stream.size()) {
        if (riff_stream.size() - cursor < kChunkHeaderSize) {
            return detail::make_wav_error("Truncated RIFF sub-chunk header");
        }

        const uint32_t chunk_size_u32 = detail::read_u32_le(riff_stream, cursor + 4U);
        const size_t chunk_payload_offset = cursor + kChunkHeaderSize;
        const auto chunk_size = static_cast<size_t>(chunk_size_u32);

        if (chunk_size > riff_stream.size() - chunk_payload_offset) {
            return detail::make_wav_error("Truncated RIFF sub-chunk payload");
        }

        const std::span<const uint8_t> payload =
            riff_stream.subspan(chunk_payload_offset, chunk_size);

        if (detail::tag_equals(riff_stream, cursor, {'f', 'm', 't', ' '})) {
            if (has_fmt) {
                return detail::make_wav_error("Duplicate 'fmt ' chunk in WAV");
            }
            if (payload.size() < 16U) {
                return detail::make_wav_error("Malformed 'fmt ' chunk: smaller than 16 bytes");
            }
            audio_format = detail::read_u16_le(payload, 0U);
            channels = detail::read_u16_le(payload, 2U);
            sample_rate = detail::read_u32_le(payload, 4U);
            block_align = detail::read_u16_le(payload, 12U);
            bits_per_sample = detail::read_u16_le(payload, 14U);

            if (audio_format != detail::kWavFormatPcm &&
                audio_format != detail::kWavFormatIeeeFloat) {
                return detail::make_wav_error(
                    "Unsupported WAV audio format tag (only PCM=1 and IEEE float=3 supported): " +
                    std::to_string(audio_format));
            }
            if (channels < detail::kMinWavChannels || channels > detail::kMaxWavChannels) {
                return detail::make_wav_error("Unsupported WAV channel count (expected 1..=8): " +
                                              std::to_string(channels));
            }
            if (sample_rate < detail::kMinWavSampleRate ||
                sample_rate > detail::kMaxWavSampleRate) {
                return detail::make_wav_error(
                    "Unsupported WAV sample rate (expected 8000..=192000 Hz): " +
                    std::to_string(sample_rate));
            }

            const uint16_t expected_bits =
                (audio_format == detail::kWavFormatPcm) ? uint16_t{16U} : uint16_t{32U};
            if (bits_per_sample != expected_bits) {
                return detail::make_wav_error("Unsupported WAV bits per sample for format: " +
                                              std::to_string(bits_per_sample));
            }

            const auto expected_block_align =
                static_cast<uint16_t>(channels * (expected_bits / 8U));
            if (block_align != expected_block_align) {
                return detail::make_wav_error("Invalid WAV block_align in 'fmt ' chunk");
            }

            has_fmt = true;
        } else if (detail::tag_equals(riff_stream, cursor, {'d', 'a', 't', 'a'})) {
            if (has_data) {
                return detail::make_wav_error("Duplicate 'data' chunk in WAV");
            }
            data_payload = payload;
            has_data = true;
        }

        const size_t pad = chunk_size & 1U;
        if (pad != 0U && chunk_payload_offset + chunk_size == riff_stream.size()) {
            // Tolerate encoders that omit the trailing pad byte on an odd final metadata chunk.
            cursor = chunk_payload_offset + chunk_size;
        } else {
            cursor = chunk_payload_offset + chunk_size + pad;
        }
    }

    if (!has_fmt) {
        return detail::make_wav_error("Missing required 'fmt ' chunk in WAV");
    }
    if (!has_data) {
        return detail::make_wav_error("Missing required 'data' chunk in WAV");
    }
    if (data_payload.empty()) {
        return detail::make_wav_error("WAV 'data' chunk is empty");
    }

    const auto frame_bytes = static_cast<size_t>(block_align);
    if ((data_payload.size() % frame_bytes) != 0U) {
        return detail::make_wav_error("WAV 'data' chunk size is not aligned to channel frame size");
    }

    const auto bytes_per_sample = static_cast<size_t>(bits_per_sample / 8U);
    const size_t total_samples = data_payload.size() / bytes_per_sample;
    if ((total_samples % static_cast<size_t>(channels)) != 0U) {
        return detail::make_wav_error("WAV sample count is not divisible by channel count");
    }

    WavReadResult result;
    result.ok = true;
    result.audio.sample_rate = sample_rate;
    result.audio.channels = channels;
    result.audio.samples.reserve(total_samples);

    if (audio_format == detail::kWavFormatPcm) {
        for (size_t offset = 0; offset < data_payload.size(); offset += 2U) {
            const int16_t raw = detail::read_i16_le(data_payload, offset);
            const float normalized = std::clamp(static_cast<float>(raw) / 32768.0f, -1.0f, 1.0f);
            result.audio.samples.push_back(normalized);
        }
    } else {
        for (size_t offset = 0; offset < data_payload.size(); offset += 4U) {
            const float raw = detail::read_f32_le(data_payload, offset);
            result.audio.samples.push_back(detail::sanitize_wav_f32(raw));
        }
    }

    return result;
}

[[nodiscard]] inline WavReadResult read_wav_file(std::string_view path) {
    constexpr uint64_t kMaxWavFileBytes = 256ULL * 1024ULL * 1024ULL;

    if (path.empty()) {
        return detail::make_wav_error("WAV file path is empty");
    }

    const std::string path_str(path);
    std::u8string u8_path;
    u8_path.reserve(path.size());
    for (const char ch : path) {
        u8_path.push_back(static_cast<char8_t>(static_cast<unsigned char>(ch)));
    }
    const std::filesystem::path fs_path(u8_path);

    std::ifstream file(fs_path, std::ios::binary | std::ios::ate);
    if (!file.is_open()) {
        return detail::make_wav_error("Failed to open WAV file: " + path_str);
    }

    const std::ifstream::pos_type end_pos = file.tellg();
    if (end_pos < 0) {
        return detail::make_wav_error("Failed to determine WAV file size: " + path_str);
    }

    const auto raw_file_size = static_cast<uint64_t>(static_cast<std::streamoff>(end_pos));
    if (raw_file_size > kMaxWavFileBytes) {
        return detail::make_wav_error("WAV file exceeds maximum supported size (256 MiB): " +
                                      path_str);
    }
    const auto file_size = static_cast<size_t>(raw_file_size);

    file.seekg(0, std::ios::beg);
    if (!file) {
        return detail::make_wav_error("Failed to seek WAV file: " + path_str);
    }

    std::vector<uint8_t> bytes(file_size);
    if (file_size > 0U) {
        file.read(reinterpret_cast<char*>(bytes.data()), static_cast<std::streamsize>(file_size));
        if (!file || static_cast<size_t>(file.gcount()) != file_size) {
            return detail::make_wav_error("Failed to read WAV file bytes: " + path_str);
        }
    }

    return parse_wav_bytes(bytes);
}

}  // namespace kotoha::test
