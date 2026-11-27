#include "MumbleAPI_v_1_0_x.h"
#include "MumblePlugin_v_1_0_x.h"
#include "PluginComponents_v_1_0_x.h"
#include "kotoha/ipc_client.hpp"
#include "kotoha/protocol.hpp"

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <limits>
#include <memory>
#include <mutex>
#include <span>
#include <string>

#ifndef KOTOHA_PLUGIN_VERSION
#define KOTOHA_PLUGIN_VERSION "0.1.0"
#endif

#ifndef KOTOHA_PLUGIN_VERSION_MAJOR
#define KOTOHA_PLUGIN_VERSION_MAJOR 0
#endif

#ifndef KOTOHA_PLUGIN_VERSION_MINOR
#define KOTOHA_PLUGIN_VERSION_MINOR 1
#endif

#ifndef KOTOHA_PLUGIN_VERSION_PATCH
#define KOTOHA_PLUGIN_VERSION_PATCH 0
#endif

namespace {

constexpr const char* kPluginName = "Kotoha Voice Translator";
constexpr const char* kPluginAuthor = "4moda";
constexpr const char* kPluginDescription =
    "Real-time voice chat subtitle & translation audio forwarder for Kotoha.";

struct PluginState {
    std::mutex lifecycle_mutex;
    mumble_plugin_id_t own_id = 0;
    MumbleAPI_v_1_0_x api{};
    bool has_api = false;
    bool initialized = false;

    std::atomic<uint32_t> local_user_id{0};
    std::atomic<bool> has_local_user_id{false};

    // Stored under lifecycle_mutex; published into atomic raw pointer for
    // wait-free, zero-lock access inside mumble_onAudioSourceFetched.
    std::unique_ptr<kotoha::ipc::IpcClient> ipc_client_owner;
    std::atomic<kotoha::ipc::IpcClient*> ipc_client{nullptr};
};

PluginState& state() {
    static PluginState instance;
    return instance;
}

[[nodiscard]] MumbleStringWrapper make_static_string(const char* text) noexcept {
    return MumbleStringWrapper{text, std::strlen(text), false};
}

[[nodiscard]] bool map_talking_state(mumble_talking_state_t raw,
                                     kotoha::ipc::TalkingStateValue& out) noexcept {
    switch (raw) {
        case MUMBLE_TS_PASSIVE:
            out = kotoha::ipc::TalkingStateValue::Passive;
            return true;
        case MUMBLE_TS_TALKING:
            out = kotoha::ipc::TalkingStateValue::Talking;
            return true;
        case MUMBLE_TS_WHISPERING:
            out = kotoha::ipc::TalkingStateValue::Whispering;
            return true;
        case MUMBLE_TS_SHOUTING:
            out = kotoha::ipc::TalkingStateValue::Shouting;
            return true;
        case MUMBLE_TS_TALKING_MUTED:
            out = kotoha::ipc::TalkingStateValue::TalkingMuted;
            return true;
        case MUMBLE_TS_INVALID:
        default:
            return false;
    }
}

void fetch_and_push_user_info(const MumbleAPI_v_1_0_x& api, mumble_plugin_id_t own_id,
                              mumble_connection_t connection, mumble_userid_t user_id,
                              kotoha::ipc::IpcClient* client) {
    if (client == nullptr || api.getUserName == nullptr) {
        return;
    }

    const char* user_name = nullptr;
    const mumble_error_t err = api.getUserName(own_id, connection, user_id, &user_name);
    if (err == MUMBLE_STATUS_OK && user_name != nullptr) {
        client->push_user_info(static_cast<uint32_t>(user_id), std::string(user_name));
        if (api.freeMemory != nullptr) {
            api.freeMemory(own_id, user_name);
        }
    }
}

}  // namespace

extern "C" {

PLUGIN_EXPORT mumble_error_t PLUGIN_CALLING_CONVENTION mumble_init(mumble_plugin_id_t id) {
    PluginState& st = state();
    std::lock_guard<std::mutex> lock(st.lifecycle_mutex);

    if (st.initialized) {
        return MUMBLE_STATUS_OK;
    }

    st.own_id = id;
    st.has_local_user_id.store(false, std::memory_order_relaxed);
    st.local_user_id.store(0, std::memory_order_relaxed);

    kotoha::ipc::IpcClientConfig cfg;
    cfg.app_name = "mumble";
    cfg.plugin_version = KOTOHA_PLUGIN_VERSION;

    auto client = std::make_unique<kotoha::ipc::IpcClient>(std::move(cfg));
    client->start();
    st.ipc_client.store(client.get(), std::memory_order_release);
    st.ipc_client_owner = std::move(client);
    st.initialized = true;

    return MUMBLE_STATUS_OK;
}

PLUGIN_EXPORT void PLUGIN_CALLING_CONVENTION mumble_shutdown() {
    PluginState& st = state();
    std::unique_ptr<kotoha::ipc::IpcClient> client_to_stop;

    {
        std::lock_guard<std::mutex> lock(st.lifecycle_mutex);
        if (!st.initialized) {
            return;
        }
        st.initialized = false;
        st.has_local_user_id.store(false, std::memory_order_relaxed);
        st.local_user_id.store(0, std::memory_order_relaxed);
        st.ipc_client.store(nullptr, std::memory_order_release);
        client_to_stop = std::move(st.ipc_client_owner);
    }

    if (client_to_stop != nullptr) {
        client_to_stop->stop();
    }
}

PLUGIN_EXPORT struct MumbleStringWrapper PLUGIN_CALLING_CONVENTION mumble_getName() {
    return make_static_string(kPluginName);
}

PLUGIN_EXPORT mumble_version_t PLUGIN_CALLING_CONVENTION mumble_getAPIVersion() {
    return MUMBLE_PLUGIN_API_VERSION;
}

PLUGIN_EXPORT void PLUGIN_CALLING_CONVENTION mumble_registerAPIFunctions(void* apiStruct) {
    if (apiStruct == nullptr) {
        return;
    }
    PluginState& st = state();
    std::lock_guard<std::mutex> lock(st.lifecycle_mutex);
    st.api = MUMBLE_API_CAST(apiStruct);
    st.has_api = true;
}

PLUGIN_EXPORT void PLUGIN_CALLING_CONVENTION mumble_releaseResource(const void* pointer) {
    (void)pointer;
}

PLUGIN_EXPORT mumble_version_t PLUGIN_CALLING_CONVENTION mumble_getVersion() {
    return mumble_version_t{
        KOTOHA_PLUGIN_VERSION_MAJOR,
        KOTOHA_PLUGIN_VERSION_MINOR,
        KOTOHA_PLUGIN_VERSION_PATCH,
    };
}

PLUGIN_EXPORT struct MumbleStringWrapper PLUGIN_CALLING_CONVENTION mumble_getAuthor() {
    return make_static_string(kPluginAuthor);
}

PLUGIN_EXPORT struct MumbleStringWrapper PLUGIN_CALLING_CONVENTION mumble_getDescription() {
    return make_static_string(kPluginDescription);
}

PLUGIN_EXPORT uint32_t PLUGIN_CALLING_CONVENTION mumble_getFeatures() {
    return MUMBLE_FEATURE_NONE;
}

PLUGIN_EXPORT uint32_t PLUGIN_CALLING_CONVENTION mumble_deactivateFeatures(uint32_t features) {
    return features;
}

PLUGIN_EXPORT void PLUGIN_CALLING_CONVENTION
mumble_onServerSynchronized(mumble_connection_t connection) {
    PluginState& st = state();
    MumbleAPI_v_1_0_x api{};
    mumble_plugin_id_t own_id = 0;
    kotoha::ipc::IpcClient* client = nullptr;

    {
        std::lock_guard<std::mutex> lock(st.lifecycle_mutex);
        if (!st.initialized || !st.has_api) {
            return;
        }
        api = st.api;
        own_id = st.own_id;
        client = st.ipc_client.load(std::memory_order_acquire);
    }

    if (client == nullptr) {
        return;
    }

    if (api.getLocalUserID != nullptr) {
        mumble_userid_t local_id = 0;
        if (api.getLocalUserID(own_id, connection, &local_id) == MUMBLE_STATUS_OK) {
            st.local_user_id.store(static_cast<uint32_t>(local_id), std::memory_order_relaxed);
            st.has_local_user_id.store(true, std::memory_order_relaxed);
        }
    }

    if (api.getAllUsers != nullptr) {
        mumble_userid_t* users = nullptr;
        size_t user_count = 0;
        if (api.getAllUsers(own_id, connection, &users, &user_count) == MUMBLE_STATUS_OK) {
            if (users != nullptr) {
                for (size_t i = 0; i < user_count; ++i) {
                    fetch_and_push_user_info(api, own_id, connection, users[i], client);
                }
                if (api.freeMemory != nullptr) {
                    api.freeMemory(own_id, users);
                }
            }
        }
    }
}

PLUGIN_EXPORT void PLUGIN_CALLING_CONVENTION
mumble_onServerDisconnected(mumble_connection_t connection) {
    (void)connection;
    PluginState& st = state();
    st.has_local_user_id.store(false, std::memory_order_relaxed);
    st.local_user_id.store(0, std::memory_order_relaxed);

    kotoha::ipc::IpcClient* client = st.ipc_client.load(std::memory_order_acquire);
    if (client != nullptr) {
        client->push_source_reset();
    }
}

PLUGIN_EXPORT void PLUGIN_CALLING_CONVENTION mumble_onUserAdded(mumble_connection_t connection,
                                                                mumble_userid_t userID) {
    PluginState& st = state();
    MumbleAPI_v_1_0_x api{};
    mumble_plugin_id_t own_id = 0;
    kotoha::ipc::IpcClient* client = nullptr;

    {
        std::lock_guard<std::mutex> lock(st.lifecycle_mutex);
        if (!st.initialized || !st.has_api) {
            return;
        }
        api = st.api;
        own_id = st.own_id;
        client = st.ipc_client.load(std::memory_order_acquire);
    }

    fetch_and_push_user_info(api, own_id, connection, userID, client);
}

PLUGIN_EXPORT void PLUGIN_CALLING_CONVENTION mumble_onUserRemoved(mumble_connection_t connection,
                                                                  mumble_userid_t userID) {
    (void)connection;
    PluginState& st = state();
    kotoha::ipc::IpcClient* client = st.ipc_client.load(std::memory_order_acquire);
    if (client != nullptr) {
        client->push_user_removed(static_cast<uint32_t>(userID));
    }
}

PLUGIN_EXPORT void PLUGIN_CALLING_CONVENTION mumble_onUserTalkingStateChanged(
    mumble_connection_t connection, mumble_userid_t userID, mumble_talking_state_t talkingState) {
    (void)connection;
    kotoha::ipc::TalkingStateValue mapped = kotoha::ipc::TalkingStateValue::Passive;
    if (!map_talking_state(talkingState, mapped)) {
        return;
    }

    PluginState& st = state();
    kotoha::ipc::IpcClient* client = st.ipc_client.load(std::memory_order_acquire);
    if (client != nullptr) {
        client->push_talking_state(static_cast<uint32_t>(userID), mapped);
    }
}

PLUGIN_EXPORT bool PLUGIN_CALLING_CONVENTION
mumble_onAudioSourceFetched(float* outputPCM, uint32_t sampleCount, uint16_t channelCount,
                            uint32_t sampleRate, bool isSpeech, mumble_userid_t userID) {
    // STRICT REAL-TIME INVARIANT:
    // No locks, no heap allocations, no syscalls, no Mumble API calls.
    if (!isSpeech || outputPCM == nullptr || sampleCount == 0U ||
        channelCount < kotoha::ipc::kMinChannels || channelCount > kotoha::ipc::kMaxChannels) {
        return false;
    }

    PluginState& st = state();
    kotoha::ipc::IpcClient* client = st.ipc_client.load(std::memory_order_acquire);
    if (client == nullptr) {
        return false;
    }

    if (static_cast<size_t>(sampleCount) >
        (std::numeric_limits<size_t>::max() / static_cast<size_t>(channelCount))) {
        return false;
    }
    const size_t total_samples =
        static_cast<size_t>(sampleCount) * static_cast<size_t>(channelCount);
    if (total_samples == 0U) {
        return false;
    }

    uint16_t flags = 0U;
    if (st.has_local_user_id.load(std::memory_order_relaxed) &&
        st.local_user_id.load(std::memory_order_relaxed) == static_cast<uint32_t>(userID)) {
        flags |= kotoha::ipc::kAudioFlagIsLocalUser;
    }

    (void)client->push_audio(static_cast<uint32_t>(userID), sampleRate, channelCount, flags,
                             std::span<const float>(outputPCM, total_samples));

    return false;
}

}  // extern "C"
