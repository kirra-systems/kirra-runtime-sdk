#pragma once

#include "r2/hal/interfaces.hpp"

#include <cstddef>
#include <cstdint>

namespace r2::application {

inline constexpr std::uint16_t kConfigurationSchema = 1U;
inline constexpr std::size_t kConfigurationImageBytes = 80U;
inline constexpr std::size_t kConfigurationSlotBytes = 128U;

struct PlatformConfiguration {
    std::uint16_t schema{kConfigurationSchema};
    bool calibrated{false};
    std::uint32_t generation{0U};
    float wheelbase_m{0.0F};
    float rear_track_m{0.0F};
    float wheel_radius_m{0.0F};
    float maximum_steering_angle_rad{0.0F};
    float maximum_speed_mps{0.0F};
    float maximum_acceleration_mps2{0.0F};
    float maximum_deceleration_mps2{0.0F};
    float maximum_jerk_mps3{0.0F};
    float maximum_steering_rate_rad_s{0.0F};
    float battery_divider_ratio{0.0F};
    std::uint16_t servo_minimum_us{0U};
    std::uint16_t servo_center_us{0U};
    std::uint16_t servo_maximum_us{0U};
    std::uint32_t left_encoder_counts_per_revolution{0U};
    std::uint32_t right_encoder_counts_per_revolution{0U};
    std::uint16_t command_timeout_ms{100U};
};

[[nodiscard]] PlatformConfiguration factory_defaults() noexcept;
[[nodiscard]] bool valid_configuration(const PlatformConfiguration& configuration) noexcept;

struct ConfigurationSlotAddresses {
    std::uint32_t a;
    std::uint32_t b;
};

class ConfigurationStore {
public:
    ConfigurationStore(hal::PersistentStorage& storage,
                       ConfigurationSlotAddresses addresses) noexcept;

    [[nodiscard]] bool load(PlatformConfiguration& configuration) const noexcept;
    [[nodiscard]] bool commit(const PlatformConfiguration& configuration) noexcept;

private:
    [[nodiscard]] bool read_slot(std::uint32_t address,
                                 PlatformConfiguration& configuration) const noexcept;
    [[nodiscard]] bool write_slot(std::uint32_t address,
                                  const PlatformConfiguration& configuration) noexcept;

    hal::PersistentStorage& storage_;
    std::uint32_t slot_a_address_;
    std::uint32_t slot_b_address_;
};

}  // namespace r2::application
