#include "r2/application/safe_hal.hpp"

#include "r2/application/control_loop.hpp"
#include "r2/hal/interfaces.hpp"

#include <cstddef>
#include <cstdint>

namespace r2::application {

std::uint64_t SafeClock::now_us() const noexcept {
    return 0U;
}

void SafeMotorBridge::set_duty_q15(std::int16_t, std::int16_t) noexcept {
    // No H-bridge present: ignore any drive request.
}

void SafeMotorBridge::disable() noexcept {}

bool SafeMotorBridge::output_enabled() const noexcept {
    return false;
}

void SafeSteeringActuator::set_pulse_us(std::uint16_t) noexcept {}

void SafeSteeringActuator::disable() noexcept {}

hal::EncoderSnapshot SafeEncoderBank::snapshot() noexcept {
    return hal::EncoderSnapshot{};
}

hal::ImuSample SafeImuDevice::sample() noexcept {
    hal::ImuSample sample{};
    sample.valid = false;
    return sample;
}

hal::BatterySample SafeBatteryMonitor::sample() noexcept {
    hal::BatterySample sample{};
    sample.voltage_valid = false;
    sample.current_valid = false;
    sample.temperature_valid = false;
    return sample;
}

bool SafeEmergencyStopInput::asserted() const noexcept {
    return true;
}

void SafeIndependentWatchdog::service() noexcept {}

std::size_t SafeTransport::read(std::uint8_t*, std::size_t) noexcept {
    return 0U;
}

bool SafeTransport::write(const std::uint8_t*, std::size_t) noexcept {
    return false;
}

HalBundle SafeHal::bundle() noexcept {
    return HalBundle{clock,   motors,         steering, encoders, imu,
                     battery, emergency_stop, watchdog, transport};
}

}  // namespace r2::application
