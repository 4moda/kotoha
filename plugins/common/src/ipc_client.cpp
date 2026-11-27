#include "kotoha/ipc_client.hpp"

#include <algorithm>
#include <array>
#include <cerrno>
#include <cstdlib>
#include <cstring>
#include <string>
#include <utility>
#include <vector>

#if defined(_WIN32)
#include <windows.h>
#else
#include <fcntl.h>
#include <poll.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#endif

namespace kotoha::ipc {
namespace {

[[nodiscard]] std::string read_env_var(const char* name) {
#if defined(_WIN32)
    char* raw = nullptr;
    size_t len = 0;
    if (_dupenv_s(&raw, &len, name) != 0 || raw == nullptr) {
        return {};
    }
    std::string value(raw);
    std::free(raw);
    return value;
#else
    const char* raw = std::getenv(name);
    if (raw == nullptr) {
        return {};
    }
    return {raw};
#endif
}

#if defined(_WIN32)
[[nodiscard]] std::wstring utf8_to_wide(std::string_view utf8) {
    if (utf8.empty()) {
        return {};
    }
    const int needed =
        MultiByteToWideChar(CP_UTF8, 0, utf8.data(), static_cast<int>(utf8.size()), nullptr, 0);
    if (needed <= 0) {
        return {};
    }
    std::wstring wide(static_cast<size_t>(needed), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, utf8.data(), static_cast<int>(utf8.size()), wide.data(),
                        needed);
    return wide;
}

[[nodiscard]] bool starts_with_pipe_prefix_ci(std::string_view path) noexcept {
    constexpr std::string_view kPrefix = R"(\\.\pipe\)";
    if (path.size() < kPrefix.size()) {
        return false;
    }
    for (size_t i = 0; i < kPrefix.size(); ++i) {
        const char a = path[i];
        const char b = kPrefix[i];
        const char la = (a >= 'A' && a <= 'Z') ? static_cast<char>(a - 'A' + 'a') : a;
        const char lb = (b >= 'A' && b <= 'Z') ? static_cast<char>(b - 'A' + 'a') : b;
        if (la != lb) {
            return false;
        }
    }
    return true;
}

[[nodiscard]] std::string normalize_windows_pipe_name(std::string_view endpoint) {
    if (starts_with_pipe_prefix_ci(endpoint)) {
        return std::string(endpoint);
    }
    std::string normalized(R"(\\.\pipe\)");
    normalized.append(endpoint);
    return normalized;
}
#endif

}  // namespace

std::string resolve_ipc_endpoint(std::string_view explicit_override) {
    std::string raw_override(explicit_override);
    if (raw_override.empty()) {
        raw_override = read_env_var("KOTOHA_IPC_ENDPOINT");
    }

#if defined(_WIN32)
    if (!raw_override.empty()) {
        return normalize_windows_pipe_name(raw_override);
    }
    return std::string(R"(\\.\pipe\kotoha_v1)");
#else
    if (!raw_override.empty()) {
        return raw_override;
    }

    std::string xdg_runtime = read_env_var("XDG_RUNTIME_DIR");
    if (!xdg_runtime.empty()) {
        while (xdg_runtime.size() > 1U && xdg_runtime.back() == '/') {
            xdg_runtime.pop_back();
        }
        if (xdg_runtime == "/") {
            return "/kotoha_v1.sock";
        }
        xdg_runtime.append("/kotoha_v1.sock");
        return xdg_runtime;
    }

    return "/tmp/kotoha_v1.sock";
#endif
}

class IpcClient::Transport {
public:
    Transport() = default;
    ~Transport() { close(); }

    Transport(const Transport&) = delete;
    Transport& operator=(const Transport&) = delete;

#if defined(_WIN32)
    bool init() {
        if (stop_event_ == nullptr) {
            stop_event_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        } else {
            ResetEvent(stop_event_);
        }
        if (io_event_ == nullptr) {
            io_event_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        }
        return stop_event_ != nullptr && io_event_ != nullptr;
    }

    void signal_stop() noexcept {
        stop_signaled_.store(true, std::memory_order_release);
        if (stop_event_ != nullptr) {
            SetEvent(stop_event_);
        }
        const HANDLE pipe = pipe_.load(std::memory_order_acquire);
        if (pipe != INVALID_HANDLE_VALUE) {
            CancelIoEx(pipe, nullptr);
        }
    }

    void reset_stop() noexcept {
        stop_signaled_.store(false, std::memory_order_release);
        if (stop_event_ != nullptr) {
            ResetEvent(stop_event_);
        }
    }

    bool connect(const std::string& endpoint) {
        close();
        if (stop_signaled_.load(std::memory_order_acquire)) {
            return false;
        }
        const std::wstring wide_endpoint = utf8_to_wide(normalize_windows_pipe_name(endpoint));
        if (wide_endpoint.empty()) {
            return false;
        }

        for (int attempt = 0; attempt < 3; ++attempt) {
            if (stop_signaled_.load(std::memory_order_acquire)) {
                return false;
            }
            HANDLE handle = CreateFileW(wide_endpoint.c_str(), GENERIC_WRITE | GENERIC_READ, 0,
                                        nullptr, OPEN_EXISTING, FILE_FLAG_OVERLAPPED, nullptr);
            if (handle != INVALID_HANDLE_VALUE) {
                pipe_.store(handle, std::memory_order_release);
                return true;
            }

            const DWORD err = GetLastError();
            if (err == ERROR_PIPE_BUSY) {
                WaitNamedPipeW(wide_endpoint.c_str(), 50);
                continue;
            }
            return false;
        }
        return false;
    }

    bool write_all(std::span<const uint8_t> data, std::chrono::milliseconds timeout) {
        const HANDLE handle = pipe_.load(std::memory_order_acquire);
        if (handle == INVALID_HANDLE_VALUE) {
            return false;
        }

        size_t offset = 0;
        while (offset < data.size()) {
            if (stop_signaled_.load(std::memory_order_acquire)) {
                return false;
            }
            ResetEvent(io_event_);
            OVERLAPPED ov{};
            ov.hEvent = io_event_;

            const DWORD remaining = static_cast<DWORD>(data.size() - offset);
            DWORD written = 0;
            const BOOL ok = WriteFile(handle, data.data() + offset, remaining, &written, &ov);

            if (!ok) {
                const DWORD err = GetLastError();
                if (err != ERROR_IO_PENDING) {
                    return false;
                }

                std::array<HANDLE, 2> handles = {io_event_, stop_event_};
                const DWORD wait_ms = static_cast<DWORD>(std::max<int64_t>(1, timeout.count()));
                const DWORD wait_res = WaitForMultipleObjects(2, handles.data(), FALSE, wait_ms);
                if (wait_res != WAIT_OBJECT_0) {
                    CancelIoEx(handle, &ov);
                    DWORD dummy = 0;
                    GetOverlappedResult(handle, &ov, &dummy, TRUE);
                    return false;
                }

                if (!GetOverlappedResult(handle, &ov, &written, FALSE)) {
                    return false;
                }
            }

            if (written == 0) {
                return false;
            }
            offset += static_cast<size_t>(written);
        }

        return true;
    }

    bool is_open() const noexcept {
        return pipe_.load(std::memory_order_acquire) != INVALID_HANDLE_VALUE;
    }

    void close() noexcept {
        const HANDLE handle = pipe_.exchange(INVALID_HANDLE_VALUE, std::memory_order_acq_rel);
        if (handle != INVALID_HANDLE_VALUE) {
            CancelIoEx(handle, nullptr);
            CloseHandle(handle);
        }
    }

    void destroy_events() noexcept {
        close();
        if (io_event_ != nullptr) {
            CloseHandle(io_event_);
            io_event_ = nullptr;
        }
        if (stop_event_ != nullptr) {
            CloseHandle(stop_event_);
            stop_event_ = nullptr;
        }
    }

private:
    std::atomic<HANDLE> pipe_{INVALID_HANDLE_VALUE};
    HANDLE stop_event_ = nullptr;
    HANDLE io_event_ = nullptr;
    std::atomic<bool> stop_signaled_{false};
#else
    bool init() {
        stop_signaled_.store(false, std::memory_order_release);
        return true;
    }

    void signal_stop() noexcept {
        stop_signaled_.store(true, std::memory_order_release);
        const int fd = fd_.load(std::memory_order_acquire);
        if (fd >= 0) {
            ::shutdown(fd, SHUT_RDWR);
        }
    }

    void reset_stop() noexcept { stop_signaled_.store(false, std::memory_order_release); }

    bool connect(const std::string& endpoint) {
        close();
        if (stop_signaled_.load(std::memory_order_acquire)) {
            return false;
        }

        sockaddr_un addr{};
        if (endpoint.empty() || endpoint.size() >= sizeof(addr.sun_path)) {
            return false;
        }

        const int fd = ::socket(AF_UNIX, SOCK_STREAM, 0);
        if (fd < 0) {
            return false;
        }

#if defined(SO_NOSIGPIPE)
        const int set = 1;
        if (::setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &set, sizeof(set)) != 0) {
            ::close(fd);
            return false;
        }
#endif

        const int flags = ::fcntl(fd, F_GETFL, 0);
        if (flags < 0 || ::fcntl(fd, F_SETFL, flags | O_NONBLOCK) != 0) {
            ::close(fd);
            return false;
        }

        addr.sun_family = AF_UNIX;
        std::memcpy(addr.sun_path, endpoint.data(), endpoint.size());
        addr.sun_path[endpoint.size()] = '\0';

        const int rc = ::connect(fd, reinterpret_cast<const sockaddr*>(&addr),
                                 static_cast<socklen_t>(sizeof(addr)));
        if (rc != 0) {
            if (errno != EINPROGRESS) {
                ::close(fd);
                return false;
            }

            pollfd pfd{};
            pfd.fd = fd;
            pfd.events = POLLOUT;
            int poll_rc = 0;
            do {
                poll_rc = ::poll(&pfd, 1, 100);
            } while (poll_rc < 0 && errno == EINTR);

            if (poll_rc <= 0 || stop_signaled_.load(std::memory_order_acquire)) {
                ::close(fd);
                return false;
            }

            int so_error = 0;
            socklen_t so_len = sizeof(so_error);
            if (::getsockopt(fd, SOL_SOCKET, SO_ERROR, &so_error, &so_len) != 0 || so_error != 0) {
                ::close(fd);
                return false;
            }
        }

        fd_.store(fd, std::memory_order_release);
        return true;
    }

    bool write_all(std::span<const uint8_t> data, std::chrono::milliseconds timeout) {
        const int fd = fd_.load(std::memory_order_acquire);
        if (fd < 0) {
            return false;
        }

        const auto deadline = std::chrono::steady_clock::now() + timeout;
        size_t offset = 0;

        while (offset < data.size()) {
            if (stop_signaled_.load(std::memory_order_acquire)) {
                return false;
            }
#if defined(MSG_NOSIGNAL)
            const int send_flags = MSG_NOSIGNAL;
#else
            const int send_flags = 0;
#endif
            const ssize_t sent = ::send(fd, data.data() + offset, data.size() - offset, send_flags);
            if (sent > 0) {
                offset += static_cast<size_t>(sent);
                continue;
            }
            if (sent == 0) {
                return false;
            }

            if (errno == EINTR) {
                continue;
            }
            if (errno != EAGAIN && errno != EWOULDBLOCK) {
                return false;
            }

            const auto now = std::chrono::steady_clock::now();
            if (now >= deadline) {
                return false;
            }
            const auto remaining_ms =
                std::chrono::duration_cast<std::chrono::milliseconds>(deadline - now).count();
            const int wait_ms = static_cast<int>(std::max<int64_t>(1, remaining_ms));

            pollfd pfd{};
            pfd.fd = fd;
            pfd.events = POLLOUT;
            const int poll_rc = ::poll(&pfd, 1, wait_ms);
            if (poll_rc < 0 && errno == EINTR) {
                continue;
            }
            if (poll_rc <= 0 || (pfd.revents & (POLLERR | POLLHUP | POLLNVAL)) != 0) {
                return false;
            }
        }

        return true;
    }

    bool is_open() const noexcept { return fd_.load(std::memory_order_acquire) >= 0; }

    void close() noexcept {
        const int fd = fd_.exchange(-1, std::memory_order_acq_rel);
        if (fd >= 0) {
            ::shutdown(fd, SHUT_RDWR);
            ::close(fd);
        }
    }

    void destroy_events() noexcept { close(); }

private:
    std::atomic<int> fd_{-1};
    std::atomic<bool> stop_signaled_{false};
#endif
};

IpcClient::IpcClient(IpcClientConfig config)
    : config_(std::move(config)), endpoint_(resolve_ipc_endpoint(config_.endpoint_override)),
      transport_(std::make_unique<Transport>()) {}

IpcClient::~IpcClient() {
    stop();
    if (transport_) {
        transport_->destroy_events();
    }
}

void IpcClient::start() {
    std::lock_guard<std::mutex> lifecycle_lock(lifecycle_mutex_);
    if (running_.load(std::memory_order_acquire) || worker_thread_.joinable()) {
        return;
    }

    {
        std::lock_guard<std::mutex> lock(control_mutex_);
        stop_requested_ = false;
        worker_finished_ = false;
        endpoint_ = resolve_ipc_endpoint(config_.endpoint_override);
    }
    stop_signaled_.store(false, std::memory_order_release);

    transport_->init();
    transport_->reset_stop();

    running_.store(true, std::memory_order_release);
    worker_thread_ = std::thread([this]() { worker_loop(); });
}

void IpcClient::stop() {
    std::lock_guard<std::mutex> lifecycle_lock(lifecycle_mutex_);
    const bool was_running = running_.load(std::memory_order_acquire);
    if (!was_running && !worker_thread_.joinable()) {
        return;
    }

    stop_signaled_.store(true, std::memory_order_release);
    {
        std::lock_guard<std::mutex> lock(control_mutex_);
        stop_requested_ = true;
    }
    control_cv_.notify_all();

    if (connected_.load(std::memory_order_acquire)) {
        std::unique_lock<std::mutex> lock(control_mutex_);
        worker_done_cv_.wait_for(lock, config_.shutdown_drain_timeout,
                                 [this]() { return worker_finished_; });
    }

    transport_->signal_stop();

    if (worker_thread_.joinable()) {
        worker_thread_.join();
    }

    transport_->close();
    connected_.store(false, std::memory_order_release);
    running_.store(false, std::memory_order_release);
}

size_t IpcClient::push_audio(uint32_t user_id, uint32_t sample_rate, uint16_t channels,
                             uint16_t flags, std::span<const float> samples) noexcept {
    if (samples.empty()) {
        return 0;
    }
    if (sample_rate < kMinSampleRate || sample_rate > kMaxSampleRate) {
        return 0;
    }
    if ((flags & ~kAudioFlagAllowedMask) != 0U) {
        return 0;
    }
    return audio_ring_.push_split(user_id, sample_rate, channels, flags, samples);
}

void IpcClient::push_user_info(uint32_t user_id, std::string name) {
    ControlEvent event;
    event.kind = ControlEvent::Kind::UserInfo;
    event.user_id = user_id;
    event.name = std::move(name);
    enqueue_control_event(std::move(event));
}

void IpcClient::push_talking_state(uint32_t user_id, TalkingStateValue state) {
    ControlEvent event;
    event.kind = ControlEvent::Kind::TalkingState;
    event.user_id = user_id;
    event.talking_state = state;
    enqueue_control_event(std::move(event));
}

void IpcClient::push_user_removed(uint32_t user_id) {
    ControlEvent event;
    event.kind = ControlEvent::Kind::UserRemoved;
    event.user_id = user_id;
    enqueue_control_event(std::move(event));
}

void IpcClient::push_source_reset() {
    ControlEvent event;
    event.kind = ControlEvent::Kind::SourceReset;
    enqueue_control_event(std::move(event));
}

bool IpcClient::is_connected() const noexcept {
    return connected_.load(std::memory_order_acquire);
}

bool IpcClient::is_running() const noexcept {
    return running_.load(std::memory_order_acquire);
}

uint64_t IpcClient::dropped_audio_frames() const noexcept {
    return audio_ring_.dropped_count();
}

uint32_t IpcClient::current_audio_seq() const noexcept {
    return audio_ring_.current_seq();
}

std::string IpcClient::resolved_endpoint() const {
    std::lock_guard<std::mutex> lock(control_mutex_);
    return endpoint_;
}

void IpcClient::enqueue_control_event(ControlEvent event) {
    {
        std::lock_guard<std::mutex> lock(control_mutex_);
        control_queue_.push_back(std::move(event));
    }
    control_cv_.notify_one();
}

bool IpcClient::write_encoded_frame(std::span<const uint8_t> frame_bytes,
                                    std::chrono::milliseconds timeout) {
    const auto clamped_timeout = std::max(std::chrono::milliseconds{10}, timeout);
    return transport_->write_all(frame_bytes, clamped_timeout);
}

bool IpcClient::send_hello() {
    size_t written = 0;
    const ProtocolError err =
        encode_hello(HelloView{config_.app_name, config_.plugin_version}, tx_buffer_, written);
    if (err != ProtocolError::Ok) {
        return false;
    }
    return write_encoded_frame(std::span<const uint8_t>(tx_buffer_.data(), written),
                               config_.shutdown_drain_timeout);
}

bool IpcClient::send_heartbeat() {
    size_t written = 0;
    const ProtocolError err = encode_heartbeat(tx_buffer_, written);
    if (err != ProtocolError::Ok) {
        return false;
    }
    return write_encoded_frame(std::span<const uint8_t>(tx_buffer_.data(), written),
                               config_.shutdown_drain_timeout);
}

bool IpcClient::send_control_event(const ControlEvent& event) {
    size_t written = 0;
    ProtocolError err = ProtocolError::Ok;

    switch (event.kind) {
        case ControlEvent::Kind::UserInfo:
            err = encode_user_info(UserInfoView{event.user_id, event.name}, tx_buffer_, written);
            break;
        case ControlEvent::Kind::TalkingState:
            err = encode_talking_state(TalkingStateMessage{event.user_id, event.talking_state},
                                       tx_buffer_, written);
            break;
        case ControlEvent::Kind::UserRemoved:
            err = encode_user_removed(UserRemovedMessage{event.user_id}, tx_buffer_, written);
            break;
        case ControlEvent::Kind::SourceReset:
            err = encode_source_reset(tx_buffer_, written);
            break;
    }

    if (err != ProtocolError::Ok) {
        // Drop malformed control payloads (e.g., invalid UTF-8) rather than poisoning connection.
        return true;
    }

    return write_encoded_frame(std::span<const uint8_t>(tx_buffer_.data(), written),
                               config_.shutdown_drain_timeout);
}

bool IpcClient::drain_audio_ring(bool& wrote_any, bool drain_all_available,
                                 std::chrono::steady_clock::time_point deadline) {
    bool io_ok = true;
    size_t iterations = 0;
    constexpr size_t kMaxBatchIterations = kDefaultAudioRingCapacity;
    auto effective_deadline = deadline;

    while (io_ok) {
        if (!drain_all_available && iterations >= kMaxBatchIterations) {
            break;
        }
        if (effective_deadline == std::chrono::steady_clock::time_point::max() &&
            stop_signaled_.load(std::memory_order_acquire)) {
            effective_deadline = std::chrono::steady_clock::now() + config_.shutdown_drain_timeout;
        }
        if (effective_deadline != std::chrono::steady_clock::time_point::max() &&
            std::chrono::steady_clock::now() >= effective_deadline) {
            break;
        }

        const bool popped = audio_ring_.try_consume([&](const auto& slot) {
            size_t written = 0;
            const AudioFrameView view{
                slot.user_id,     slot.seq,
                slot.sample_rate, slot.channels,
                slot.flags,       std::span<const float>(slot.samples.data(), slot.sample_count),
            };
            const ProtocolError err = encode_audio_frame(view, tx_buffer_, written);
            if (err != ProtocolError::Ok) {
                return;
            }

            auto io_timeout = config_.shutdown_drain_timeout;
            if (effective_deadline != std::chrono::steady_clock::time_point::max()) {
                const auto rem = std::chrono::duration_cast<std::chrono::milliseconds>(
                    effective_deadline - std::chrono::steady_clock::now());
                io_timeout = std::max(std::chrono::milliseconds{10}, rem);
            }

            if (write_encoded_frame(std::span<const uint8_t>(tx_buffer_.data(), written),
                                    io_timeout)) {
                wrote_any = true;
            } else {
                io_ok = false;
            }
        });
        if (!popped) {
            break;
        }
        ++iterations;
    }
    return io_ok;
}

bool IpcClient::drain_pending_events(bool& wrote_any, bool& stop_after_drain,
                                     bool& last_was_source_reset,
                                     std::chrono::steady_clock::time_point deadline) {
    wrote_any = false;
    stop_after_drain = false;

    std::deque<ControlEvent> pending_controls;
    {
        std::lock_guard<std::mutex> lock(control_mutex_);
        pending_controls.swap(control_queue_);
        stop_after_drain = stop_requested_;
    }

    const auto is_destructive = [](const ControlEvent& ev) {
        return ev.kind == ControlEvent::Kind::UserRemoved ||
               ev.kind == ControlEvent::Kind::SourceReset;
    };

    // Process control events in strict chronological FIFO order. Whenever a destructive
    // lifecycle event (UserRemoved / SourceReset) is reached, drain all pending audio frames
    // first so preceding speech audio is never dropped before the lifecycle transition.
    for (const ControlEvent& event : pending_controls) {
        if (deadline != std::chrono::steady_clock::time_point::max() &&
            std::chrono::steady_clock::now() >= deadline) {
            return false;
        }

        if (is_destructive(event)) {
            bool audio_wrote = false;
            if (!drain_audio_ring(audio_wrote, true, deadline)) {
                return false;
            }
            if (audio_wrote) {
                wrote_any = true;
                last_was_source_reset = false;
            }
        }

        if (!send_control_event(event)) {
            return false;
        }
        wrote_any = true;
        last_was_source_reset = (event.kind == ControlEvent::Kind::SourceReset);
    }

    // Drain any remaining audio frames after all queued control events.
    bool tail_audio_wrote = false;
    if (!drain_audio_ring(tail_audio_wrote, stop_after_drain, deadline)) {
        return false;
    }
    if (tail_audio_wrote) {
        wrote_any = true;
        last_was_source_reset = false;
    }

    return true;
}

void IpcClient::worker_loop() {
    auto last_tx_time = std::chrono::steady_clock::now();
    const auto idle_wait = std::clamp(config_.idle_poll_interval, std::chrono::milliseconds{2},
                                      std::chrono::milliseconds{5});
    bool last_was_source_reset = false;
    std::string active_endpoint;
    {
        std::lock_guard<std::mutex> lock(control_mutex_);
        active_endpoint = endpoint_;
    }

    while (true) {
        if (!connected_.load(std::memory_order_acquire)) {
            {
                std::lock_guard<std::mutex> lock(control_mutex_);
                if (stop_requested_) {
                    break;
                }
            }

            if (transport_->connect(active_endpoint)) {
                if (send_hello()) {
                    connected_.store(true, std::memory_order_release);
                    last_tx_time = std::chrono::steady_clock::now();
                    last_was_source_reset = false;
                    continue;
                }
                transport_->close();
            }

            std::unique_lock<std::mutex> lock(control_mutex_);
            control_cv_.wait_for(lock, config_.reconnect_interval,
                                 [this]() { return stop_requested_; });
            if (stop_requested_) {
                break;
            }
            continue;
        }

        bool wrote_any = false;
        bool stop_after_drain = false;
        if (!drain_pending_events(wrote_any, stop_after_drain, last_was_source_reset,
                                  std::chrono::steady_clock::time_point::max())) {
            transport_->close();
            connected_.store(false, std::memory_order_release);
            if (stop_after_drain) {
                break;
            }
            std::unique_lock<std::mutex> lock(control_mutex_);
            control_cv_.wait_for(lock, config_.reconnect_interval,
                                 [this]() { return stop_requested_; });
            if (stop_requested_) {
                break;
            }
            continue;
        }

        const auto now = std::chrono::steady_clock::now();
        if (wrote_any) {
            last_tx_time = now;
        }

        if (stop_after_drain) {
            const auto deadline = now + config_.shutdown_drain_timeout;
            // Drain any remaining items that arrived concurrently before emitting final
            // SourceReset.
            bool extra_wrote = false;
            bool unused_stop = false;
            if (drain_pending_events(extra_wrote, unused_stop, last_was_source_reset, deadline) &&
                !last_was_source_reset) {
                ControlEvent reset_event;
                reset_event.kind = ControlEvent::Kind::SourceReset;
                (void)send_control_event(reset_event);
            }
            transport_->signal_stop();
            transport_->close();
            connected_.store(false, std::memory_order_release);
            break;
        }

        if ((now - last_tx_time) >= config_.heartbeat_interval) {
            if (!send_heartbeat()) {
                transport_->close();
                connected_.store(false, std::memory_order_release);
                continue;
            }
            last_tx_time = std::chrono::steady_clock::now();
        }

        bool has_more = !audio_ring_.empty();
        if (!has_more) {
            std::lock_guard<std::mutex> lock(control_mutex_);
            has_more = !control_queue_.empty() || stop_requested_;
        }

        if (!has_more) {
            std::unique_lock<std::mutex> lock(control_mutex_);
            control_cv_.wait_for(lock, idle_wait,
                                 [this]() { return stop_requested_ || !control_queue_.empty(); });
        }
    }

    connected_.store(false, std::memory_order_release);
    {
        std::lock_guard<std::mutex> lock(control_mutex_);
        worker_finished_ = true;
    }
    worker_done_cv_.notify_all();
}

}  // namespace kotoha::ipc
