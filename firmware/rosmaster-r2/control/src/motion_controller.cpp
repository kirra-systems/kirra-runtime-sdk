#include "r2/control/motion_controller.hpp"

#include <algorithm>
#include <cmath>

namespace r2::control {
namespace {

[[nodiscard]] bool valid_positive(const double value) noexcept {
    return std::isfinite(value) && value > 0.0;
}

[[nodiscard]] double approach(const double current,
                              const double target,
                              const double maximum_step) noexcept {
    return current + std::clamp(target - current, -maximum_step, maximum_step);
}

}  // namespace

JerkLimitedAxis::JerkLimitedAxis(const MotionLimits limits) noexcept : limits_(limits) {}

double JerkLimitedAxis::update(const double requested_velocity_mps,
                               const double dt_s) noexcept {
    if (!std::isfinite(requested_velocity_mps) || !valid_positive(dt_s) ||
        !valid_positive(limits_.maximum_speed_mps) ||
        !valid_positive(limits_.maximum_acceleration_mps2) ||
        !valid_positive(limits_.maximum_deceleration_mps2) ||
        !valid_positive(limits_.maximum_jerk_mps3)) {
        reset();
        return 0.0;
    }

    // The absolute envelope is applied before rate shaping. A stale internal
    // value is also clamped so a configuration reduction takes effect at once.
    const auto target = std::clamp(requested_velocity_mps,
                                   -limits_.maximum_speed_mps,
                                   limits_.maximum_speed_mps);
    velocity_mps_ = std::clamp(velocity_mps_,
                               -limits_.maximum_speed_mps,
                               limits_.maximum_speed_mps);

    const auto requested_acceleration = (target - velocity_mps_) / dt_s;
    const auto accelerating =
        std::abs(target) > std::abs(velocity_mps_) &&
        target * velocity_mps_ >= 0.0;
    const auto acceleration_limit = accelerating
                                        ? limits_.maximum_acceleration_mps2
                                        : limits_.maximum_deceleration_mps2;
    const auto bounded_acceleration =
        std::clamp(requested_acceleration, -acceleration_limit, acceleration_limit);
    acceleration_mps2_ = approach(
        acceleration_mps2_,
        bounded_acceleration,
        limits_.maximum_jerk_mps3 * dt_s);

    const auto previous_error = target - velocity_mps_;
    const auto candidate = velocity_mps_ + acceleration_mps2_ * dt_s;
    const auto new_error = target - candidate;
    if (previous_error == 0.0 || previous_error * new_error <= 0.0) {
        velocity_mps_ = target;
        acceleration_mps2_ = 0.0;
    } else {
        velocity_mps_ = std::clamp(candidate,
                                   -limits_.maximum_speed_mps,
                                   limits_.maximum_speed_mps);
    }
    return velocity_mps_;
}

void JerkLimitedAxis::reset(const double velocity_mps) noexcept {
    velocity_mps_ = std::isfinite(velocity_mps) ? velocity_mps : 0.0;
    acceleration_mps2_ = 0.0;
}

double JerkLimitedAxis::velocity_mps() const noexcept {
    return velocity_mps_;
}

double JerkLimitedAxis::acceleration_mps2() const noexcept {
    return acceleration_mps2_;
}

VelocityPid::VelocityPid(const PidGains gains) noexcept : gains_(gains) {}

double VelocityPid::update(const double target_mps,
                           const double measured_mps,
                           const double dt_s,
                           const double minimum_output,
                           const double maximum_output) noexcept {
    if (!std::isfinite(target_mps) || !std::isfinite(measured_mps) ||
        !valid_positive(dt_s) || !std::isfinite(minimum_output) ||
        !std::isfinite(maximum_output) || minimum_output >= maximum_output) {
        reset();
        return 0.0;
    }

    const auto error = target_mps - measured_mps;
    const auto derivative = initialized_
                                ? -(measured_mps - previous_measurement_) / dt_s
                                : 0.0;
    previous_measurement_ = measured_mps;
    initialized_ = true;

    const auto unsaturated =
        gains_.feedforward * target_mps +
        gains_.proportional * error +
        integral_ +
        gains_.derivative * derivative;
    const auto output = std::clamp(unsaturated, minimum_output, maximum_output);

    integral_ +=
        (gains_.integral * error + gains_.anti_windup * (output - unsaturated)) * dt_s;
    if (!std::isfinite(integral_)) {
        reset();
        return 0.0;
    }
    integral_ = std::clamp(integral_, minimum_output, maximum_output);
    return output;
}

void VelocityPid::reset() noexcept {
    integral_ = 0.0;
    previous_measurement_ = 0.0;
    initialized_ = false;
}

MotionController::MotionController(const kinematics::VehicleGeometry geometry,
                                   const MotionLimits limits,
                                   const PidGains left_gains,
                                   const PidGains right_gains) noexcept
    : geometry_(geometry),
      limits_(limits),
      speed_limiter_(limits),
      left_pid_(left_gains),
      right_pid_(right_gains) {}

MotionOutput MotionController::update(const kinematics::BodyCommand& requested,
                                      const double measured_left_mps,
                                      const double measured_right_mps,
                                      const double dt_s) noexcept {
    if (!std::isfinite(requested.longitudinal_velocity_mps) ||
        !std::isfinite(requested.yaw_rate_rad_s) ||
        !std::isfinite(measured_left_mps) ||
        !std::isfinite(measured_right_mps) ||
        !valid_positive(dt_s) ||
        !valid_positive(limits_.maximum_steering_rate_rad_s)) {
        reset();
        return {0.0, 0.0, 0.0, kinematics::KinematicsStatus::non_finite_input};
    }

    const auto bounded_speed =
        speed_limiter_.update(requested.longitudinal_velocity_mps, dt_s);
    const auto requested_curvature =
        std::abs(requested.longitudinal_velocity_mps) > 1.0e-3
            ? requested.yaw_rate_rad_s / requested.longitudinal_velocity_mps
            : 0.0;
    const auto bounded_yaw_rate = bounded_speed * requested_curvature;
    const auto kinematic = kinematics::inverse_ackermann(
        geometry_, {bounded_speed, bounded_yaw_rate});
    if (kinematic.status != kinematics::KinematicsStatus::ok) {
        reset();
        return {0.0, 0.0, 0.0, kinematic.status};
    }

    steering_angle_rad_ = approach(
        steering_angle_rad_,
        kinematic.value.steering_angle_rad,
        limits_.maximum_steering_rate_rad_s * dt_s);

    return {
        left_pid_.update(kinematic.value.left_rear_velocity_mps,
                         measured_left_mps, dt_s, -1.0, 1.0),
        right_pid_.update(kinematic.value.right_rear_velocity_mps,
                          measured_right_mps, dt_s, -1.0, 1.0),
        steering_angle_rad_,
        kinematics::KinematicsStatus::ok,
    };
}

void MotionController::reset() noexcept {
    speed_limiter_.reset();
    left_pid_.reset();
    right_pid_.reset();
    steering_angle_rad_ = 0.0;
}

}  // namespace r2::control
