#include "r2/safety/safety_manager.hpp"

namespace r2::safety {

void SafetyManager::begin_self_test() noexcept {
    if (state_ == SafetyState::boot) {
        state_ = SafetyState::self_test;
    }
}

void SafetyManager::complete_self_test(const bool passed) noexcept {
    if (state_ != SafetyState::self_test) {
        raise(Fault::self_test_failed, true);
        return;
    }
    if (!passed) {
        raise(Fault::self_test_failed, true);
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

void SafetyManager::disarm() noexcept {
    if (state_ != SafetyState::firmware_update) {
        state_ = SafetyState::standby;
    }
}

void SafetyManager::request_firmware_update() noexcept {
    if (state_ == SafetyState::standby && active_faults_ == 0U) {
        state_ = SafetyState::firmware_update;
    }
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
    } else if (active_faults_ != 0U) {
        transition_to_safe_state();
    } else if (state_ == SafetyState::controlled_stop) {
        state_ = SafetyState::standby;
    }
}

bool SafetyManager::clear_recoverable_faults(const bool physical_acknowledgement) noexcept {
    if (!physical_acknowledgement || active_faults_ != 0U) {
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

bool SafetyManager::bridge_must_be_disabled() const noexcept {
    constexpr auto immediate_disable_faults =
        bit(Fault::emergency_stop) |
        bit(Fault::battery_overcurrent) |
        bit(Fault::motor_overtemperature) |
        bit(Fault::control_deadline_missed) |
        bit(Fault::encoder_implausible) |
        bit(Fault::motor_runaway) |
        bit(Fault::steering_implausible) |
        bit(Fault::self_test_failed);
    return state_ == SafetyState::boot ||
           state_ == SafetyState::self_test ||
           state_ == SafetyState::standby ||
           state_ == SafetyState::fault_latched ||
           state_ == SafetyState::firmware_update ||
           ((active_faults_ | latched_faults_) & immediate_disable_faults) != 0U;
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
