#pragma once

#include "r2/kinematics/ackermann.hpp"

#include <cstdint>

namespace r2::control {

struct MotionLimits {
    double maximum_speed_mps;
    double maximum_acceleration_mps2;
    double maximum_deceleration_mps2;
    double maximum_jerk_mps3;
    double maximum_steering_rate_rad_s;
};

struct PidGains {
    double proportional;
    double integral;
    double derivative;
    double feedforward;
    double anti_windup;
};

struct WheelPidGains {
    PidGains left;
    PidGains right;
};

class JerkLimitedAxis {
public:
    explicit JerkLimitedAxis(const MotionLimits& limits) noexcept;

    [[nodiscard]] double update(double requested_velocity_mps,
                                double dt_s) noexcept;
    void reset(double velocity_mps = 0.0) noexcept;
    [[nodiscard]] double velocity_mps() const noexcept;
    [[nodiscard]] double acceleration_mps2() const noexcept;

private:
    MotionLimits limits_;
    double velocity_mps_{0.0};
    double acceleration_mps2_{0.0};
};

class VelocityPid {
public:
    explicit VelocityPid(const PidGains& gains) noexcept;

    [[nodiscard]] double update(double target_mps,
                                double measured_mps,
                                double dt_s,
                                double minimum_output,
                                double maximum_output) noexcept;
    void reset() noexcept;
    [[nodiscard]] bool valid() const noexcept;

private:
    PidGains gains_;
    double integral_{0.0};
    double previous_measurement_{0.0};
    bool initialized_{false};
    bool gains_valid_{false};
};

struct MotionOutput {
    double left_motor_command;
    double right_motor_command;
    double steering_angle_rad;
    kinematics::KinematicsStatus status;
};

class MotionController {
public:
    MotionController(kinematics::VehicleGeometry geometry,
                     MotionLimits limits,
                     WheelPidGains wheel_gains) noexcept;

    [[nodiscard]] MotionOutput update(
        const kinematics::BodyCommand& requested,
        double measured_left_mps,
        double measured_right_mps,
        double dt_s) noexcept;
    void reset() noexcept;

private:
    kinematics::VehicleGeometry geometry_;
    MotionLimits limits_;
    JerkLimitedAxis speed_limiter_;
    VelocityPid left_pid_;
    VelocityPid right_pid_;
    double steering_angle_rad_{0.0};
    bool configuration_valid_{false};
};

}  // namespace r2::control
