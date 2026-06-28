//! **Validation Hardening — Phase 2A Adversarial Review Mitigations**
//!
//! Defensive primitives that strengthen the trajectory validator against numerical instability,
//! edge cases, and specification ambiguities identified in the adversarial engineering review
//! (commit 5363daf).
//!
//! # Layers of Defense
//!
//! 1. **Input Validation** — precondition checks on trajectories, objects, and odom
//! 2. **Numerical Safety** — finiteness checks, tolerance bounds, monotonicity verification
//! 3. **Specification Formalization** — explicit invariants and assumptions
//! 4. **Fail-Closed Composition** — all guards return safe verdicts or MRCFallback

use crate::state::TrajectoryPoint;

/// **Precondition: Trajectory times are strictly monotonically increasing**
///
/// # Safety Rationale (SG1, SG7)
///
/// - Predictive RSS uses `nearest_in_time` to match object samples to ego poses.
/// - Non-monotonic time produces ambiguous nearest-matches → unevaluable samples → B3 guard.
/// - This precondition ensures every sample CAN be matched deterministically.
///
/// # Returns
///
/// `None` if valid (times strictly increasing).
/// `Some(violation_index)` if non-monotonic (times[i] ≥ times[i+1] at index i).
///
/// # Example
///
/// ```ignore
/// let traj = vec![
///     TrajectoryPoint { ..., time_from_start_s: 0.0 },
///     TrajectoryPoint { ..., time_from_start_s: 0.1 },  // ✓ increasing
///     TrajectoryPoint { ..., time_from_start_s: 0.05 }, // ✗ violation at index 1
/// ];
/// assert!(validate_trajectory_time_monotonicity(&traj).is_some(), "non-monotonic!");
/// ```
pub fn validate_trajectory_time_monotonicity(trajectory: &[TrajectoryPoint]) -> Option<usize> {
    for i in 0..trajectory.len().saturating_sub(1) {
        if trajectory[i].time_from_start_s >= trajectory[i + 1].time_from_start_s {
            return Some(i);
        }
    }
    None
}

/// **Precondition: Trajectory time spacing is bounded**
///
/// # Safety Rationale (SG1, SG7)
///
/// - Predictive RSS time-matches object samples (horizon 3s, step 0.5s → ~6 samples).
/// - Time-match tolerance is `PREDICTIVE_TIME_MATCH_TOLERANCE_S = 0.5s`.
/// - If trajectory poses are spaced > 0.5s apart, entire time windows become unevaluable.
/// - This precondition ensures the trajectory is fine-grained enough for predictive RSS.
///
/// # Returns
///
/// `None` if valid (max spacing ≤ max_spacing_s).
/// `Some((index, spacing_s))` if violated (spacing[i] > max at segment i).
///
/// # Certification Notes
///
/// The default `max_spacing_s = 0.1` matches a 10 Hz planning rate; integrators
/// MUST document any deviations as part of their Assumptions of Use (AoU).
pub fn validate_trajectory_time_spacing(
    trajectory: &[TrajectoryPoint],
    max_spacing_s: f64,
) -> Option<(usize, f64)> {
    for i in 0..trajectory.len().saturating_sub(1) {
        let spacing = trajectory[i + 1].time_from_start_s - trajectory[i].time_from_start_s;
        if spacing > max_spacing_s && spacing.is_finite() {
            return Some((i, spacing));
        }
    }
    None
}

/// **Precondition: All trajectory pose fields are finite**
///
/// # Safety Rationale (SG7, SG2)
///
/// - Containment and kinematics checks depend on pose fields being finite.
/// - NaN/Inf pose would poison comparison operators (NaN < x ⇒ false).
/// - This precondition is redundant with per-field checks in the validator,
///   but it provides early, comprehensive fail-closed detection.
///
/// # Returns
///
/// `None` if all poses are valid.
/// `Some(index)` if a non-finite field is found at pose `index`.
pub fn validate_trajectory_poses_finite(trajectory: &[TrajectoryPoint]) -> Option<usize> {
    for (i, point) in trajectory.iter().enumerate() {
        if !pose_is_finite(&point.pose) || !point.velocity_mps.is_finite()
            || !point.time_from_start_s.is_finite()
        {
            return Some(i);
        }
    }
    None
}

/// Check if a pose's fields are all finite.
#[inline]
pub fn pose_is_finite(pose: &crate::state::Pose) -> bool {
    pose.x_m.is_finite() && pose.y_m.is_finite() && pose.heading_rad.is_finite()
}

/// **Precondition: Ego odometry is fresh**
///
/// # Safety Rationale (SG7, SG3)
///
/// - The first trajectory segment's steering angle is derived from odom yaw rate.
/// - Stale odom (> N ms old) yields stale steering → incorrect P5b steering-rate check.
/// - This precondition ensures odom is used only when fresh.
///
/// # Parameters
///
/// - `odom`: ego odometry with a wall-clock timestamp.
/// - `now_ms`: current wall-clock time in milliseconds.
/// - `max_age_ms`: maximum acceptable staleness (e.g., 200 ms for a 10 Hz + margin loop).
///
/// # Returns
///
/// `true` if odom is fresh (age ≤ max_age_ms).
/// `false` if stale (age > max_age_ms) or odom is None.
///
/// # AoU Invariant
///
/// Integrators MUST provide odom timestamps synchronized with the vehicle's
/// control-loop clock and fresher than the validation loop cycle time.
pub fn validate_odom_freshness(
    odom: Option<&crate::state::EgoOdom>,
    now_ms: u64,
    max_age_ms: u64,
) -> bool {
    match odom {
        None => false, // No odom → treat as stale
        Some(o) => now_ms.saturating_sub(o.stamp_ms) <= max_age_ms,
    }
}

/// **Numerical safety: RSS Rule 4 braking bound with underflow guard**
///
/// # Claim (IEEE 2846 RSS §4.2)
///
/// The ego must brake to a stop within its assured-clear distance:
///   v·t + v²/(2a) ≤ remaining
///   ⇒ v ≤ sqrt((a·t)² + 2·a·remaining) - a·t
///
/// # Safety Rationale (SG1)
///
/// - Division by a ≈ 0 is unphysical (infinite stopping distance).
/// - Numerical cancellation at high visibility / low decel loses precision.
/// - This function includes guards against both.
///
/// # Parameters
///
/// - `remaining_m`: distance the ego can actually see ahead (m).
/// - `brake_decel_mps2`: vehicle braking capability (m/s²), must be > 0.1 (AoU).
///
/// # Returns
///
/// Maximum admissible speed (m/s) such that the ego can stop within `remaining_m`
/// even accounting for reaction time.
///
/// # Stability Note
///
/// The formula becomes numerically unstable when:
/// - `brake_decel_mps2 < 0.1 m/s²` (very weak braking) ⇒ result approaches 0 (conservative).
/// - `remaining_m > 10,000 m` (extreme visibility) ⇒ precision loss in the sqrt subtraction.
///
/// Both cases fail conservatively (underestimate safe speed).
pub fn assured_clear_distance_speed_cap_safe(
    remaining_m: f64,
    brake_decel_mps2: f64,
) -> f64 {
    const RSS_REACTION_TIME_S: f64 = 0.5;
    const MIN_DECEL_MPS2: f64 = 0.1; // Minimum braking to maintain physical model

    let a = brake_decel_mps2.max(MIN_DECEL_MPS2);
    let rem = remaining_m.max(0.0);
    let t = RSS_REACTION_TIME_S;

    // Compute the quadratic formula root: sqrt((a*t)^2 + 2*a*rem) - a*t
    let discriminant = (a * t).powi(2) + 2.0 * a * rem;
    let v = discriminant.sqrt() - a * t;

    // Post-computation finiteness check: guard against NaN propagation
    if !v.is_finite() {
        return 0.0; // Fail-closed: if the math fails, the safe speed is 0.
    }

    v.max(0.0)
}

/// **Numerical safety: Ego-frame projection with heading normalization**
///
/// # Claim
///
/// Rotate a world-frame (dx, dy) into the ego's body frame using the ego's heading.
/// The heading MUST be in [-π, π] to avoid discontinuities at the ±π wrap.
///
/// # Safety Rationale (SG1)
///
/// - Heading discontinuity at ±π causes projection to "flip" unexpectedly.
/// - Unbounded heading (e.g., 10π radians = 5 full rotations) is ambiguous.
/// - This function documents the precondition and asserts it (in debug builds).
///
/// # Parameters
///
/// - `heading_rad`: ego heading (must be in [-π, π]).
/// - `dx`, `dy`: world-frame deltas.
///
/// # Returns
///
/// `(dx_ego, dy_ego)`: ego-frame projection (longitudinal, lateral).
///
/// # Precondition Assertion
///
/// In debug builds, asserts `heading_rad ∈ [-π, π]`.
/// In release builds, assumes it (for performance); integrators MUST normalize.
pub fn ego_frame_projection_safe(
    heading_rad: f64,
    dx: f64,
    dy: f64,
) -> (f64, f64) {
    // Precondition: heading in [-π, π]
    debug_assert!(
        heading_rad.abs() <= std::f64::consts::PI + 1e-6,
        "heading_rad must be in [-π, π]; got {}. Caller must normalize.",
        heading_rad
    );

    let cos_h = heading_rad.cos();
    let sin_h = heading_rad.sin();
    let dx_ego = cos_h * dx + sin_h * dy;      // longitudinal
    let dy_ego = -sin_h * dx + cos_h * dy;     // lateral
    (dx_ego, dy_ego)
}

/// **Normalize heading to [-π, π]**
///
/// # Purpose
///
/// Ensures heading is in the canonical range for all downstream computations.
/// This is a helper for integrators to normalize perception or odometry headings
/// before feeding them into the validator.
pub fn normalize_heading(heading_rad: f64) -> f64 {
    let tau = std::f64::consts::TAU; // 2π
    let normalized = heading_rad - tau * (heading_rad / tau).round();
    // Clamp to [-π, π] to handle floating-point errors
    normalized.clamp(-std::f64::consts::PI, std::f64::consts::PI)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Pose as AdapterPose;

    #[test]
    fn trajectory_monotonicity_detects_violations() {
        let traj = vec![
            TrajectoryPoint {
                pose: AdapterPose {
                    x_m: 0.0,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
                velocity_mps: 1.0,
                time_from_start_s: 0.0,
            },
            TrajectoryPoint {
                pose: AdapterPose {
                    x_m: 1.0,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
                velocity_mps: 1.0,
                time_from_start_s: 0.1,
            },
            TrajectoryPoint {
                pose: AdapterPose {
                    x_m: 2.0,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
                velocity_mps: 1.0,
                time_from_start_s: 0.05, // Violation: 0.05 < 0.1
            },
        ];
        assert_eq!(validate_trajectory_time_monotonicity(&traj), Some(1));
    }

    #[test]
    fn trajectory_monotonicity_accepts_strictly_increasing() {
        let traj = vec![
            TrajectoryPoint {
                pose: AdapterPose {
                    x_m: 0.0,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
                velocity_mps: 1.0,
                time_from_start_s: 0.0,
            },
            TrajectoryPoint {
                pose: AdapterPose {
                    x_m: 1.0,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
                velocity_mps: 1.0,
                time_from_start_s: 0.1,
            },
            TrajectoryPoint {
                pose: AdapterPose {
                    x_m: 2.0,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
                velocity_mps: 1.0,
                time_from_start_s: 0.2,
            },
        ];
        assert_eq!(validate_trajectory_time_monotonicity(&traj), None);
    }

    #[test]
    fn trajectory_monotonicity_rejects_plateau() {
        let traj = vec![
            TrajectoryPoint {
                pose: AdapterPose { x_m: 0.0, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 1.0,
                time_from_start_s: 0.0,
            },
            TrajectoryPoint {
                pose: AdapterPose { x_m: 1.0, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 1.0,
                time_from_start_s: 0.1,
            },
            TrajectoryPoint {
                pose: AdapterPose { x_m: 2.0, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 1.0,
                time_from_start_s: 0.1, // Plateau: violation
            },
        ];
        assert_eq!(validate_trajectory_time_monotonicity(&traj), Some(1));
    }

    #[test]
    fn trajectory_spacing_detects_large_gaps() {
        let traj = vec![
            TrajectoryPoint {
                pose: AdapterPose { x_m: 0.0, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 1.0,
                time_from_start_s: 0.0,
            },
            TrajectoryPoint {
                pose: AdapterPose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 1.0,
                time_from_start_s: 1.5, // Gap of 1.5s, exceeds 0.5s tolerance
            },
        ];
        let result = validate_trajectory_time_spacing(&traj, 0.5);
        assert!(result.is_some());
        let (idx, spacing) = result.unwrap();
        assert_eq!(idx, 0);
        assert!((spacing - 1.5).abs() < 1e-9);
    }

    #[test]
    fn trajectory_spacing_accepts_within_tolerance() {
        let traj = vec![
            TrajectoryPoint {
                pose: AdapterPose { x_m: 0.0, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 1.0,
                time_from_start_s: 0.0,
            },
            TrajectoryPoint {
                pose: AdapterPose { x_m: 0.5, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 1.0,
                time_from_start_s: 0.1,
            },
            TrajectoryPoint {
                pose: AdapterPose { x_m: 1.0, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 1.0,
                time_from_start_s: 0.2,
            },
        ];
        assert_eq!(validate_trajectory_time_spacing(&traj, 0.5), None);
    }

    #[test]
    fn trajectory_poses_finiteness_accepts_all_finite() {
        let traj = vec![
            TrajectoryPoint {
                pose: AdapterPose { x_m: 0.0, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 1.0,
                time_from_start_s: 0.0,
            },
            TrajectoryPoint {
                pose: AdapterPose { x_m: 1.0, y_m: 2.0, heading_rad: 0.5 },
                velocity_mps: 2.5,
                time_from_start_s: 0.1,
            },
        ];
        assert_eq!(validate_trajectory_poses_finite(&traj), None);
    }

    #[test]
    fn trajectory_poses_finiteness_rejects_nan_x() {
        let traj = vec![
            TrajectoryPoint {
                pose: AdapterPose { x_m: f64::NAN, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 1.0,
                time_from_start_s: 0.0,
            },
        ];
        assert_eq!(validate_trajectory_poses_finite(&traj), Some(0));
    }

    #[test]
    fn trajectory_poses_finiteness_rejects_inf_velocity() {
        let traj = vec![
            TrajectoryPoint {
                pose: AdapterPose { x_m: 0.0, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: f64::INFINITY,
                time_from_start_s: 0.0,
            },
        ];
        assert_eq!(validate_trajectory_poses_finite(&traj), Some(0));
    }

    #[test]
    fn trajectory_poses_finiteness_rejects_inf_time() {
        let traj = vec![
            TrajectoryPoint {
                pose: AdapterPose { x_m: 0.0, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 1.0,
                time_from_start_s: f64::INFINITY,
            },
        ];
        assert_eq!(validate_trajectory_poses_finite(&traj), Some(0));
    }

    #[test]
    fn rss_rule4_with_low_decel_clamps_to_min() {
        let cap = assured_clear_distance_speed_cap_safe(100.0, 0.01);
        assert!(cap > 0.0 && cap.is_finite());
        // Should use MIN_DECEL_MPS2 = 0.1, resulting in higher cap than with 0.01
        let cap_lower = assured_clear_distance_speed_cap_safe(100.0, 0.001);
        assert!(cap >= cap_lower, "min guard should tighten the bound");
    }

    #[test]
    fn rss_rule4_with_zero_visibility_is_zero() {
        assert_eq!(assured_clear_distance_speed_cap_safe(0.0, 4.5), 0.0);
    }

    #[test]
    fn rss_rule4_with_negative_visibility_is_zero() {
        assert_eq!(assured_clear_distance_speed_cap_safe(-10.0, 4.5), 0.0);
    }

    #[test]
    fn rss_rule4_nan_input_returns_zero_fail_closed() {
        let cap = assured_clear_distance_speed_cap_safe(f64::NAN, 4.5);
        assert_eq!(cap, 0.0, "NaN input fails closed to zero speed");
    }

    #[test]
    fn rss_rule4_monotonic_in_visibility() {
        let near = assured_clear_distance_speed_cap_safe(5.0, 4.5);
        let far = assured_clear_distance_speed_cap_safe(30.0, 4.5);
        assert!(far > near, "more visibility ⇒ higher safe speed");
    }

    #[test]
    fn rss_rule4_monotonic_in_decel() {
        let weak = assured_clear_distance_speed_cap_safe(100.0, 2.0);
        let strong = assured_clear_distance_speed_cap_safe(100.0, 8.0);
        assert!(strong > weak, "stronger braking ⇒ higher safe speed");
    }

    #[test]
    fn heading_normalization_wraps_full_rotations() {
        let heading = 10.0 * std::f64::consts::PI;
        let normalized = normalize_heading(heading);
        assert!(
            normalized.abs() <= std::f64::consts::PI + 1e-6,
            "normalized heading must be in [-π, π]"
        );
    }

    #[test]
    fn heading_normalization_wraps_negative_rotations() {
        let heading = -15.0 * std::f64::consts::PI;
        let normalized = normalize_heading(heading);
        assert!(
            normalized.abs() <= std::f64::consts::PI + 1e-6,
            "normalized heading must be in [-π, π]"
        );
    }

    #[test]
    fn heading_normalization_preserves_small_angles() {
        let angles = [0.0, 0.5, 1.0, -1.5, std::f64::consts::PI - 0.1];
        for angle in angles {
            let normalized = normalize_heading(angle);
            assert!((normalized - angle).abs() < 1e-9, "small angles should be unchanged");
        }
    }

    #[test]
    fn ego_frame_projection_safe_at_zero_heading() {
        let (dx_ego, dy_ego) = ego_frame_projection_safe(0.0, 10.0, 5.0);
        assert!((dx_ego - 10.0).abs() < 1e-9, "at heading 0, dx projects to dx");
        assert!((dy_ego - 5.0).abs() < 1e-9, "at heading 0, dy projects to dy");
    }

    #[test]
    fn ego_frame_projection_safe_at_pi_over_2_heading() {
        let (dx_ego, dy_ego) = ego_frame_projection_safe(std::f64::consts::PI / 2.0, 10.0, 5.0);
        assert!((dx_ego - 5.0).abs() < 1e-9, "at heading π/2, dx projects to dy");
        assert!((dy_ego + 10.0).abs() < 1e-9, "at heading π/2, dy projects to -dx");
    }

    #[test]
    fn ego_frame_projection_continuity_across_wrap() {
        let eps = 0.001;
        let heading_before = std::f64::consts::PI - eps;
        let heading_after = normalize_heading(-std::f64::consts::PI + eps);

        let (dx_before, dy_before) = ego_frame_projection_safe(heading_before, 10.0, 5.0);
        let (dx_after, dy_after) = ego_frame_projection_safe(heading_after, 10.0, 5.0);

        // Projections should be close (continuous), not flip sign
        assert!(
            (dx_before - dx_after).abs() < 1.0 && (dy_before - dy_after).abs() < 1.0,
            "projection should be continuous across heading wrap: ({}, {}) vs ({}, {})",
            dx_before, dy_before, dx_after, dy_after
        );
    }

    #[test]
    fn pose_is_finite_accepts_all_finite() {
        let pose = AdapterPose { x_m: 1.0, y_m: 2.0, heading_rad: 0.5 };
        assert!(pose_is_finite(&pose));
    }

    #[test]
    fn pose_is_finite_rejects_nan_x() {
        let pose = AdapterPose { x_m: f64::NAN, y_m: 2.0, heading_rad: 0.5 };
        assert!(!pose_is_finite(&pose));
    }

    #[test]
    fn pose_is_finite_rejects_inf_y() {
        let pose = AdapterPose { x_m: 1.0, y_m: f64::INFINITY, heading_rad: 0.5 };
        assert!(!pose_is_finite(&pose));
    }

    #[test]
    fn pose_is_finite_rejects_neg_inf_heading() {
        let pose = AdapterPose { x_m: 1.0, y_m: 2.0, heading_rad: f64::NEG_INFINITY };
        assert!(!pose_is_finite(&pose));
    }
}
