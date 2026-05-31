/// Runtime safety state produced by RSS evaluation.
#[derive(Debug, Clone)]
pub struct RssState {
    pub safe: bool,
    pub longitudinal_margin: f64,
    pub lateral_margin: f64,
}

// ---------------------------------------------------------------------------
// Fail-safe defence in depth
// ---------------------------------------------------------------------------

/// Returned when RSS inputs are invalid or a computation is non-finite.
/// A deliberately unreachable required separation: forces the governor to
/// treat the situation as unsafe (clamp / stop) rather than ever reading a
/// misconfiguration as "no gap required". Large but FINITE so it does not
/// propagate Inf / NaN downstream.
///
/// Background: every safe-distance computation here divides by a brake or
/// lateral-accel parameter. If that parameter is zero, the division yields
/// NaN; `NaN.max(0.0) == 0.0` in Rust, which would silently report that no
/// gap is required (the unsafe direction). On any invalid input we instead
/// return this large finite distance — the governor will clamp or stop.
pub const RSS_FAILSAFE_DISTANCE_M: f64 = 1.0e6;

#[inline]
fn finite_positive(x: f64) -> bool {
    x.is_finite() && x > 0.0
}

/// Computes the lateral RSS safe-distance per IEEE 2846-2022 §5.2.
///
/// Returns the minimum required lateral separation (metres) between ego and
/// an object, accounting for both actors' reaction and braking distances.
/// Lateral velocities may be signed (positive = right, negative = left);
/// absolute values are used so the margin is always non-negative.
///
/// Parameters:
///   ego_lat_vel   — ego lateral velocity (m/s, signed)
///   obj_lat_vel   — object lateral velocity (m/s, signed)
///   lat_accel_max — maximum lateral acceleration / deceleration (m/s²);
///                   must be finite and > 0 or this function fails safe
///   reaction_time — actor reaction / response time (s); must be finite
///
/// On any invalid input (non-finite, or `lat_accel_max <= 0`) returns
/// `RSS_FAILSAFE_DISTANCE_M`. This is defence in depth — the primary
/// defence is validating the asset profile at load time (see module-level
/// note about the absence of a profile loader as of this writing).
// SAFETY: SG1 SG9 | REQ: rss-lateral-distance-failsafe | TEST: test_lat_zero_accel_is_failsafe,test_lat_nan_input_is_failsafe,test_rss_zero_ego_velocity,test_rss_result_is_finite_and_nonnegative
// (≅ Occy SG1 RSS over horizon. Non-finite or non-positive input returns
//  RSS_FAILSAFE_DISTANCE_M — defence-in-depth fail-closed for SG9.)
pub fn lateral_safe_distance(
    ego_lat_vel: f64,
    obj_lat_vel: f64,
    lat_accel_max: f64,
    reaction_time: f64,
) -> f64 {
    // Note: no debug_assert! here. The runtime guard below is the
    // authoritative safety contract; a debug_assert! would panic in
    // dev/test builds for the very inputs the fail-safe tests drive
    // (zero / non-finite divisors), making the tested fail-safe path
    // unreachable from #[cfg(test)] code.
    if !(finite_positive(lat_accel_max)
        && ego_lat_vel.is_finite()
        && obj_lat_vel.is_finite()
        && reaction_time.is_finite())
    {
        // TODO: route this through the project's safety-event / telemetry
        // channel so a bad parameter is loudly visible, not silently
        // absorbed. No such channel exists in parko-core today; tracked
        // as a follow-up alongside the missing asset-profile loader.
        return RSS_FAILSAFE_DISTANCE_M;
    }

    let lateral_stop_distance = |lat_vel: f64| -> f64 {
        let v = lat_vel.abs();
        let d_reaction = v * reaction_time + 0.5 * lat_accel_max * reaction_time.powi(2);
        let v_after = v + lat_accel_max * reaction_time;
        let d_brake = v_after.powi(2) / (2.0 * lat_accel_max);
        d_reaction + d_brake
    };
    let margin = lateral_stop_distance(ego_lat_vel) + lateral_stop_distance(obj_lat_vel);
    if !margin.is_finite() {
        return RSS_FAILSAFE_DISTANCE_M;
    }
    margin.max(0.0)
}

/// Computes the longitudinal RSS safe-distance per IEEE 2846-2022 §5.1.
///
/// Returns the minimum required gap (metres) between ego and lead vehicle.
/// The result is clamped to 0.0 — a negative raw value means the lead is
/// pulling away fast enough that no gap is needed.
///
/// Parameters:
///   ego_vel       — ego longitudinal velocity (m/s); must be finite
///   lead_vel      — lead-vehicle longitudinal velocity (m/s); must be finite
///   reaction_time — ego reaction / response time (s); must be finite
///   accel_max     — maximum ego acceleration during response phase (m/s²);
///                   must be finite (may be 0.0)
///   brake_min     — minimum ego braking deceleration after response (m/s²);
///                   must be finite and > 0 or this function fails safe
///   brake_max     — maximum lead-vehicle braking deceleration (m/s²);
///                   must be finite and > 0 or this function fails safe
///
/// On any invalid input (non-finite, or `brake_min <= 0`, or
/// `brake_max <= 0`) returns `RSS_FAILSAFE_DISTANCE_M`.
// SAFETY: SG1 SG9 | REQ: rss-longitudinal-distance-failsafe | TEST: test_rss_equal_speeds,test_rss_ego_faster,test_long_nan_input_is_failsafe,test_long_zero_brake_min_is_failsafe_not_zero,test_long_zero_brake_max_is_failsafe_not_zero,test_long_negative_brake_min_is_failsafe
// (≅ Occy SG1 longitudinal collision RSS. Non-finite or non-positive
//  brake/accel returns RSS_FAILSAFE_DISTANCE_M — fail-closed via SG9.)
pub fn longitudinal_safe_distance(
    ego_vel: f64,
    lead_vel: f64,
    reaction_time: f64,
    accel_max: f64,
    brake_min: f64,
    brake_max: f64,
) -> f64 {
    // See lateral note: no debug_assert! — runtime guard is the contract.
    if !(finite_positive(brake_min)
        && finite_positive(brake_max)
        && ego_vel.is_finite()
        && lead_vel.is_finite()
        && reaction_time.is_finite()
        && accel_max.is_finite())
    {
        // TODO: surface through a safety-event channel — see lateral.
        return RSS_FAILSAFE_DISTANCE_M;
    }

    let d_response = ego_vel * reaction_time + 0.5 * accel_max * reaction_time.powi(2);
    let v_after = ego_vel + accel_max * reaction_time;
    let d_brake_ego = v_after.powi(2) / (2.0 * brake_min);
    let d_brake_lead = lead_vel.powi(2) / (2.0 * brake_max);

    let raw = d_response + d_brake_ego - d_brake_lead;
    if !raw.is_finite() {
        return RSS_FAILSAFE_DISTANCE_M;
    }
    raw.max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-6;

    /// Equal speeds: ego must maintain reaction + brake gap even when matched.
    /// Hand-computed: d_response=5.375, d_brake_ego=132.25/12, d_brake_lead=6.25
    /// → 487/48 ≈ 10.145833
    #[test]
    fn test_rss_equal_speeds() {
        let result = longitudinal_safe_distance(10.0, 10.0, 0.5, 3.0, 6.0, 8.0);
        let expected = 487.0_f64 / 48.0;
        assert!(
            (result - expected).abs() < EPS,
            "equal speeds: got {result}, expected {expected}"
        );
    }

    /// Ego faster than lead: larger gap required.
    /// Hand-computed: d_response=10.375, d_brake_ego=462.25/12, d_brake_lead=1.5625
    /// → 142/3 ≈ 47.333333
    #[test]
    fn test_rss_ego_faster() {
        let result = longitudinal_safe_distance(20.0, 5.0, 0.5, 3.0, 6.0, 8.0);
        let expected = 142.0_f64 / 3.0;
        assert!(
            (result - expected).abs() < EPS,
            "ego faster: got {result}, expected {expected}"
        );
    }

    /// Lead much faster than ego: lead is pulling away; required gap clamps to 0.
    /// Raw: 2.875 + 42.25/12 − 56.25 ≈ −49.85 → clamped to 0.0
    #[test]
    fn test_rss_lead_faster_returns_zero() {
        let result = longitudinal_safe_distance(5.0, 30.0, 0.5, 3.0, 6.0, 8.0);
        assert_eq!(result, 0.0, "lead faster: result must clamp to 0.0, got {result}");
    }

    /// Both vehicles stopped: only reaction-phase creep creates a required gap.
    /// Hand-computed: d_response=0.375, d_brake_ego=2.25/12=0.1875, d_brake_lead=0
    /// → 0.5625
    #[test]
    fn test_rss_zero_ego_velocity() {
        let result = longitudinal_safe_distance(0.0, 0.0, 0.5, 3.0, 6.0, 8.0);
        let expected = 0.5625_f64;
        assert!(
            (result - expected).abs() < EPS,
            "zero velocity: got {result}, expected {expected}"
        );
    }

    /// Large velocities must not produce NaN, Inf, or negative values.
    #[test]
    fn test_rss_result_is_finite_and_nonnegative() {
        let result = longitudinal_safe_distance(100.0, 80.0, 0.5, 5.0, 8.0, 10.0);
        assert!(result.is_finite(), "large velocities must produce finite result, got {result}");
        assert!(result >= 0.0, "result must be non-negative, got {result}");
    }

    // ── lateral_safe_distance ────────────────────────────────────────────────

    /// Converging actors at equal speed: both stopping distances sum.
    /// Both |v|=5.0, a=4.0, t=0.5:
    ///   d_reaction = 5*0.5 + 0.5*4*0.25 = 3.0
    ///   v_after = 7.0 → d_brake = 49/8 = 6.125
    ///   d_total = 9.125 each → margin = 18.25
    #[test]
    fn test_lateral_converging_fast() {
        let result = lateral_safe_distance(5.0, -5.0, 4.0, 0.5);
        let expected = 18.25_f64;
        assert!(
            (result - expected).abs() < EPS,
            "converging fast: got {result}, expected {expected}"
        );
    }

    /// Both actors stationary: only reaction-phase creep contributes.
    /// |v|=0, a=4.0, t=0.5:
    ///   d_reaction = 0 + 0.5*4*0.25 = 0.5
    ///   v_after = 2.0 → d_brake = 4/8 = 0.5
    ///   d_total = 1.0 each → margin = 2.0
    #[test]
    fn test_lateral_both_stationary() {
        let result = lateral_safe_distance(0.0, 0.0, 4.0, 0.5);
        let expected = 2.0_f64;
        assert!(
            (result - expected).abs() < EPS,
            "both stationary: got {result}, expected {expected}"
        );
    }

    /// Asymmetric speeds produce asymmetric but summed margin.
    /// ego |v|=3.0: d_reaction=2.0, v_after=5.0, d_brake=25/8=3.125 → 5.125
    /// obj |v|=1.0: d_reaction=1.0, v_after=3.0, d_brake=9/8=1.125  → 2.125
    /// margin = 7.25
    #[test]
    fn test_lateral_asymmetric_speeds() {
        let result = lateral_safe_distance(3.0, 1.0, 4.0, 0.5);
        let expected = 7.25_f64;
        assert!(
            (result - expected).abs() < EPS,
            "asymmetric speeds: got {result}, expected {expected}"
        );
    }

    /// Negative ego velocity: absolute value must be used; result identical
    /// to the positive-velocity case.
    #[test]
    fn test_lateral_negative_velocity_matches_positive() {
        let pos = lateral_safe_distance(3.0, 1.0, 4.0, 0.5);
        let neg = lateral_safe_distance(-3.0, -1.0, 4.0, 0.5);
        assert!(
            (pos - neg).abs() < EPS,
            "negated velocities must yield same margin: pos={pos}, neg={neg}"
        );
    }

    /// Large lateral velocities must not produce NaN, Inf, or negative values.
    #[test]
    fn test_lateral_result_is_finite_and_nonnegative() {
        let result = lateral_safe_distance(30.0, -25.0, 6.0, 0.5);
        assert!(result.is_finite(), "large velocities: result must be finite, got {result}");
        assert!(result >= 0.0, "result must be non-negative, got {result}");
    }

    // ── fail-safe on invalid inputs ─────────────────────────────────────────
    //
    // The unsafe direction for these functions is "report a small required
    // gap (or 0.0) when the inputs were actually invalid". On any invalid
    // input we instead return RSS_FAILSAFE_DISTANCE_M (a deliberately
    // unreachable required separation) so the governor clamps / stops.

    /// brake_min = 0 with stationary ego (raw numerator would be 0) must NOT
    /// collapse to 0.0 via the NaN→0.0 sink — must fail safe.
    #[test]
    fn test_long_zero_brake_min_is_failsafe_not_zero() {
        let r = longitudinal_safe_distance(0.0, 0.0, 0.5, 3.0, 0.0, 8.0);
        assert!(
            r >= RSS_FAILSAFE_DISTANCE_M,
            "zero brake_min must fail safe (unreachable distance), got {r}"
        );
    }

    /// brake_max = 0 must fail safe (lead-brake divisor → NaN otherwise).
    #[test]
    fn test_long_zero_brake_max_is_failsafe_not_zero() {
        let r = longitudinal_safe_distance(10.0, 5.0, 0.5, 3.0, 6.0, 0.0);
        assert!(
            r >= RSS_FAILSAFE_DISTANCE_M,
            "zero brake_max must fail safe, got {r}"
        );
    }

    /// NaN input to longitudinal_safe_distance must yield the fail-safe
    /// distance, never 0.0.
    #[test]
    fn test_long_nan_input_is_failsafe() {
        let r = longitudinal_safe_distance(f64::NAN, 10.0, 0.5, 3.0, 6.0, 8.0);
        assert!(
            r >= RSS_FAILSAFE_DISTANCE_M,
            "NaN ego_vel must fail safe, got {r}"
        );
    }

    /// Negative brake_min (would be physically nonsensical) must fail safe.
    #[test]
    fn test_long_negative_brake_min_is_failsafe() {
        let r = longitudinal_safe_distance(10.0, 5.0, 0.5, 3.0, -6.0, 8.0);
        assert!(
            r >= RSS_FAILSAFE_DISTANCE_M,
            "negative brake_min must fail safe, got {r}"
        );
    }

    /// lat_accel_max = 0 with stationary actors (raw numerator would be 0)
    /// must fail safe — the 0/0 NaN would otherwise collapse to 0.0 m.
    #[test]
    fn test_lat_zero_accel_is_failsafe() {
        let r = lateral_safe_distance(0.0, 0.0, 0.0, 0.5);
        assert!(
            r >= RSS_FAILSAFE_DISTANCE_M,
            "zero lat_accel_max must fail safe, got {r}"
        );
    }

    /// NaN reaction_time on lateral must fail safe.
    #[test]
    fn test_lat_nan_input_is_failsafe() {
        let r = lateral_safe_distance(3.0, 1.0, 4.0, f64::NAN);
        assert!(
            r >= RSS_FAILSAFE_DISTANCE_M,
            "NaN reaction_time must fail safe, got {r}"
        );
    }
}
