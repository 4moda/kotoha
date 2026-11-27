#include "MumbleAPI_v_1_0_x.h"
#include "MumblePlugin_v_1_0_x.h"
#include "PluginComponents_v_1_0_x.h"
#include "kotoha/protocol.hpp"

#include <algorithm>
#include <array>
#include <chrono>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <memory>
#include <mutex>
#include <string>
#include <unordered_set>
#include <variant>
#include <vector>

#if defined(_WIN32)
#include <windows.h>
#else
#include <dlfcn.h>
#include <poll.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#endif

#ifndef KOTOHA_MUMBLE_PLUGIN_PATH
#define KOTOHA_MUMBLE_PLUGIN_PATH ""
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

struct MockApiTracker {
    std::mutex mutex;
    mumble_plugin_id_t expected_caller_id = 77;
    mumble_connection_t expected_connection = 9;
    mumble_userid_t local_user_id = 101;
    std::unordered_set<const void*> live_allocations;
    size_t total_allocations = 0;
    size_t total_frees = 0;

    void* alloc_copy_cstr(const char* src) {
        const size_t len = std::strlen(src);
        auto* buf = new char[len + 1];
        std::memcpy(buf, src, len + 1);
        std::lock_guard<std::mutex> lock(mutex);
        live_allocations.insert(buf);
        ++total_allocations;
        return buf;
    }

    mumble_userid_t* alloc_users(std::initializer_list<mumble_userid_t> ids) {
        auto* buf = new mumble_userid_t[ids.size()];
        size_t i = 0;
        for (const mumble_userid_t id : ids) {
            buf[i++] = id;
        }
        std::lock_guard<std::mutex> lock(mutex);
        live_allocations.insert(buf);
        ++total_allocations;
        return buf;
    }

    bool free_tracked(mumble_plugin_id_t caller_id, const void* ptr) {
        if (caller_id != expected_caller_id || ptr == nullptr) {
            return false;
        }
        std::lock_guard<std::mutex> lock(mutex);
        const auto it = live_allocations.find(ptr);
        if (it == live_allocations.end()) {
            return false;
        }
        live_allocations.erase(it);
        ++total_frees;
        delete[] static_cast<const char*>(ptr);
        return true;
    }
};

MockApiTracker& tracker() {
    static MockApiTracker instance;
    return instance;
}

extern "C" {

mumble_error_t PLUGIN_CALLING_CONVENTION mock_freeMemory(mumble_plugin_id_t callerID,
                                                         const void* pointer) {
    if (tracker().free_tracked(callerID, pointer)) {
        return MUMBLE_STATUS_OK;
    }
    return MUMBLE_EC_POINTER_NOT_FOUND;
}

mumble_error_t PLUGIN_CALLING_CONVENTION mock_getLocalUserID(mumble_plugin_id_t callerID,
                                                             mumble_connection_t connection,
                                                             mumble_userid_t* userID) {
    if (callerID != tracker().expected_caller_id || connection != tracker().expected_connection ||
        userID == nullptr) {
        return MUMBLE_EC_INVALID_PLUGIN_ID;
    }
    *userID = tracker().local_user_id;
    return MUMBLE_STATUS_OK;
}

mumble_error_t PLUGIN_CALLING_CONVENTION mock_getAllUsers(mumble_plugin_id_t callerID,
                                                          mumble_connection_t connection,
                                                          mumble_userid_t** users,
                                                          size_t* userCount) {
    if (callerID != tracker().expected_caller_id || connection != tracker().expected_connection) {
        return MUMBLE_EC_INVALID_PLUGIN_ID;
    }
    if (userCount != nullptr) {
        *userCount = 2;
    }
    if (users != nullptr) {
        *users = tracker().alloc_users({101U, 202U});
    }
    return MUMBLE_STATUS_OK;
}

mumble_error_t PLUGIN_CALLING_CONVENTION mock_getUserName(mumble_plugin_id_t callerID,
                                                          mumble_connection_t connection,
                                                          mumble_userid_t userID,
                                                          const char** userName) {
    if (callerID != tracker().expected_caller_id || connection != tracker().expected_connection ||
        userName == nullptr) {
        return MUMBLE_EC_INVALID_PLUGIN_ID;
    }
    if (userID == 101U) {
        *userName = static_cast<const char*>(tracker().alloc_copy_cstr("LocalBob"));
        return MUMBLE_STATUS_OK;
    }
    if (userID == 202U) {
        *userName = static_cast<const char*>(tracker().alloc_copy_cstr("Alice"));
        return MUMBLE_STATUS_OK;
    }
    if (userID == 303U) {
        *userName = static_cast<const char*>(tracker().alloc_copy_cstr("Charlie"));
        return MUMBLE_STATUS_OK;
    }
    return MUMBLE_EC_USER_NOT_FOUND;
}

}  // extern "C"

#if !defined(_WIN32)
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

    std::vector<Message> read_messages_until_eof(std::chrono::milliseconds timeout) {
        std::vector<Message> messages;
        std::vector<uint8_t> rx_buf;
        const auto deadline = std::chrono::steady_clock::now() + timeout;

        while (std::chrono::steady_clock::now() < deadline) {
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
#endif

struct DynamicLibrary {
#if defined(_WIN32)
    HMODULE handle = nullptr;
    explicit DynamicLibrary(const char* path) : handle(LoadLibraryA(path)) {
        CHECK(handle != nullptr);
    }
    ~DynamicLibrary() {
        if (handle != nullptr) {
            FreeLibrary(handle);
        }
    }
    template <typename Fn> Fn resolve(const char* symbol) const {
        auto ptr = reinterpret_cast<Fn>(GetProcAddress(handle, symbol));
        CHECK(ptr != nullptr);
        return ptr;
    }
#else
    void* handle = nullptr;
    explicit DynamicLibrary(const char* path) : handle(::dlopen(path, RTLD_NOW)) {
        if (handle == nullptr) {
            std::cerr << "dlopen failed for " << path << ": " << ::dlerror() << '\n';
        }
        CHECK(handle != nullptr);
    }
    ~DynamicLibrary() {
        if (handle != nullptr) {
            ::dlclose(handle);
        }
    }
    template <typename Fn> Fn resolve(const char* symbol) const {
        void* raw = ::dlsym(handle, symbol);
        if (raw == nullptr) {
            std::cerr << "dlsym failed for " << symbol << ": " << ::dlerror() << '\n';
        }
        CHECK(raw != nullptr);
        return reinterpret_cast<Fn>(raw);
    }
#endif
};

}  // namespace

int main() {
    CHECK(std::strlen(KOTOHA_MUMBLE_PLUGIN_PATH) > 0U);

#if !defined(_WIN32)
    const std::string sock_path =
        "/tmp/kotoha-abi-" + std::to_string(static_cast<long>(::getpid())) + ".sock";
    auto server = std::make_unique<MockUdsServer>(sock_path);
    CHECK(::setenv("KOTOHA_IPC_ENDPOINT", sock_path.c_str(), 1) == 0);
#endif

    DynamicLibrary lib(KOTOHA_MUMBLE_PLUGIN_PATH);

    using FnInit = mumble_error_t(PLUGIN_CALLING_CONVENTION*)(mumble_plugin_id_t);
    using FnShutdown = void(PLUGIN_CALLING_CONVENTION*)();
    using FnGetName = MumbleStringWrapper(PLUGIN_CALLING_CONVENTION*)();
    using FnGetAPIVersion = mumble_version_t(PLUGIN_CALLING_CONVENTION*)();
    using FnRegisterAPI = void(PLUGIN_CALLING_CONVENTION*)(void*);
    using FnReleaseResource = void(PLUGIN_CALLING_CONVENTION*)(const void*);
    using FnGetVersion = mumble_version_t(PLUGIN_CALLING_CONVENTION*)();
    using FnGetAuthor = MumbleStringWrapper(PLUGIN_CALLING_CONVENTION*)();
    using FnGetDescription = MumbleStringWrapper(PLUGIN_CALLING_CONVENTION*)();
    using FnGetFeatures = uint32_t(PLUGIN_CALLING_CONVENTION*)();
    using FnDeactivateFeatures = uint32_t(PLUGIN_CALLING_CONVENTION*)(uint32_t);
    using FnOnServerSync = void(PLUGIN_CALLING_CONVENTION*)(mumble_connection_t);
    using FnOnServerDisc = void(PLUGIN_CALLING_CONVENTION*)(mumble_connection_t);
    using FnOnUserAdded = void(PLUGIN_CALLING_CONVENTION*)(mumble_connection_t, mumble_userid_t);
    using FnOnUserRemoved = void(PLUGIN_CALLING_CONVENTION*)(mumble_connection_t, mumble_userid_t);
    using FnOnTalkingChanged = void(PLUGIN_CALLING_CONVENTION*)(
        mumble_connection_t, mumble_userid_t, mumble_talking_state_t);
    using FnOnAudioSourceFetched = bool(PLUGIN_CALLING_CONVENTION*)(
        float*, uint32_t, uint16_t, uint32_t, bool, mumble_userid_t);

    auto fn_init = lib.resolve<FnInit>("mumble_init");
    auto fn_shutdown = lib.resolve<FnShutdown>("mumble_shutdown");
    auto fn_get_name = lib.resolve<FnGetName>("mumble_getName");
    auto fn_get_api_ver = lib.resolve<FnGetAPIVersion>("mumble_getAPIVersion");
    auto fn_register_api = lib.resolve<FnRegisterAPI>("mumble_registerAPIFunctions");
    auto fn_release_res = lib.resolve<FnReleaseResource>("mumble_releaseResource");
    auto fn_get_version = lib.resolve<FnGetVersion>("mumble_getVersion");
    auto fn_get_author = lib.resolve<FnGetAuthor>("mumble_getAuthor");
    auto fn_get_desc = lib.resolve<FnGetDescription>("mumble_getDescription");
    auto fn_get_features = lib.resolve<FnGetFeatures>("mumble_getFeatures");
    auto fn_deactivate_features = lib.resolve<FnDeactivateFeatures>("mumble_deactivateFeatures");
    auto fn_on_sync = lib.resolve<FnOnServerSync>("mumble_onServerSynchronized");
    auto fn_on_disc = lib.resolve<FnOnServerDisc>("mumble_onServerDisconnected");
    auto fn_on_user_added = lib.resolve<FnOnUserAdded>("mumble_onUserAdded");
    auto fn_on_user_removed = lib.resolve<FnOnUserRemoved>("mumble_onUserRemoved");
    auto fn_on_talking = lib.resolve<FnOnTalkingChanged>("mumble_onUserTalkingStateChanged");
    auto fn_on_audio = lib.resolve<FnOnAudioSourceFetched>("mumble_onAudioSourceFetched");

    // Pre-init metadata checks
    const MumbleStringWrapper name = fn_get_name();
    CHECK(std::string(name.data, name.size) == "Kotoha Voice Translator");
    CHECK(!name.needsReleasing);

    const mumble_version_t api_ver = fn_get_api_ver();
    CHECK(api_ver == MUMBLE_PLUGIN_API_VERSION);

    const mumble_version_t plugin_ver = fn_get_version();
    CHECK(plugin_ver.major == 0 && plugin_ver.minor == 1 && plugin_ver.patch == 0);

    const MumbleStringWrapper author = fn_get_author();
    CHECK(std::string(author.data, author.size) == "4moda");
    CHECK(!author.needsReleasing);

    const MumbleStringWrapper desc = fn_get_desc();
    CHECK(desc.size > 0 && !desc.needsReleasing);

    CHECK(fn_get_features() == MUMBLE_FEATURE_NONE);
    CHECK(fn_deactivate_features(MUMBLE_FEATURE_AUDIO) == MUMBLE_FEATURE_AUDIO);
    fn_release_res(nullptr);
    fn_release_res(name.data);

    MumbleAPI_v_1_0_x mock_api{};
    mock_api.freeMemory = &mock_freeMemory;
    mock_api.getLocalUserID = &mock_getLocalUserID;
    mock_api.getAllUsers = &mock_getAllUsers;
    mock_api.getUserName = &mock_getUserName;

    fn_register_api(&mock_api);
    CHECK(fn_init(tracker().expected_caller_id) == MUMBLE_STATUS_OK);

#if !defined(_WIN32)
    CHECK(server->accept_one(1500));
#endif

    // Trigger synchronized -> fetches local user ID + all users (101, 202)
    fn_on_sync(tracker().expected_connection);

    // User 303 added later
    fn_on_user_added(tracker().expected_connection, 303U);

    // TalkingState: invalid (-1) and out-of-range (99) must be filtered out; Talking must pass
    fn_on_talking(tracker().expected_connection, 202U, MUMBLE_TS_INVALID);
    const int raw_out_of_range = 99;
    mumble_talking_state_t out_of_range_state = MUMBLE_TS_INVALID;
    std::memcpy(&out_of_range_state, &raw_out_of_range, sizeof(out_of_range_state));
    fn_on_talking(tracker().expected_connection, 202U, out_of_range_state);
    fn_on_talking(tracker().expected_connection, 202U, MUMBLE_TS_TALKING);

    // Audio: isSpeech = false must be ignored; isSpeech = true must forward untouched & return
    // false
    std::array<float, 8> silence_pcm = {0.9f, -0.9f, 0.8f, -0.8f, 0.7f, -0.7f, 0.6f, -0.6f};
    const auto silence_copy = silence_pcm;
    CHECK(!fn_on_audio(silence_pcm.data(), 4U, 2U, 48000U, false, 202U));
    CHECK(silence_pcm == silence_copy);

    std::array<float, 8> speech_pcm = {0.1f, -0.1f, 0.2f, -0.2f, 0.3f, -0.3f, 0.4f, -0.4f};
    const auto speech_copy = speech_pcm;
    CHECK(!fn_on_audio(speech_pcm.data(), 4U, 2U, 48000U, true, 202U));
    CHECK(speech_pcm == speech_copy);

    // Local user audio should carry IS_LOCAL_USER flag
    std::array<float, 4> local_pcm = {0.5f, -0.5f, 0.25f, -0.25f};
    CHECK(!fn_on_audio(local_pcm.data(), 2U, 2U, 48000U, true, 101U));

    // User removed & server disconnected
    fn_on_user_removed(tracker().expected_connection, 202U);
    fn_on_disc(tracker().expected_connection);

    // Clean shutdown
    fn_shutdown();

    // Verify zero memory leaks from Mumble API allocations
    {
        std::lock_guard<std::mutex> lock(tracker().mutex);
        CHECK(tracker().total_allocations == 4U);  // 1 getAllUsers array + 3 getUserName strings
        CHECK(tracker().total_frees == 4U);
        CHECK(tracker().live_allocations.empty());
    }

#if !defined(_WIN32)
    const auto msgs = server->read_messages_until_eof(std::chrono::milliseconds{1500});
    CHECK(!msgs.empty());

    // First message MUST be Hello("mumble", "0.1.0")
    const auto* hello = std::get_if<HelloMessage>(&msgs[0]);
    CHECK(hello != nullptr);
    CHECK(hello->app_name == "mumble");
    CHECK(hello->plugin_version == "0.1.0");

    size_t user_info_count = 0;
    size_t talking_state_count = 0;
    size_t audio_frame_count = 0;
    size_t user_removed_count = 0;
    size_t source_reset_count = 0;
    bool saw_local_bob = false;
    bool saw_alice = false;
    bool saw_charlie = false;
    bool saw_remote_audio = false;
    bool saw_local_audio = false;

    for (size_t i = 1; i < msgs.size(); ++i) {
        const auto& m = msgs[i];
        if (const auto* ui = std::get_if<UserInfoMessage>(&m)) {
            ++user_info_count;
            if (ui->user_id == 101U && ui->name == "LocalBob") {
                saw_local_bob = true;
            } else if (ui->user_id == 202U && ui->name == "Alice") {
                saw_alice = true;
            } else if (ui->user_id == 303U && ui->name == "Charlie") {
                saw_charlie = true;
            }
        } else if (const auto* ts = std::get_if<TalkingStateMessage>(&m)) {
            ++talking_state_count;
            CHECK(ts->user_id == 202U);
            CHECK(ts->state == TalkingStateValue::Talking);
        } else if (const auto* af = std::get_if<AudioFrameMessage>(&m)) {
            ++audio_frame_count;
            if (af->user_id == 202U) {
                saw_remote_audio = true;
                CHECK(af->sample_rate == 48000U);
                CHECK(af->channels == 2U);
                CHECK(af->flags == 0U);
                CHECK(af->pcm_samples.size() == 8U);
            } else if (af->user_id == 101U) {
                saw_local_audio = true;
                CHECK(af->flags == kAudioFlagIsLocalUser);
                CHECK(af->pcm_samples.size() == 4U);
            }
        } else if (const auto* ur = std::get_if<UserRemovedMessage>(&m)) {
            ++user_removed_count;
            CHECK(ur->user_id == 202U);
        } else if (std::holds_alternative<SourceResetMessage>(m)) {
            ++source_reset_count;
        }
    }

    CHECK(user_info_count == 3U);
    CHECK(saw_local_bob && saw_alice && saw_charlie);
    CHECK(talking_state_count == 1U);  // Invalid & out-of-range states were filtered
    CHECK(audio_frame_count == 2U);    // isSpeech=false frame was ignored
    CHECK(saw_remote_audio && saw_local_audio);
    CHECK(user_removed_count == 1U);
    CHECK(source_reset_count >= 1U);

    ::unsetenv("KOTOHA_IPC_ENDPOINT");
#endif

    std::cout << "test_mumble_plugin_abi: all assertions passed\n";
    return 0;
}
