#pragma once

#include <array>
#include <cstddef>
#include <cstdint>

namespace r2::protocol {

inline constexpr std::uint16_t kMagic = 0x3252U;
inline constexpr std::uint8_t kProtocolMajor = 1U;
inline constexpr std::uint8_t kProtocolMinor = 0U;
inline constexpr std::size_t kMaximumPayload = 192U;
inline constexpr std::size_t kHeaderSize = 20U;
inline constexpr std::size_t kCrcSize = 4U;
inline constexpr std::size_t kMaximumDecodedFrame =
    kHeaderSize + kMaximumPayload + kCrcSize;
inline constexpr std::size_t kMaximumEncodedFrame =
    kMaximumDecodedFrame + (kMaximumDecodedFrame / 254U) + 2U;
inline constexpr std::uint8_t kKnownFlagMask = 0x0FU;

// Flag bit 2: payload carries a 16-byte HMAC-SHA-256 truncated authentication
// tag at the end.  The tag covers the canonical header (with payload_length
// already counting the tag) and the application payload preceding the tag.
// Set automatically by encode_authenticated; verified by decode_authenticated.
inline constexpr std::uint8_t kFlagAuthTag = 0x04U;

// Size of the per-link HMAC-SHA-256 key provisioned on each device.
inline constexpr std::size_t kMacKeySize = 32U;
// Size of the truncated HMAC-SHA-256 authentication tag appended to the payload
// when kFlagAuthTag is set.
inline constexpr std::size_t kMacTagSize = 16U;
// The wire tag width must stay in lock-step with the primitive: hmac_sha256_truncated
// returns a 16-byte tag and constant_time_equal compares exactly 16 bytes. Changing
// one without the others would silently desync verification, so pin it here.
static_assert(kMacTagSize == 16U,
              "kMacTagSize must match hmac_sha256_truncated() output and "
              "constant_time_equal() width (16 bytes)");

enum class MessageType : std::uint8_t {
    hello = 1U,
    capabilities = 2U,
    time_sync_request = 3U,
    time_sync_response = 4U,
    motion_command = 16U,
    command_acknowledgement = 17U,
    arm = 18U,
    activate = 19U,
    disarm = 20U,
    acknowledge_fault = 21U,
    robot_state = 32U,
    odometry = 33U,
    imu = 34U,
    battery = 35U,
    diagnostics = 36U,
    fault_event = 37U,
    configuration_get = 48U,
    configuration_set = 49U,
    calibration = 50U,
    enter_bootloader = 64U,
    firmware_block = 65U,
    firmware_commit = 66U,
};

enum class DecodeStatus : std::uint8_t {
    ok,
    empty,
    oversized,
    malformed_cobs,
    bad_magic,
    unsupported_version,
    invalid_length,
    crc_mismatch,
    unknown_message,
    invalid_flags,
    // Returned by decode when the received frame carries kFlagAuthTag. The
    // unauthenticated path fails closed and requires callers to use
    // decode_authenticated() for tagged frames, so output remains Frame{}.
    auth_required,
    // Returned by decode_authenticated when:
    //   • the AUTH_TAG flag (kFlagAuthTag) is absent, or
    //   • the payload is shorter than kMacTagSize, or
    //   • the HMAC-SHA-256 tag does not match the expected value.
    // In all three cases the output frame is left zero-initialised (never
    // partially populated with application data).
    auth_mac_mismatch,
};

struct Frame {
    MessageType type{MessageType::hello};
    std::uint8_t flags{0U};
    std::uint32_t sequence{0U};
    std::uint64_t source_time_us{0U};
    std::uint16_t payload_length{0U};
    std::array<std::uint8_t, kMaximumPayload> payload{};
};

struct EncodedFrame {
    std::array<std::uint8_t, kMaximumEncodedFrame> bytes{};
    std::size_t length{0U};
};

[[nodiscard]] std::uint32_t crc32c(const std::uint8_t* data,
                                   std::size_t length) noexcept;
[[nodiscard]] bool encode(const Frame& frame, EncodedFrame& output) noexcept;
// decode: decodes a frame through the unauthenticated path.
//
// Frames carrying kFlagAuthTag are rejected with auth_required; callers must
// use decode_authenticated() so the appended HMAC tag is verified before any
// payload is released. On any failure output remains a zero-initialised
// Frame{}.
[[nodiscard]] DecodeStatus decode(const std::uint8_t* encoded,
                                  std::size_t encoded_length,
                                  Frame& output) noexcept;

// encode_authenticated: appends a 16-byte HMAC-SHA-256 tag (kMacTagSize) to
// the payload, sets the kFlagAuthTag flag, and encodes the frame normally.
// The tag covers the canonical header (with payload_length counting the tag)
// and the application payload that precedes it; the frame's existing sequence
// field is therefore bound into the tag, resisting replay with a modified seq.
//
// Preconditions:
//   • frame.payload_length <= kMaximumPayload - kMacTagSize
//   • frame.type must be a known MessageType
//   • frame.flags bits 4–7 must be zero
//
// Returns false (and sets output.length = 0) on any constraint violation.
// The caller must NOT set kFlagAuthTag in frame.flags; it is set by this function.
[[nodiscard]] bool encode_authenticated(
    const Frame& frame,
    const std::array<std::uint8_t, kMacKeySize>& key,
    EncodedFrame& output) noexcept;

// decode_authenticated: decodes a frame and verifies its HMAC-SHA-256 tag.
// The kFlagAuthTag flag MUST be set in the received frame; a frame without the
// flag is rejected with auth_mac_mismatch (fail-closed: no positive trust on
// unauthenticated frames).
//
// On success (DecodeStatus::ok):
//   • output.payload_length and output.payload do NOT include the 16-byte tag.
//   • kFlagAuthTag is cleared from output.flags (tag consumed by verification).
//
// On any failure the output frame is left as a zero-initialised Frame{} —
// application data is never partially populated after a failed MAC check.
[[nodiscard]] DecodeStatus decode_authenticated(
    const std::uint8_t* encoded,
    std::size_t encoded_length,
    const std::array<std::uint8_t, kMacKeySize>& key,
    Frame& output) noexcept;

class SequenceTracker {
public:
    explicit SequenceTracker(std::uint32_t maximum_forward_jump = 1'000'000U) noexcept;
    [[nodiscard]] bool accept(std::uint32_t candidate) noexcept;
    void reset() noexcept;
    [[nodiscard]] bool initialized() const noexcept;
    [[nodiscard]] std::uint32_t last() const noexcept;

private:
    std::uint32_t maximum_forward_jump_;
    std::uint32_t last_{0U};
    bool initialized_{false};
};

}  // namespace r2::protocol
