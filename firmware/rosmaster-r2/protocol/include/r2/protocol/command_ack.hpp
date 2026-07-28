#pragma once

#include <cstddef>
#include <cstdint>

#include <r2/safety/safety_manager.hpp>

// COMMAND_ACK payload codec (docs/PROTOCOL.md §COMMAND_ACK).
//
// wire.cpp frames; this codes the ONE payload whose meaning a host must act on.
// It is the normative implementation: the Linux bridge's Rust equivalent is
// differentially tested against it over the same bytes, and where the two
// disagree this one is right.
//
// An ACK proves protocol HANDLING, not physical movement. `accepted` says the
// command was well-formed, in-sequence, fresh and admissible in the current
// state. It says nothing about wheels turning.

namespace r2::protocol {

inline constexpr std::size_t kCommandAckPayloadSize = 28U;

// Normative result values. Unassigned values (8+) are rejected on decode, never
// coerced: a result the receiver does not understand is not evidence that a
// command succeeded, and mapping it to the nearest known code would turn "I do
// not know what happened" into "accepted".
enum class AckResult : std::uint16_t {
    accepted = 0U,
    // Honoured in MODIFIED form, tightened against the calibrated envelope.
    // Success-with-derate, NOT a refusal — effective = min(commanded,
    // calibrated_hard_limit), so a clamp means the MCU's envelope was tighter.
    clamped = 1U,
    stale = 2U,
    replay = 3U,
    unauthenticated = 4U,
    invalid = 5U,
    disarmed = 6U,
    faulted = 7U,
};

inline constexpr std::uint16_t kMaximumAssignedAckResult = 7U;

struct CommandAck {
    std::uint32_t command_id{0U};
    // The frame sequence being acknowledged. Zero means UNSOLICITED: a
    // watchdog-driven controlled stop answers no frame.
    std::uint32_t received_sequence{0U};
    std::uint64_t applied_at_us{0U};
    // 🔴 The default is `invalid`, NOT `accepted`.
    //
    // decode_command_ack resets the destination on any failure, and a
    // zero-initialised struct would leave `accepted` (value 0) sitting in the
    // one field a caller is most likely to branch on. A caller that ignores
    // the status would then read a REJECTED payload as a successful command.
    //
    // Defaulting to `invalid` makes the fail-closed direction the one you get
    // for free. It is deliberately not the zero value: "the numbers are fixed
    // by PROTOCOL.md" and "the safe default is 0" are different requirements,
    // and where they conflict the safe default wins in the struct rather than
    // on the wire.
    AckResult result{AckResult::invalid};
    std::uint8_t safety_state{
        static_cast<std::uint8_t>(r2::safety::SafetyState::standby)};
    std::uint8_t reserved{0U};
    std::uint64_t active_faults{0U};
};

enum class AckDecodeStatus : std::uint8_t {
    ok,
    // Payload was not exactly kCommandAckPayloadSize bytes. Not "at least":
    // a longer payload means the sender believes in a field this decoder does
    // not have, which is a version disagreement, not extra data to ignore.
    invalid_length,
    // result >= 8 (unassigned).
    unknown_result,
    // safety_state >= 8 (not a SafetyState discriminant).
    unknown_safety_state,
    // reserved != 0. v1 defines it as zero; accepting a non-zero value would
    // silently consume the escape hatch a future version needs.
    reserved_not_zero,
};

// encode_command_ack: writes exactly kCommandAckPayloadSize bytes.
//
// Returns false without writing when `capacity` is too small or when the
// struct carries a value this version cannot represent (an unassigned result,
// an unknown safety_state, a non-zero reserved). Refusing to ENCODE an
// unrepresentable value keeps a malformed ack off the wire in the first place,
// rather than leaving the receiver to catch it.
[[nodiscard]] bool encode_command_ack(const CommandAck& ack,
                                      std::uint8_t* output,
                                      std::size_t capacity) noexcept;

// decode_command_ack: fail-closed. On any non-ok status `output` is left as a
// zero-initialised CommandAck{} — never partially populated, so a caller that
// ignores the status cannot read a half-parsed result and act on it.
[[nodiscard]] AckDecodeStatus decode_command_ack(const std::uint8_t* payload,
                                                 std::size_t length,
                                                 CommandAck& output) noexcept;

}  // namespace r2::protocol
