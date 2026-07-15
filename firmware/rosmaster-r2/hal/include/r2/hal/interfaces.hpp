#pragma once

#include <cstddef>
#include <cstdint>

namespace r2::hal {

struct EncoderSnapshot {
    std::int64_t left_count;
    std::int64_t right_count;
    std::int32_t left_delta;
    std::int32_t right_delta;
    std::uint64_t captured_at_us;
};

struct ImuSample {
    float acceleration_mps2[3];
    float angular_velocity_rad_s[3];
    float magnetic_field_t[3];
    float temperature_c;
    std::uint64_t captured_at_us;
    bool valid;
};

struct BatterySample {
    float voltage_v;
    float current_a;
    float temperature_c;
    std::uint64_t captured_at_us;
    bool voltage_valid;
    bool current_valid;
    bool temperature_valid;
};

class MonotonicClock {
public:
    virtual ~MonotonicClock() = default;
    [[nodiscard]] virtual std::uint64_t now_us() const noexcept = 0;
};

class MotorBridge {
public:
    virtual ~MotorBridge() = default;
    virtual void set_duty_q15(std::int16_t left, std::int16_t right) noexcept = 0;
    virtual void disable() noexcept = 0;
    [[nodiscard]] virtual bool output_enabled() const noexcept = 0;
};

class EncoderBank {
public:
    virtual ~EncoderBank() = default;
    [[nodiscard]] virtual EncoderSnapshot snapshot() noexcept = 0;
};

class SteeringActuator {
public:
    virtual ~SteeringActuator() = default;
    virtual void set_pulse_us(std::uint16_t pulse_us) noexcept = 0;
    virtual void disable() noexcept = 0;
};

class ImuDevice {
public:
    virtual ~ImuDevice() = default;
    [[nodiscard]] virtual ImuSample sample() noexcept = 0;
};

class BatteryMonitor {
public:
    virtual ~BatteryMonitor() = default;
    [[nodiscard]] virtual BatterySample sample() noexcept = 0;
};

class EmergencyStopInput {
public:
    virtual ~EmergencyStopInput() = default;
    [[nodiscard]] virtual bool asserted() const noexcept = 0;
};

class IndependentWatchdog {
public:
    virtual ~IndependentWatchdog() = default;
    virtual void service() noexcept = 0;
};

class Transport {
public:
    virtual ~Transport() = default;
    [[nodiscard]] virtual std::size_t read(std::uint8_t* destination,
                                           std::size_t capacity) noexcept = 0;
    [[nodiscard]] virtual bool write(const std::uint8_t* source,
                                     std::size_t length) noexcept = 0;
};

class PersistentStorage {
public:
    virtual ~PersistentStorage() = default;
    [[nodiscard]] virtual bool read(std::uint32_t address,
                                    std::uint8_t* destination,
                                    std::size_t length) const noexcept = 0;
    [[nodiscard]] virtual bool erase(std::uint32_t address,
                                     std::size_t length) noexcept = 0;
    [[nodiscard]] virtual bool write(std::uint32_t address,
                                     const std::uint8_t* source,
                                     std::size_t length) noexcept = 0;
};

}  // namespace r2::hal
