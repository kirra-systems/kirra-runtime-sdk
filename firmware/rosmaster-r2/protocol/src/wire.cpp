#include "r2/protocol/wire.hpp"
#include "r2/protocol/mac.hpp"

#include <algorithm>
#include <array>
#include <cstddef>
#include <cstdint>

namespace r2::protocol {
namespace {

void put_u16(std::uint8_t* output, const std::uint16_t value) noexcept {
    output[0] = static_cast<std::uint8_t>(value);
    output[1] = static_cast<std::uint8_t>(value >> 8U);
}

void put_u32(std::uint8_t* output, const std::uint32_t value) noexcept {
    for (std::size_t index = 0U; index < 4U; ++index) {
        output[index] = static_cast<std::uint8_t>(value >> (index * 8U));
    }
}

void put_u64(std::uint8_t* output, const std::uint64_t value) noexcept {
    for (std::size_t index = 0U; index < 8U; ++index) {
        output[index] = static_cast<std::uint8_t>(value >> (index * 8U));
    }
}

[[nodiscard]] std::uint16_t get_u16(const std::uint8_t* input) noexcept {
    return static_cast<std::uint16_t>(
        static_cast<std::uint16_t>(input[0]) |
        (static_cast<std::uint16_t>(input[1]) << 8U));
}

[[nodiscard]] std::uint32_t get_u32(const std::uint8_t* input) noexcept {
    std::uint32_t value = 0U;
    for (std::size_t index = 0U; index < 4U; ++index) {
        value |= static_cast<std::uint32_t>(input[index]) << (index * 8U);
    }
    return value;
}

[[nodiscard]] std::uint64_t get_u64(const std::uint8_t* input) noexcept {
    std::uint64_t value = 0U;
    for (std::size_t index = 0U; index < 8U; ++index) {
        value |= static_cast<std::uint64_t>(input[index]) << (index * 8U);
    }
    return value;
}

[[nodiscard]] bool cobs_encode(const std::uint8_t* input,
                               const std::size_t input_length,
                               std::uint8_t* output,
                               const std::size_t capacity,
                               std::size_t& output_length) noexcept {
    if (capacity == 0U) {
        return false;
    }
    std::size_t read_index = 0U;
    std::size_t write_index = 1U;
    std::size_t code_index = 0U;
    std::uint8_t code = 1U;

    while (read_index < input_length) {
        if (input[read_index] == 0U) {
            if (code_index >= capacity) {
                return false;
            }
            output[code_index] = code;
            code = 1U;
            code_index = write_index;
            ++write_index;
            ++read_index;
        } else {
            if (write_index >= capacity) {
                return false;
            }
            output[write_index] = input[read_index];
            ++write_index;
            ++read_index;
            ++code;
            if (code == 0xFFU) {
                if (code_index >= capacity) {
                    return false;
                }
                output[code_index] = code;
                code = 1U;
                code_index = write_index;
                ++write_index;
            }
        }
    }
    if (code_index >= capacity) {
        return false;
    }
    output[code_index] = code;
    output_length = write_index;
    return true;
}

[[nodiscard]] bool cobs_decode(const std::uint8_t* input,
                               const std::size_t input_length,
                               std::uint8_t* output,
                               const std::size_t capacity,
                               std::size_t& output_length) noexcept {
    std::size_t read_index = 0U;
    std::size_t write_index = 0U;
    while (read_index < input_length) {
        const auto code = input[read_index];
        if (code == 0U) {
            return false;
        }
        ++read_index;
        const auto copy_count = static_cast<std::size_t>(code - 1U);
        if (read_index + copy_count > input_length ||
            write_index + copy_count > capacity) {
            return false;
        }
        for (std::size_t index = 0U; index < copy_count; ++index) {
            output[write_index++] = input[read_index++];
        }
        if (code != 0xFFU && read_index < input_length) {
            if (write_index >= capacity) {
                return false;
            }
            output[write_index++] = 0U;
        }
    }
    output_length = write_index;
    return true;
}

[[nodiscard]] bool known_message_type(const std::uint8_t raw) noexcept {
    switch (static_cast<MessageType>(raw)) {
        case MessageType::hello:
        case MessageType::capabilities:
        case MessageType::time_sync_request:
        case MessageType::time_sync_response:
        case MessageType::motion_command:
        case MessageType::command_acknowledgement:
        case MessageType::arm:
        case MessageType::activate:
        case MessageType::disarm:
        case MessageType::acknowledge_fault:
        case MessageType::robot_state:
        case MessageType::odometry:
        case MessageType::imu:
        case MessageType::battery:
        case MessageType::diagnostics:
        case MessageType::fault_event:
        case MessageType::configuration_get:
        case MessageType::configuration_set:
        case MessageType::calibration:
        case MessageType::enter_bootloader:
        case MessageType::firmware_block:
        case MessageType::firmware_commit:
            return true;
    }
    return false;
}

}  // namespace

std::uint32_t crc32c(const std::uint8_t* data, const std::size_t length) noexcept {
    std::uint32_t crc = 0xFFFF'FFFFU;
    for (std::size_t index = 0U; index < length; ++index) {
        crc ^= data[index];
        for (std::uint8_t bit = 0U; bit < 8U; ++bit) {
            const auto mask = static_cast<std::uint32_t>(
                -static_cast<std::int32_t>(crc & 1U));
            crc = (crc >> 1U) ^ (0x82F6'3B78U & mask);
        }
    }
    return ~crc;
}

bool encode(const Frame& frame, EncodedFrame& output) noexcept {
    if (frame.payload_length > kMaximumPayload ||
        !known_message_type(static_cast<std::uint8_t>(frame.type)) ||
        (frame.flags & static_cast<std::uint8_t>(~kKnownFlagMask)) != 0U) {
        output.length = 0U;
        return false;
    }

    std::array<std::uint8_t, kMaximumDecodedFrame> decoded{};
    put_u16(&decoded[0], kMagic);
    decoded[2] = kProtocolMajor;
    decoded[3] = kProtocolMinor;
    decoded[4] = static_cast<std::uint8_t>(frame.type);
    decoded[5] = frame.flags;
    put_u16(&decoded[6], frame.payload_length);
    put_u32(&decoded[8], frame.sequence);
    put_u64(&decoded[12], frame.source_time_us);
    std::copy_n(frame.payload.begin(), frame.payload_length, decoded.begin() + kHeaderSize);

    const auto body_length = kHeaderSize + frame.payload_length;
    put_u32(&decoded[body_length], crc32c(decoded.data(), body_length));
    std::size_t encoded_length = 0U;
    if (!cobs_encode(decoded.data(),
                     body_length + kCrcSize,
                     output.bytes.data(),
                     output.bytes.size() - 1U,
                     encoded_length)) {
        output.length = 0U;
        return false;
    }
    output.bytes[encoded_length] = 0U;
    output.length = encoded_length + 1U;
    return true;
}

DecodeStatus decode(const std::uint8_t* encoded,
                    std::size_t encoded_length,
                    Frame& output) noexcept {
    output = Frame{};
    if (encoded == nullptr || encoded_length == 0U) {
        return DecodeStatus::empty;
    }
    if (encoded_length > kMaximumEncodedFrame) {
        return DecodeStatus::oversized;
    }
    if (encoded[encoded_length - 1U] == 0U) {
        --encoded_length;
    }
    if (encoded_length == 0U) {
        return DecodeStatus::empty;
    }

    std::array<std::uint8_t, kMaximumDecodedFrame> decoded{};
    std::size_t decoded_length = 0U;
    if (!cobs_decode(encoded, encoded_length, decoded.data(), decoded.size(), decoded_length)) {
        return DecodeStatus::malformed_cobs;
    }
    if (decoded_length < kHeaderSize + kCrcSize) {
        return DecodeStatus::invalid_length;
    }
    if (get_u16(&decoded[0]) != kMagic) {
        return DecodeStatus::bad_magic;
    }
    if (decoded[2] != kProtocolMajor) {
        return DecodeStatus::unsupported_version;
    }

    const auto payload_length = get_u16(&decoded[6]);
    if (payload_length > kMaximumPayload ||
        decoded_length != kHeaderSize + payload_length + kCrcSize) {
        return DecodeStatus::invalid_length;
    }
    const auto body_length = kHeaderSize + payload_length;
    if (get_u32(&decoded[body_length]) != crc32c(decoded.data(), body_length)) {
        return DecodeStatus::crc_mismatch;
    }
    if (!known_message_type(decoded[4])) {
        return DecodeStatus::unknown_message;
    }
    if ((decoded[5] & static_cast<std::uint8_t>(~kKnownFlagMask)) != 0U) {
        return DecodeStatus::invalid_flags;
    }
    if ((decoded[5] & kFlagAuthTag) != 0U) {
        return DecodeStatus::auth_required;
    }

    output.type = static_cast<MessageType>(decoded[4]);
    output.flags = decoded[5];
    output.payload_length = payload_length;
    output.sequence = get_u32(&decoded[8]);
    output.source_time_us = get_u64(&decoded[12]);
    output.payload.fill(0U);
    std::copy_n(decoded.begin() + kHeaderSize, payload_length, output.payload.begin());
    return DecodeStatus::ok;
}

SequenceTracker::SequenceTracker(const std::uint32_t maximum_forward_jump) noexcept
    : maximum_forward_jump_(maximum_forward_jump) {}

bool SequenceTracker::accept(const std::uint32_t candidate) noexcept {
    if (maximum_forward_jump_ == 0U ||
        maximum_forward_jump_ > 0x7FFF'FFFFU) {
        return false;
    }
    if (!initialized_) {
        initialized_ = true;
        last_ = candidate;
        return true;
    }
    const auto forward = candidate - last_;
    if (forward == 0U || forward > maximum_forward_jump_) {
        return false;
    }
    last_ = candidate;
    return true;
}

void SequenceTracker::reset() noexcept {
    last_ = 0U;
    initialized_ = false;
}

bool SequenceTracker::initialized() const noexcept {
    return initialized_;
}

std::uint32_t SequenceTracker::last() const noexcept {
    return last_;
}

bool encode_authenticated(const Frame& frame,
                           const std::array<std::uint8_t, kMacKeySize>& key,
                           EncodedFrame& output) noexcept {
    // Validate input: same reserved-bit and type checks as encode(), plus the
    // payload must leave room for the 16-byte MAC tag.
    if (frame.payload_length > static_cast<std::uint16_t>(kMaximumPayload - kMacTagSize) ||
        !known_message_type(static_cast<std::uint8_t>(frame.type)) ||
        (frame.flags & static_cast<std::uint8_t>(~kKnownFlagMask)) != 0U) {
        output.length = 0U;
        return false;
    }

    // Build the canonical decoded bytes with AUTH_TAG flag set and the
    // extended payload_length (original + kMacTagSize) in the header.
    std::array<std::uint8_t, kMaximumDecodedFrame> decoded{};
    put_u16(&decoded[0], kMagic);
    decoded[2] = kProtocolMajor;
    decoded[3] = kProtocolMinor;
    decoded[4] = static_cast<std::uint8_t>(frame.type);
    decoded[5] = frame.flags | kFlagAuthTag;
    const auto extended_payload =
        static_cast<std::uint16_t>(static_cast<std::size_t>(frame.payload_length) + kMacTagSize);
    put_u16(&decoded[6], extended_payload);
    put_u32(&decoded[8], frame.sequence);
    put_u64(&decoded[12], frame.source_time_us);
    std::copy_n(frame.payload.begin(), frame.payload_length,
                decoded.begin() + static_cast<std::ptrdiff_t>(kHeaderSize));

    // Compute HMAC-SHA-256 over header + original payload (before the tag).
    // The header already contains the extended payload_length (kMacTagSize added)
    // so the sequence field is bound into the MAC input.
    const auto body_before_tag = kHeaderSize + static_cast<std::size_t>(frame.payload_length);
    const auto tag = internal::hmac_sha256_truncated(key, decoded.data(), body_before_tag);

    // Append the 16-byte tag after the original payload
    std::copy_n(tag.begin(), kMacTagSize,
                decoded.begin() + static_cast<std::ptrdiff_t>(body_before_tag));

    // CRC32C over the full body (header + extended payload including tag)
    const auto extended_body = kHeaderSize + static_cast<std::size_t>(extended_payload);
    put_u32(&decoded[extended_body], crc32c(decoded.data(), extended_body));

    // COBS encode
    std::size_t encoded_length = 0U;
    if (!cobs_encode(decoded.data(),
                     extended_body + kCrcSize,
                     output.bytes.data(),
                     output.bytes.size() - 1U,
                     encoded_length)) {
        output.length = 0U;
        return false;
    }
    output.bytes[encoded_length] = 0U;
    output.length = encoded_length + 1U;
    return true;
}

DecodeStatus decode_authenticated(const std::uint8_t* encoded,
                                   std::size_t encoded_length,
                                   const std::array<std::uint8_t, kMacKeySize>& key,
                                   Frame& output) noexcept {
    // Start with a zeroed output — never return partial application data.
    output = Frame{};

    if (encoded == nullptr || encoded_length == 0U) {
        return DecodeStatus::empty;
    }
    if (encoded_length > kMaximumEncodedFrame) {
        return DecodeStatus::oversized;
    }
    if (encoded[encoded_length - 1U] == 0U) {
        --encoded_length;
    }
    if (encoded_length == 0U) {
        return DecodeStatus::empty;
    }

    std::array<std::uint8_t, kMaximumDecodedFrame> decoded{};
    std::size_t decoded_length = 0U;
    if (!cobs_decode(encoded, encoded_length, decoded.data(), decoded.size(), decoded_length)) {
        return DecodeStatus::malformed_cobs;
    }
    if (decoded_length < kHeaderSize + kCrcSize) {
        return DecodeStatus::invalid_length;
    }
    if (get_u16(&decoded[0]) != kMagic) {
        return DecodeStatus::bad_magic;
    }
    if (decoded[2] != kProtocolMajor) {
        return DecodeStatus::unsupported_version;
    }

    const auto payload_length = get_u16(&decoded[6]);
    if (payload_length > kMaximumPayload ||
        decoded_length != kHeaderSize + payload_length + kCrcSize) {
        return DecodeStatus::invalid_length;
    }
    const auto body_length = kHeaderSize + static_cast<std::size_t>(payload_length);
    if (get_u32(&decoded[body_length]) != crc32c(decoded.data(), body_length)) {
        return DecodeStatus::crc_mismatch;
    }

    // Fail-closed: reject frames that do not carry an authentication tag, and
    // frames where the payload is too short to contain one.
    if ((decoded[5] & kFlagAuthTag) == 0U ||
        static_cast<std::size_t>(payload_length) < kMacTagSize) {
        return DecodeStatus::auth_mac_mismatch;
    }

    // Compute the expected tag over: header (with extended payload_length) +
    // application payload (everything before the tag).
    const auto content_length =
        static_cast<std::size_t>(payload_length) - kMacTagSize;
    const auto body_before_tag = kHeaderSize + content_length;
    const auto expected_tag =
        internal::hmac_sha256_truncated(key, decoded.data(), body_before_tag);

    // Constant-time comparison — must run to completion regardless of mismatch.
    std::array<std::uint8_t, kMacTagSize> received_tag{};
    std::copy_n(decoded.begin() + static_cast<std::ptrdiff_t>(body_before_tag),
                kMacTagSize, received_tag.begin());
    if (!internal::constant_time_equal(expected_tag, received_tag)) {
        // output is already Frame{} — do not leak partial data.
        return DecodeStatus::auth_mac_mismatch;
    }

    // MAC verified.  Now perform the remaining structural checks.
    if (!known_message_type(decoded[4])) {
        return DecodeStatus::unknown_message;
    }
    if ((decoded[5] & static_cast<std::uint8_t>(~kKnownFlagMask)) != 0U) {
        return DecodeStatus::invalid_flags;
    }

    // Populate output, stripping the consumed tag from the payload.
    output.type = static_cast<MessageType>(decoded[4]);
    // Clear AUTH_TAG from output flags: the tag has been verified and consumed.
    output.flags = decoded[5] & static_cast<std::uint8_t>(~kFlagAuthTag);
    output.payload_length = static_cast<std::uint16_t>(content_length);
    output.sequence = get_u32(&decoded[8]);
    output.source_time_us = get_u64(&decoded[12]);
    output.payload.fill(0U);
    std::copy_n(decoded.begin() + static_cast<std::ptrdiff_t>(kHeaderSize),
                content_length, output.payload.begin());
    return DecodeStatus::ok;
}

}  // namespace r2::protocol
