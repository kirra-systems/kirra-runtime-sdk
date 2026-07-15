#include "r2/application/configuration.hpp"

#include "r2/protocol/wire.hpp"

#include <array>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <limits>

namespace r2::application {
namespace {

constexpr std::uint32_t kConfigurationMagic = 0x3243'3252U;
constexpr std::size_t kCrcOffset = kConfigurationImageBytes - sizeof(std::uint32_t);

void put_u16(std::uint8_t* output, const std::uint16_t value) noexcept {
    output[0] = static_cast<std::uint8_t>(value);
    output[1] = static_cast<std::uint8_t>(value >> 8U);
}

void put_u32(std::uint8_t* output, const std::uint32_t value) noexcept {
    for (std::size_t index = 0U; index < 4U; ++index) {
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

void put_float(std::uint8_t* output, const float value) noexcept {
    static_assert(sizeof(float) == sizeof(std::uint32_t));
    std::uint32_t bits = 0U;
    std::memcpy(&bits, &value, sizeof(bits));
    put_u32(output, bits);
}

[[nodiscard]] float get_float(const std::uint8_t* input) noexcept {
    const auto bits = get_u32(input);
    float value = 0.0F;
    std::memcpy(&value, &bits, sizeof(value));
    return value;
}

[[nodiscard]] std::array<std::uint8_t, kConfigurationImageBytes> serialize(
    const PlatformConfiguration& configuration) noexcept {
    std::array<std::uint8_t, kConfigurationImageBytes> image{};
    put_u32(&image[0], kConfigurationMagic);
    put_u16(&image[4], configuration.schema);
    put_u16(&image[6], configuration.calibrated ? 1U : 0U);
    put_u32(&image[8], configuration.generation);
    put_float(&image[12], configuration.wheelbase_m);
    put_float(&image[16], configuration.rear_track_m);
    put_float(&image[20], configuration.wheel_radius_m);
    put_float(&image[24], configuration.maximum_steering_angle_rad);
    put_float(&image[28], configuration.maximum_speed_mps);
    put_float(&image[32], configuration.maximum_acceleration_mps2);
    put_float(&image[36], configuration.maximum_deceleration_mps2);
    put_float(&image[40], configuration.maximum_jerk_mps3);
    put_float(&image[44], configuration.maximum_steering_rate_rad_s);
    put_float(&image[48], configuration.battery_divider_ratio);
    put_u16(&image[52], configuration.servo_minimum_us);
    put_u16(&image[54], configuration.servo_center_us);
    put_u16(&image[56], configuration.servo_maximum_us);
    put_u32(&image[58], configuration.left_encoder_counts_per_revolution);
    put_u32(&image[62], configuration.right_encoder_counts_per_revolution);
    put_u16(&image[66], configuration.command_timeout_ms);
    put_u32(&image[kCrcOffset], protocol::crc32c(image.data(), kCrcOffset));
    return image;
}

[[nodiscard]] bool deserialize(
    const std::array<std::uint8_t, kConfigurationImageBytes>& image,
    PlatformConfiguration& configuration) noexcept {
    if (get_u32(&image[0]) != kConfigurationMagic ||
        get_u32(&image[kCrcOffset]) != protocol::crc32c(image.data(), kCrcOffset)) {
        return false;
    }
    PlatformConfiguration candidate{};
    candidate.schema = get_u16(&image[4]);
    candidate.calibrated = (get_u16(&image[6]) & 1U) != 0U;
    candidate.generation = get_u32(&image[8]);
    candidate.wheelbase_m = get_float(&image[12]);
    candidate.rear_track_m = get_float(&image[16]);
    candidate.wheel_radius_m = get_float(&image[20]);
    candidate.maximum_steering_angle_rad = get_float(&image[24]);
    candidate.maximum_speed_mps = get_float(&image[28]);
    candidate.maximum_acceleration_mps2 = get_float(&image[32]);
    candidate.maximum_deceleration_mps2 = get_float(&image[36]);
    candidate.maximum_jerk_mps3 = get_float(&image[40]);
    candidate.maximum_steering_rate_rad_s = get_float(&image[44]);
    candidate.battery_divider_ratio = get_float(&image[48]);
    candidate.servo_minimum_us = get_u16(&image[52]);
    candidate.servo_center_us = get_u16(&image[54]);
    candidate.servo_maximum_us = get_u16(&image[56]);
    candidate.left_encoder_counts_per_revolution = get_u32(&image[58]);
    candidate.right_encoder_counts_per_revolution = get_u32(&image[62]);
    candidate.command_timeout_ms = get_u16(&image[66]);
    if (!valid_configuration(candidate)) {
        return false;
    }
    configuration = candidate;
    return true;
}

[[nodiscard]] bool in_range(const float value,
                            const float minimum,
                            const float maximum) noexcept {
    return std::isfinite(value) && value >= minimum && value <= maximum;
}

[[nodiscard]] bool generation_is_newer(const std::uint32_t candidate,
                                       const std::uint32_t reference) noexcept {
    const auto delta = candidate - reference;
    return delta != 0U && delta < (std::numeric_limits<std::uint32_t>::max() / 2U);
}

}  // namespace

PlatformConfiguration factory_defaults() noexcept {
    return {};
}

bool valid_configuration(const PlatformConfiguration& configuration) noexcept {
    if (configuration.schema != kConfigurationSchema ||
        configuration.command_timeout_ms < 20U ||
        configuration.command_timeout_ms > 1'000U) {
        return false;
    }
    if (!configuration.calibrated) {
        return true;
    }
    return in_range(configuration.wheelbase_m, 0.1F, 1.0F) &&
           in_range(configuration.rear_track_m, 0.1F, 1.0F) &&
           in_range(configuration.wheel_radius_m, 0.01F, 0.25F) &&
           in_range(configuration.maximum_steering_angle_rad, 0.05F, 1.2F) &&
           in_range(configuration.maximum_speed_mps, 0.05F, 5.0F) &&
           in_range(configuration.maximum_acceleration_mps2, 0.05F, 10.0F) &&
           in_range(configuration.maximum_deceleration_mps2, 0.05F, 20.0F) &&
           in_range(configuration.maximum_jerk_mps3, 0.05F, 100.0F) &&
           in_range(configuration.maximum_steering_rate_rad_s, 0.05F, 20.0F) &&
           in_range(configuration.battery_divider_ratio, 1.0F, 20.0F) &&
           configuration.servo_minimum_us >= 500U &&
           configuration.servo_minimum_us < configuration.servo_center_us &&
           configuration.servo_center_us < configuration.servo_maximum_us &&
           configuration.servo_maximum_us <= 2'500U &&
           configuration.left_encoder_counts_per_revolution > 0U &&
           configuration.right_encoder_counts_per_revolution > 0U;
}

ConfigurationStore::ConfigurationStore(hal::PersistentStorage& storage,
                                       const std::uint32_t slot_a_address,
                                       const std::uint32_t slot_b_address) noexcept
    : storage_(storage),
      slot_a_address_(slot_a_address),
      slot_b_address_(slot_b_address) {}

bool ConfigurationStore::load(PlatformConfiguration& configuration) const noexcept {
    PlatformConfiguration slot_a{};
    PlatformConfiguration slot_b{};
    const auto a_valid = read_slot(slot_a_address_, slot_a);
    const auto b_valid = read_slot(slot_b_address_, slot_b);
    if (!a_valid && !b_valid) {
        configuration = factory_defaults();
        return false;
    }
    if (a_valid && (!b_valid || generation_is_newer(slot_a.generation, slot_b.generation))) {
        configuration = slot_a;
    } else {
        configuration = slot_b;
    }
    return true;
}

bool ConfigurationStore::commit(const PlatformConfiguration& configuration) noexcept {
    if (!valid_configuration(configuration)) {
        return false;
    }
    PlatformConfiguration current{};
    const auto has_current = load(current);
    auto candidate = configuration;
    candidate.generation = has_current ? current.generation + 1U : 1U;

    const auto target = !has_current ||
                                (read_slot(slot_a_address_, current) &&
                                 current.generation + 1U == candidate.generation)
                            ? slot_b_address_
                            : slot_a_address_;
    return write_slot(target, candidate);
}

bool ConfigurationStore::read_slot(
    const std::uint32_t address,
    PlatformConfiguration& configuration) const noexcept {
    std::array<std::uint8_t, kConfigurationImageBytes> image{};
    return storage_.read(address, image.data(), image.size()) &&
           deserialize(image, configuration);
}

bool ConfigurationStore::write_slot(
    const std::uint32_t address,
    const PlatformConfiguration& configuration) noexcept {
    const auto image = serialize(configuration);
    if (!storage_.erase(address, kConfigurationSlotBytes) ||
        !storage_.write(address, image.data(), image.size())) {
        return false;
    }
    PlatformConfiguration verified{};
    return read_slot(address, verified) &&
           verified.generation == configuration.generation;
}

}  // namespace r2::application
