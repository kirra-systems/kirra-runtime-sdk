#pragma once

// Fail-closed HAL seams for a boot with no concrete drivers present (#967).
//
// Every seam reports the safe / no-device state so the SafetyManager
// immobilizes the platform: the emergency stop reads asserted, the battery and
// IMU samples read invalid, the transport is silent, and the actuators stay
// disabled. Wiring the Application against a SafeHal therefore boots straight
// into a latched-safe, bridge-disabled state — the correct default before the
// real STM32 drivers exist.
//
// Uses: the target bring-up image can boot against SafeHal to prove a safe
// power-on; the host tests assert the driverless-boot immobilization property;
// and each concrete driver from #967 replaces its Safe* counterpart one seam at
// a time without touching the control loop.

#include "r2/application/control_loop.hpp"
#include "r2/hal/interfaces.hpp"

#include <cstddef>
#include <cstdint>

namespace r2::application {

// A monotonic clock with no real time base (returns 0). run_cycle() then falls
// back to the nominal loop period; nothing actuates in the safe boot, so a
// frozen clock is harmless. A concrete driver replaces this with a real timer.
class SafeClock final : public hal::MonotonicClock {
public:
    [[nodiscard]] std::uint64_t now_us() const noexcept override;
};

class SafeMotorBridge final : public hal::MotorBridge {
public:
    void set_duty_q15(std::int16_t left, std::int16_t right) noexcept override;
    void disable() noexcept override;
    [[nodiscard]] bool output_enabled() const noexcept override;
};

class SafeSteeringActuator final : public hal::SteeringActuator {
public:
    void set_pulse_us(std::uint16_t pulse_us) noexcept override;
    void disable() noexcept override;
};

// Reports zero motion (no encoders wired).
class SafeEncoderBank final : public hal::EncoderBank {
public:
    [[nodiscard]] hal::EncoderSnapshot snapshot() noexcept override;
};

// Reports an invalid sample so imu_sane is false.
class SafeImuDevice final : public hal::ImuDevice {
public:
    [[nodiscard]] hal::ImuSample sample() noexcept override;
};

// Reports all channels invalid so no battery input reads safe.
class SafeBatteryMonitor final : public hal::BatteryMonitor {
public:
    [[nodiscard]] hal::BatterySample sample() noexcept override;
};

// Reads asserted: with no real e-stop wired, the platform stays e-stopped.
class SafeEmergencyStopInput final : public hal::EmergencyStopInput {
public:
    [[nodiscard]] bool asserted() const noexcept override;
};

class SafeIndependentWatchdog final : public hal::IndependentWatchdog {
public:
    void service() noexcept override;
};

// Silent: no frames are ever delivered.
class SafeTransport final : public hal::Transport {
public:
    [[nodiscard]] std::size_t read(std::uint8_t* destination,
                                   std::size_t capacity) noexcept override;
    [[nodiscard]] bool write(const std::uint8_t* source,
                             std::size_t length) noexcept override;
};

// Owns one instance of each fail-closed seam and hands out a HalBundle wired to
// them. Statically allocate one SafeHal and pass bundle() to the Application.
struct SafeHal {
    SafeClock clock;
    SafeMotorBridge motors;
    SafeSteeringActuator steering;
    SafeEncoderBank encoders;
    SafeImuDevice imu;
    SafeBatteryMonitor battery;
    SafeEmergencyStopInput emergency_stop;
    SafeIndependentWatchdog watchdog;
    SafeTransport transport;

    [[nodiscard]] HalBundle bundle() noexcept;
};

}  // namespace r2::application
