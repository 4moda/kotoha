#include "wav_reader.hpp"

#include <array>
#include <bit>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <limits>
#include <string>
#include <vector>

namespace {

using namespace kotoha::test;

void check_impl(bool cond, const char* expr, int line) {
    if (!cond) {
        std::cerr << "FAIL at line " << line << ": " << expr << '\n';
        std::exit(1);
    }
}

#define CHECK(expr) check_impl(static_cast<bool>(expr), #expr, __LINE__)

void append_u16_le(std::vector<uint8_t>& out, uint16_t val) {
    out.push_back(static_cast<uint8_t>(val & 0xFFU));
    out.push_back(static_cast<uint8_t>((val >> 8U) & 0xFFU));
}

void append_u32_le(std::vector<uint8_t>& out, uint32_t val) {
    out.push_back(static_cast<uint8_t>(val & 0xFFU));
    out.push_back(static_cast<uint8_t>((val >> 8U) & 0xFFU));
    out.push_back(static_cast<uint8_t>((val >> 16U) & 0xFFU));
    out.push_back(static_cast<uint8_t>((val >> 24U) & 0xFFU));
}

void append_i16_le(std::vector<uint8_t>& out, int16_t val) {
    append_u16_le(out, std::bit_cast<uint16_t>(val));
}

void append_f32_le(std::vector<uint8_t>& out, float val) {
    append_u32_le(out, std::bit_cast<uint32_t>(val));
}

void append_tag(std::vector<uint8_t>& out, const char* tag) {
    for (size_t i = 0; i < 4U; ++i) {
        out.push_back(static_cast<uint8_t>(tag[i]));
    }
}

std::vector<uint8_t> make_fmt_chunk(uint16_t audio_format, uint16_t channels, uint32_t sample_rate,
                                    uint16_t bits_per_sample, uint16_t extra_bytes = 0) {
    std::vector<uint8_t> chunk;
    append_tag(chunk, "fmt ");
    append_u32_le(chunk, 16U + static_cast<uint32_t>(extra_bytes));
    append_u16_le(chunk, audio_format);
    append_u16_le(chunk, channels);
    append_u32_le(chunk, sample_rate);
    const auto block_align = static_cast<uint16_t>(channels * (bits_per_sample / 8U));
    const uint32_t byte_rate = sample_rate * static_cast<uint32_t>(block_align);
    append_u32_le(chunk, byte_rate);
    append_u16_le(chunk, block_align);
    append_u16_le(chunk, bits_per_sample);
    for (uint16_t i = 0; i < extra_bytes; ++i) {
        chunk.push_back(0U);
    }
    if ((extra_bytes & 1U) != 0U) {
        chunk.push_back(0U);
    }
    return chunk;
}

std::vector<uint8_t> make_subchunk(const char* tag, const std::vector<uint8_t>& payload) {
    std::vector<uint8_t> chunk;
    append_tag(chunk, tag);
    append_u32_le(chunk, static_cast<uint32_t>(payload.size()));
    chunk.insert(chunk.end(), payload.begin(), payload.end());
    if ((payload.size() & 1U) != 0U) {
        chunk.push_back(0xAAU);  // RIFF word-alignment pad byte
    }
    return chunk;
}

std::vector<uint8_t> wrap_riff_wave(const std::vector<std::vector<uint8_t>>& chunks) {
    size_t body_size = 4U;
    for (const auto& c : chunks) {
        body_size += c.size();
    }
    std::vector<uint8_t> wav;
    wav.reserve(8U + body_size);
    append_tag(wav, "RIFF");
    append_u32_le(wav, static_cast<uint32_t>(body_size));
    append_tag(wav, "WAVE");
    for (const auto& c : chunks) {
        wav.insert(wav.end(), c.begin(), c.end());
    }
    return wav;
}

bool approx_equal(float a, float b, float eps = 1e-6f) {
    return std::fabs(a - b) <= eps;
}

void test_pcm16_mono_wav_file() {
    std::vector<uint8_t> pcm_bytes;
    const std::array<int16_t, 5> raw_samples = {
        int16_t{-32768}, int16_t{-16384}, int16_t{0}, int16_t{16384}, int16_t{32767},
    };
    for (const int16_t s : raw_samples) {
        append_i16_le(pcm_bytes, s);
    }

    const auto fmt = make_fmt_chunk(1U, 1U, 16000U, 16U);
    const auto data = make_subchunk("data", pcm_bytes);
    const auto wav = wrap_riff_wave({fmt, data});

    const auto stamp = std::chrono::steady_clock::now().time_since_epoch().count();
    const std::filesystem::path tmp_path =
        std::filesystem::temp_directory_path() /
        ("kotoha_test_pcm16_mono_" + std::to_string(stamp) + ".wav");
    {
        std::ofstream out(tmp_path, std::ios::binary);
        CHECK(out.is_open());
        out.write(reinterpret_cast<const char*>(wav.data()),
                  static_cast<std::streamsize>(wav.size()));
        CHECK(static_cast<bool>(out));
    }

    const WavReadResult res = read_wav_file(tmp_path.string());
    std::error_code ec;
    std::filesystem::remove(tmp_path, ec);

    CHECK(res.ok);
    CHECK(res.error.empty());
    CHECK(res.audio.sample_rate == 16000U);
    CHECK(res.audio.channels == 1U);
    CHECK(res.audio.samples.size() == 5U);
    CHECK(approx_equal(res.audio.samples[0], -1.0f));
    CHECK(approx_equal(res.audio.samples[1], -0.5f));
    CHECK(approx_equal(res.audio.samples[2], 0.0f));
    CHECK(approx_equal(res.audio.samples[3], 0.5f));
    CHECK(approx_equal(res.audio.samples[4], 32767.0f / 32768.0f));
}

void test_pcm16_stereo_with_extra_odd_chunks() {
    // Odd-sized JUNK (3 bytes + 1 pad), LIST/INFO (5 bytes + 1 pad), and fact (4 bytes)
    // plus extensible 18-byte fmt chunk (cbSize=0), stereo PCM data, and trailing odd JUNK chunk.
    const auto junk = make_subchunk("JUNK", {0x01U, 0x02U, 0x03U});
    const auto list = make_subchunk("LIST", {'I', 'N', 'F', 'O', 'X'});
    const auto fmt = make_fmt_chunk(1U, 2U, 48000U, 16U, 2U);
    const auto fact = make_subchunk("fact", {0x02U, 0x00U, 0x00U, 0x00U});
    const auto tail_junk = make_subchunk("JUNK", {0x5AU});

    std::vector<uint8_t> pcm_bytes;
    append_i16_le(pcm_bytes, 16384);   // frame 0 L
    append_i16_le(pcm_bytes, -16384);  // frame 0 R
    append_i16_le(pcm_bytes, -32768);  // frame 1 L
    append_i16_le(pcm_bytes, 8192);    // frame 1 R
    const auto data = make_subchunk("data", pcm_bytes);

    const auto wav = wrap_riff_wave({junk, list, fmt, fact, data, tail_junk});
    const WavReadResult res = parse_wav_bytes(wav);

    CHECK(res.ok);
    CHECK(res.audio.sample_rate == 48000U);
    CHECK(res.audio.channels == 2U);
    CHECK(res.audio.samples.size() == 4U);
    CHECK(approx_equal(res.audio.samples[0], 0.5f));
    CHECK(approx_equal(res.audio.samples[1], -0.5f));
    CHECK(approx_equal(res.audio.samples[2], -1.0f));
    CHECK(approx_equal(res.audio.samples[3], 0.25f));
}

void test_ieee_float32_wav_and_sanitization() {
    std::vector<uint8_t> pcm_bytes;
    append_f32_le(pcm_bytes, 0.75f);
    append_f32_le(pcm_bytes, -1.75f);
    append_f32_le(pcm_bytes, 2.25f);
    append_f32_le(pcm_bytes, std::numeric_limits<float>::quiet_NaN());
    append_f32_le(pcm_bytes, std::numeric_limits<float>::infinity());
    append_f32_le(pcm_bytes, -std::numeric_limits<float>::infinity());

    const auto fmt = make_fmt_chunk(3U, 2U, 44100U, 32U);
    const auto data = make_subchunk("data", pcm_bytes);
    const auto wav = wrap_riff_wave({fmt, data});

    const WavReadResult res = parse_wav_bytes(wav);
    CHECK(res.ok);
    CHECK(res.audio.sample_rate == 44100U);
    CHECK(res.audio.channels == 2U);
    CHECK(res.audio.samples.size() == 6U);
    CHECK(approx_equal(res.audio.samples[0], 0.75f));
    CHECK(approx_equal(res.audio.samples[1], -1.0f));
    CHECK(approx_equal(res.audio.samples[2], 1.0f));
    CHECK(approx_equal(res.audio.samples[3], 0.0f));
    CHECK(approx_equal(res.audio.samples[4], 0.0f));
    CHECK(approx_equal(res.audio.samples[5], 0.0f));

    // Boundary check: 8 channels at 192kHz
    std::vector<uint8_t> pcm8_bytes;
    for (int ch = 0; ch < 8; ++ch) {
        append_f32_le(pcm8_bytes, 0.1f * static_cast<float>(ch));
    }
    const auto fmt8 = make_fmt_chunk(3U, 8U, 192000U, 32U);
    const auto data8 = make_subchunk("data", pcm8_bytes);
    const WavReadResult res8 = parse_wav_bytes(wrap_riff_wave({fmt8, data8}));
    CHECK(res8.ok);
    CHECK(res8.audio.channels == 8U);
    CHECK(res8.audio.sample_rate == 192000U);
    CHECK(res8.audio.samples.size() == 8U);
}

void test_rejections() {
    // Missing file
    {
        const std::filesystem::path missing_path =
            std::filesystem::temp_directory_path() / "kotoha_definitely_missing_12345.wav";
        const WavReadResult res = read_wav_file(missing_path.string());
        CHECK(!res.ok);
        CHECK(!res.error.empty());
    }

    // Empty path
    {
        const WavReadResult res = read_wav_file("");
        CHECK(!res.ok);
    }

    // Truncated header / bad RIFF / bad WAVE
    {
        const std::vector<uint8_t> too_short = {'R', 'I', 'F', 'F', 4U, 0U, 0U, 0U};
        CHECK(!parse_wav_bytes(too_short).ok);

        const std::vector<uint8_t> bad_magic = {'O', 'g', 'g', 'S', 4U,  0U,
                                                0U,  0U,  'W', 'A', 'V', 'E'};
        CHECK(!parse_wav_bytes(bad_magic).ok);

        const std::vector<uint8_t> bad_wave = {'R', 'I', 'F', 'F', 4U,  0U,
                                               0U,  0U,  'A', 'V', 'I', ' '};
        CHECK(!parse_wav_bytes(bad_wave).ok);
    }

    // Unsupported audio_format (e.g. 6 = A-law)
    {
        const auto fmt = make_fmt_chunk(6U, 1U, 16000U, 16U);
        const auto data = make_subchunk("data", {0x00U, 0x00U});
        CHECK(!parse_wav_bytes(wrap_riff_wave({fmt, data})).ok);
    }

    // Unsupported bits_per_sample (24-bit PCM and 64-bit IEEE float)
    {
        const auto fmt24 = make_fmt_chunk(1U, 1U, 48000U, 24U);
        const auto data24 = make_subchunk("data", {0x00U, 0x00U, 0x00U});
        CHECK(!parse_wav_bytes(wrap_riff_wave({fmt24, data24})).ok);

        const auto fmt64 = make_fmt_chunk(3U, 1U, 48000U, 64U);
        const auto data64 = make_subchunk("data", std::vector<uint8_t>(8U, 0U));
        CHECK(!parse_wav_bytes(wrap_riff_wave({fmt64, data64})).ok);
    }

    // Invalid channels (0 and 9)
    {
        const auto fmt0 = make_fmt_chunk(1U, 0U, 16000U, 16U);
        const auto data0 = make_subchunk("data", {0x00U, 0x00U});
        CHECK(!parse_wav_bytes(wrap_riff_wave({fmt0, data0})).ok);

        const auto fmt9 = make_fmt_chunk(1U, 9U, 16000U, 16U);
        const std::vector<uint8_t> data9_bytes(18U, 0U);
        const auto data9 = make_subchunk("data", data9_bytes);
        CHECK(!parse_wav_bytes(wrap_riff_wave({fmt9, data9})).ok);
    }

    // Out-of-range sample_rate (7999 and 192001)
    {
        const auto fmt_low = make_fmt_chunk(1U, 1U, 7999U, 16U);
        const auto data = make_subchunk("data", {0x00U, 0x00U});
        CHECK(!parse_wav_bytes(wrap_riff_wave({fmt_low, data})).ok);

        const auto fmt_high = make_fmt_chunk(1U, 1U, 192001U, 16U);
        CHECK(!parse_wav_bytes(wrap_riff_wave({fmt_high, data})).ok);
    }

    // Empty data chunk
    {
        const auto fmt = make_fmt_chunk(1U, 1U, 16000U, 16U);
        const auto data = make_subchunk("data", {});
        CHECK(!parse_wav_bytes(wrap_riff_wave({fmt, data})).ok);
    }

    // Unaligned stereo data chunk (2 bytes instead of multiple of 4 bytes)
    {
        const auto fmt = make_fmt_chunk(1U, 2U, 48000U, 16U);
        const auto data = make_subchunk("data", {0x01U, 0x02U});
        CHECK(!parse_wav_bytes(wrap_riff_wave({fmt, data})).ok);
    }

    // Missing data chunk
    {
        const auto fmt = make_fmt_chunk(1U, 1U, 16000U, 16U);
        CHECK(!parse_wav_bytes(wrap_riff_wave({fmt})).ok);
    }

    // Missing fmt chunk
    {
        const auto data = make_subchunk("data", {0x00U, 0x00U});
        CHECK(!parse_wav_bytes(wrap_riff_wave({data})).ok);
    }

    // Duplicate fmt or data chunk
    {
        const auto fmt = make_fmt_chunk(1U, 1U, 16000U, 16U);
        const auto data = make_subchunk("data", {0x00U, 0x00U});
        CHECK(!parse_wav_bytes(wrap_riff_wave({fmt, fmt, data})).ok);
        CHECK(!parse_wav_bytes(wrap_riff_wave({fmt, data, data})).ok);
    }

    // Truncated RIFF size and truncated middle chunk payload
    {
        const auto fmt = make_fmt_chunk(1U, 1U, 16000U, 16U);
        const auto data = make_subchunk("data", {0x00U, 0x00U});
        auto wav = wrap_riff_wave({fmt, data});
        wav.pop_back();
        CHECK(!parse_wav_bytes(wav).ok);

        // Trailing odd chunk without EOF pad byte is tolerated
        const auto junk = make_subchunk("JUNK", {0x01U, 0x02U, 0x03U});
        auto odd_wav = wrap_riff_wave({fmt, data, junk});
        odd_wav.pop_back();
        odd_wav[4] = static_cast<uint8_t>(odd_wav.size() - 8U);
        CHECK(parse_wav_bytes(odd_wav).ok);

        // Middle odd chunk missing its pad byte corrupts subsequent header and must fail
        auto bad_mid = wrap_riff_wave({junk, fmt, data});
        bad_mid.erase(bad_mid.begin() + 12 + 8 + 3);  // remove the 1-byte pad after JUNK
        bad_mid[4] = static_cast<uint8_t>(bad_mid.size() - 8U);
        CHECK(!parse_wav_bytes(bad_mid).ok);

        // Overflow safety: malformed sub-chunk claiming UINT32_MAX (0xFFFFFFFF) payload bytes
        std::vector<uint8_t> overflow_chunk;
        append_tag(overflow_chunk, "JUNK");
        append_u32_le(overflow_chunk, std::numeric_limits<uint32_t>::max());
        overflow_chunk.insert(overflow_chunk.end(), {0x01U, 0x02U, 0x03U, 0x04U});
        std::vector<uint8_t> overflow_wav;
        append_tag(overflow_wav, "RIFF");
        append_u32_le(overflow_wav, static_cast<uint32_t>(4U + overflow_chunk.size()));
        append_tag(overflow_wav, "WAVE");
        overflow_wav.insert(overflow_wav.end(), overflow_chunk.begin(), overflow_chunk.end());
        CHECK(!parse_wav_bytes(overflow_wav).ok);
    }
}

}  // namespace

int main() {
    test_pcm16_mono_wav_file();
    test_pcm16_stereo_with_extra_odd_chunks();
    test_ieee_float32_wav_and_sanitization();
    test_rejections();
    std::cout << "test_wav_reader: all assertions passed\n";
    return 0;
}
