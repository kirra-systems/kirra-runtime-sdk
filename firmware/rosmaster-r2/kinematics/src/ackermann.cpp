#include "r2/kinematics/ackermann.hpp"

#include <algorithm>
#include <cmath>

namespace r2::kinematics {
namespace {

constexpr AckermannSetpoint kZeroSetpoint{0.0, 0.0, 0.0, 0.0};

[[nodiscard]] bool finite(double value) noexcept {
    return std::isfinite(value);
}

}  // namespace

bool valid_geometry(const VehicleGeometry& geometry) noexcept {
    return finite(geometry.wheelbase_m) &&
           geometry.wheelbase_m >= 0.05 && geometry.wheelbase_m <= 2.0 &&
           finite(geometry.rear_track_m) &&
           geometry.rear_track_m >= 0.05 && geometry.rear_track_m <= 2.0 &&
           finite(geometry.wheel_radius_m) &&
           geometry.wheel_radius_m >= 0.005 && geometry.wheel_radius_m <= 0.5 &&
           finite(geometry.maximum_road_wheel_angle_rad) &&
           geometry.maximum_road_wheel_angle_rad >= 0.01 &&
           geometry.maximum_road_wheel_angle_rad <= 1.2;
}

KinematicsResult inverse_ackermann(const VehicleGeometry& geometry,
                                   const BodyCommand& command,
                                   const double stationary_epsilon_mps,
                                   const double yaw_epsilon_rad_s) noexcept {
    if (!valid_geometry(geometry)) {
        return {KinematicsStatus::invalid_geometry, kZeroSetpoint, false};
    }
    if (!finite(command.longitudinal_velocity_mps) ||
        !finite(command.yaw_rate_rad_s) ||
        !finite(stationary_epsilon_mps) ||
        !finite(yaw_epsilon_rad_s) ||
        stationary_epsilon_mps < 0.0 ||
        yaw_epsilon_rad_s < 0.0) {
        return {KinematicsStatus::non_finite_input, kZeroSetpoint, false};
    }

    const auto speed = command.longitudinal_velocity_mps;
    const auto yaw_rate = command.yaw_rate_rad_s;
    if (std::abs(speed) <= stationary_epsilon_mps) {
        if (std::abs(yaw_rate) > yaw_epsilon_rad_s) {
            return {KinematicsStatus::infeasible_stationary_turn, kZeroSetpoint, false};
        }
        return {KinematicsStatus::ok, kZeroSetpoint, false};
    }

    const auto curvature = yaw_rate / speed;
    if (!finite(curvature)) {
        return {KinematicsStatus::non_finite_input, kZeroSetpoint, false};
    }
    const auto requested_steering = std::atan(geometry.wheelbase_m * curvature);
    const auto steering = std::clamp(
        requested_steering,
        -geometry.maximum_road_wheel_angle_rad,
        geometry.maximum_road_wheel_angle_rad);
    const auto bounded_curvature = std::tan(steering) / geometry.wheelbase_m;
    const auto bounded_yaw_rate = speed * bounded_curvature;
    const auto half_track_rate = 0.5 * geometry.rear_track_m * bounded_yaw_rate;
    const auto left_velocity = speed - half_track_rate;
    const auto right_velocity = speed + half_track_rate;
    if (!finite(requested_steering) ||
        !finite(bounded_curvature) ||
        !finite(bounded_yaw_rate) ||
        !finite(left_velocity) ||
        !finite(right_velocity)) {
        return {KinematicsStatus::non_finite_input, kZeroSetpoint, false};
    }

    return {
        KinematicsStatus::ok,
        {
            left_velocity,
            right_velocity,
            steering,
            bounded_curvature,
        },
        steering != requested_steering,
    };
}

BodyCommand forward_ackermann(const VehicleGeometry& geometry,
                              const double left_rear_velocity_mps,
                              const double right_rear_velocity_mps,
                              const double steering_angle_rad) noexcept {
    if (!valid_geometry(geometry) ||
        !finite(left_rear_velocity_mps) ||
        !finite(right_rear_velocity_mps) ||
        !finite(steering_angle_rad)) {
        return {0.0, 0.0};
    }

    const auto velocity =
        0.5 * left_rear_velocity_mps + 0.5 * right_rear_velocity_mps;
    const auto bounded_steering = std::clamp(
        steering_angle_rad,
        -geometry.maximum_road_wheel_angle_rad,
        geometry.maximum_road_wheel_angle_rad);
    const auto yaw_rate =
        velocity * std::tan(bounded_steering) / geometry.wheelbase_m;
    if (!finite(velocity) || !finite(yaw_rate)) {
        return {0.0, 0.0};
    }
    return {velocity, yaw_rate};
}

}  // namespace r2::kinematics
