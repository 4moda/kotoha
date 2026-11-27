#include "kotoha/protocol.hpp"

#include <array>
#include <cmath>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <limits>
#include <string>
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

void test_max_pcm_samples_for_channels() {
    CHECK(max_pcm_samples_for_channels(0) == 0);
    CHECK(max_pcm_samples_for_channels(9) == 0);
    CHECK(max_pcm_samples_for_channels(1) == 16380);
    CHECK(max_pcm_samples_for_channels(2) == 16380);
    CHECK(max_pcm_samples_for_channels(3) == 16380);
    CHECK(max_pcm_samples_for_channels(4) == 16380);
    CHECK(max_pcm_samples_for_channels(5) == 16380);
    CHECK(max_pcm_samples_for_channels(6) == 16380);
    CHECK(max_pcm_samples_for_channels(7) == 16380);
    CHECK(max_pcm_samples_for_channels(8) == 16376);
    for (uint16_t ch = 1; ch <= 8; ++ch) {
        const size_t max_s = max_pcm_samples_for_channels(ch);
        CHECK(max_s % ch == 0);
        CHECK(16 + (max_s * sizeof(float)) <= kMaxPayloadLen);
    }
}

void test_golden_bytes_and_roundtrips() {
    // 1. Hello golden byte vector
    {
        const EncodeResult enc = encode_hello_frame(HelloView{"mumble", "0.1.0"});
        CHECK(enc.error == ProtocolError::Ok);
        const std::vector<uint8_t> expected = {
            0x56, 0x54, 0x01, 0x00, 0x0F, 0x00, 0x00, 0x00,  // Header: VT, v1, Hello, len=15
            0x06, 0x00, 'm',  'u',  'm',  'b',  'l',  'e',   // app_name_len=6 + "mumble"
            0x05, 0x00, '0',  '.',  '1',  '.',  '0',         // ver_len=5 + "0.1.0"
        };
        CHECK(enc.bytes == expected);

        const DecodeResult dec = decode_frame(enc.bytes);
        CHECK(dec.error == ProtocolError::Ok);
        CHECK(dec.bytes_consumed == expected.size());
        const auto* hello = std::get_if<HelloMessage>(&dec.message);
        CHECK(hello != nullptr);
        CHECK(hello->app_name == "mumble");
        CHECK(hello->plugin_version == "0.1.0");
    }

    // Empty plugin_version allowed (0..=64)
    {
        const EncodeResult enc = encode_hello_frame(HelloView{"ts3", ""});
        CHECK(enc.error == ProtocolError::Ok);
        CHECK(enc.bytes.size() == kHeaderSize + 7);
        const DecodeResult dec = decode_frame(enc.bytes);
        CHECK(dec.error == ProtocolError::Ok);
        const auto* hello = std::get_if<HelloMessage>(&dec.message);
        CHECK(hello != nullptr);
        CHECK(hello->app_name == "ts3");
        CHECK(hello->plugin_version.empty());
    }

    // 2. AudioFrame golden bytes & sanitization
    {
        const std::array<float, 4> dirty_samples = {
            0.5f,
            -2.5f,
            std::numeric_limits<float>::quiet_NaN(),
            std::numeric_limits<float>::infinity(),
        };
        const EncodeResult enc = encode_audio_frame(AudioFrameView{
            0x12345678U,
            0x0000002AU,
            48000U,
            2U,
            kAudioFlagIsLocalUser,
            dirty_samples,
        });
        CHECK(enc.error == ProtocolError::Ok);
        CHECK(enc.bytes.size() == kHeaderSize + 16 + 16);

        // Header checks
        CHECK(enc.bytes[0] == 0x56 && enc.bytes[1] == 0x54);
        CHECK(enc.bytes[2] == 0x01 && enc.bytes[3] == 0x01);
        CHECK(enc.bytes[4] == 32 && enc.bytes[5] == 0 && enc.bytes[6] == 0 && enc.bytes[7] == 0);
        // user_id LE = 0x12345678
        CHECK(enc.bytes[8] == 0x78 && enc.bytes[9] == 0x56 && enc.bytes[10] == 0x34 &&
              enc.bytes[11] == 0x12);
        // seq LE = 42
        CHECK(enc.bytes[12] == 0x2A && enc.bytes[13] == 0x00);
        // sample_rate LE = 48000 (0x0000BB80)
        CHECK(enc.bytes[16] == 0x80 && enc.bytes[17] == 0xBB && enc.bytes[18] == 0x00 &&
              enc.bytes[19] == 0x00);
        // channels = 2, flags = 1
        CHECK(enc.bytes[20] == 0x02 && enc.bytes[21] == 0x00);
        CHECK(enc.bytes[22] == 0x01 && enc.bytes[23] == 0x00);

        const DecodeResult dec = decode_frame(enc.bytes);
        CHECK(dec.error == ProtocolError::Ok);
        const auto* frame = std::get_if<AudioFrameMessage>(&dec.message);
        CHECK(frame != nullptr);
        CHECK(frame->user_id == 0x12345678U);
        CHECK(frame->seq == 42U);
        CHECK(frame->sample_rate == 48000U);
        CHECK(frame->channels == 2U);
        CHECK(frame->flags == kAudioFlagIsLocalUser);
        CHECK(frame->pcm_samples.size() == 4U);
        CHECK(frame->pcm_samples[0] == 0.5f);
        CHECK(frame->pcm_samples[1] == -1.0f);
        CHECK(frame->pcm_samples[2] == 0.0f);
        CHECK(frame->pcm_samples[3] == 0.0f);
    }

    // 2b. AudioFrame 8-channel exact 65520-byte payload (16 + 16376 * 4 = 65520 <= 65536)
    {
        const size_t max_8ch = max_pcm_samples_for_channels(8);
        std::vector<float> samples(max_8ch, 0.25f);
        const EncodeResult enc = encode_audio_frame(AudioFrameView{
            9U,
            7U,
            192000U,
            8U,
            0U,
            samples,
        });
        CHECK(enc.error == ProtocolError::Ok);
        CHECK(enc.bytes.size() == kHeaderSize + 16 + (16376U * 4U));
        const DecodeResult dec = decode_frame(enc.bytes);
        CHECK(dec.error == ProtocolError::Ok);
        const auto* frame = std::get_if<AudioFrameMessage>(&dec.message);
        CHECK(frame != nullptr);
        CHECK(frame->pcm_samples.size() == 16376U);
    }

    // 2c. AudioFrame 4ch exact 65536-byte payload (16 + 16380 * 4 = 65536)
    {
        const size_t max_4ch = max_pcm_samples_for_channels(4);
        std::vector<float> samples(max_4ch, -0.75f);
        const EncodeResult enc = encode_audio_frame(AudioFrameView{
            1U,
            0U,
            8000U,
            4U,
            0U,
            samples,
        });
        CHECK(enc.error == ProtocolError::Ok);
        CHECK(enc.bytes.size() == kMaxFrameSize);
        const DecodeResult dec = decode_frame(enc.bytes);
        CHECK(dec.error == ProtocolError::Ok);
    }

    // 3. TalkingState golden vector & all 5 enum variants
    for (uint8_t s = 0; s <= 4; ++s) {
        const TalkingStateMessage msg{77U, static_cast<TalkingStateValue>(s)};
        const EncodeResult enc = encode_talking_state_frame(msg);
        CHECK(enc.error == ProtocolError::Ok);
        CHECK(enc.bytes.size() == kHeaderSize + 5);
        CHECK(enc.bytes[3] == 0x02);
        CHECK(enc.bytes[4] == 0x05);
        CHECK(enc.bytes[8] == 77 && enc.bytes[12] == s);

        const DecodeResult dec = decode_frame(enc.bytes);
        CHECK(dec.error == ProtocolError::Ok);
        const auto* out = std::get_if<TalkingStateMessage>(&dec.message);
        CHECK(out != nullptr && *out == msg);
    }

    // 4. UserInfo golden vector (including multibyte UTF-8 and empty string)
    {
        const std::string jp_name = "\xE3\x82\xB3\xE3\x83\x88\xE3\x83\x8F";
        const EncodeResult enc = encode_user_info_frame(UserInfoView{55U, jp_name});
        CHECK(enc.error == ProtocolError::Ok);
        CHECK(enc.bytes[3] == 0x03);
        const DecodeResult dec = decode_frame(enc.bytes);
        CHECK(dec.error == ProtocolError::Ok);
        const auto* info = std::get_if<UserInfoMessage>(&dec.message);
        CHECK(info != nullptr);
        CHECK(info->user_id == 55U);
        CHECK(info->name == jp_name);
    }
    {
        const EncodeResult enc = encode_user_info_frame(UserInfoView{56U, ""});
        CHECK(enc.error == ProtocolError::Ok);
        CHECK(enc.bytes.size() == kHeaderSize + 4);
        const DecodeResult dec = decode_frame(enc.bytes);
        CHECK(dec.error == ProtocolError::Ok);
        const auto* info = std::get_if<UserInfoMessage>(&dec.message);
        CHECK(info != nullptr && info->user_id == 56U && info->name.empty());
    }

    // 5. Heartbeat golden byte vector
    {
        const EncodeResult enc = encode_heartbeat_frame();
        CHECK(enc.error == ProtocolError::Ok);
        const std::vector<uint8_t> expected = {0x56, 0x54, 0x01, 0x04, 0x00, 0x00, 0x00, 0x00};
        CHECK(enc.bytes == expected);
        const DecodeResult dec = decode_frame(enc.bytes);
        CHECK(dec.error == ProtocolError::Ok);
        CHECK(std::holds_alternative<HeartbeatMessage>(dec.message));
    }

    // 6. UserRemoved golden byte vector
    {
        const EncodeResult enc = encode_user_removed_frame(UserRemovedMessage{0xAABBCCDDU});
        CHECK(enc.error == ProtocolError::Ok);
        const std::vector<uint8_t> expected = {
            0x56, 0x54, 0x01, 0x05, 0x04, 0x00, 0x00, 0x00, 0xDD, 0xCC, 0xBB, 0xAA,
        };
        CHECK(enc.bytes == expected);
        const DecodeResult dec = decode_frame(enc.bytes);
        CHECK(dec.error == ProtocolError::Ok);
        const auto* removed = std::get_if<UserRemovedMessage>(&dec.message);
        CHECK(removed != nullptr && removed->user_id == 0xAABBCCDDU);
    }

    // 7. SourceReset golden byte vector
    {
        const EncodeResult enc = encode_source_reset_frame();
        CHECK(enc.error == ProtocolError::Ok);
        const std::vector<uint8_t> expected = {0x56, 0x54, 0x01, 0x06, 0x00, 0x00, 0x00, 0x00};
        CHECK(enc.bytes == expected);
        const DecodeResult dec = decode_frame(enc.bytes);
        CHECK(dec.error == ProtocolError::Ok);
        CHECK(std::holds_alternative<SourceResetMessage>(dec.message));
    }
}

void test_rejection_cases() {
    // Bad magic
    {
        const std::vector<uint8_t> bytes = {0x56, 0x55, 0x01, 0x04, 0x00, 0x00, 0x00, 0x00};
        CHECK(decode_frame(bytes).error == ProtocolError::InvalidMagic);
    }

    // Bad version
    {
        const std::vector<uint8_t> bytes = {0x56, 0x54, 0x02, 0x04, 0x00, 0x00, 0x00, 0x00};
        CHECK(decode_frame(bytes).error == ProtocolError::UnsupportedVersion);
    }

    // Unknown msg_type
    {
        const std::vector<uint8_t> bytes = {0x56, 0x54, 0x01, 0x07, 0x00, 0x00, 0x00, 0x00};
        CHECK(decode_frame(bytes).error == ProtocolError::UnknownMessageType);
    }

    // Payload > 65536 (65537)
    {
        const std::vector<uint8_t> hdr = {0x56, 0x54, 0x01, 0x01, 0x01, 0x00, 0x01, 0x00};
        CHECK(decode_header(hdr).error == ProtocolError::PayloadTooLarge);
    }

    // Encoder rejects > 64KB AudioFrame
    {
        std::vector<float> too_many(16381, 0.0f);
        const EncodeResult enc = encode_audio_frame(AudioFrameView{1, 1, 48000, 1, 0, too_many});
        CHECK(enc.error == ProtocolError::PayloadTooLarge);
    }

    // Encoder rejects unaligned stereo AudioFrame
    {
        const std::array<float, 3> odd_samples = {0.1f, 0.2f, 0.3f};
        const EncodeResult enc = encode_audio_frame(AudioFrameView{1, 1, 48000, 2, 0, odd_samples});
        CHECK(enc.error == ProtocolError::UnalignedPcmPayload);
    }

    // Encoder rejects invalid Hello lengths & NUL bytes & invalid UTF-8
    {
        CHECK(encode_hello_frame(HelloView{"", "0.1.0"}).error == ProtocolError::InvalidHelloField);
        std::string too_long(65, 'a');
        CHECK(encode_hello_frame(HelloView{too_long, "0.1.0"}).error ==
              ProtocolError::InvalidHelloField);
        CHECK(encode_hello_frame(HelloView{"mumble", too_long}).error ==
              ProtocolError::InvalidHelloField);

        const std::string with_nul = std::string("mum") + '\0' + "ble";
        CHECK(encode_hello_frame(HelloView{with_nul, "0.1.0"}).error ==
              ProtocolError::ContainsNulByte);
        const std::string bad_utf8 = "\xFF\xFE";
        CHECK(encode_hello_frame(HelloView{bad_utf8, "0.1.0"}).error == ProtocolError::InvalidUtf8);
    }

    // Decoder rejects NaN/out-of-range PCM and reserved flags and invalid TalkingState
    {
        EncodeResult enc = encode_audio_frame(AudioFrameView{
            1,
            0,
            48000,
            1,
            0,
            std::array<float, 1>{0.25f},
        });
        CHECK(enc.error == ProtocolError::Ok);

        // Corrupt sample to NaN bits (0x7FC00000)
        enc.bytes[24] = 0x00;
        enc.bytes[25] = 0x00;
        enc.bytes[26] = 0xC0;
        enc.bytes[27] = 0x7F;
        CHECK(decode_frame(enc.bytes).error == ProtocolError::InvalidPcmSample);

        // Corrupt sample to 1.5f (0x3FC00000)
        enc.bytes[24] = 0x00;
        enc.bytes[25] = 0x00;
        enc.bytes[26] = 0xC0;
        enc.bytes[27] = 0x3F;
        CHECK(decode_frame(enc.bytes).error == ProtocolError::InvalidPcmSample);

        // Restore valid sample, set reserved flag bit 1 (0x0002)
        enc.bytes[24] = 0x00;
        enc.bytes[25] = 0x00;
        enc.bytes[26] = 0x00;
        enc.bytes[27] = 0x00;
        enc.bytes[22] = 0x02;
        CHECK(decode_frame(enc.bytes).error == ProtocolError::InvalidFlags);
    }

    {
        const std::vector<uint8_t> bad_ts = {
            0x56, 0x54, 0x01, 0x02, 0x05, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x05,
        };
        CHECK(decode_frame(bad_ts).error == ProtocolError::InvalidTalkingState);
    }

    // UserInfo NUL byte rejection
    {
        const std::string with_nul = std::string("al") + '\0' + "ice";
        CHECK(encode_user_info_frame(UserInfoView{1, with_nul}).error ==
              ProtocolError::ContainsNulByte);
    }

    // Truncated trailing bytes
    {
        EncodeResult enc = encode_heartbeat_frame();
        enc.bytes.pop_back();
        CHECK(decode_frame(enc.bytes).error == ProtocolError::TruncatedFrame);
    }
}

}  // namespace

int main() {
    test_max_pcm_samples_for_channels();
    test_golden_bytes_and_roundtrips();
    test_rejection_cases();
    std::cout << "test_protocol: all assertions passed\n";
    return 0;
}
