#include "kotoha/ipc_client.hpp"
#include "kotoha/protocol.hpp"

#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdlib>
#include <iostream>
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
};

void print_usage(const char* argv0) {
    std::cerr << "Usage: " << argv0
              << " [--endpoint <path>] [--source <name>] [--version <ver>]"
                 " [--user-id <id>] [--user-name <name>] [--frames <n>]"
                 " [--duration-ms <ms>] [--with-heartbeat] [--connect-timeout-ms <ms>]\n";
}

bool parse_u32(std::string_view text, uint32_t& out) {
    if (text.empty()) {
        return false;
    }
    char* end = nullptr;
    const std::string copy(text);
    const unsigned long val = std::strtoul(copy.c_str(), &end, 10);
    if (end == nullptr || *end != '\0') {
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
        } else {
            std::cerr << "Unknown option: " << arg << '\n';
            return false;
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

    IpcClientConfig cfg;
    cfg.app_name = opts.source;
    cfg.plugin_version = opts.version;
    cfg.endpoint_override = opts.endpoint;
    cfg.reconnect_interval = std::chrono::milliseconds{100};
    cfg.heartbeat_interval =
        opts.with_heartbeat ? std::chrono::milliseconds{150} : std::chrono::milliseconds{1000};

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

    // Generate 48kHz stereo 440Hz sine wave frames (20ms = 960 samples/ch = 1920 floats per frame)
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
        (void)client.push_audio(opts.user_id, kSampleRate, kChannels, 0U, frame_pcm);
        if (step_sleep.count() > 0) {
            std::this_thread::sleep_for(step_sleep);
        }
    }

    client.push_talking_state(opts.user_id, TalkingStateValue::Passive);

    if (opts.with_heartbeat) {
        std::this_thread::sleep_for(std::chrono::milliseconds{250});
    }

    client.push_user_removed(opts.user_id);
    std::this_thread::sleep_for(std::chrono::milliseconds{30});

    client.stop();
    return 0;
}
