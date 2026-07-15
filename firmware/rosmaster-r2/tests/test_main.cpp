#include "r2/application/configuration.hpp"
#include "r2/control/motion_controller.hpp"
#include "r2/diagnostics/metrics.hpp"
#include "r2/kinematics/ackermann.hpp"
#include "r2/protocol/wire.hpp"
#include "r2/safety/safety_manager.hpp"

#include <array>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>

namespace {

int failures = 0;

#define CHECK(condition)                                                        \
    do {                                                                        \
        if (!(condition)) {                                                      \
            std::fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #condition); \
            ++failures;                                                          \
        }                                                                        \
    } while (false)

[[nodiscard]] bool near(const double left,
                        const double right,
                        const double tolerance = 1.0e-9) {
    return std::abs(left - right) <= tolerance;
}

class MemoryFlash final : public r2::hal::PersistentStorage {
public:
    std::size_t erase_block_size() const noexcept override {
        return r2::application::kConfigurationSlotBytes;
    }

    bool read(const std::uint32_t address,
              std::uint8_t* destination,
              const std::size_t length) const noexcept override {
        if (static_cast<std::size_t>(address) + length > bytes.size()) {
            return false;
        }
        std::memcpy(destination, bytes.data() + address, length);
        return true;
    }

    bool erase(const std::uint32_t address, const std::size_t length) noexcept override {
        if (static_cast<std::size_t>(address) + length > bytes.size()) {
            return false;
        }
        std::memset(bytes.data() + address, 0xFF, length);
        return true;
    }

    bool write(const std::uint32_t address,
               const std::uint8_t* source,
               const std::size_t length) noexcept override {
        if (fail_next_write ||
            static_cast<std::size_t>(address) + length > bytes.size()) {
            fail_next_write = false;
            return false;
        }
        if (fail_after_bytes < length) {
            std::memcpy(bytes.data() + address, source, fail_after_bytes);
            fail_after_bytes = std::numeric_limits<std::size_t>::max();
            return false;
        }
        std::memcpy(bytes.data() + address, source, length);
        return true;
    }

    std::array<std::uint8_t, 4'096U> bytes{};
    bool fail_next_write{false};
    std::size_t fail_after_bytes{std::numeric_limits<std::size_t>::max()};
};

r2::application::PlatformConfiguration calibrated_configuration() {
    r2::application::PlatformConfiguration configuration{};
    configuration.calibrated = true;
    configuration.wheelbase_m = 0.23F;
    configuration.rear_track_m = 0.19F;
    configuration.wheel_radius_m = 0.033F;
    configuration.maximum_steering_angle_rad = 0.55F;
    configuration.maximum_speed_mps = 1.0F;
    configuration.maximum_acceleration_mps2 = 1.5F;
    configuration.maximum_deceleration_mps2 = 2.0F;
    configuration.maximum_jerk_mps3 = 8.0F;
    configuration.maximum_steering_rate_rad_s = 2.0F;
    configuration.battery_divider_ratio = 4.03F;
    configuration.servo_minimum_us = 1'000U;
    configuration.servo_center_us = 1'500U;
    configuration.servo_maximum_us = 2'000U;
    configuration.left_encoder_counts_per_revolution = 835U;
    configuration.right_encoder_counts_per_revolution = 835U;
    return configuration;
}

void test_crc_and_protocol() {
    constexpr std::array<std::uint8_t, 9U> check{
        '1', '2', '3', '4', '5', '6', '7', '8', '9'};
    CHECK(r2::protocol::crc32c(check.data(), check.size()) == 0xE306'9283U);

    r2::protocol::Frame frame{};
    frame.type = r2::protocol::MessageType::motion_command;
    frame.flags = 3U;
    frame.sequence = 0xFFFF'FFFEU;
    frame.source_time_us = 123'456'789U;
    frame.payload_length = 6U;
    frame.payload[0] = 0U;
    frame.payload[1] = 1U;
    frame.payload[2] = 0U;
    frame.payload[3] = 2U;
    frame.payload[4] = 3U;
    frame.payload[5] = 0U;

    r2::protocol::EncodedFrame encoded{};
    CHECK(r2::protocol::encode(frame, encoded));
    CHECK(encoded.length > 0U);
    CHECK(encoded.bytes[encoded.length - 1U] == 0U);

    r2::protocol::Frame decoded{};
    CHECK(r2::protocol::decode(encoded.bytes.data(), encoded.length, decoded) ==
          r2::protocol::DecodeStatus::ok);
    CHECK(decoded.type == frame.type);
    CHECK(decoded.flags == frame.flags);
    CHECK(decoded.sequence == frame.sequence);
    CHECK(decoded.source_time_us == frame.source_time_us);
    CHECK(decoded.payload_length == frame.payload_length);
    CHECK(std::memcmp(decoded.payload.data(), frame.payload.data(), frame.payload_length) == 0);

    encoded.bytes[3] ^= 0x40U;
    CHECK(r2::protocol::decode(encoded.bytes.data(), encoded.length, decoded) !=
          r2::protocol::DecodeStatus::ok);

    r2::protocol::SequenceTracker tracker{10U};
    CHECK(tracker.accept(0xFFFF'FFFEU));
    CHECK(tracker.accept(0xFFFF'FFFFU));
    CHECK(tracker.accept(0U));
    CHECK(!tracker.accept(0U));
    CHECK(!tracker.accept(20U));

    frame.type = static_cast<r2::protocol::MessageType>(0xFEU);
    CHECK(!r2::protocol::encode(frame, encoded));
    frame.type = r2::protocol::MessageType::motion_command;
    frame.flags = 0x80U;
    CHECK(!r2::protocol::encode(frame, encoded));

    r2::protocol::SequenceTracker invalid_tracker{0x8000'0000U};
    CHECK(!invalid_tracker.accept(1U));
}

void test_kinematics() {
    constexpr r2::kinematics::VehicleGeometry geometry{
        0.23, 0.19, 0.033, 0.55};
    const auto straight =
        r2::kinematics::inverse_ackermann(geometry, {1.0, 0.0});
    CHECK(straight.status == r2::kinematics::KinematicsStatus::ok);
    CHECK(near(straight.value.left_rear_velocity_mps, 1.0));
    CHECK(near(straight.value.right_rear_velocity_mps, 1.0));
    CHECK(near(straight.value.steering_angle_rad, 0.0));

    const auto left =
        r2::kinematics::inverse_ackermann(geometry, {1.0, 1.0});
    CHECK(left.status == r2::kinematics::KinematicsStatus::ok);
    CHECK(left.value.left_rear_velocity_mps < left.value.right_rear_velocity_mps);
    CHECK(left.value.steering_angle_rad > 0.0);
    const auto reconstructed = r2::kinematics::forward_ackermann(
        geometry,
        left.value.left_rear_velocity_mps,
        left.value.right_rear_velocity_mps,
        left.value.steering_angle_rad);
    CHECK(near(reconstructed.longitudinal_velocity_mps, 1.0));
    CHECK(near(reconstructed.yaw_rate_rad_s, 1.0));

    const auto impossible =
        r2::kinematics::inverse_ackermann(geometry, {0.0, 1.0});
    CHECK(impossible.status ==
          r2::kinematics::KinematicsStatus::infeasible_stationary_turn);
    const auto non_finite =
        r2::kinematics::inverse_ackermann(geometry, {NAN, 0.0});
    CHECK(non_finite.status == r2::kinematics::KinematicsStatus::non_finite_input);
    const auto overflowing = r2::kinematics::inverse_ackermann(
        geometry, {0.002, std::numeric_limits<double>::max()});
    CHECK(overflowing.status ==
          r2::kinematics::KinematicsStatus::non_finite_input);
    const auto forward_overflow = r2::kinematics::forward_ackermann(
        geometry,
        std::numeric_limits<double>::max(),
        std::numeric_limits<double>::max(),
        0.55);
    CHECK(forward_overflow.longitudinal_velocity_mps == 0.0);
    CHECK(forward_overflow.yaw_rate_rad_s == 0.0);
}

void test_control() {
    constexpr r2::control::MotionLimits limits{2.0, 1.0, 2.0, 10.0, 2.0};
    r2::control::JerkLimitedAxis limiter{limits};
    const auto first = limiter.update(10.0, 0.01);
    CHECK(first > 0.0 && first < 0.01);
    for (std::size_t index = 0U; index < 5'000U; ++index) {
        (void)limiter.update(10.0, 0.001);
    }
    CHECK(limiter.velocity_mps() <= limits.maximum_speed_mps);

    constexpr r2::control::PidGains gains{1.0, 1.0, 0.0, 0.2, 5.0};
    r2::control::VelocityPid pid{gains};
    for (std::size_t index = 0U; index < 10'000U; ++index) {
        const auto output = pid.update(100.0, 0.0, 0.001, -1.0, 1.0);
        CHECK(std::isfinite(output));
        CHECK(output <= 1.0 && output >= -1.0);
    }
    const auto recovered = pid.update(0.0, 0.0, 0.001, -1.0, 1.0);
    CHECK(std::abs(recovered) < 1.0);

    constexpr r2::control::PidGains invalid_gains{NAN, 1.0, 0.0, 0.0, 1.0};
    r2::control::VelocityPid invalid_pid{invalid_gains};
    CHECK(invalid_pid.update(1.0, 0.0, 0.001, -1.0, 1.0) == 0.0);
}

void test_motion_controller_composition() {
    constexpr r2::kinematics::VehicleGeometry geometry{
        0.23, 0.19, 0.033, 0.55};
    constexpr r2::control::MotionLimits limits{
        1.5, 1.0, 2.0, 8.0, 2.0};
    constexpr r2::control::PidGains gains{
        0.8, 1.2, 0.01, 0.4, 4.0};
    r2::control::MotionController controller{
        geometry, limits, {gains, gains}};

    r2::control::MotionOutput output{};
    double previous_steering = 0.0;
    for (std::size_t index = 0U; index < 200U; ++index) {
        output = controller.update({1.0, 0.5}, 0.0, 0.0, 0.001);
        CHECK(std::abs(output.steering_angle_rad - previous_steering) <=
              limits.maximum_steering_rate_rad_s * 0.001 + 1.0e-12);
        previous_steering = output.steering_angle_rad;
    }
    CHECK(output.status == r2::kinematics::KinematicsStatus::ok);
    CHECK(output.left_motor_command > 0.0);
    CHECK(output.right_motor_command > 0.0);
    CHECK(output.steering_angle_rad > 0.0);
    CHECK(output.steering_angle_rad <= geometry.maximum_road_wheel_angle_rad);

    output = controller.update({0.0, 1.0}, 0.0, 0.0, 0.001);
    CHECK(output.status ==
          r2::kinematics::KinematicsStatus::infeasible_stationary_turn);
    CHECK(output.left_motor_command == 0.0);
    CHECK(output.right_motor_command == 0.0);

    output = controller.update({NAN, 0.0}, 0.0, 0.0, 0.001);
    CHECK(output.status == r2::kinematics::KinematicsStatus::non_finite_input);
    controller.reset();

    constexpr r2::control::PidGains bad_gains{NAN, 0.0, 0.0, 0.0, 0.0};
    r2::control::MotionController invalid_controller{
        geometry, limits, {bad_gains, gains}};
    output = invalid_controller.update({1.0, 0.0}, 0.0, 0.0, 0.001);
    CHECK(output.status ==
          r2::kinematics::KinematicsStatus::invalid_configuration);
    CHECK(output.left_motor_command == 0.0);
    CHECK(output.right_motor_command == 0.0);

    output = controller.update({1.0, 0.0}, 101.0, 0.0, 0.001);
    CHECK(output.status == r2::kinematics::KinematicsStatus::non_finite_input);
}

void test_safety() {
    r2::safety::SafetyManager manager{};
    CHECK(manager.bridge_must_be_disabled());
    manager.begin_self_test();
    manager.complete_self_test(true);
    CHECK(manager.state() == r2::safety::SafetyState::standby);
    CHECK(manager.arm());
    CHECK(manager.activate());
    CHECK(manager.motion_permitted());

    const r2::safety::SafetyInputs healthy{
        false, true, true, true, true, true, true, true, false, true, true, false};
    manager.evaluate(healthy);
    CHECK(manager.motion_permitted());

    auto stale = healthy;
    stale.command_fresh = false;
    manager.evaluate(stale);
    CHECK(manager.state() == r2::safety::SafetyState::controlled_stop);
    CHECK(manager.controlled_stop_required());
    CHECK(!manager.bridge_must_be_disabled());

    stale.motion_stopped = true;
    manager.evaluate(stale);
    CHECK(manager.state() == r2::safety::SafetyState::standby);
    CHECK(manager.bridge_must_be_disabled());
    manager.evaluate(healthy);
    CHECK(manager.arm());
    CHECK(manager.activate());

    auto estop = healthy;
    estop.emergency_stop_asserted = true;
    manager.evaluate(estop);
    CHECK(manager.state() == r2::safety::SafetyState::fault_latched);
    CHECK(manager.bridge_must_be_disabled());
    CHECK(!manager.clear_recoverable_faults(true));
    manager.evaluate(healthy);
    CHECK(manager.clear_recoverable_faults(true));
    CHECK(manager.state() == r2::safety::SafetyState::standby);

    r2::safety::SafetyManager illegal{};
    CHECK(!illegal.disarm());
    CHECK(!illegal.clear_recoverable_faults(true));
    illegal.begin_self_test();
    illegal.complete_self_test(false);
    CHECK(illegal.state() == r2::safety::SafetyState::fault_latched);
    illegal.evaluate(healthy);
    CHECK(!illegal.clear_recoverable_faults(true));
    CHECK(!illegal.request_firmware_update());
}

void test_configuration_rollback() {
    MemoryFlash flash{};
    flash.bytes.fill(0xFFU);
    r2::application::ConfigurationStore store{flash, {0U, 2'048U}};
    r2::application::PlatformConfiguration loaded{};
    CHECK(!store.load(loaded));
    CHECK(!loaded.calibrated);
    CHECK(r2::application::valid_configuration(loaded));

    auto configuration = calibrated_configuration();
    CHECK(store.commit(configuration));
    CHECK(store.load(loaded));
    CHECK(loaded.generation == 1U);
    CHECK(loaded.calibrated);
    CHECK(near(loaded.wheelbase_m, configuration.wheelbase_m, 1.0e-6));

    configuration.maximum_speed_mps = 1.2F;
    flash.fail_next_write = true;
    CHECK(!store.commit(configuration));
    CHECK(store.load(loaded));
    CHECK(loaded.generation == 1U);
    CHECK(near(loaded.maximum_speed_mps, 1.0, 1.0e-6));

    flash.fail_after_bytes = 24U;
    CHECK(!store.commit(configuration));
    CHECK(store.load(loaded));
    CHECK(loaded.generation == 1U);

    CHECK(store.commit(configuration));
    CHECK(store.load(loaded));
    CHECK(loaded.generation == 2U);
    CHECK(near(loaded.maximum_speed_mps, 1.2, 1.0e-6));

    MemoryFlash conflicting_flash{};
    conflicting_flash.bytes.fill(0xFFU);
    r2::application::ConfigurationStore conflicting_store{
        conflicting_flash, {0U, 2'048U}};
    configuration.maximum_speed_mps = 1.0F;
    CHECK(conflicting_store.commit(configuration));
    std::memcpy(conflicting_flash.bytes.data() + 2'048U,
                conflicting_flash.bytes.data(),
                r2::application::kConfigurationImageBytes);
    const float conflicting_speed = 1.4F;
    std::uint32_t speed_bits = 0U;
    std::memcpy(&speed_bits, &conflicting_speed, sizeof(speed_bits));
    for (std::size_t index = 0U; index < 4U; ++index) {
        conflicting_flash.bytes[2'048U + 28U + index] =
            static_cast<std::uint8_t>(speed_bits >> (index * 8U));
    }
    const auto crc = r2::protocol::crc32c(
        conflicting_flash.bytes.data() + 2'048U, 76U);
    for (std::size_t index = 0U; index < 4U; ++index) {
        conflicting_flash.bytes[2'048U + 76U + index] =
            static_cast<std::uint8_t>(crc >> (index * 8U));
    }
    CHECK(!conflicting_store.load(loaded));
    CHECK(!loaded.calibrated);

    r2::application::ConfigurationStore overlapping_store{
        conflicting_flash, {0U, 128U}};
    CHECK(!overlapping_store.commit(configuration));
}

void test_diagnostics() {
    r2::diagnostics::TimingHistogram histogram{};
    histogram.observe(5U, 10U);
    histogram.observe(11U, 10U);
    CHECK(histogram.samples() == 2U);
    CHECK(histogram.deadline_misses() == 1U);
    CHECK(histogram.maximum_us() == 11U);
}

}  // namespace

int main() {
    test_crc_and_protocol();
    test_kinematics();
    test_control();
    test_motion_controller_composition();
    test_safety();
    test_configuration_rollback();
    test_diagnostics();
    if (failures != 0) {
        std::fprintf(stderr, "%d test assertion(s) failed\n", failures);
        return 1;
    }
    std::puts("all r2 platform tests passed");
    return 0;
}
