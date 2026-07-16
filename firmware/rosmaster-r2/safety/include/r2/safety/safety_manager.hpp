#pragma once

#include <cstdint>

namespace r2::safety {

enum class SafetyState : std::uint8_t {
    boot,
    self_test,
    standby,
    armed,
    active,
    controlled_stop,
    fault_latched,
    firmware_update,
};

enum class Fault : std::uint16_t {
    none = 0U,
    emergency_stop = 1ULL << 0U,
    command_timeout = 1ULL << 1U,
    brownout = 1ULL << 2U,
    battery_undervoltage = 1ULL << 3U,
    battery_overcurrent = 1ULL << 4U,
    motor_overtemperature = 1ULL << 5U,
    control_deadline_missed = 1ULL << 6U,
    encoder_implausible = 1ULL << 7U,
    motor_runaway = 1ULL << 8U,
    steering_implausible = 1ULL << 9U,
    imu_invalid = 1ULL << 10U,
    communication_corrupt = 1ULL << 11U,
    configuration_invalid = 1ULL << 12U,
    watchdog_precursor = 1ULL << 13U,
    self_test_failed = 1ULL << 14U,
};

struct SafetyInputs {
    bool emergency_stop_asserted;
    bool command_fresh;
    bool battery_voltage_safe;
    bool battery_current_safe;
    bool thermal_safe;
    bool encoder_plausible;
    bool steering_plausible;
    bool imu_sane;
    bool motor_runaway;
    bool control_deadline_met;
    bool communication_healthy;
    // Power-supply rails within the brownout threshold (true = stable). A false
    // raises Fault::brownout — the "fail-closed on brownout" safety guarantee.
    bool supply_stable;
    // Watchdog serviced within its window (true = healthy). A false raises
    // Fault::watchdog_precursor — the "fail-closed on watchdog" safety guarantee.
    bool watchdog_healthy;
    bool motion_stopped;
    // Active configuration passes valid_configuration() AND has calibrated=true.
    // Set to false for any uncalibrated record (including factory_defaults()) to
    // raise Fault::configuration_invalid and prevent arming. The caller is
    // responsible for evaluating
    // `r2::application::valid_configuration(cfg) && cfg.calibrated`.
    bool configuration_valid;
};

// Consecutive-tick debounce for the "sustained = hard fault" momentary faults
// (battery_undervoltage, imu_invalid, communication_corrupt). A transient (a
// single voltage sag, a bad IMU sample, a burst of line noise) is momentary and
// auto-clears; the same condition held for this many CONSECUTIVE evaluate()
// ticks is not noise but a real fault, and escalates to a latched fault that
// requires operator acknowledgement. Expressed in control-loop ticks — tune to
// the deployment's evaluate() cadence so it represents a meaningful sustained
// interval (e.g. at a 100 Hz control loop this is ~0.5 s).
inline constexpr std::uint32_t kPersistentFaultLatchThreshold = 50U;

class SafetyManager {
public:
    void begin_self_test() noexcept;
    void complete_self_test(bool passed) noexcept;
    [[nodiscard]] bool arm() noexcept;
    [[nodiscard]] bool activate() noexcept;
    [[nodiscard]] bool disarm() noexcept;
    [[nodiscard]] bool request_firmware_update() noexcept;
    void evaluate(const SafetyInputs& inputs) noexcept;
    [[nodiscard]] bool clear_recoverable_faults(bool physical_acknowledgement) noexcept;

    [[nodiscard]] SafetyState state() const noexcept;
    [[nodiscard]] std::uint64_t active_faults() const noexcept;
    [[nodiscard]] std::uint64_t latched_faults() const noexcept;
    [[nodiscard]] bool motion_permitted() const noexcept;
    [[nodiscard]] bool controlled_stop_required() const noexcept;
    [[nodiscard]] bool bridge_must_be_disabled() const noexcept;

private:
    void raise(Fault fault, bool latch) noexcept;
    // Raises `fault` momentarily while `active`, and escalates it to a latched
    // fault once it has been continuously active for kPersistentFaultLatchThreshold
    // ticks. `streak` is the caller-owned consecutive-active counter; it resets
    // to zero on any tick the condition is not active (so only a *sustained*
    // fault latches, never an intermittent one).
    void raise_persistent(Fault fault, bool active, std::uint32_t& streak) noexcept;
    void transition_to_safe_state() noexcept;

    SafetyState state_{SafetyState::boot};
    std::uint64_t active_faults_{0U};
    std::uint64_t latched_faults_{0U};
    // Per-fault consecutive-active tick counters backing raise_persistent().
    std::uint32_t undervoltage_streak_{0U};
    std::uint32_t imu_invalid_streak_{0U};
    std::uint32_t communication_streak_{0U};
};

[[nodiscard]] constexpr std::uint64_t bit(const Fault fault) noexcept {
    return static_cast<std::uint64_t>(fault);
}

}  // namespace r2::safety
