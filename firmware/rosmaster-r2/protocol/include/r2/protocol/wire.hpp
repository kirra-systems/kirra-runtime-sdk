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

enum class MessageType : std::uint8_t {
    hello = 1U,
    capabilities = 2U,
    time_sync_request = 3U,
    time_sync_response = 4U,
    motion_command = 16U,
    command_acknowledgement = 17U,
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
[[nodiscard]] DecodeStatus decode(const std::uint8_t* encoded,
                                  std::size_t encoded_length,
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
