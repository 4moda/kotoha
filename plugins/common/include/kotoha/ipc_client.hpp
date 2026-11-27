#pragma once

#include "kotoha/protocol.hpp"
#include "kotoha/ring_buffer.hpp"

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <deque>
#include <memory>
#include <mutex>
#include <span>
#include <string>
#include <string_view>
#include <thread>

namespace kotoha::ipc {

struct IpcClientConfig {
    std::string app_name = "mumble";
    std::string plugin_version = "0.1.0";
    std::string endpoint_override{};
    std::chrono::milliseconds reconnect_interval{1000};
    std::chrono::milliseconds heartbeat_interval{1000};
    std::chrono::milliseconds idle_poll_interval{3};
    std::chrono::milliseconds shutdown_drain_timeout{250};
};

/// Resolves the OS-native IPC endpoint using DESIGN.md Section 3.1 precedence:
/// 1. KOTOHA_IPC_ENDPOINT environment variable (or explicit override)
/// 2. Windows: \\.\pipe\kotoha_v1
/// 3. POSIX: $XDG_RUNTIME_DIR/kotoha_v1.sock (trailing slash normalized)
/// 4. POSIX fallback: /tmp/kotoha_v1.sock
[[nodiscard]] std::string resolve_ipc_endpoint(std::string_view explicit_override = {});

class IpcClient {
public:
    explicit IpcClient(IpcClientConfig config = {});
    ~IpcClient();

    IpcClient(const IpcClient&) = delete;
    IpcClient& operator=(const IpcClient&) = delete;
    IpcClient(IpcClient&&) = delete;
    IpcClient& operator=(IpcClient&&) = delete;

    void start();
    void stop();

    /// Wait-free, lock-free, zero-allocation, zero-syscall audio submission.
    /// Safe to invoke directly from real-time audio callback threads.
    /// Oversized buffers are split into channel-aligned chunks.
    size_t push_audio(uint32_t user_id, uint32_t sample_rate, uint16_t channels, uint16_t flags,
                      std::span<const float> samples) noexcept;

    void push_user_info(uint32_t user_id, std::string name);
    void push_talking_state(uint32_t user_id, TalkingStateValue state);
    void push_user_removed(uint32_t user_id);
    void push_source_reset();

    [[nodiscard]] bool is_connected() const noexcept;
    [[nodiscard]] bool is_running() const noexcept;
    [[nodiscard]] uint64_t dropped_audio_frames() const noexcept;
    [[nodiscard]] uint32_t current_audio_seq() const noexcept;
    [[nodiscard]] std::string resolved_endpoint() const;

private:
    struct ControlEvent {
        enum class Kind : uint8_t {
            UserInfo,
            TalkingState,
            UserRemoved,
            SourceReset,
        };

        Kind kind = Kind::SourceReset;
        uint32_t user_id = 0;
        TalkingStateValue talking_state = TalkingStateValue::Passive;
        std::string name;
    };

    class Transport;

    void worker_loop();
    void enqueue_control_event(ControlEvent event);
    bool drain_pending_events(bool& wrote_any, bool& stop_after_drain, bool& last_was_source_reset,
                              std::chrono::steady_clock::time_point deadline);
    bool drain_audio_ring(bool& wrote_any, bool drain_all_available,
                          std::chrono::steady_clock::time_point deadline);
    bool send_control_event(const ControlEvent& event);
    bool send_hello();
    bool send_heartbeat();
    bool write_encoded_frame(std::span<const uint8_t> frame_bytes,
                             std::chrono::milliseconds timeout);

    IpcClientConfig config_;
    std::string endpoint_;

    SpscAudioRingBuffer<kDefaultAudioRingCapacity, kDefaultMaxAudioSlotSamples> audio_ring_;
    std::array<uint8_t, kMaxFrameSize> tx_buffer_{};

    std::mutex control_mutex_;
    std::condition_variable control_cv_;
    std::deque<ControlEvent> non_destructive_queue_;
    std::deque<ControlEvent> destructive_queue_;
    bool stop_requested_ = false;

    std::atomic<bool> running_{false};
    std::atomic<bool> connected_{false};
    std::atomic<bool> stop_signaled_{false};

    std::unique_ptr<Transport> transport_;
    std::thread worker_thread_;
};

}  // namespace kotoha::ipc
