#include "r2/safety/safety_manager.hpp"

#include <cstdint>

namespace r2::safety {

void SafetyManager::begin_self_test() noexcept {
    if (state_ == SafetyState::boot) {
        state_ = SafetyState::self_test;
    }
}

void SafetyManager::complete_self_test(const bool passed) noexcept {
    if (state_ != SafetyState::self_test) {
        raise(Fault::self_test_failed, true);
        state_ = SafetyState::fault_latched;
        return;
    }
    if (!passed) {
        raise(Fault::self_test_failed, true);
        state_ = SafetyState::fault_latched;
        return;
    }
    state_ = SafetyState::standby;
}

bool SafetyManager::arm() noexcept {
    if (state_ != SafetyState::standby || active_faults_ != 0U || latched_faults_ != 0U) {
        return false;
    }
    state_ = SafetyState::armed;
    return true;
}

bool SafetyManager::activate() noexcept {
    if (state_ != SafetyState::armed || active_faults_ != 0U || latched_faults_ != 0U) {
        return false;
    }
    state_ = SafetyState::active;
    return true;
}

bool SafetyManager::disarm() noexcept {
    if (state_ == SafetyState::standby) {
        return true;
    }
    if (state_ == SafetyState::armed) {
        state_ = SafetyState::standby;
        return true;
    }
    if (state_ == SafetyState::active) {
        state_ = SafetyState::controlled_stop;
        return true;
    }
    return false;
}

bool SafetyManager::request_firmware_update() noexcept {
    if (state_ == SafetyState::standby &&
        active_faults_ == 0U &&
        latched_faults_ == 0U) {
        state_ = SafetyState::firmware_update;
        return true;
    }
    return false;
}

void SafetyManager::evaluate(const SafetyInputs& inputs) noexcept {
    active_faults_ = 0U;
    if (inputs.emergency_stop_asserted) {
        raise(Fault::emergency_stop, true);
    }
    if (!inputs.command_fresh &&
        (state_ == SafetyState::active || state_ == SafetyState::controlled_stop)) {
        raise(Fault::command_timeout, false);
    }
    if (!inputs.battery_voltage_safe) {
        raise(Fault::battery_undervoltage, false);
    }
    if (!inputs.supply_stable) {
        raise(Fault::brownout, true);
    }
    if (!inputs.watchdog_healthy) {
        raise(Fault::watchdog_precursor, true);
    }
    if (!inputs.battery_current_safe) {
        raise(Fault::battery_overcurrent, true);
    }
    if (!inputs.thermal_safe) {
        raise(Fault::motor_overtemperature, true);
    }
    if (!inputs.encoder_plausible) {
        raise(Fault::encoder_implausible, true);
    }
    if (!inputs.steering_plausible) {
        raise(Fault::steering_implausible, true);
    }
    if (!inputs.imu_sane) {
        raise(Fault::imu_invalid, false);
    }
    if (inputs.motor_runaway) {
        raise(Fault::motor_runaway, true);
    }
    if (!inputs.control_deadline_met) {
        raise(Fault::control_deadline_missed, true);
    }
    if (!inputs.communication_healthy) {
        raise(Fault::communication_corrupt, false);
    }

    if (latched_faults_ != 0U) {
        state_ = SafetyState::fault_latched;
    } else if (state_ == SafetyState::controlled_stop) {
        if (inputs.motion_stopped) {
            state_ = SafetyState::standby;
        }
    } else if (active_faults_ != 0U) {
        transition_to_safe_state();
    }
}

bool SafetyManager::clear_recoverable_faults(const bool physical_acknowledgement) noexcept {
    constexpr auto requires_reset_and_post =
        bit(Fault::self_test_failed) | bit(Fault::configuration_invalid);
    if (state_ != SafetyState::fault_latched ||
        !physical_acknowledgement ||
        active_faults_ != 0U ||
        (latched_faults_ & requires_reset_and_post) != 0U) {
        return false;
    }
    latched_faults_ = 0U;
    state_ = SafetyState::standby;
    return true;
}

SafetyState SafetyManager::state() const noexcept {
    return state_;
}

std::uint64_t SafetyManager::active_faults() const noexcept {
    return active_faults_;
}

std::uint64_t SafetyManager::latched_faults() const noexcept {
    return latched_faults_;
}

bool SafetyManager::motion_permitted() const noexcept {
    return state_ == SafetyState::active && active_faults_ == 0U &&
           latched_faults_ == 0U;
}

bool SafetyManager::controlled_stop_required() const noexcept {
    return state_ == SafetyState::controlled_stop && !bridge_must_be_disabled();
}

bool SafetyManager::bridge_must_be_disabled() const noexcept {
    constexpr auto immediate_disable_faults =
        bit(Fault::emergency_stop) |
        bit(Fault::brownout) |
        bit(Fault::battery_overcurrent) |
        bit(Fault::motor_overtemperature) |
        bit(Fault::control_deadline_missed) |
        bit(Fault::encoder_implausible) |
        bit(Fault::motor_runaway) |
        bit(Fault::steering_implausible) |
        bit(Fault::watchdog_precursor) |
        bit(Fault::self_test_failed);
    // Fail-closed / default-deny: enumerate the states in which the H-bridge MAY
    // be enabled (armed pre-charge, active drive, controlled-stop deceleration),
    // and require a clean immediate-disable set. An unknown / corrupt `state_`
    // value therefore DISABLES the bridge. The prior form allow-listed the *safe*
    // states, so any unlisted state_ left the bridge enabled by default (a
    // fail-OPEN default on a bit-flip / out-of-range value). This is behaviour-
    // identical for every valid state.
    const bool bridge_permitted_state =
        state_ == SafetyState::armed ||
        state_ == SafetyState::active ||
        state_ == SafetyState::controlled_stop;
    const bool immediate_disable =
        ((active_faults_ | latched_faults_) & immediate_disable_faults) != 0U;
    return !bridge_permitted_state || immediate_disable;
}

void SafetyManager::raise(const Fault fault, const bool latch) noexcept {
    active_faults_ |= bit(fault);
    if (latch) {
        latched_faults_ |= bit(fault);
    }
}

void SafetyManager::transition_to_safe_state() noexcept {
    if (state_ == SafetyState::active || state_ == SafetyState::armed) {
        state_ = SafetyState::controlled_stop;
    }
}

}  // namespace r2::safety
