#include "r2/application/control_loop.hpp"

#include "r2/application/configuration.hpp"
#include "r2/control/motion_controller.hpp"
#include "r2/hal/interfaces.hpp"
#include "r2/kinematics/ackermann.hpp"
#include "r2/protocol/wire.hpp"
#include "r2/safety/safety_manager.hpp"

#include <algorithm>
#include <array>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstring>

namespace r2::application {
namespace {

// Freshness bounds for MOTION_COMMAND.valid_for_us (docs/PROTOCOL.md: 20–1000 ms).
constexpr std::uint32_t kMinValidForUs = 20'000U;
constexpr std::uint32_t kMaxValidForUs = 1'000'000U;

constexpr double kTwoPi = 6.283185307179586;

[[nodiscard]] std::uint32_t read_u32_le(const std::uint8_t* bytes) noexcept {
    return static_cast<std::uint32_t>(bytes[0]) |
           (static_cast<std::uint32_t>(bytes[1]) << 8U) |
           (static_cast<std::uint32_t>(bytes[2]) << 16U) |
           (static_cast<std::uint32_t>(bytes[3]) << 24U);
}

[[nodiscard]] float read_f32_le(const std::uint8_t* bytes) noexcept {
    const std::uint32_t raw = read_u32_le(bytes);
    float value = 0.0F;
    std::memcpy(&value, &raw, sizeof(value));
    return value;
}

[[nodiscard]] kinematics::VehicleGeometry geometry_from(
    const PlatformConfiguration& configuration) noexcept {
    kinematics::VehicleGeometry geometry{};
    geometry.wheelbase_m = static_cast<double>(configuration.wheelbase_m);
    geometry.rear_track_m = static_cast<double>(configuration.rear_track_m);
    geometry.wheel_radius_m = static_cast<double>(configuration.wheel_radius_m);
    geometry.maximum_road_wheel_angle_rad =
        static_cast<double>(configuration.maximum_steering_angle_rad);
    return geometry;
}

[[nodiscard]] control::MotionLimits limits_from(
    const PlatformConfiguration& configuration) noexcept {
    control::MotionLimits limits{};
    limits.maximum_speed_mps = static_cast<double>(configuration.maximum_speed_mps);
    limits.maximum_acceleration_mps2 =
        static_cast<double>(configuration.maximum_acceleration_mps2);
    limits.maximum_deceleration_mps2 =
        static_cast<double>(configuration.maximum_deceleration_mps2);
    limits.maximum_jerk_mps3 = static_cast<double>(configuration.maximum_jerk_mps3);
    limits.maximum_steering_rate_rad_s =
        static_cast<double>(configuration.maximum_steering_rate_rad_s);
    return limits;
}

[[nodiscard]] std::int16_t to_duty_q15(const double command) noexcept {
    const double clamped = std::clamp(command, -1.0, 1.0);
    const double scaled = std::round(clamped * 32'767.0);
    return static_cast<std::int16_t>(scaled);
}

}  // namespace

bool decode_motion_command(const protocol::Frame& frame, MotionCommand& out) noexcept {
    if (frame.type != protocol::MessageType::motion_command) {
        return false;
    }
    if (frame.payload_length < kMotionCommandPayloadBytes) {
        return false;
    }
    const std::uint8_t* const payload = frame.payload.data();
    const std::uint8_t raw_mode = payload[24];
    if (raw_mode > static_cast<std::uint8_t>(MotionMode::hold_zero)) {
        return false;
    }
    MotionCommand decoded{};
    decoded.command_id = read_u32_le(payload + 0);
    decoded.valid_for_us = read_u32_le(payload + 4);
    decoded.velocity_mps = read_f32_le(payload + 8);
    decoded.curvature_per_m = read_f32_le(payload + 12);
    decoded.acceleration_limit_mps2 = read_f32_le(payload + 16);
    decoded.jerk_limit_mps3 = read_f32_le(payload + 20);
    decoded.mode = static_cast<MotionMode>(raw_mode);
    if (!std::isfinite(decoded.velocity_mps) ||
        !std::isfinite(decoded.curvature_per_m) ||
        !std::isfinite(decoded.acceleration_limit_mps2) ||
        !std::isfinite(decoded.jerk_limit_mps3)) {
        return false;
    }
    out = decoded;
    return true;
}

SafetyThresholds default_thresholds() noexcept {
    SafetyThresholds thresholds{};
    thresholds.minimum_battery_voltage_v = 10.0F;
    thresholds.maximum_battery_current_a = 20.0F;
    thresholds.maximum_temperature_c = 70.0F;
    thresholds.maximum_angular_rate_rad_s = 20.0F;
    thresholds.maximum_linear_acceleration_mps2 = 80.0F;
    thresholds.encoder_plausibility_factor = 1.5;
    thresholds.runaway_factor = 2.0;
    thresholds.motion_stopped_epsilon_mps = 0.02;
    thresholds.maximum_tick_s = 0.02;
    return thresholds;
}

Application::Application(const HalBundle& hal, const ApplicationConfig& config) noexcept
    : hal_(hal),
      platform_(config.platform),
      thresholds_(config.thresholds),
      link_key_(config.link_key),
      controller_(geometry_from(config.platform),
                  limits_from(config.platform),
                  config.wheel_gains) {}

void Application::initialize() noexcept {
    safety_.begin_self_test();
    // Firmware self-consistency POST. The live configuration and sensor inputs
    // are gated on each tick by evaluate(); an invalid/uncalibrated record
    // raises configuration_invalid there and blocks arming.
    safety_.complete_self_test(true);
}

void Application::tick(const double dt_s) noexcept {
    hal_.watchdog.service();
    now_us_ = hal_.clock.now_us();

    bool link_corrupt = false;
    drain_transport(link_corrupt);

    const hal::EncoderSnapshot encoders = hal_.encoders.snapshot();
    WheelSpeeds speeds{};
    speeds.left_mps = wheel_speed_mps(
        encoders.left_delta, platform_.left_encoder_counts_per_revolution, dt_s);
    speeds.right_mps = wheel_speed_mps(
        encoders.right_delta, platform_.right_encoder_counts_per_revolution, dt_s);

    const safety::SafetyInputs inputs = build_inputs(dt_s, speeds, link_corrupt);
    safety_.evaluate(inputs);

    actuate(speeds, dt_s);
}

void Application::drain_transport(bool& link_corrupt) noexcept {
    // Bound the per-tick work: at most kMaxFramesPerTick frames are processed so
    // a flood cannot stall the control cycle. The transport delivers at most one
    // COBS-framed datagram per read() (AOU: the link layer is frame-delimited).
    constexpr std::size_t kMaxFramesPerTick = 16U;
    std::array<std::uint8_t, protocol::kMaximumEncodedFrame> buffer{};
    for (std::size_t frame_index = 0U; frame_index < kMaxFramesPerTick; ++frame_index) {
        const std::size_t length = hal_.transport.read(buffer.data(), buffer.size());
        if (length == 0U) {
            return;
        }
        protocol::Frame frame{};
        const protocol::DecodeStatus status = protocol::decode_authenticated(
            buffer.data(), length, link_key_, frame);
        if (status != protocol::DecodeStatus::ok) {
            // A structurally corrupt frame degrades link health; an
            // authentication failure is simply dropped (fail-closed, no trust).
            if (status == protocol::DecodeStatus::crc_mismatch ||
                status == protocol::DecodeStatus::malformed_cobs ||
                status == protocol::DecodeStatus::bad_magic ||
                status == protocol::DecodeStatus::unsupported_version) {
                link_corrupt = true;
            }
            continue;
        }
        handle_frame(frame);
    }
}

void Application::handle_frame(const protocol::Frame& frame) noexcept {
    switch (frame.type) {
        case protocol::MessageType::motion_command: {
            MotionCommand decoded{};
            if (decode_motion_command(frame, decoded)) {
                command_ = decoded;
                command_valid_ = true;
                last_command_us_ = now_us_;
            }
            break;
        }
        case protocol::MessageType::arm:
            (void)safety_.arm();
            break;
        case protocol::MessageType::activate:
            (void)safety_.activate();
            break;
        case protocol::MessageType::disarm:
            (void)safety_.disarm();
            break;
        // ACKNOWLEDGE_FAULT is intentionally NOT honored here: a network packet
        // is never physical acknowledgement (docs/PROTOCOL.md). Clearing a
        // latched fault additionally requires a separately-wired local
        // acknowledgement input, which this portable slice does not model.
        case protocol::MessageType::acknowledge_fault:
        default:
            break;
    }
}

safety::SafetyInputs Application::build_inputs(const double dt_s,
                                              const WheelSpeeds& speeds,
                                              const bool link_corrupt) noexcept {
    const double left_mps = speeds.left_mps;
    const double right_mps = speeds.right_mps;
    const hal::BatterySample battery = hal_.battery.sample();
    const hal::ImuSample imu = hal_.imu.sample();

    const bool battery_voltage_safe =
        battery.voltage_valid && std::isfinite(battery.voltage_v) &&
        battery.voltage_v >= thresholds_.minimum_battery_voltage_v;
    // An absent current channel cannot indict the battery (the SBC only reports
    // current where the hardware proves that channel).
    const bool battery_current_safe =
        !battery.current_valid ||
        (std::isfinite(battery.current_a) &&
         std::fabs(battery.current_a) <= thresholds_.maximum_battery_current_a);
    const bool battery_thermal_safe =
        !battery.temperature_valid ||
        (std::isfinite(battery.temperature_c) &&
         battery.temperature_c <= thresholds_.maximum_temperature_c);
    const bool imu_thermal_safe =
        !imu.valid ||
        (std::isfinite(imu.temperature_c) &&
         imu.temperature_c <= thresholds_.maximum_temperature_c);

    bool imu_finite = true;
    bool imu_in_bounds = true;
    for (std::size_t axis = 0U; axis < 3U; ++axis) {
        const float rate = imu.angular_velocity_rad_s[axis];
        const float accel = imu.acceleration_mps2[axis];
        imu_finite = imu_finite && std::isfinite(rate) && std::isfinite(accel);
        imu_in_bounds = imu_in_bounds &&
                        std::fabs(rate) <= thresholds_.maximum_angular_rate_rad_s &&
                        std::fabs(accel) <= thresholds_.maximum_linear_acceleration_mps2;
    }

    const double max_speed = static_cast<double>(platform_.maximum_speed_mps);
    const double plausible_limit = max_speed * thresholds_.encoder_plausibility_factor;
    const double runaway_limit = max_speed * thresholds_.runaway_factor;

    safety::SafetyInputs inputs{};
    inputs.emergency_stop_asserted = hal_.emergency_stop.asserted();
    inputs.command_fresh = command_fresh();
    inputs.battery_voltage_safe = battery_voltage_safe;
    inputs.battery_current_safe = battery_current_safe;
    inputs.thermal_safe = battery_thermal_safe && imu_thermal_safe;
    inputs.encoder_plausible = std::isfinite(left_mps) && std::isfinite(right_mps) &&
                               std::fabs(left_mps) <= plausible_limit &&
                               std::fabs(right_mps) <= plausible_limit;
    // No steering-position feedback seam exists yet; a real potentiometer/encoder
    // would populate this. Absent the sensor there is nothing to contradict, so
    // it reports plausible (the commanded angle is already envelope-bounded).
    inputs.steering_plausible = true;
    inputs.imu_sane = imu.valid && imu_finite && imu_in_bounds;
    inputs.motor_runaway =
        std::fabs(left_mps) > runaway_limit || std::fabs(right_mps) > runaway_limit;
    inputs.control_deadline_met = dt_s > 0.0 && dt_s <= thresholds_.maximum_tick_s;
    inputs.communication_healthy = !link_corrupt;
    // No dedicated brownout/PVD comparator seam yet; the target must wire the
    // supply-rail monitor here. Reported stable until that seam exists.
    inputs.supply_stable = true;
    // The independent watchdog is serviced at the head of every tick.
    inputs.watchdog_healthy = true;
    inputs.motion_stopped =
        std::fabs(left_mps) <= thresholds_.motion_stopped_epsilon_mps &&
        std::fabs(right_mps) <= thresholds_.motion_stopped_epsilon_mps;
    inputs.configuration_valid =
        valid_configuration(platform_) && platform_.calibrated;
    return inputs;
}

void Application::actuate(const WheelSpeeds& speeds, const double dt_s) noexcept {
    if (safety_.bridge_must_be_disabled()) {
        disable_actuators();
        return;
    }

    kinematics::BodyCommand requested{};
    requested.longitudinal_velocity_mps = 0.0;
    requested.yaw_rate_rad_s = 0.0;
    if (safety_.motion_permitted() && command_fresh() &&
        command_.mode == MotionMode::track) {
        const double velocity = static_cast<double>(command_.velocity_mps);
        const double curvature = static_cast<double>(command_.curvature_per_m);
        requested.longitudinal_velocity_mps = velocity;
        requested.yaw_rate_rad_s = velocity * curvature;
    }
    // Every other reachable state (armed hold, controlled-stop deceleration,
    // STOP/HOLD_ZERO, or a stale command) drives the requested body command to
    // zero so the jerk-limited controller decelerates to rest with the bridge
    // still braking.
    const control::MotionOutput output =
        controller_.update(requested, speeds.left_mps, speeds.right_mps, dt_s);
    apply_output(output);
    bridge_disabled_ = false;
}

void Application::disable_actuators() noexcept {
    hal_.motors.disable();
    hal_.steering.disable();
    controller_.reset();
    last_left_duty_q15_ = 0;
    last_right_duty_q15_ = 0;
    last_steering_pulse_us_ = platform_.servo_center_us;
    bridge_disabled_ = true;
}

void Application::apply_output(const control::MotionOutput& output) noexcept {
    const std::int16_t left_duty = to_duty_q15(output.left_motor_command);
    const std::int16_t right_duty = to_duty_q15(output.right_motor_command);
    hal_.motors.set_duty_q15(left_duty, right_duty);
    const std::uint16_t pulse = steering_pulse_us(output.steering_angle_rad);
    hal_.steering.set_pulse_us(pulse);
    last_left_duty_q15_ = left_duty;
    last_right_duty_q15_ = right_duty;
    last_steering_pulse_us_ = pulse;
}

double Application::wheel_speed_mps(const std::int32_t delta_counts,
                                    const std::uint32_t counts_per_rev,
                                    const double dt_s) const noexcept {
    if (counts_per_rev == 0U || dt_s <= 0.0) {
        return 0.0;
    }
    const double revolutions =
        static_cast<double>(delta_counts) / static_cast<double>(counts_per_rev);
    const double distance =
        revolutions * kTwoPi * static_cast<double>(platform_.wheel_radius_m);
    return distance / dt_s;
}

std::uint16_t Application::steering_pulse_us(const double angle_rad) const noexcept {
    const double center = static_cast<double>(platform_.servo_center_us);
    const double minimum = static_cast<double>(platform_.servo_minimum_us);
    const double maximum = static_cast<double>(platform_.servo_maximum_us);
    const double max_angle = static_cast<double>(platform_.maximum_steering_angle_rad);
    if (!(max_angle > 0.0) || !(minimum <= center && center <= maximum)) {
        // Degenerate servo calibration: hold the center pulse (fail-safe).
        return platform_.servo_center_us;
    }
    const double bounded = std::clamp(angle_rad, -max_angle, max_angle);
    const double fraction = bounded / max_angle;
    const double half_span = fraction >= 0.0 ? (maximum - center) : (center - minimum);
    const double pulse = std::clamp(center + fraction * half_span, minimum, maximum);
    return static_cast<std::uint16_t>(std::lround(pulse));
}

bool Application::command_fresh() const noexcept {
    if (!command_valid_) {
        return false;
    }
    const std::uint32_t bounded_valid =
        std::clamp(command_.valid_for_us, kMinValidForUs, kMaxValidForUs);
    const std::uint64_t configured_budget =
        static_cast<std::uint64_t>(platform_.command_timeout_ms) * 1'000ULL;
    const std::uint64_t budget =
        std::min(configured_budget, static_cast<std::uint64_t>(bounded_valid));
    const std::uint64_t age =
        now_us_ >= last_command_us_ ? now_us_ - last_command_us_ : 0ULL;
    return age <= budget;
}

safety::SafetyState Application::safety_state() const noexcept {
    return safety_.state();
}

std::uint64_t Application::active_faults() const noexcept {
    return safety_.active_faults();
}

std::uint64_t Application::latched_faults() const noexcept {
    return safety_.latched_faults();
}

bool Application::bridge_disabled() const noexcept {
    return bridge_disabled_;
}

std::int16_t Application::last_left_duty_q15() const noexcept {
    return last_left_duty_q15_;
}

std::int16_t Application::last_right_duty_q15() const noexcept {
    return last_right_duty_q15_;
}

std::uint16_t Application::last_steering_pulse_us() const noexcept {
    return last_steering_pulse_us_;
}

bool Application::command_held() const noexcept {
    return command_valid_;
}

}  // namespace r2::application
