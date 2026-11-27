#include "kotoha/ipc_client.hpp"
#include "kotoha/protocol.hpp"

#include <atomic>
#include <chrono>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <iostream>
#include <memory>
#include <sstream>
#include <string>
#include <thread>
#include <variant>
#include <vector>

#if !defined(_WIN32)
#include <poll.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#endif

namespace {

using namespace kotoha::ipc;

void check_impl(bool cond, const char* expr, int line) {
    if (!cond) {
        std::cerr << "FAIL at line " << line << ": " << expr << '\n';
        std::exit(1);
    }
}

#define CHECK(expr) check_impl(static_cast<bool>(expr), #expr, __LINE__)

void test_endpoint_resolution() {
#if !defined(_WIN32)
    // Explicit override always wins
    CHECK(resolve_ipc_endpoint("/tmp/custom_override.sock") == "/tmp/custom_override.sock");

    const char* old_ep = std::getenv("KOTOHA_IPC_ENDPOINT");
    const std::string saved_ep = old_ep != nullptr ? old_ep : "";
    const bool had_ep = old_ep != nullptr;

    const char* old_xdg = std::getenv("XDG_RUNTIME_DIR");
    const std::string saved_xdg = old_xdg != nullptr ? old_xdg : "";
    const bool had_xdg = old_xdg != nullptr;

    const char* old_tmpdir = std::getenv("TMPDIR");
    const std::string saved_tmpdir = old_tmpdir != nullptr ? old_tmpdir : "";
    const bool had_tmpdir = old_tmpdir != nullptr;
    const std::string uid_str = std::to_string(static_cast<unsigned long>(::geteuid()));

    setenv("KOTOHA_IPC_ENDPOINT", "/tmp/env_override.sock", 1);
    setenv("XDG_RUNTIME_DIR", "/run/user/1000/", 1);
    CHECK(resolve_ipc_endpoint() == "/tmp/env_override.sock");

    unsetenv("KOTOHA_IPC_ENDPOINT");
    CHECK(resolve_ipc_endpoint() == "/run/user/1000/kotoha_v1.sock");

    // No fallback directly inside world-writable /tmp: a per-user private dir is used instead.
    unsetenv("XDG_RUNTIME_DIR");
    unsetenv("TMPDIR");
    CHECK(resolve_ipc_endpoint() == "/tmp/kotoha-" + uid_str + "/kotoha_v1.sock");

    setenv("TMPDIR", "/var/folders/ab/cd/T/", 1);
    CHECK(resolve_ipc_endpoint() == "/var/folders/ab/cd/T/kotoha-" + uid_str + "/kotoha_v1.sock");

    if (had_tmpdir) {
        setenv("TMPDIR", saved_tmpdir.c_str(), 1);
    } else {
        unsetenv("TMPDIR");
    }

    if (had_ep) {
        setenv("KOTOHA_IPC_ENDPOINT", saved_ep.c_str(), 1);
    } else {
        unsetenv("KOTOHA_IPC_ENDPOINT");
    }
    if (had_xdg) {
        setenv("XDG_RUNTIME_DIR", saved_xdg.c_str(), 1);
    } else {
        unsetenv("XDG_RUNTIME_DIR");
    }
#else
    CHECK(resolve_ipc_endpoint("custom_pipe") == R"(\\.\pipe\custom_pipe)");
    CHECK(resolve_ipc_endpoint(R"(\\.\pipe\explicit)") == R"(\\.\pipe\explicit)");
#endif
}

[[nodiscard]] std::vector<std::string> split_tabs(const std::string& line) {
    std::vector<std::string> cols;
    std::string col;
    std::istringstream stream(line);
    while (std::getline(stream, col, '\t')) {
        cols.push_back(col);
    }
    return cols;
}

[[nodiscard]] std::string fixture_value(const std::string& col) {
    return col == "-" ? std::string() : col;
}

/// Checks the C++ resolver against the fixture shared with the Rust resolver tests.
void test_endpoint_resolution_parity_fixture() {
    std::ifstream fixture(KOTOHA_ENDPOINT_FIXTURE);
    CHECK(fixture.is_open());
    std::string line;
    size_t checked = 0;
    while (std::getline(fixture, line)) {
        if (!line.empty() && line.back() == '\r') {
            line.pop_back();
        }
        if (line.empty() || line.front() == '#') {
            continue;
        }
        const auto cols = split_tabs(line);
        CHECK(cols.size() == 6U);
        PosixEndpointEnv env;
        env.endpoint_override = fixture_value(cols[0]);
        env.xdg_runtime_dir = fixture_value(cols[1]);
        env.tmpdir = fixture_value(cols[2]);
        env.euid = static_cast<uint32_t>(std::stoul(cols[3]));
        const std::string actual = resolve_posix_ipc_endpoint(env);
        if (actual != cols[4]) {
            std::cerr << "parity mismatch: got '" << actual << "' expected '" << cols[4]
                      << "' for line: " << line << '\n';
        }
        CHECK(actual == cols[4]);
        ++checked;
    }
    CHECK(checked >= 10U);
}

#if !defined(_WIN32)
void test_unix_peer_uid_check() {
    std::array<int, 2> fds{-1, -1};
    CHECK(::socketpair(AF_UNIX, SOCK_STREAM, 0, fds.data()) == 0);
    const auto self = static_cast<uint32_t>(::geteuid());
    CHECK(unix_peer_has_uid(fds[0], self));
    CHECK(unix_peer_has_uid(fds[1], self));
    CHECK(!unix_peer_has_uid(fds[0], self + 1U));
    ::close(fds[0]);
    ::close(fds[1]);
    CHECK(!unix_peer_has_uid(-1, self));
}

class MockUdsServer {
public:
    explicit MockUdsServer(std::string socket_path)
        : socket_path_(std::move(socket_path)), listen_fd_(::socket(AF_UNIX, SOCK_STREAM, 0)) {
        ::unlink(socket_path_.c_str());
        CHECK(listen_fd_ >= 0);

        sockaddr_un addr{};
        CHECK(socket_path_.size() < sizeof(addr.sun_path));
        addr.sun_family = AF_UNIX;
        std::memcpy(addr.sun_path, socket_path_.data(), socket_path_.size());
        addr.sun_path[socket_path_.size()] = '\0';

        CHECK(::bind(listen_fd_, reinterpret_cast<const sockaddr*>(&addr), sizeof(addr)) == 0);
        CHECK(::listen(listen_fd_, 4) == 0);
    }

    ~MockUdsServer() {
        close_client();
        if (listen_fd_ >= 0) {
            ::close(listen_fd_);
        }
        ::unlink(socket_path_.c_str());
    }

    bool accept_one(int timeout_ms) {
        pollfd pfd{};
        pfd.fd = listen_fd_;
        pfd.events = POLLIN;
        const int rc = ::poll(&pfd, 1, timeout_ms);
        if (rc <= 0) {
            return false;
        }
        client_fd_ = ::accept(listen_fd_, nullptr, nullptr);
        return client_fd_ >= 0;
    }

    void close_client() {
        if (client_fd_ >= 0) {
            ::close(client_fd_);
            client_fd_ = -1;
        }
    }

    std::vector<Message> read_messages_until_eof_or_count(size_t target_count,
                                                          std::chrono::milliseconds timeout,
                                                          bool ignore_heartbeats = false) {
        std::vector<Message> messages;
        std::vector<uint8_t> rx_buf;
        const auto deadline = std::chrono::steady_clock::now() + timeout;

        auto non_heartbeat_count = [&]() {
            if (!ignore_heartbeats) {
                return messages.size();
            }
            size_t count = 0;
            for (const auto& m : messages) {
                if (!std::holds_alternative<HeartbeatMessage>(m)) {
                    ++count;
                }
            }
            return count;
        };

        while (non_heartbeat_count() < target_count &&
               std::chrono::steady_clock::now() < deadline) {
            pollfd pfd{};
            pfd.fd = client_fd_;
            pfd.events = POLLIN;
            const auto rem = std::chrono::duration_cast<std::chrono::milliseconds>(
                                 deadline - std::chrono::steady_clock::now())
                                 .count();
            const int poll_rc = ::poll(&pfd, 1, static_cast<int>(std::max<int64_t>(1, rem)));
            if (poll_rc < 0) {
                break;
            }
            if (poll_rc == 0) {
                continue;
            }

            uint8_t chunk[4096];
            const ssize_t n = ::recv(client_fd_, chunk, sizeof(chunk), 0);
            if (n <= 0) {
                break;
            }
            rx_buf.insert(rx_buf.end(), chunk, chunk + n);

            while (rx_buf.size() >= kHeaderSize) {
                const HeaderDecodeResult hdr = decode_header(rx_buf);
                CHECK(hdr.error == ProtocolError::Ok);
                const size_t frame_len = kHeaderSize + hdr.header.payload_len;
                if (rx_buf.size() < frame_len) {
                    break;
                }
                const DecodeResult dec =
                    decode_frame(std::span<const uint8_t>(rx_buf.data(), frame_len));
                CHECK(dec.error == ProtocolError::Ok);
                messages.push_back(dec.message);
                rx_buf.erase(rx_buf.begin(), rx_buf.begin() + static_cast<ptrdiff_t>(frame_len));
            }
        }
        return messages;
    }

private:
    std::string socket_path_;
    int listen_fd_ = -1;
    int client_fd_ = -1;
};

template <typename Predicate> bool wait_until(Predicate&& pred, std::chrono::milliseconds timeout) {
    const auto deadline = std::chrono::steady_clock::now() + timeout;
    while (std::chrono::steady_clock::now() < deadline) {
        if (pred()) {
            return true;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds{5});
    }
    return pred();
}

void test_ipc_client_flow_and_reconnect() {
    const std::string sock_path =
        "/tmp/kotoha-test-" + std::to_string(static_cast<long>(::getpid())) + ".sock";
    auto server = std::make_unique<MockUdsServer>(sock_path);

    IpcClientConfig cfg;
    cfg.app_name = "mumble";
    cfg.plugin_version = "0.1.0";
    cfg.endpoint_override = sock_path;
    cfg.reconnect_interval = std::chrono::milliseconds{50};
    cfg.heartbeat_interval = std::chrono::milliseconds{60};
    cfg.idle_poll_interval = std::chrono::milliseconds{2};
    cfg.shutdown_drain_timeout = std::chrono::milliseconds{150};

    IpcClient client(cfg);
    client.start();

    CHECK(server->accept_one(1000));
    CHECK(wait_until([&]() { return client.is_connected(); }, std::chrono::milliseconds{500}));

    // Wait briefly so Hello + at least 1 Heartbeat are sent
    const auto first_batch =
        server->read_messages_until_eof_or_count(2, std::chrono::milliseconds{1000});
    CHECK(first_batch.size() >= 2);
    const auto* hello = std::get_if<HelloMessage>(&first_batch[0]);
    CHECK(hello != nullptr);
    CHECK(hello->app_name == "mumble");
    CHECK(hello->plugin_version == "0.1.0");
    CHECK(std::holds_alternative<HeartbeatMessage>(first_batch[1]));

    // Push UserInfo, TalkingState, Audio, UserRemoved, then a new UserInfo(43) ->
    // verify strict chronological FIFO ordering (UserRemoved(42) must precede UserInfo(43),
    // and AudioFrame must precede UserRemoved(42)).
    client.push_user_info(42U, "Alice");
    client.push_talking_state(42U, TalkingStateValue::Talking);
    const std::array<float, 4> pcm = {0.25f, -0.25f, 0.5f, -0.5f};
    CHECK(client.push_audio(42U, 48000U, 2U, kAudioFlagIsLocalUser, pcm) == 1U);
    client.push_user_removed(42U);
    client.push_user_info(43U, "Bob");

    const auto event_batch =
        server->read_messages_until_eof_or_count(5, std::chrono::milliseconds{1000}, true);

    std::vector<Message> ordered;
    for (const auto& m : event_batch) {
        if (!std::holds_alternative<HeartbeatMessage>(m)) {
            ordered.push_back(m);
        }
    }
    CHECK(ordered.size() == 5);
    const auto* ui0 = std::get_if<UserInfoMessage>(&ordered[0]);
    CHECK(ui0 != nullptr && ui0->user_id == 42U && ui0->name == "Alice");
    CHECK(std::holds_alternative<TalkingStateMessage>(ordered[1]));
    // AudioFrame must arrive before destructive UserRemoved
    CHECK(std::holds_alternative<AudioFrameMessage>(ordered[2]));
    const auto* ur3 = std::get_if<UserRemovedMessage>(&ordered[3]);
    CHECK(ur3 != nullptr && ur3->user_id == 42U);
    // Chronological UserInfo after UserRemoved must NOT be reordered ahead of UserRemoved
    const auto* ui4 = std::get_if<UserInfoMessage>(&ordered[4]);
    CHECK(ui4 != nullptr && ui4->user_id == 43U && ui4->name == "Bob");

    // Simulate full server shutdown & restart; client must reconnect and re-send Hello
    server.reset();
    {
        auto server2 = std::make_unique<MockUdsServer>(sock_path);
        CHECK(server2->accept_one(1000));
        const auto reconnect_raw =
            server2->read_messages_until_eof_or_count(1, std::chrono::milliseconds{1000}, true);
        std::vector<Message> reconnect_msgs;
        for (const auto& m : reconnect_raw) {
            if (!std::holds_alternative<HeartbeatMessage>(m)) {
                reconnect_msgs.push_back(m);
            }
        }
        CHECK(reconnect_msgs.size() == 1);
        const auto* hello2 = std::get_if<HelloMessage>(&reconnect_msgs[0]);
        CHECK(hello2 != nullptr && hello2->app_name == "mumble");

        // Push audio right before stop() -> stop() must drain audio before SourceReset
        const std::array<float, 2> tail_pcm = {0.125f, -0.125f};
        CHECK(client.push_audio(42U, 48000U, 1U, 0U, tail_pcm) == 1U);
        client.stop();

        const auto shutdown_msgs =
            server2->read_messages_until_eof_or_count(16, std::chrono::milliseconds{1000});
        bool saw_tail_audio = false;
        bool saw_source_reset = false;
        size_t audio_idx = 0;
        size_t reset_idx = 0;
        for (size_t i = 0; i < shutdown_msgs.size(); ++i) {
            if (const auto* af = std::get_if<AudioFrameMessage>(&shutdown_msgs[i])) {
                if (af->seq == 1U) {
                    saw_tail_audio = true;
                    audio_idx = i;
                }
            } else if (std::holds_alternative<SourceResetMessage>(shutdown_msgs[i])) {
                saw_source_reset = true;
                reset_idx = i;
            }
        }
        CHECK(saw_tail_audio);
        CHECK(saw_source_reset);
        CHECK(audio_idx < reset_idx);
    }
}
/// A server listening as another uid (simulating a squatted endpoint) must never receive data:
/// the client closes the connection after the peer-uid check and keeps reporting disconnected.
/// Requires root (to listen() with a different effective uid); skipped otherwise.
void test_client_rejects_server_of_other_uid() {
    if (::geteuid() != 0) {
        std::cout << "test_client_rejects_server_of_other_uid: skipped (requires root)\n";
        return;
    }
    constexpr uid_t kOtherUid = 65534;
    const std::string sock_path =
        "/tmp/kotoha-test-foreign-" + std::to_string(static_cast<long>(::getpid())) + ".sock";
    ::unlink(sock_path.c_str());

    // SO_PEERCRED reports the credentials captured at listen() time, so create, bind and listen
    // with a temporarily switched effective uid, then switch back before starting the client.
    CHECK(::seteuid(kOtherUid) == 0);
    const int listen_fd = ::socket(AF_UNIX, SOCK_STREAM, 0);
    sockaddr_un addr{};
    addr.sun_family = AF_UNIX;
    std::memcpy(addr.sun_path, sock_path.data(), sock_path.size());
    const bool bound =
        listen_fd >= 0 &&
        ::bind(listen_fd, reinterpret_cast<const sockaddr*>(&addr), sizeof(addr)) == 0 &&
        ::listen(listen_fd, 8) == 0;
    CHECK(::seteuid(0) == 0);
    CHECK(bound);

    IpcClientConfig cfg;
    cfg.endpoint_override = sock_path;
    cfg.reconnect_interval = std::chrono::milliseconds{20};
    cfg.heartbeat_interval = std::chrono::milliseconds{20};
    IpcClient client(cfg);
    client.start();

    size_t accepted = 0;
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds{600};
    while (std::chrono::steady_clock::now() < deadline) {
        CHECK(!client.is_connected());
        pollfd pfd{};
        pfd.fd = listen_fd;
        pfd.events = POLLIN;
        if (::poll(&pfd, 1, 20) <= 0) {
            continue;
        }
        const int conn = ::accept(listen_fd, nullptr, nullptr);
        if (conn < 0) {
            continue;
        }
        ++accepted;
        // The client must have closed without sending anything (no Hello, no audio).
        pollfd cpfd{};
        cpfd.fd = conn;
        cpfd.events = POLLIN;
        CHECK(::poll(&cpfd, 1, 500) == 1);
        std::array<uint8_t, 64> buf{};
        CHECK(::recv(conn, buf.data(), buf.size(), 0) == 0);
        ::close(conn);
    }
    CHECK(!client.is_connected());
    client.stop();
    // The worker retried on its reconnect interval rather than giving up after one attempt.
    CHECK(accepted >= 2U);

    ::close(listen_fd);
    ::unlink(sock_path.c_str());
}
#endif

}  // namespace

int main() {
    test_endpoint_resolution();
    test_endpoint_resolution_parity_fixture();
#if !defined(_WIN32)
    test_unix_peer_uid_check();
    test_ipc_client_flow_and_reconnect();
    test_client_rejects_server_of_other_uid();
#endif
    std::cout << "test_ipc_client: all assertions passed\n";
    return 0;
}
