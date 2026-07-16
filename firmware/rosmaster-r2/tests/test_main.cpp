#include "r2/application/configuration.hpp"
#include "r2/boot/image_verifier.hpp"
#include "r2/control/motion_controller.hpp"
#include "r2/diagnostics/metrics.hpp"
#include "r2/kinematics/ackermann.hpp"
#include "r2/protocol/wire.hpp"
// Included here (not just from wire.cpp) so the MAC known-answer test can pin the
// SHA-256 / HMAC-SHA-256 primitive against fixed vectors — see test_mac_known_answer_vectors().
#include "r2/protocol/mac.hpp"
#include "r2/safety/safety_manager.hpp"
// Shared decoder fuzz oracle (fuzz/ is on this target's include path); the same
// decode_one() drives the libFuzzer target in fuzz/decode_fuzz_libfuzzer.cpp.
#include "decode_fuzz.hpp"

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

    // Named field assignment (not a positional aggregate list): value-init zeroes
    // every field to the fault-asserting `false`, then only the healthy signals are
    // set true by name. Resilient to future SafetyInputs field add/reorder — a new
    // field defaults to the safe value instead of silently shifting the columns.
    const r2::safety::SafetyInputs healthy = [] {
        r2::safety::SafetyInputs in{};
        in.command_fresh = true;
        in.battery_voltage_safe = true;
        in.battery_current_safe = true;
        in.thermal_safe = true;
        in.encoder_plausible = true;
        in.steering_plausible = true;
        in.imu_sane = true;
        in.control_deadline_met = true;
        in.communication_healthy = true;
        in.supply_stable = true;
        in.watchdog_healthy = true;
        in.configuration_valid = true;
        return in;
    }();
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

    // H3: brownout is wired and fails closed (latched + bridge disabled).
    r2::safety::SafetyManager brownout_mgr{};
    brownout_mgr.begin_self_test();
    brownout_mgr.complete_self_test(true);
    CHECK(brownout_mgr.arm());
    CHECK(brownout_mgr.activate());
    auto brownout = healthy;
    brownout.supply_stable = false;
    brownout_mgr.evaluate(brownout);
    CHECK(brownout_mgr.state() == r2::safety::SafetyState::fault_latched);
    CHECK(brownout_mgr.bridge_must_be_disabled());

    // H3: watchdog precursor is wired and fails closed (latched + bridge disabled).
    r2::safety::SafetyManager watchdog_mgr{};
    watchdog_mgr.begin_self_test();
    watchdog_mgr.complete_self_test(true);
    CHECK(watchdog_mgr.arm());
    CHECK(watchdog_mgr.activate());
    auto watchdog = healthy;
    watchdog.watchdog_healthy = false;
    watchdog_mgr.evaluate(watchdog);
    CHECK(watchdog_mgr.state() == r2::safety::SafetyState::fault_latched);
    CHECK(watchdog_mgr.bridge_must_be_disabled());

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

void test_safety_configuration_invalid() {
    // H5: an uncalibrated configuration (factory_defaults() has calibrated=false)
    // must surface as Fault::configuration_invalid, prevent arming, disable the
    // bridge, and be unrecoverable without a reset + POST.
    const auto defaults = r2::application::factory_defaults();
    CHECK(!defaults.calibrated);
    // factory_defaults() is structurally valid (schema and timeout in range) but
    // not calibrated; the gate is in evaluate(), not valid_configuration().
    CHECK(r2::application::valid_configuration(defaults));

    r2::safety::SafetyManager mgr{};
    mgr.begin_self_test();
    mgr.complete_self_test(true);
    CHECK(mgr.state() == r2::safety::SafetyState::standby);

    // Build inputs that are otherwise fully healthy but report an uncalibrated
    // configuration (configuration_valid=false).
    const r2::safety::SafetyInputs uncalibrated = [] {
        r2::safety::SafetyInputs in{};
        in.command_fresh = true;
        in.battery_voltage_safe = true;
        in.battery_current_safe = true;
        in.thermal_safe = true;
        in.encoder_plausible = true;
        in.steering_plausible = true;
        in.imu_sane = true;
        in.control_deadline_met = true;
        in.communication_healthy = true;
        in.supply_stable = true;
        in.watchdog_healthy = true;
        in.configuration_valid = false;
        return in;
    }();

    mgr.evaluate(uncalibrated);

    // Fault must be both active and latched.
    CHECK((mgr.active_faults() & r2::safety::bit(r2::safety::Fault::configuration_invalid)) != 0U);
    CHECK((mgr.latched_faults() & r2::safety::bit(r2::safety::Fault::configuration_invalid)) != 0U);

    // State machine must be in fault_latched; arming and motion are refused.
    CHECK(mgr.state() == r2::safety::SafetyState::fault_latched);
    CHECK(!mgr.arm());
    CHECK(!mgr.motion_permitted());
    CHECK(mgr.bridge_must_be_disabled());

    // configuration_invalid requires a reset + POST to clear — physical ack alone
    // is insufficient (matches the requires_reset_and_post policy).
    CHECK(!mgr.clear_recoverable_faults(true));

    // The configuration_invalid latch must persist across a subsequent fully-healthy
    // evaluation — active faults clear, but the latched fault still requires reset+POST.
    const r2::safety::SafetyInputs healthy = [] {
        r2::safety::SafetyInputs in{};
        in.command_fresh = true;
        in.battery_voltage_safe = true;
        in.battery_current_safe = true;
        in.thermal_safe = true;
        in.encoder_plausible = true;
        in.steering_plausible = true;
        in.imu_sane = true;
        in.control_deadline_met = true;
        in.communication_healthy = true;
        in.supply_stable = true;
        in.watchdog_healthy = true;
        in.configuration_valid = true;
        return in;
    }();
    mgr.evaluate(healthy);
    CHECK(!mgr.clear_recoverable_faults(true));
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

void test_mac_known_answer_vectors() {
    // Known-answer tests pin the SHA-256 / HMAC-SHA-256 primitive against fixed
    // published vectors. Without these, every encode<->decode roundtrip in
    // test_mac_authentication() shares the same hmac_sha256_truncated(), so a
    // deterministic-but-wrong hash would still pass every one of them. These KATs
    // catch that class of defect (a wrong constant, shift, or endianness bug).

    // KAT-1: SHA-256("abc") — NIST FIPS 180-2 example B.1. Pins the hash core
    // independently of the HMAC/wire layers.
    {
        const std::array<std::uint8_t, 3U> msg{{'a', 'b', 'c'}};
        r2::protocol::internal::Sha256Ctx ctx{};
        r2::protocol::internal::sha256_init(ctx);
        r2::protocol::internal::sha256_update(ctx, msg.data(), msg.size());
        const auto digest = r2::protocol::internal::sha256_finalize(ctx);
        const std::array<std::uint8_t, 32U> expected{{
            0xbaU, 0x78U, 0x16U, 0xbfU, 0x8fU, 0x01U, 0xcfU, 0xeaU,
            0x41U, 0x41U, 0x40U, 0xdeU, 0x5dU, 0xaeU, 0x22U, 0x23U,
            0xb0U, 0x03U, 0x61U, 0xa3U, 0x96U, 0x17U, 0x7aU, 0x9cU,
            0xb4U, 0x10U, 0xffU, 0x61U, 0xf2U, 0x00U, 0x15U, 0xadU}};
        CHECK(digest == expected);
    }

    // KAT-2: HMAC-SHA-256 (truncated to 128 bits) — RFC 4231 Test Case 2.
    // key = "Jefe", data = "what do ya want for nothing?".
    // hmac_sha256_truncated takes a fixed 32-byte key; placing the 4-byte "Jefe"
    // in the low bytes of a zero-filled 32-byte array produces the identical HMAC
    // key block K0 ("Jefe" followed by zeros to the 64-byte block) as a native
    // 4-byte key would, so RFC 4231's expected tag applies unchanged. Full tag:
    //   5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843
    // truncated to the first 16 bytes => 5bdcc146bf60754e6a042426089575c7.
    {
        std::array<std::uint8_t, r2::protocol::kMacKeySize> key{};
        key[0] = 0x4aU;  // 'J'
        key[1] = 0x65U;  // 'e'
        key[2] = 0x66U;  // 'f'
        key[3] = 0x65U;  // 'e'
        const std::array<std::uint8_t, 28U> msg{{
            'w', 'h', 'a', 't', ' ', 'd', 'o', ' ', 'y', 'a', ' ', 'w', 'a', 'n',
            't', ' ', 'f', 'o', 'r', ' ', 'n', 'o', 't', 'h', 'i', 'n', 'g', '?'}};
        const auto tag = r2::protocol::internal::hmac_sha256_truncated(
            key, msg.data(), msg.size());
        const std::array<std::uint8_t, 16U> expected{{
            0x5bU, 0xdcU, 0xc1U, 0x46U, 0xbfU, 0x60U, 0x75U, 0x4eU,
            0x6aU, 0x04U, 0x24U, 0x26U, 0x08U, 0x95U, 0x75U, 0xc7U}};
        CHECK(tag == expected);
    }
}

void test_mac_authentication() {
    // Provisioned per-link key (32-byte, non-zero for a realistic test)
    std::array<std::uint8_t, r2::protocol::kMacKeySize> key{};
    for (std::size_t i = 0U; i < key.size(); ++i) {
        key[i] = static_cast<std::uint8_t>(i + 1U);
    }

    // ── T1: valid roundtrip ────────────────────────────────────────────────
    r2::protocol::Frame frame{};
    frame.type            = r2::protocol::MessageType::motion_command;
    frame.flags           = 0U;
    frame.sequence        = 42U;
    frame.source_time_us  = 100'000U;
    frame.payload_length  = 4U;
    frame.payload[0]      = 0xAAU;
    frame.payload[1]      = 0xBBU;
    frame.payload[2]      = 0xCCU;
    frame.payload[3]      = 0xDDU;

    r2::protocol::EncodedFrame enc{};
    CHECK(r2::protocol::encode_authenticated(frame, key, enc));
    CHECK(enc.length > 0U);
    CHECK(enc.bytes[enc.length - 1U] == 0U);

    r2::protocol::Frame dec{};
    CHECK(r2::protocol::decode_authenticated(enc.bytes.data(), enc.length, key, dec) ==
          r2::protocol::DecodeStatus::ok);
    // Payload is delivered without the appended MAC tag
    CHECK(dec.type == frame.type);
    CHECK(dec.sequence == frame.sequence);
    CHECK(dec.source_time_us == frame.source_time_us);
    CHECK(dec.payload_length == frame.payload_length);
    CHECK(std::memcmp(dec.payload.data(), frame.payload.data(), frame.payload_length) == 0);
    // AUTH_TAG flag is consumed by verification and cleared in the output
    CHECK((dec.flags & r2::protocol::kFlagAuthTag) == 0U);

    // Plain decode must refuse AUTH_TAG frames and leave output zero-initialised.
    r2::protocol::Frame plain_decode_of_auth{};
    plain_decode_of_auth.type = r2::protocol::MessageType::motion_command;
    plain_decode_of_auth.payload[0] = 0x5AU;
    CHECK(r2::protocol::decode(enc.bytes.data(), enc.length, plain_decode_of_auth) ==
          r2::protocol::DecodeStatus::auth_required);
    CHECK(plain_decode_of_auth.sequence == 0U);
    CHECK(plain_decode_of_auth.payload_length == 0U);
    CHECK(plain_decode_of_auth.payload[0] == 0U);

    // ── T2: wrong key → auth_mac_mismatch ─────────────────────────────────
    std::array<std::uint8_t, r2::protocol::kMacKeySize> wrong_key{};
    wrong_key[0] = 0xFFU;  // differs from key[0] = 0x01
    r2::protocol::Frame dec2{};
    CHECK(r2::protocol::decode_authenticated(enc.bytes.data(), enc.length, wrong_key, dec2) ==
          r2::protocol::DecodeStatus::auth_mac_mismatch);
    // Output must not be populated on a failed MAC check
    CHECK(dec2.sequence == 0U);
    CHECK(dec2.payload_length == 0U);

    // ── T3: forged frame – valid CRC32C but zero/wrong MAC tag ────────────
    // Build with encode() (no HMAC) but with AUTH_TAG flag and a fake tag
    // payload. CRC will be valid; MAC will not match.
    r2::protocol::Frame forged{};
    forged.type           = r2::protocol::MessageType::motion_command;
    forged.flags          = r2::protocol::kFlagAuthTag;
    forged.sequence       = 42U;
    forged.source_time_us = 100'000U;
    // Payload = kMacTagSize zeros (a plausible tag slot, but all-zero ≠ real HMAC)
    forged.payload_length = static_cast<std::uint16_t>(r2::protocol::kMacTagSize);
    // payload is zero-initialised (wrong tag)

    r2::protocol::EncodedFrame forged_enc{};
    CHECK(r2::protocol::encode(forged, forged_enc));  // CRC-only, no HMAC
    r2::protocol::Frame forged_dec{};
    CHECK(r2::protocol::decode_authenticated(
              forged_enc.bytes.data(), forged_enc.length, key, forged_dec) ==
          r2::protocol::DecodeStatus::auth_mac_mismatch);

    // ── T4: absent AUTH_TAG flag → auth_mac_mismatch (fail-closed) ────────
    r2::protocol::Frame plain{};
    plain.type            = r2::protocol::MessageType::motion_command;
    plain.sequence        = 42U;
    plain.source_time_us  = 100'000U;
    plain.payload_length  = 4U;
    r2::protocol::EncodedFrame plain_enc{};
    CHECK(r2::protocol::encode(plain, plain_enc));
    r2::protocol::Frame plain_dec{};
    CHECK(r2::protocol::decode_authenticated(
              plain_enc.bytes.data(), plain_enc.length, key, plain_dec) ==
          r2::protocol::DecodeStatus::auth_mac_mismatch);
    CHECK(r2::protocol::decode(plain_enc.bytes.data(), plain_enc.length, plain_dec) ==
          r2::protocol::DecodeStatus::ok);
    CHECK(plain_dec.payload_length == plain.payload_length);
    CHECK(plain_dec.sequence == plain.sequence);

    // ── T5: sequence binding – different sequences produce different MACs ──
    r2::protocol::Frame frame_s1 = frame;
    r2::protocol::Frame frame_s2 = frame;
    frame_s2.sequence = frame.sequence + 1U;

    r2::protocol::EncodedFrame enc_s1{}, enc_s2{};
    CHECK(r2::protocol::encode_authenticated(frame_s1, key, enc_s1));
    CHECK(r2::protocol::encode_authenticated(frame_s2, key, enc_s2));

    // Same payload and key but different sequence → different authenticated bytes
    CHECK(enc_s1.length == enc_s2.length);
    CHECK(std::memcmp(enc_s1.bytes.data(), enc_s2.bytes.data(), enc_s1.length) != 0);

    // Both decode correctly under the correct key, yielding the original sequences
    r2::protocol::Frame out_s1{}, out_s2{};
    CHECK(r2::protocol::decode_authenticated(enc_s1.bytes.data(), enc_s1.length, key, out_s1) ==
          r2::protocol::DecodeStatus::ok);
    CHECK(out_s1.sequence == frame_s1.sequence);
    CHECK(r2::protocol::decode_authenticated(enc_s2.bytes.data(), enc_s2.length, key, out_s2) ==
          r2::protocol::DecodeStatus::ok);
    CHECK(out_s2.sequence == frame_s2.sequence);

    // Decode s1 bytes with s2's key (wrong key) must fail
    CHECK(r2::protocol::decode_authenticated(enc_s1.bytes.data(), enc_s1.length, wrong_key, out_s1) ==
          r2::protocol::DecodeStatus::auth_mac_mismatch);

    // ── T6: encode_authenticated rejects oversized payload ────────────────
    r2::protocol::Frame big{};
    big.type           = r2::protocol::MessageType::motion_command;
    big.payload_length =
        static_cast<std::uint16_t>(r2::protocol::kMaximumPayload - r2::protocol::kMacTagSize + 1U);
    r2::protocol::EncodedFrame big_enc{};
    CHECK(!r2::protocol::encode_authenticated(big, key, big_enc));
    CHECK(big_enc.length == 0U);
}

void test_image_verifier_failclosed() {
    // H4: a default-constructed / zero-initialized image verdict MUST read as a
    // rejection, never as `accepted`. (The static_assert in the header enforces
    // this at compile time; this keeps the header in a compiled TU so the guard
    // stays live, and documents the contract for a future verify() backend.)
    const r2::boot::VerificationResult defaulted{};
    CHECK(defaulted == r2::boot::VerificationResult::rejected);
    CHECK(defaulted != r2::boot::VerificationResult::accepted);
    CHECK(static_cast<std::uint8_t>(r2::boot::VerificationResult::rejected) == 0U);
}

void test_decode_fuzz() {
    // Deterministic, sanitizer-exercised sweep of the untrusted decode entry
    // points — the per-PR complement to the libFuzzer target
    // (fuzz/decode_fuzz_libfuzzer.cpp). A fixed-seed xorshift generates both
    // raw-random buffers and byte-mutated valid frames; every input must satisfy
    // the decode invariants (no partial frame on rejection, bounded output, no
    // AUTH_TAG leak) and must not trip ASAN/UBSAN. Reproducible: same seed every
    // run, so a discovered crash is replayable.
    std::uint64_t state = 0x9E3779B97F4A7C15ULL;
    const auto next = [&state]() noexcept -> std::uint64_t {
        state ^= state << 13U;
        state ^= state >> 7U;
        state ^= state << 17U;
        return state;
    };

    std::array<std::uint8_t, r2::protocol::kMacKeySize> key{};
    for (std::size_t i = 0U; i < key.size(); ++i) {
        key[i] = static_cast<std::uint8_t>(0xA5U ^ static_cast<std::uint8_t>(i));
    }

    // Two valid encoded frames used as mutation bases: a plain frame and an
    // authenticated one, so mutations explore both decode paths near-valid.
    r2::protocol::Frame base{};
    base.type = r2::protocol::MessageType::motion_command;
    base.sequence = 7U;
    base.source_time_us = 1234U;
    base.payload_length = 8U;
    for (std::size_t i = 0U; i < 8U; ++i) {
        base.payload[i] = static_cast<std::uint8_t>(i + 1U);
    }
    r2::protocol::EncodedFrame enc_plain{};
    r2::protocol::EncodedFrame enc_auth{};
    CHECK(r2::protocol::encode(base, enc_plain));
    CHECK(r2::protocol::encode_authenticated(base, key, enc_auth));

    std::array<std::uint8_t, 320U> buf{};
    bool all_invariants_hold = true;
    for (int iteration = 0; iteration < 20000; ++iteration) {
        std::size_t length = 0U;
        const std::uint64_t mode = next() % 3U;
        if (mode == 0U) {
            length = static_cast<std::size_t>(next() % (buf.size() + 1U));
            for (std::size_t i = 0U; i < length; ++i) {
                buf[i] = static_cast<std::uint8_t>(next());
            }
        } else {
            const r2::protocol::EncodedFrame& src = (mode == 1U) ? enc_plain : enc_auth;
            length = src.length;
            for (std::size_t i = 0U; i < length; ++i) {
                buf[i] = src.bytes[i];
            }
            const std::uint64_t mutations = (next() % 4U) + 1U;
            for (std::uint64_t m = 0U; m < mutations && length > 0U; ++m) {
                const std::size_t pos = static_cast<std::size_t>(next() % length);
                buf[pos] = static_cast<std::uint8_t>(next());
            }
        }
        all_invariants_hold =
            all_invariants_hold && r2::protocol::fuzz::decode_one(buf.data(), length);
    }
    CHECK(all_invariants_hold);

    // The unmutated valid frames must still round-trip cleanly through their
    // respective decode paths (guards against an over-strict oracle).
    r2::protocol::Frame out{};
    CHECK(r2::protocol::decode(enc_plain.bytes.data(), enc_plain.length, out) ==
          r2::protocol::DecodeStatus::ok);
    CHECK(r2::protocol::decode_authenticated(enc_auth.bytes.data(), enc_auth.length, key, out) ==
          r2::protocol::DecodeStatus::ok);
}

}  // namespace

int main() {
    test_crc_and_protocol();
    test_kinematics();
    test_control();
    test_motion_controller_composition();
    test_safety();
    test_safety_configuration_invalid();
    test_configuration_rollback();
    test_diagnostics();
    test_image_verifier_failclosed();
    test_mac_known_answer_vectors();
    test_mac_authentication();
    test_decode_fuzz();
    if (failures != 0) {
        std::fprintf(stderr, "%d test assertion(s) failed\n", failures);
        return 1;
    }
    std::puts("all r2 platform tests passed");
    return 0;
}
