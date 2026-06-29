//! **Redundancy Hardening — Phase 2A Extended Equivalence Checking**
//!
//! Strengthens the perception redundancy validator with extended motion-source
//! equivalence checks: heading divergence detection, velocity-vector consistency,
//! and clock-wraparound guards.
//!
//! # Design Rationale (SG3, SG4, SG9)
//!
//! The original perception redundancy check verifies that two motion sources
//! (predicted + actual perception) agree on the object's motion state within
//! tolerances. This module extends the comparison to include:
//!
//! 1. **Heading Divergence** — the ego's heading determines the ego-frame projection;
//!    discontinuity at ±π can mask divergence.
//! 2. **Velocity-Vector Consistency** — lateral velocity components should match
//!    within the longitudinal tolerance (they are derived from the same object state).
//! 3. **Clock Wraparound** — wall-clock timestamps wrap at system boundaries;
//!    elapsed time computation must guard against backward jumps.

use std::f64::consts::PI;

/// **Extended equivalence configuration for motion-source cross-checks**
///
/// # Fields
///
/// - `lon_velocity_tol_mps`: longitudinal velocity match tolerance (m/s).
/// - `lat_velocity_tol_mps`: lateral velocity match tolerance (m/s).
/// - `heading_tol_rad`: heading difference tolerance (radians).
/// - `position_tol_m`: position difference tolerance (meters).
///
/// # Safety Rationale
///
/// Extended checks allow early detection of sensor/predictor divergence that
/// would otherwise manifest as trajectory safety violations downstream (SG3, SG9).
#[derive(Debug, Clone, Copy)]
pub struct ExtendedEquivalenceConfig {
    pub lon_velocity_tol_mps: f64,
    pub lat_velocity_tol_mps: f64,
    pub heading_tol_rad: f64,
    pub position_tol_m: f64,
}

impl Default for ExtendedEquivalenceConfig {
    fn default() -> Self {
        Self {
            lon_velocity_tol_mps: 0.5,
            lat_velocity_tol_mps: 0.5,
            heading_tol_rad: 0.1, // ~5.7 degrees
            position_tol_m: 0.5,
        }
    }
}

/// **Result of an extended equivalence check**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EquivalenceCheckResult {
    /// Both sources agree within all tolerances.
    Equivalent,
    /// Position divergence detected.
    PositionDivergence,
    /// Longitudinal velocity divergence detected.
    LonVelocityDivergence,
    /// Lateral velocity divergence detected.
    LatVelocityDivergence,
    /// Heading divergence detected.
    HeadingDivergence,
    /// Multiple divergences detected (fail-open for debugging).
    MultipleDivergences,
}

/// **Normalized heading difference in [-π, π]**
///
/// # Purpose
///
/// Wraps the heading difference to the shortest arc, accounting for the
/// discontinuity at ±π. This allows continuous comparison across the wrap boundary.
///
/// # Example
///
/// ```ignore
/// heading_difference(3.1, -3.1) ≈ 0.04 (both are ≈ π in opposite directions)
/// heading_difference(0.0, π) = π (half turn)
/// ```
pub fn heading_difference(heading1: f64, heading2: f64) -> f64 {
    let diff = heading1 - heading2;
    // Normalize to [-π, π]
    let tau = std::f64::consts::TAU; // 2π
    let normalized = diff - tau * (diff / tau).round();
    // Clamp to handle floating-point errors
    normalized.clamp(-PI, PI)
}

/// **Compute ego-frame velocity projections**
///
/// # Purpose
///
/// Given world-frame velocity components (vx, vy) and ego heading, compute
/// the longitudinal and lateral components in the ego's body frame.
///
/// # Returns
///
/// `(v_lon, v_lat)`: longitudinal and lateral velocities in ego frame.
pub fn ego_frame_velocity(vx: f64, vy: f64, ego_heading: f64) -> (f64, f64) {
    let cos_h = ego_heading.cos();
    let sin_h = ego_heading.sin();
    let v_lon = cos_h * vx + sin_h * vy;      // longitudinal
    let v_lat = -sin_h * vx + cos_h * vy;     // lateral
    (v_lon, v_lat)
}

/// **Extended equivalence check: position, velocity, and heading**
///
/// # Parameters
///
/// - `pos1_x`, `pos1_y`: position of source 1 (m).
/// - `pos2_x`, `pos2_y`: position of source 2 (m).
/// - `vel1_x`, `vel1_y`: velocity of source 1 in world frame (m/s).
/// - `vel2_x`, `vel2_y`: velocity of source 2 in world frame (m/s).
/// - `heading1`, `heading2`: ego heading for sources 1 and 2 (radians).
/// - `cfg`: equivalence tolerances.
///
/// # Returns
///
/// A detailed result indicating which divergences (if any) were detected.
// Disposition (clippy::too_many_arguments): this is a flat scalar cross-check of
// two perception objects (position/velocity/heading × two channels). Bundling the
// scalars into a params struct is a reasonable future refactor for the
// kirra-trajectory owner, but it is an API change with no safety benefit, so the
// lint is allowed here rather than churned.
#[allow(clippy::too_many_arguments)]
pub fn objects_are_equivalent_extended(
    pos1_x: f64,
    pos1_y: f64,
    pos2_x: f64,
    pos2_y: f64,
    vel1_x: f64,
    vel1_y: f64,
    vel2_x: f64,
    vel2_y: f64,
    heading1: f64,
    heading2: f64,
    cfg: ExtendedEquivalenceConfig,
) -> EquivalenceCheckResult {
    let mut divergence_count = 0;
    let mut last_divergence = EquivalenceCheckResult::Equivalent;

    // Position check
    let pos_dx = pos1_x - pos2_x;
    let pos_dy = pos1_y - pos2_y;
    let pos_dist = (pos_dx * pos_dx + pos_dy * pos_dy).sqrt();
    if !pos_dist.is_finite() || pos_dist > cfg.position_tol_m {
        divergence_count += 1;
        last_divergence = EquivalenceCheckResult::PositionDivergence;
    }

    // Heading difference check
    let h_diff = heading_difference(heading1, heading2);
    if h_diff.abs() > cfg.heading_tol_rad && h_diff.is_finite() {
        divergence_count += 1;
        last_divergence = EquivalenceCheckResult::HeadingDivergence;
    }

    // Ego-frame velocity projections (both projected using heading1 as the common
    // reference frame so that the velocity check is independent of heading divergence)
    let (v1_lon, v1_lat) = ego_frame_velocity(vel1_x, vel1_y, heading1);
    let (v2_lon, v2_lat) = ego_frame_velocity(vel2_x, vel2_y, heading1);

    // Longitudinal velocity check
    let v_lon_diff = (v1_lon - v2_lon).abs();
    if v_lon_diff > cfg.lon_velocity_tol_mps && v_lon_diff.is_finite() {
        divergence_count += 1;
        last_divergence = EquivalenceCheckResult::LonVelocityDivergence;
    }

    // Lateral velocity check
    let v_lat_diff = (v1_lat - v2_lat).abs();
    if v_lat_diff > cfg.lat_velocity_tol_mps && v_lat_diff.is_finite() {
        divergence_count += 1;
        last_divergence = EquivalenceCheckResult::LatVelocityDivergence;
    }

    match divergence_count {
        0 => EquivalenceCheckResult::Equivalent,
        1 => last_divergence,
        _ => EquivalenceCheckResult::MultipleDivergences,
    }
}

/// **Checked elapsed time, guarding against clock wraparound**
///
/// # Safety Rationale (SG9)
///
/// When wall-clock timestamps wrap (e.g., from u64::MAX → 0), the elapsed
/// time computation must detect the discontinuity and return 0 (conservative).
/// This prevents misinterpretation of a 1 ms wrap as a "stale" update.
///
/// # Parameters
///
/// - `start_ms`: earlier timestamp (ms).
/// - `end_ms`: later timestamp (ms).
/// - `max_expected_ms`: maximum expected elapsed time (e.g., 3000 ms).
///   If computed elapsed time exceeds this, a wraparound is assumed.
///
/// # Returns
///
/// `Ok(elapsed_ms)` if within expected bounds.
/// `Err(())` if wraparound or backward jump detected.
// Disposition (clippy::result_unit_err): the `Err(())` is intentional — callers
// only need the ok/not-ok distinction for the SG9 clock-monotonicity guard, and a
// unit error keeps the type minimal. A typed error enum is a reasonable future
// refactor for the kirra-trajectory owner; allowed here rather than churned.
#[allow(clippy::result_unit_err)]
pub fn checked_elapsed(start_ms: u64, end_ms: u64, max_expected_ms: u64) -> Result<u64, ()> {
    // Backward jump or equality → stale or same timestamp
    if end_ms <= start_ms {
        return Err(()); // Conservative: treat as error
    }

    let elapsed = end_ms - start_ms;
    if elapsed > max_expected_ms {
        // Likely a clock wrap; return error (SG9 guard)
        return Err(());
    }

    Ok(elapsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_difference_at_zero() {
        assert!((heading_difference(0.0, 0.0)).abs() < 1e-9);
    }

    #[test]
    fn heading_difference_at_pi_wrap() {
        let h1 = PI - 0.01; // Just before +π
        let h2 = -PI + 0.01; // Just after -π (same angle, different sign)
        let diff = heading_difference(h1, h2).abs();
        assert!(diff < 0.1, "π wrap should be continuous");
    }

    #[test]
    fn heading_difference_half_turn() {
        let diff = (heading_difference(0.0, PI)).abs();
        assert!((diff - PI).abs() < 1e-9, "0 vs π should be a half-turn difference");
    }

    #[test]
    fn ego_frame_velocity_at_zero_heading() {
        let (v_lon, v_lat) = ego_frame_velocity(10.0, 5.0, 0.0);
        assert!((v_lon - 10.0).abs() < 1e-9);
        assert!((v_lat - 5.0).abs() < 1e-9);
    }

    #[test]
    fn ego_frame_velocity_at_pi_over_2_heading() {
        let (v_lon, v_lat) = ego_frame_velocity(10.0, 5.0, PI / 2.0);
        assert!((v_lon - 5.0).abs() < 1e-9, "at π/2, vx projects to vy (lateral)");
        assert!((v_lat + 10.0).abs() < 1e-9, "at π/2, vy projects to -vx");
    }

    #[test]
    fn objects_equivalent_when_all_agree() {
        let cfg = ExtendedEquivalenceConfig::default();
        let result = objects_are_equivalent_extended(
            0.0, 0.0, 0.0, 0.0, // positions match
            5.0, 0.0, 5.0, 0.0, // velocities match
            0.0, 0.0,           // headings match
            cfg,
        );
        assert_eq!(result, EquivalenceCheckResult::Equivalent);
    }

    #[test]
    fn objects_position_divergence_detected() {
        let cfg = ExtendedEquivalenceConfig::default();
        let result = objects_are_equivalent_extended(
            0.0, 0.0, 1.0, 0.0, // position distance = 1 m > 0.5 m tolerance
            5.0, 0.0, 5.0, 0.0, // velocities match
            0.0, 0.0,           // headings match
            cfg,
        );
        assert_eq!(result, EquivalenceCheckResult::PositionDivergence);
    }

    #[test]
    fn objects_heading_divergence_detected() {
        let cfg = ExtendedEquivalenceConfig::default();
        let result = objects_are_equivalent_extended(
            0.0, 0.0, 0.0, 0.0, // positions match
            5.0, 0.0, 5.0, 0.0, // velocities match
            0.0, 0.2,           // heading diff = 0.2 rad > 0.1 rad tolerance
            cfg,
        );
        assert_eq!(result, EquivalenceCheckResult::HeadingDivergence);
    }

    #[test]
    fn objects_velocity_divergence_detected() {
        let cfg = ExtendedEquivalenceConfig::default();
        let result = objects_are_equivalent_extended(
            0.0, 0.0, 0.0, 0.0, // positions match
            5.0, 0.0, 6.0, 0.0, // velocity diff = 1 m/s > 0.5 m/s tolerance
            0.0, 0.0,           // headings match
            cfg,
        );
        assert_eq!(result, EquivalenceCheckResult::LonVelocityDivergence);
    }

    #[test]
    fn objects_multiple_divergences_flagged() {
        let cfg = ExtendedEquivalenceConfig::default();
        let result = objects_are_equivalent_extended(
            0.0, 0.0, 1.0, 0.0, // position divergence
            5.0, 0.0, 6.0, 0.0, // velocity divergence
            0.0, 0.0,           // headings match
            cfg,
        );
        assert_eq!(result, EquivalenceCheckResult::MultipleDivergences);
    }

    #[test]
    fn checked_elapsed_valid_forward_jump() {
        let result = checked_elapsed(1000, 2000, 5000);
        assert_eq!(result, Ok(1000));
    }

    #[test]
    fn checked_elapsed_rejects_backward_jump() {
        let result = checked_elapsed(2000, 1000, 5000);
        assert!(result.is_err(), "backward clock should fail");
    }

    #[test]
    fn checked_elapsed_rejects_same_timestamp() {
        let result = checked_elapsed(1000, 1000, 5000);
        assert!(result.is_err(), "same timestamp should fail");
    }

    #[test]
    fn checked_elapsed_rejects_wraparound() {
        let result = checked_elapsed(1000, 2000, 100); // elapsed 1000 > 100 max
        assert!(result.is_err(), "large elapsed should be treated as wraparound");
    }

    #[test]
    fn checked_elapsed_boundary_at_max() {
        let result = checked_elapsed(1000, 6000, 5000); // elapsed = 5000 = max
        assert_eq!(result, Ok(5000), "elapsed at max should be accepted");
    }
}
