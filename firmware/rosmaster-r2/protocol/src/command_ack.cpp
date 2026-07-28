#include <r2/protocol/command_ack.hpp>

namespace r2::protocol {
namespace {

constexpr std::uint8_t kMaximumSafetyState =
    static_cast<std::uint8_t>(r2::safety::SafetyState::firmware_update);

void put_u16(std::uint8_t* p, std::uint16_t v) noexcept {
    p[0] = static_cast<std::uint8_t>(v & 0xFFU);
    p[1] = static_cast<std::uint8_t>((v >> 8U) & 0xFFU);
}

void put_u32(std::uint8_t* p, std::uint32_t v) noexcept {
    for (std::size_t i = 0U; i < 4U; ++i) {
        p[i] = static_cast<std::uint8_t>((v >> (8U * i)) & 0xFFU);
    }
}

void put_u64(std::uint8_t* p, std::uint64_t v) noexcept {
    for (std::size_t i = 0U; i < 8U; ++i) {
        p[i] = static_cast<std::uint8_t>((v >> (8U * i)) & 0xFFU);
    }
}

std::uint16_t get_u16(const std::uint8_t* p) noexcept {
    return static_cast<std::uint16_t>(static_cast<std::uint16_t>(p[0]) |
                                      static_cast<std::uint16_t>(p[1]) << 8U);
}

std::uint32_t get_u32(const std::uint8_t* p) noexcept {
    std::uint32_t v = 0U;
    for (std::size_t i = 0U; i < 4U; ++i) {
        v |= static_cast<std::uint32_t>(p[i]) << (8U * i);
    }
    return v;
}

std::uint64_t get_u64(const std::uint8_t* p) noexcept {
    std::uint64_t v = 0U;
    for (std::size_t i = 0U; i < 8U; ++i) {
        v |= static_cast<std::uint64_t>(p[i]) << (8U * i);
    }
    return v;
}

}  // namespace

bool encode_command_ack(const CommandAck& ack,
                        std::uint8_t* output,
                        std::size_t capacity) noexcept {
    if (output == nullptr || capacity < kCommandAckPayloadSize) {
        return false;
    }
    // Refuse to put an unrepresentable value on the wire. The receiver would
    // reject it anyway; catching it here means a firmware bug shows up at the
    // sender instead of as an unexplained refusal at the far end.
    if (static_cast<std::uint16_t>(ack.result) > kMaximumAssignedAckResult) {
        return false;
    }
    if (ack.safety_state > kMaximumSafetyState) {
        return false;
    }
    if (ack.reserved != 0U) {
        return false;
    }

    put_u32(output + 0U, ack.command_id);
    put_u32(output + 4U, ack.received_sequence);
    put_u64(output + 8U, ack.applied_at_us);
    put_u16(output + 16U, static_cast<std::uint16_t>(ack.result));
    output[18U] = ack.safety_state;
    output[19U] = 0U;
    put_u64(output + 20U, ack.active_faults);
    return true;
}

AckDecodeStatus decode_command_ack(const std::uint8_t* payload,
                                   std::size_t length,
                                   CommandAck& output) noexcept {
    output = CommandAck{};
    if (payload == nullptr || length != kCommandAckPayloadSize) {
        return AckDecodeStatus::invalid_length;
    }

    // Validate BEFORE populating, so a rejected payload never leaves a
    // partially-filled struct behind for a caller that ignored the status.
    const std::uint16_t result = get_u16(payload + 16U);
    if (result > kMaximumAssignedAckResult) {
        return AckDecodeStatus::unknown_result;
    }
    const std::uint8_t safety_state = payload[18U];
    if (safety_state > kMaximumSafetyState) {
        return AckDecodeStatus::unknown_safety_state;
    }
    if (payload[19U] != 0U) {
        return AckDecodeStatus::reserved_not_zero;
    }

    output.command_id = get_u32(payload + 0U);
    output.received_sequence = get_u32(payload + 4U);
    output.applied_at_us = get_u64(payload + 8U);
    output.result = static_cast<AckResult>(result);
    output.safety_state = safety_state;
    output.reserved = 0U;
    output.active_faults = get_u64(payload + 20U);
    return AckDecodeStatus::ok;
}

}  // namespace r2::protocol
