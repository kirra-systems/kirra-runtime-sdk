#pragma once

#include <cstdint>

namespace r2::kinematics {

struct VehicleGeometry {
    double wheelbase_m;
    double rear_track_m;
    double wheel_radius_m;
    double maximum_road_wheel_angle_rad;
};

struct BodyCommand {
    double longitudinal_velocity_mps;
    double yaw_rate_rad_s;
};

struct AckermannSetpoint {
    double left_rear_velocity_mps;
    double right_rear_velocity_mps;
    double steering_angle_rad;
    double curvature_per_m;
};

enum class KinematicsStatus : std::uint8_t {
    ok,
    invalid_configuration,
    invalid_geometry,
    non_finite_input,
    infeasible_stationary_turn,
};

struct KinematicsResult {
    KinematicsStatus status;
    AckermannSetpoint value;
    bool steering_saturated;
};

[[nodiscard]] bool valid_geometry(const VehicleGeometry& geometry) noexcept;

[[nodiscard]] KinematicsResult inverse_ackermann(
    const VehicleGeometry& geometry,
    const BodyCommand& command,
    double stationary_epsilon_mps = 1.0e-3,
    double yaw_epsilon_rad_s = 1.0e-3) noexcept;

[[nodiscard]] BodyCommand forward_ackermann(
    const VehicleGeometry& geometry,
    double left_rear_velocity_mps,
    double right_rear_velocity_mps,
    double steering_angle_rad) noexcept;

}  // namespace r2::kinematics
