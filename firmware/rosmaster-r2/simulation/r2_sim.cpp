#include "r2/control/motion_controller.hpp"
#include "r2/kinematics/ackermann.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstddef>
#include <cstdio>

int main() {
    constexpr r2::kinematics::VehicleGeometry geometry{0.23, 0.19, 0.033, 0.55};
    constexpr r2::control::MotionLimits limits{1.5, 1.0, 2.0, 8.0, 2.0};
    constexpr r2::control::PidGains gains{0.8, 1.2, 0.01, 0.4, 4.0};
    r2::control::MotionController controller{geometry, limits, gains, gains};

    constexpr double dt_s = 0.001;
    constexpr std::size_t steps = 20'000U;
    double left_velocity = 0.0;
    double right_velocity = 0.0;
    double maximum_motor_command = 0.0;
    double maximum_steering = 0.0;

    const auto started = std::chrono::steady_clock::now();
    for (std::size_t step = 0U; step < steps; ++step) {
        const auto time_s = static_cast<double>(step) * dt_s;
        const r2::kinematics::BodyCommand request{
            time_s < 2.0 ? 0.8 : (time_s < 12.0 ? 1.0 : 0.0),
            time_s >= 2.0 && time_s < 8.0 ? 0.7 : 0.0,
        };
        const auto output =
            controller.update(request, left_velocity, right_velocity, dt_s);
        if (output.status != r2::kinematics::KinematicsStatus::ok ||
            !std::isfinite(output.left_motor_command) ||
            !std::isfinite(output.right_motor_command) ||
            !std::isfinite(output.steering_angle_rad)) {
            std::fprintf(stderr, "invalid controller output at step %zu\n", step);
            return 1;
        }

        maximum_motor_command = std::max(
            maximum_motor_command,
            std::max(std::abs(output.left_motor_command),
                     std::abs(output.right_motor_command)));
        maximum_steering =
            std::max(maximum_steering, std::abs(output.steering_angle_rad));

        // First-order wheel plant. This is a deterministic software integration
        // fixture, not an identified model of the physical R2.
        constexpr double motor_gain_mps = 1.7;
        constexpr double time_constant_s = 0.08;
        left_velocity +=
            ((output.left_motor_command * motor_gain_mps) - left_velocity) *
            (dt_s / time_constant_s);
        right_velocity +=
            ((output.right_motor_command * motor_gain_mps) - right_velocity) *
            (dt_s / time_constant_s);
    }
    const auto elapsed = std::chrono::duration_cast<std::chrono::microseconds>(
        std::chrono::steady_clock::now() - started);

    const auto final_body = r2::kinematics::forward_ackermann(
        geometry, left_velocity, right_velocity, 0.0);
    const auto average_step_us =
        static_cast<double>(elapsed.count()) / static_cast<double>(steps);
    std::printf(
        "steps=%zu host_elapsed_us=%lld host_average_step_us=%.6f "
        "max_motor=%.6f max_steering_rad=%.6f final_speed_mps=%.6f\n",
        steps,
        static_cast<long long>(elapsed.count()),
        average_step_us,
        maximum_motor_command,
        maximum_steering,
        final_body.longitudinal_velocity_mps);

    if (maximum_motor_command > 1.0 ||
        maximum_steering > geometry.maximum_road_wheel_angle_rad ||
        std::abs(final_body.longitudinal_velocity_mps) > 0.01) {
        std::fputs("simulation invariant failed\n", stderr);
        return 1;
    }
    return 0;
}
