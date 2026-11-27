#pragma once

#include "kotoha/protocol.hpp"

#include <algorithm>
#include <array>
#include <atomic>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <new>
#include <span>
#include <vector>

namespace kotoha::ipc {

inline constexpr size_t kDefaultMaxAudioSlotSamples = 4096;
inline constexpr size_t kDefaultAudioRingCapacity = 128;

inline constexpr size_t kCacheLineSize = 64;

template <size_t MaxSlotSamples = kDefaultMaxAudioSlotSamples> struct AudioSlot {
    static_assert(MaxSlotSamples > 0, "MaxSlotSamples must be positive");
    static_assert(MaxSlotSamples <=
                      (static_cast<size_t>(kMaxPayloadLen) - kAudioFrameHeaderSize) / sizeof(float),
                  "MaxSlotSamples must fit inside a single 64KB AudioFrame payload");

    uint32_t user_id = 0;
    uint32_t seq = 0;
    uint32_t sample_rate = 48000;
    uint16_t channels = 1;
    uint16_t flags = 0;
    uint32_t sample_count = 0;
    std::array<float, MaxSlotSamples> samples{};
};

#if defined(_MSC_VER)
#pragma warning(push)
#pragma warning(disable : 4324)  // Intentional cache-line alignment padding for lock-free SPSC
#endif

template <size_t Capacity = kDefaultAudioRingCapacity,
          size_t MaxSlotSamples = kDefaultMaxAudioSlotSamples>
class SpscAudioRingBuffer {
public:
    static_assert(Capacity >= 2, "Ring buffer capacity must be >= 2");
    static_assert((Capacity & (Capacity - 1)) == 0, "Capacity must be a power of two");

    using Slot = AudioSlot<MaxSlotSamples>;

    SpscAudioRingBuffer() : slots_(Capacity) {}

    SpscAudioRingBuffer(const SpscAudioRingBuffer&) = delete;
    SpscAudioRingBuffer& operator=(const SpscAudioRingBuffer&) = delete;
    SpscAudioRingBuffer(SpscAudioRingBuffer&&) = delete;
    SpscAudioRingBuffer& operator=(SpscAudioRingBuffer&&) = delete;
    ~SpscAudioRingBuffer() = default;

    [[nodiscard]] static constexpr size_t capacity() noexcept { return Capacity; }

    [[nodiscard]] static constexpr size_t max_slot_samples() noexcept { return MaxSlotSamples; }

    [[nodiscard]] static constexpr size_t
    max_chunk_samples_for_channels(uint16_t channels) noexcept {
        const size_t protocol_max = max_pcm_samples_for_channels(channels);
        if (protocol_max == 0) {
            return 0;
        }
        const size_t raw_limit = std::min(MaxSlotSamples, protocol_max);
        return (raw_limit / static_cast<size_t>(channels)) * static_cast<size_t>(channels);
    }

    /// Wait-free, lock-free, zero-allocation single-chunk push.
    /// Always consumes one sequence number (even when dropped on full) so the
    /// receiver detects the exact gap. Overflow policy is strictly Drop-New.
    bool try_push_chunk(uint32_t user_id, uint32_t sample_rate, uint16_t channels, uint16_t flags,
                        std::span<const float> samples) noexcept {
        if (channels < kMinChannels || channels > kMaxChannels) {
            return false;
        }
        if ((samples.size() % static_cast<size_t>(channels)) != 0U) {
            return false;
        }
        if (samples.size() > max_chunk_samples_for_channels(channels)) {
            return false;
        }

        const uint32_t seq = next_seq_.fetch_add(1U, std::memory_order_relaxed);
        const size_t write_idx = write_idx_.load(std::memory_order_relaxed);
        const size_t read_idx = read_idx_.load(std::memory_order_acquire);

        if ((write_idx - read_idx) >= Capacity) {
            dropped_count_.fetch_add(1U, std::memory_order_relaxed);
            return false;
        }

        Slot& slot = slots_[write_idx & kIndexMask];
        slot.user_id = user_id;
        slot.seq = seq;
        slot.sample_rate = sample_rate;
        slot.channels = channels;
        slot.flags = flags;
        slot.sample_count = static_cast<uint32_t>(samples.size());
        if (!samples.empty()) {
            std::memcpy(slot.samples.data(), samples.data(), samples.size() * sizeof(float));
        }

        write_idx_.store(write_idx + 1U, std::memory_order_release);
        return true;
    }

    /// Wait-free, lock-free, zero-allocation audio push that splits oversized
    /// buffers into channel-aligned chunks <= min(MaxSlotSamples, max_pcm_samples_for_channels).
    /// Returns the number of chunks successfully pushed.
    size_t push_split(uint32_t user_id, uint32_t sample_rate, uint16_t channels, uint16_t flags,
                      std::span<const float> samples) noexcept {
        if (channels < kMinChannels || channels > kMaxChannels) {
            return 0;
        }
        if ((samples.size() % static_cast<size_t>(channels)) != 0U) {
            return 0;
        }
        const size_t max_chunk = max_chunk_samples_for_channels(channels);
        if (max_chunk == 0) {
            return 0;
        }

        if (samples.empty()) {
            return try_push_chunk(user_id, sample_rate, channels, flags, samples) ? 1U : 0U;
        }

        size_t pushed = 0;
        size_t offset = 0;
        while (offset < samples.size()) {
            const size_t remaining = samples.size() - offset;
            const size_t chunk_len = std::min(remaining, max_chunk);
            if (try_push_chunk(user_id, sample_rate, channels, flags,
                               samples.subspan(offset, chunk_len))) {
                ++pushed;
            }
            offset += chunk_len;
        }
        return pushed;
    }

    /// Wait-free, lock-free single-consumer pop into caller-owned slot.
    bool try_pop(Slot& out) noexcept {
        const size_t read_idx = read_idx_.load(std::memory_order_relaxed);
        const size_t write_idx = write_idx_.load(std::memory_order_acquire);

        if (read_idx == write_idx) {
            return false;
        }

        const Slot& slot = slots_[read_idx & kIndexMask];
        out.user_id = slot.user_id;
        out.seq = slot.seq;
        out.sample_rate = slot.sample_rate;
        out.channels = slot.channels;
        out.flags = slot.flags;
        out.sample_count = slot.sample_count;
        if (slot.sample_count > 0U) {
            std::memcpy(out.samples.data(), slot.samples.data(),
                        static_cast<size_t>(slot.sample_count) * sizeof(float));
        }

        read_idx_.store(read_idx + 1U, std::memory_order_release);
        return true;
    }

    /// Wait-free single-consumer pop invoking a visitor on a const Slot& view
    /// without copying unused tail samples out of the slot.
    template <typename Callback>
    bool
    try_consume(Callback&& callback) noexcept(noexcept(callback(std::declval<const Slot&>()))) {
        const size_t read_idx = read_idx_.load(std::memory_order_relaxed);
        const size_t write_idx = write_idx_.load(std::memory_order_acquire);

        if (read_idx == write_idx) {
            return false;
        }

        const Slot& slot = slots_[read_idx & kIndexMask];
        callback(slot);
        read_idx_.store(read_idx + 1U, std::memory_order_release);
        return true;
    }

    [[nodiscard]] bool empty() const noexcept {
        const size_t read_idx = read_idx_.load(std::memory_order_acquire);
        const size_t write_idx = write_idx_.load(std::memory_order_acquire);
        return read_idx == write_idx;
    }

    [[nodiscard]] size_t size_approx() const noexcept {
        const size_t write_idx = write_idx_.load(std::memory_order_acquire);
        const size_t read_idx = read_idx_.load(std::memory_order_acquire);
        return write_idx - read_idx;
    }

    [[nodiscard]] uint64_t dropped_count() const noexcept {
        return dropped_count_.load(std::memory_order_relaxed);
    }

    [[nodiscard]] uint32_t current_seq() const noexcept {
        return next_seq_.load(std::memory_order_relaxed);
    }

private:
    static constexpr size_t kIndexMask = Capacity - 1;

    // Heap-allocated once at construction; never resized or freed on hot path.
    std::vector<Slot> slots_;

    alignas(kCacheLineSize) std::atomic<size_t> write_idx_{0};
    alignas(kCacheLineSize) std::atomic<size_t> read_idx_{0};
    alignas(kCacheLineSize) std::atomic<uint32_t> next_seq_{0};
    alignas(kCacheLineSize) std::atomic<uint64_t> dropped_count_{0};
};

#if defined(_MSC_VER)
#pragma warning(pop)
#endif

}  // namespace kotoha::ipc
