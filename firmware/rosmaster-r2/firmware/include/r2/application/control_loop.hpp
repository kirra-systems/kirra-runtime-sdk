#pragma once

// Portable control-loop orchestrator for the ROSMASTER R2 MCU.
//
// The Application wires the abstract HAL seams (r2/hal/interfaces.hpp) to the
// SafetyManager, the MotionController and the authenticated R2CP decoder so
// that a single tick(dt_s) is the whole per-cycle safety/actuation pipeline.
// It is device-agnostic: it holds only references to the pure-virtual HAL
// interfaces, so the same object is driven by real silicon peripherals on the
// target and by mock seams in the host tests. No allocation, no exceptions.
//
// Fail-closed invariants enforced here (see docs/SAFETY_AND_PRODUCTION.md):
//   * A command reaches the state machine ONLY through decode_authenticated —
//     an unauthenticated or tampered frame is dropped and never actuates.
//   * Every actuation is gated on the SafetyManager: bridge_must_be_disabled()
//     hard-cuts the H-bridge and servo before any drive decision; motion is
//     issued only while motion_permitted() AND a fresh command is held.
//   * An uncalibrated / invalid configuration raises configuration_invalid and
//     immobilizes the platform (arming is refused, the bridge stays disabled).

#include "r2/application/configuration.hpp"
#include "r2/control/motion_controller.hpp"
#include "r2/hal/interfaces.hpp"
#include "r2/kinematics/ackermann.hpp"
#include "r2/protocol/wire.hpp"
#include "r2/safety/safety_manager.hpp"

#include <array>
#include <cstdint>

namespace r2::application {

// MOTION_COMMAND payload mode (docs/PROTOCOL.md §MOTION_COMMAND). STOP and
// HOLD_ZERO both request zero body velocity; TRACK requests the carried
// velocity/curvature pair. Any other raw value is rejected by the decoder.
enum class MotionMode : std::uint8_t {
    stop = 0U,
    track = 1U,
    hold_zero = 2U,
};

// Decoded MOTION_COMMAND payload. Mirrors the on-wire layout AFTER
// decode_authenticated() has stripped the trailing authentication tag:
//   command_id:u32, valid_for_us:u32, velocity_mps:f32, curvature_per_m:f32,
//   acceleration_limit_mps2:f32, jerk_limit_mps3:f32, mode:u8, reserved[3].
struct MotionCommand {
    std::uint32_t command_id{0U};
    std::uint32_t valid_for_us{0U};
    float velocity_mps{0.0F};
    float curvature_per_m{0.0F};
    float acceleration_limit_mps2{0.0F};
    float jerk_limit_mps3{0.0F};
    MotionMode mode{MotionMode::stop};
};

// Minimum decoded (tag-stripped) MOTION_COMMAND payload length in bytes.
inline constexpr std::size_t kMotionCommandPayloadBytes = 28U;

// Faithfully decode a verified MOTION_COMMAND frame. Returns false (leaving
// `out` untouched) for a wrong message type, a short payload, an unknown mode
// byte, or a non-finite scalar — fail-closed: a frame that cannot be decoded
// exactly is never turned into motion.
[[nodiscard]] bool decode_motion_command(const protocol::Frame& frame,
                                         MotionCommand& out) noexcept;

// Deployment safety thresholds that the SafetyInputs mapping needs but the
// PlatformConfiguration does not carry. These are calibration-owned; the
// default_thresholds() values are conservative placeholders and MUST be set
// per deployment. The *_v / *_a / *_c / *_rad_s fields are compared against the
// float HAL sample fields directly (kept float to avoid double-promotion); the
// factor/epsilon/budget fields participate in the double-precision speed math.
struct SafetyThresholds {
    float minimum_battery_voltage_v{0.0F};
    float maximum_battery_current_a{0.0F};
    float maximum_temperature_c{0.0F};
    float maximum_angular_rate_rad_s{0.0F};
    float maximum_linear_acceleration_mps2{0.0F};
    // Encoder-implied wheel speed above maximum_speed_mps * this factor is
    // treated as implausible (a sensor/gearing fault).
    double encoder_plausibility_factor{0.0};
    // Encoder-implied wheel speed above maximum_speed_mps * this factor is
    // treated as motor runaway (bridge hard-disable).
    double runaway_factor{0.0};
    // |wheel speed| at or below this is "stopped" (lets controlled_stop settle
    // back to standby).
    double motion_stopped_epsilon_mps{0.0};
    // A tick whose dt exceeds this budget missed its control deadline.
    double maximum_tick_s{0.0};
};

// Conservative default thresholds. NOT a production calibration — a deployment
// must override these with values validated against its own hardware.
[[nodiscard]] SafetyThresholds default_thresholds() noexcept;

// A pair of measured rear-wheel ground speeds derived from the encoder deltas.
// Bundled so it travels as one value through the tick, rather than as two
// adjacent doubles that a caller could transpose.
struct WheelSpeeds {
    double left_mps{0.0};
    double right_mps{0.0};
};

// The HAL seams the control loop drives. Held by reference; every peripheral
// must outlive the Application (statically allocated on the target).
struct HalBundle {
    hal::MonotonicClock& clock;
    hal::MotorBridge& motors;
    hal::SteeringActuator& steering;
    hal::EncoderBank& encoders;
    hal::ImuDevice& imu;
    hal::BatteryMonitor& battery;
    hal::EmergencyStopInput& emergency_stop;
    hal::IndependentWatchdog& watchdog;
    hal::Transport& transport;
};

// Boot-time configuration for the Application. PlatformConfiguration carries the
// kinematics/envelope; wheel_gains and thresholds are calibration; link_key is
// the per-link HMAC-SHA-256 key that authenticates every inbound command.
struct ApplicationConfig {
    PlatformConfiguration platform{};
    control::WheelPidGains wheel_gains{};
    SafetyThresholds thresholds{};
    std::array<std::uint8_t, protocol::kMacKeySize> link_key{};
};

class Application {
public:
    Application(const HalBundle& hal, const ApplicationConfig& config) noexcept;

    // Run power-on self-test and settle to standby. Must be called once before
    // ticking. A future slice will drive a real hardware POST here; today it
    // asserts firmware self-consistency and lets evaluate() gate on the live
    // configuration/fault inputs.
    void initialize() noexcept;

    // One control cycle: service the watchdog, drain and authenticate inbound
    // frames, sample the sensors, evaluate the safety state, and actuate (or
    // fail-closed disable). dt_s is the elapsed time since the previous tick.
    void tick(double dt_s) noexcept;

    // Observability accessors (used by the host tests and by a future telemetry
    // path). None of these mutate the loop.
    [[nodiscard]] safety::SafetyState safety_state() const noexcept;
    [[nodiscard]] std::uint64_t active_faults() const noexcept;
    [[nodiscard]] std::uint64_t latched_faults() const noexcept;
    [[nodiscard]] bool bridge_disabled() const noexcept;
    [[nodiscard]] std::int16_t last_left_duty_q15() const noexcept;
    [[nodiscard]] std::int16_t last_right_duty_q15() const noexcept;
    [[nodiscard]] std::uint16_t last_steering_pulse_us() const noexcept;
    [[nodiscard]] bool command_held() const noexcept;

private:
    void drain_transport(bool& link_corrupt) noexcept;
    void handle_frame(const protocol::Frame& frame) noexcept;
    [[nodiscard]] safety::SafetyInputs build_inputs(double dt_s,
                                                    const WheelSpeeds& speeds,
                                                    bool link_corrupt) noexcept;
    void actuate(const WheelSpeeds& speeds, double dt_s) noexcept;
    void disable_actuators() noexcept;
    void apply_output(const control::MotionOutput& output) noexcept;
    [[nodiscard]] double wheel_speed_mps(std::int32_t delta_counts,
                                         std::uint32_t counts_per_rev,
                                         double dt_s) const noexcept;
    [[nodiscard]] std::uint16_t steering_pulse_us(double angle_rad) const noexcept;
    [[nodiscard]] bool command_fresh() const noexcept;

    HalBundle hal_;
    PlatformConfiguration platform_;
    SafetyThresholds thresholds_;
    std::array<std::uint8_t, protocol::kMacKeySize> link_key_;
    safety::SafetyManager safety_{};
    control::MotionController controller_;
    MotionCommand command_{};
    bool command_valid_{false};
    std::uint64_t last_command_us_{0U};
    std::uint64_t now_us_{0U};
    std::int16_t last_left_duty_q15_{0};
    std::int16_t last_right_duty_q15_{0};
    std::uint16_t last_steering_pulse_us_{0U};
    bool bridge_disabled_{true};
};

}  // namespace r2::application
