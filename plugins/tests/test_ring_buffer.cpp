#include "kotoha/ring_buffer.hpp"

#include <atomic>
#include <cstdlib>
#include <iostream>
#include <thread>
#include <vector>

namespace {

using namespace kotoha::ipc;

void check_impl(bool cond, const char* expr, int line) {
    if (!cond) {
        std::cerr << "FAIL at line " << line << ": " << expr << '\n';
        std::exit(1);
    }
}

#define CHECK(expr) check_impl(static_cast<bool>(expr), #expr, __LINE__)

void test_drop_new_and_seq_gaps() {
    SpscAudioRingBuffer<4, 64> ring;
    std::vector<float> samples = {0.1f, -0.1f, 0.2f, -0.2f};

    // Push 4 chunks -> full
    for (uint32_t i = 0; i < 4; ++i) {
        CHECK(ring.try_push_chunk(100 + i, 48000, 2, 0, samples));
    }
    CHECK(ring.size_approx() == 4);
    CHECK(ring.current_seq() == 4);
    CHECK(ring.dropped_count() == 0);

    // Push 3 more chunks while full -> all dropped (Drop-New), seq advances to 7
    for (uint32_t i = 0; i < 3; ++i) {
        CHECK(!ring.try_push_chunk(999, 48000, 2, 0, samples));
    }
    CHECK(ring.dropped_count() == 3);
    CHECK(ring.current_seq() == 7);

    // Pop the 4 original chunks -> must preserve oldest 4 (Drop-New never overwrites unread)
    AudioSlot<64> out;
    for (uint32_t i = 0; i < 4; ++i) {
        CHECK(ring.try_pop(out));
        CHECK(out.seq == i);
        CHECK(out.user_id == 100 + i);
    }

    // Next push receives seq == 7 -> gap from 3 to 7 reveals 3 dropped frames
    CHECK(ring.try_push_chunk(200, 48000, 2, kAudioFlagIsLocalUser, samples));
    CHECK(ring.try_pop(out));
    CHECK(out.seq == 7);
    CHECK(out.user_id == 200);
    CHECK(out.flags == kAudioFlagIsLocalUser);
}

void test_chunk_splitting_and_channel_alignment() {
    SpscAudioRingBuffer<8, 128> ring;

    // For 6 channels and MaxSlotSamples=128: max_chunk = (128 / 6) * 6 = 126
    CHECK(ring.max_chunk_samples_for_channels(6) == 126);

    // Push 300 floats (50 6ch frames) -> should split into 126 + 126 + 48
    std::vector<float> pcm(300);
    for (size_t i = 0; i < pcm.size(); ++i) {
        pcm[i] = static_cast<float>(i % 100) * 0.01f;
    }

    const size_t pushed = ring.push_split(7, 48000, 6, 0, pcm);
    CHECK(pushed == 3);

    AudioSlot<128> slot;
    std::vector<float> reconstructed;
    std::vector<uint32_t> counts;
    while (ring.try_pop(slot)) {
        CHECK(slot.channels == 6);
        CHECK(slot.sample_count % 6 == 0);
        counts.push_back(slot.sample_count);
        for (uint32_t i = 0; i < slot.sample_count; ++i) {
            reconstructed.push_back(slot.samples[i]);
        }
    }

    CHECK((counts == std::vector<uint32_t>{126U, 126U, 48U}));
    CHECK(reconstructed == pcm);

    // Unaligned sample count must be rejected cleanly
    std::vector<float> unaligned(5, 0.0f);
    CHECK(ring.push_split(7, 48000, 2, 0, unaligned) == 0);
}

void test_spsc_concurrent_stress() {
    constexpr size_t kTotalAttempts = 20000;
    SpscAudioRingBuffer<64, 32> ring;
    std::atomic<bool> producer_done{false};
    std::atomic<uint64_t> consumed_count{0};
    std::atomic<bool> seq_monotonic_ok{true};
    std::atomic<bool> payload_integrity_ok{true};

    std::thread consumer([&]() {
        AudioSlot<32> slot;
        bool has_prev = false;
        uint32_t prev_seq = 0;

        auto process_slot = [&](const AudioSlot<32>& s) {
            if (has_prev) {
                const uint32_t delta = s.seq - prev_seq;
                if (delta == 0 || delta >= 0x80000000U) {
                    seq_monotonic_ok.store(false, std::memory_order_relaxed);
                }
            }
            has_prev = true;
            prev_seq = s.seq;

            const float expected_marker = static_cast<float>((s.seq % 1000U)) * 0.001f;
            if (s.sample_count != 8U || s.samples[0] != expected_marker ||
                s.samples[7] != -expected_marker) {
                payload_integrity_ok.store(false, std::memory_order_relaxed);
            }
            consumed_count.fetch_add(1U, std::memory_order_relaxed);
        };

        while (!producer_done.load(std::memory_order_acquire)) {
            while (ring.try_pop(slot)) {
                process_slot(slot);
            }
            std::this_thread::yield();
        }
        while (ring.try_pop(slot)) {
            process_slot(slot);
        }
    });

    std::thread producer([&]() {
        std::vector<float> buf(8, 0.0f);
        for (size_t i = 0; i < kTotalAttempts; ++i) {
            const float marker = static_cast<float>(i % 1000U) * 0.001f;
            buf[0] = marker;
            buf[7] = -marker;
            ring.try_push_chunk(1U, 48000U, 2U, 0U, buf);
        }
        producer_done.store(true, std::memory_order_release);
    });

    producer.join();
    consumer.join();

    CHECK(seq_monotonic_ok.load());
    CHECK(payload_integrity_ok.load());
    CHECK(ring.current_seq() == kTotalAttempts);
    CHECK(consumed_count.load() + ring.dropped_count() == kTotalAttempts);
}

}  // namespace

int main() {
    test_drop_new_and_seq_gaps();
    test_chunk_splitting_and_channel_alignment();
    test_spsc_concurrent_stress();
    std::cout << "test_ring_buffer: all assertions passed\n";
    return 0;
}
