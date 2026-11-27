#include "kotoha/ipc_client.hpp"
#include "kotoha/protocol.hpp"
#include "wav_reader.hpp"

#include <algorithm>
#include <cerrno>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdlib>
#include <iostream>
#include <limits>
#include <span>
#include <string>
#include <string_view>
#include <thread>
#include <vector>

namespace {

using namespace kotoha::ipc;

struct CliOptions {
    std::string endpoint;
    std::string source = "mumble";
    std::string version = "0.1.0";
    uint32_t user_id = 42;
    std::string user_name = "Alice";
    uint32_t frames = 5;
    uint32_t duration_ms = 0;
    bool with_heartbeat = false;
    uint32_t connect_timeout_ms = 5000;
    std::string wav_path;
    bool has_wav = false;
    uint32_t tail_silence_ms = 0;
    bool tail_silence_explicit = false;
    bool wait_stdin_eof = false;
};

void print_usage(const char* argv0) {
    std::cerr << "Usage: " << argv0
              << " [--endpoint <path>] [--source <name>] [--version <ver>]"
                 " [--user-id <id>] [--user-name <name>] [--frames <n>]"
                 " [--duration-ms <ms>] [--with-heartbeat] [--connect-timeout-ms <ms>]"
                 " [--wav <path>] [--tail-silence-ms <ms>] [--wait-stdin-eof]\n";
}

bool parse_u32(std::string_view text, uint32_t& out) {
    if (text.empty()) {
        return false;
    }
    for (const char ch : text) {
        if (ch < '0' || ch > '9') {
            return false;
        }
    }
    char* end = nullptr;
    const std::string copy(text);
    errno = 0;
    const unsigned long long val = std::strtoull(copy.c_str(), &end, 10);
    if (errno == ERANGE || end == nullptr || *end != '\0' ||
        val > std::numeric_limits<uint32_t>::max()) {
        return false;
    }
    out = static_cast<uint32_t>(val);
    return true;
}

bool parse_args(int argc, char** argv, CliOptions& opts) {
    for (int i = 1; i < argc; ++i) {
        const std::string_view arg(argv[i]);
        auto next_value = [&](std::string_view flag) -> const char* {
            if (i + 1 >= argc) {
                std::cerr << "Missing value for " << flag << '\n';
                return nullptr;
            }
            return argv[++i];
        };

        if (arg == "--help" || arg == "-h") {
            print_usage(argv[0]);
            std::exit(0);
        } else if (arg == "--endpoint") {
            const char* val = next_value(arg);
            if (val == nullptr) {
                return false;
            }
            opts.endpoint = val;
        } else if (arg == "--source") {
            const char* val = next_value(arg);
            if (val == nullptr) {
                return false;
            }
            opts.source = val;
        } else if (arg == "--version") {
            const char* val = next_value(arg);
            if (val == nullptr) {
                return false;
            }
            opts.version = val;
        } else if (arg == "--user-id") {
            const char* val = next_value(arg);
            if (val == nullptr || !parse_u32(val, opts.user_id)) {
                return false;
            }
        } else if (arg == "--user-name") {
            const char* val = next_value(arg);
            if (val == nullptr) {
                return false;
            }
            opts.user_name = val;
        } else if (arg == "--frames") {
            const char* val = next_value(arg);
            if (val == nullptr || !parse_u32(val, opts.frames)) {
                return false;
            }
        } else if (arg == "--duration-ms") {
            const char* val = next_value(arg);
            if (val == nullptr || !parse_u32(val, opts.duration_ms)) {
                return false;
            }
        } else if (arg == "--connect-timeout-ms") {
            const char* val = next_value(arg);
            if (val == nullptr || !parse_u32(val, opts.connect_timeout_ms)) {
                return false;
            }
        } else if (arg == "--with-heartbeat") {
            opts.with_heartbeat = true;
        } else if (arg == "--wav") {
            const char* val = next_value(arg);
            if (val == nullptr) {
                return false;
            }
            opts.wav_path = val;
            opts.has_wav = true;
        } else if (arg == "--tail-silence-ms") {
            const char* val = next_value(arg);
            if (val == nullptr || !parse_u32(val, opts.tail_silence_ms)) {
                return false;
            }
            opts.tail_silence_explicit = true;
        } else if (arg == "--wait-stdin-eof") {
            opts.wait_stdin_eof = true;
        } else {
            std::cerr << "Unknown option: " << arg << '\n';
            return false;
        }
    }

    if (!opts.tail_silence_explicit) {
        opts.tail_silence_ms = opts.has_wav ? 800U : 0U;
    }

    return true;
}

size_t compute_chunk_frames(uint32_t sample_rate, uint16_t channels) {
    // 100 ms per chunk, capped at 1600 frames per chunk so even high sample rates
    // stay well below the 4096-float SPSC ring slot limit across 1..=2 channels and
    // split cleanly across multi-channel audio.
    const size_t hundred_ms_frames = std::max<size_t>(1U, static_cast<size_t>(sample_rate) / 10U);
    const size_t max_frames_per_ring_slot =
        std::max<size_t>(1U, kDefaultMaxAudioSlotSamples / static_cast<size_t>(channels));
    return std::min({hundred_ms_frames, size_t{1600U}, max_frames_per_ring_slot});
}

bool stream_pcm_chunked(IpcClient& client, uint32_t user_id, uint32_t sample_rate,
                        uint16_t channels, std::span<const float> samples,
                        std::chrono::milliseconds pacing_delay) {
    if (samples.empty() || channels == 0U) {
        return true;
    }

    const size_t frames_per_chunk = compute_chunk_frames(sample_rate, channels);
    const size_t chunk_sample_limit = frames_per_chunk * static_cast<size_t>(channels);

    // Count queued audio frames pushed into the ring buffer locally so we can yield/sleep
    // whenever we approach ring capacity and let the worker thread drain smoothly without
    // ever triggering Drop-New.
    size_t chunks_since_pause = 0U;
    constexpr size_t kBackpressureBurstLimit = kDefaultAudioRingCapacity / 4U;

    size_t offset = 0U;
    while (offset < samples.size()) {
        const size_t chunk_len = std::min(chunk_sample_limit, samples.size() - offset);
        const auto chunk = samples.subspan(offset, chunk_len);
        const size_t pushed = client.push_audio(user_id, sample_rate, channels, 0U, chunk);
        if (pushed == 0U || client.dropped_audio_frames() > 0U) {
            std::cerr << "Dropped audio frame while streaming WAV (dropped="
                      << client.dropped_audio_frames() << ")\n";
            return false;
        }
        offset += chunk_len;
        ++chunks_since_pause;

        if (offset < samples.size()) {
            const auto sleep_for = (chunks_since_pause >= kBackpressureBurstLimit)
                                       ? std::max(pacing_delay, std::chrono::milliseconds{15})
                                       : pacing_delay;
            if (chunks_since_pause >= kBackpressureBurstLimit) {
                chunks_since_pause = 0U;
            }
            if (sleep_for.count() > 0) {
                std::this_thread::sleep_for(sleep_for);
            }
        }
    }
    return true;
}

bool stream_silence_chunked(IpcClient& client, uint32_t user_id, uint32_t sample_rate,
                            uint16_t channels, uint32_t silence_ms,
                            std::chrono::milliseconds pacing_delay) {
    if (silence_ms == 0U || channels == 0U) {
        return true;
    }
    const uint64_t total_frames =
        (static_cast<uint64_t>(sample_rate) * static_cast<uint64_t>(silence_ms)) / 1000ULL;
    if (total_frames == 0U) {
        return true;
    }

    const size_t frames_per_chunk = compute_chunk_frames(sample_rate, channels);
    std::vector<float> zero_chunk(frames_per_chunk * static_cast<size_t>(channels), 0.0f);

    uint64_t remaining_frames = total_frames;
    size_t chunks_since_pause = 0U;
    constexpr size_t kBackpressureBurstLimit = kDefaultAudioRingCapacity / 4U;

    while (remaining_frames > 0U) {
        const size_t batch_frames = static_cast<size_t>(
            std::min<uint64_t>(remaining_frames, static_cast<uint64_t>(frames_per_chunk)));
        const auto chunk =
            std::span<const float>(zero_chunk.data(), batch_frames * static_cast<size_t>(channels));
        const size_t pushed = client.push_audio(user_id, sample_rate, channels, 0U, chunk);
        if (pushed == 0U || client.dropped_audio_frames() > 0U) {
            std::cerr << "Dropped audio frame while streaming tail silence (dropped="
                      << client.dropped_audio_frames() << ")\n";
            return false;
        }
        remaining_frames -= static_cast<uint64_t>(batch_frames);
        ++chunks_since_pause;

        if (remaining_frames > 0U) {
            const auto sleep_for = (chunks_since_pause >= kBackpressureBurstLimit)
                                       ? std::max(pacing_delay, std::chrono::milliseconds{15})
                                       : pacing_delay;
            if (chunks_since_pause >= kBackpressureBurstLimit) {
                chunks_since_pause = 0U;
            }
            if (sleep_for.count() > 0) {
                std::this_thread::sleep_for(sleep_for);
            }
        }
    }
    return true;
}

}  // namespace

int main(int argc, char** argv) {
    CliOptions opts;
    if (!parse_args(argc, argv, opts)) {
        print_usage(argv[0]);
        return 2;
    }

    kotoha::test::WavAudio wav_audio;
    const bool use_wav = opts.has_wav;
    if (use_wav) {
        kotoha::test::WavReadResult wav_result = kotoha::test::read_wav_file(opts.wav_path);
        if (!wav_result) {
            std::cerr << "Failed to read WAV file '" << opts.wav_path << "': " << wav_result.error
                      << '\n';
            return 2;
        }
        wav_audio = std::move(wav_result.audio);
    }

    IpcClientConfig cfg;
    cfg.app_name = opts.source;
    cfg.plugin_version = opts.version;
    cfg.endpoint_override = opts.endpoint;
    cfg.reconnect_interval = std::chrono::milliseconds{100};
    cfg.heartbeat_interval =
        opts.with_heartbeat ? std::chrono::milliseconds{150} : std::chrono::milliseconds{1000};
    cfg.idle_poll_interval = std::chrono::milliseconds{2};
    cfg.shutdown_drain_timeout = std::chrono::milliseconds{2000};

    IpcClient client(cfg);
    client.start();

    const auto connect_deadline =
        std::chrono::steady_clock::now() + std::chrono::milliseconds{opts.connect_timeout_ms};
    while (!client.is_connected()) {
        if (std::chrono::steady_clock::now() >= connect_deadline) {
            std::cerr << "Timed out connecting to IPC endpoint: " << client.resolved_endpoint()
                      << '\n';
            client.stop();
            return 1;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds{10});
    }

    client.push_user_info(opts.user_id, opts.user_name);
    client.push_talking_state(opts.user_id, TalkingStateValue::Talking);

    bool stream_ok = true;
    constexpr auto kWavChunkSleep = std::chrono::milliseconds{10};
    if (use_wav) {
        stream_ok = stream_pcm_chunked(client, opts.user_id, wav_audio.sample_rate,
                                       wav_audio.channels, wav_audio.samples, kWavChunkSleep);
        if (stream_ok && opts.tail_silence_ms > 0U) {
            stream_ok =
                stream_silence_chunked(client, opts.user_id, wav_audio.sample_rate,
                                       wav_audio.channels, opts.tail_silence_ms, kWavChunkSleep);
        }
    } else {
        // Generate 48kHz stereo 440Hz sine wave frames (20ms = 960 samples/ch = 1920 floats per
        // frame)
        constexpr uint32_t kSampleRate = 48000;
        constexpr uint16_t kChannels = 2;
        constexpr size_t kSamplesPerChannel = 960;
        constexpr double kFrequencyHz = 440.0;
        constexpr double kTwoPi = 6.28318530717958647692;

        std::vector<float> frame_pcm(kSamplesPerChannel * kChannels);
        const auto step_sleep = (opts.duration_ms > 0 && opts.frames > 0)
                                    ? std::chrono::milliseconds{opts.duration_ms / opts.frames}
                                    : std::chrono::milliseconds{20};

        for (uint32_t f = 0; f < opts.frames; ++f) {
            for (size_t i = 0; i < kSamplesPerChannel; ++i) {
                const size_t global_sample = static_cast<size_t>(f) * kSamplesPerChannel + i;
                const double phase =
                    kTwoPi * kFrequencyHz * static_cast<double>(global_sample) / kSampleRate;
                const auto val = static_cast<float>(0.25 * std::sin(phase));
                frame_pcm[i * 2U] = val;
                frame_pcm[i * 2U + 1U] = val;
            }
            const size_t pushed =
                client.push_audio(opts.user_id, kSampleRate, kChannels, 0U, frame_pcm);
            if (pushed == 0U || client.dropped_audio_frames() > 0U) {
                std::cerr << "Dropped audio frame while streaming synthetic frames (dropped="
                          << client.dropped_audio_frames() << ")\n";
                stream_ok = false;
                break;
            }
            if (step_sleep.count() > 0) {
                std::this_thread::sleep_for(step_sleep);
            }
        }

        if (stream_ok && opts.tail_silence_ms > 0U) {
            stream_ok = stream_silence_chunked(client, opts.user_id, kSampleRate, kChannels,
                                               opts.tail_silence_ms, kWavChunkSleep);
        }
    }

    client.push_talking_state(opts.user_id, TalkingStateValue::Passive);

    if (opts.with_heartbeat) {
        std::this_thread::sleep_for(std::chrono::milliseconds{250});
    }

    if (opts.wait_stdin_eof) {
        char ch = '\0';
        while (std::cin.get(ch)) {
            // Keep connection alive (sending heartbeats) until parent closes stdin.
        }
    }

    client.push_user_removed(opts.user_id);
    std::this_thread::sleep_for(std::chrono::milliseconds{30});

    client.stop();
    if (!stream_ok || client.dropped_audio_frames() > 0U) {
        std::cerr << "Audio streaming failed or dropped frames (dropped="
                  << client.dropped_audio_frames() << ")\n";
        return 1;
    }
    return 0;
}
