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

/// Longitudinal half-window (metres) within which a **lateral** RSS shortfall is
/// treated as a genuine conflict by the trajectory/scene checkers.
///
/// RSS (Shalev-Shwartz et al.; IEEE 2846-2022 §5) defines a *dangerous* state as
/// the **conjunction** of an unsafe longitudinal AND an unsafe lateral distance —
/// two vehicles cannot collide laterally unless they are also longitudinally
/// close (alongside or imminently so). A checker that flags a lateral shortfall
/// for an object that is longitudinally FAR (a lead well ahead, or oncoming
/// traffic safely passing in the next lane) is therefore over-conservative: it
/// rejects motions RSS deems safe.
///
/// Both safety checkers keep the lateral safe-distance as a fail-closed
/// *defence-in-depth* layer (catching a cut-in beside the ego) and **gate** it on
/// longitudinal proximity. For those gates this constant is the **FLOOR** of the
/// window, not a fixed ceiling: the trajectory checker
/// (`kirra_trajectory::validation`) uses `RSS_LONGITUDINAL_CONFLICT_M.max(lon_required)`
/// and the diverse scene-RSS checker (`parko_kirra`) uses
/// `RSS_LONGITUDINAL_CONFLICT_M.max(required_long)`, so the window grows with closing
/// speed (#683/#684). A fixed 8 m ceiling deemed a high-speed cut-in originating
/// farther ahead than 8 m "laterally safe" — at the 22.35 m/s ODD cap reaction-time
/// travel alone is ~11 m — and skipped a cut-in in the 2.5–4.0 m lateral band once it
/// was >8 m ahead. The floor keeps an urban-minimum conflict window for low-speed
/// cases; above it, the longitudinal safe distance governs how far ahead a lateral
/// shortfall is still a conflict. (Non-gating references — e.g. a planner's test
/// positioning — may still read the bare 8 m value.)
///
/// (The dominant longitudinal RSS — car-following / head-on — is UNCHANGED and
/// fully governs any object that is longitudinally unsafe at any range.)
pub const RSS_LONGITUDINAL_CONFLICT_M: f64 = 8.0;

/// Lateral half-window (metres) within which the **longitudinal** RSS (rear-end /
/// head-on) is treated as a real conflict by the trajectory/scene checkers.
///
/// The dual of `RSS_LONGITUDINAL_CONFLICT_M`: a longitudinal collision is only
/// geometrically possible when the two vehicles' footprints **laterally overlap**
/// (one is in the other's path). Applying the longitudinal car-following / head-on
/// bound to an object the ego is laterally CLEAR of — a vehicle being passed in the
/// next lane, or oncoming traffic safely in the adjacent lane — is over-conservative
/// (COMPETITIVE_PLANNER_ANALYSIS §4): it was the reason a car *centered* in the ego
/// lane could not be overtaken (clearing the wider lane-alignment band needed more
/// side room than a normal lane affords).
///
/// The value is a passenger-vehicle footprint overlap (≈ two half-widths) plus a
/// small lateral-fluctuation margin — wide enough to catch any in-path object,
/// narrow enough that a normal-clearance pass is no longer a longitudinal conflict.
/// (A laterally-CLOSING object is separately covered by the lateral RSS, which is
/// itself gated on longitudinal proximity — together they approximate the RSS
/// danger conjunction while each remains a fail-closed layer.)
pub const RSS_LONGITUDINAL_OVERLAP_M: f64 = 2.5;

/// Object/ego lateral-velocity magnitude (m/s) above which a same-lane object is treated as
/// **cutting in** (a genuine side-collision risk) rather than a straight-running lead or a
/// member of a stationary queue. The lateral (side) RSS is the CONJUNCTION partner of the
/// longitudinal check: a side collision needs the pair ABREAST (longitudinally unsafe) OR
/// closing laterally. Below this threshold a *longitudinally-safe* object triggers no lateral
/// veto — admitting a safe same-lane stop (a stopped queue / a stopped lead the ego halts
/// behind) that the reaction-time swerve term in `lateral_safe_distance` otherwise rejected
/// (the §4 over-rejection) — while any real lateral closing is still caught. Small, so only
/// genuine lateral stillness is admitted; the gate fails closed on a non-finite separation first.
pub const RSS_LATERAL_MOTION_EPS_MPS: f64 = 0.1;

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
///
/// # IEEE 2846 fidelity notes (#408 — RESOLVED by the split primitive; WP-07)
///
/// The two documented limitations of this single-parameter form — (1) one
/// `lat_accel_max` collapsing the standard's distinct `a_lat,accel,max`
/// (response-phase drift) and `a_lat,brake,min` (post-response braking) roles,
/// whose per-actor stop distance is NON-MONOTONIC in the shared value
/// (`d(total)/da = rt^2 - v^2/(2a^2)`), so no single value is worst-case for
/// both; and (2) the implicit `mu = 0` lateral-fluctuation margin — are now
/// resolved by [`lateral_safe_distance_split`] (#408 Option A), which takes
/// the two roles separately plus an explicit additive `mu`. Every production
/// caller uses the split; THIS single-parameter form is retained as the exact
/// legacy semantics (`split(a, a, rt, mu=0)`) for comparison tests and any
/// integrator not yet carrying a split profile. The split with
/// `lat_brake_min <= lat_accel_max` and `mu >= 0` is STRICTLY >= this form on
/// every input (property-tested), so migrating callers is always
/// conservative-or-equal. Role values remain safety-case parameters
/// (VALIDATION-PENDING at the call sites).
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

    // Legacy single-parameter semantics == the split with both roles equal
    // and no fluctuation margin (see the fidelity note above).
    lateral_safe_distance_split(
        ego_lat_vel,
        obj_lat_vel,
        lat_accel_max,
        lat_accel_max,
        reaction_time,
        0.0,
    )
}

/// Computes the lateral RSS safe-distance per IEEE 2846-2022 §5.2 with the
/// standard's two lateral roles taken SEPARATELY (#408 Option A / WP-07),
/// plus the explicit lateral-fluctuation margin `mu_lateral_m`.
///
/// Per actor: during the response time each actor is assumed to drift TOWARD
/// the other with up to `lat_accel_max` (`a_lat,accel,max` — worst-case
/// drift), then laterally brake with at least `lat_brake_min`
/// (`a_lat,brake,min` — weakest committed braking):
///
/// ```text
/// d_reaction = v*rt + 0.5*a_accel*rt^2
/// v_after    = v + a_accel*rt
/// d_brake    = v_after^2 / (2*a_brake_min)
/// required   = d(ego) + d(obj) + mu_lateral_m
/// ```
///
/// Conservatism: with `lat_brake_min <= lat_accel_max` and
/// `mu_lateral_m >= 0` this is >= the legacy single-parameter
/// [`lateral_safe_distance`] on EVERY input (the brake denominator can only
/// shrink and `mu` only adds) — property-tested below. The two role values
/// and `mu` are safety-case parameters; the shipped defaults at the call
/// sites are VALIDATION-PENDING.
///
/// Fail-closed guards: non-finite velocities, a non-finite OR NEGATIVE
/// `reaction_time` (a negative reaction time would shrink both the response
/// distance and the braking start speed — the opposite of defence-in-depth),
/// non-positive `lat_accel_max` or `lat_brake_min`, or a non-finite/negative
/// `mu_lateral_m` all return `RSS_FAILSAFE_DISTANCE_M` (a hostile/corrupt
/// parameter must never shrink the bound), as does a non-finite sum.
// SAFETY: SG1 SG9 | REQ: rss-lateral-distance-failsafe | TEST: test_lat_split_conservative_vs_legacy,test_lat_split_mu_is_additive,test_lat_split_invalid_brake_is_failsafe,test_lat_split_negative_mu_is_failsafe
pub fn lateral_safe_distance_split(
    ego_lat_vel: f64,
    obj_lat_vel: f64,
    lat_accel_max: f64,
    lat_brake_min: f64,
    reaction_time: f64,
    mu_lateral_m: f64,
) -> f64 {
    if !(finite_positive(lat_accel_max)
        && finite_positive(lat_brake_min)
        && ego_lat_vel.is_finite()
        && obj_lat_vel.is_finite()
        && reaction_time.is_finite()
        && reaction_time >= 0.0
        && mu_lateral_m.is_finite()
        && mu_lateral_m >= 0.0)
    {
        return RSS_FAILSAFE_DISTANCE_M;
    }

    let lateral_stop_distance = |lat_vel: f64| -> f64 {
        let v = lat_vel.abs();
        // Response phase: worst-case drift toward the other actor.
        let d_reaction = v * reaction_time + 0.5 * lat_accel_max * reaction_time.powi(2);
        let v_after = v + lat_accel_max * reaction_time;
        // Braking phase: weakest committed lateral braking.
        let d_brake = v_after.powi(2) / (2.0 * lat_brake_min);
        d_reaction + d_brake
    };
    let margin =
        lateral_stop_distance(ego_lat_vel) + lateral_stop_distance(obj_lat_vel) + mu_lateral_m;
    if !margin.is_finite() {
        return RSS_FAILSAFE_DISTANCE_M;
    }
    margin.max(0.0)
}

/// [`lateral_safe_distance_split`] for a **provably stationary ego** — the
/// object's full worst-case lateral envelope plus the fluctuation margin, with
/// the ego's hypothetical response-phase swerve term dropped.
///
/// Rationale (EP-08, the stopped-pose rule): the split form charges the ego a
/// response-phase drift (`a·ρ²/2` + its braking tail) even at zero lateral
/// velocity — the worst case for a vehicle whose future motion is UNKNOWN. A
/// trajectory checker evaluating a pose at which the committed trajectory is
/// STOPPED knows that hypothesis is false: a stationary vehicle drifts
/// nowhere, and per RSS responsibility (Shalev-Shwartz §3.4) it has completed
/// its proper response — only the OBJECT's closing envelope can create the
/// conflict. Dropping the ego term is therefore not a relaxation of the
/// object's bound: a cut-in that can reach the stopped ego is still rejected
/// (its own reaction + brake envelope and `mu` are charged in full).
///
/// Same fail-closed guards as the split form; conservativeness relation:
/// `stationary_ego(obj, …) <= split(0.0, obj, …)` on every valid input (the
/// difference is exactly the zero-velocity ego term), property-tested below.
// SAFETY: SG1 SG9 | REQ: rss-lateral-stationary-ego-failsafe | TEST: test_lat_stationary_ego_is_split_minus_ego_term,test_lat_stationary_ego_invalid_params_failsafe
pub fn lateral_safe_distance_split_stationary_ego(
    obj_lat_vel: f64,
    lat_accel_max: f64,
    lat_brake_min: f64,
    reaction_time: f64,
    mu_lateral_m: f64,
) -> f64 {
    if !(finite_positive(lat_accel_max)
        && finite_positive(lat_brake_min)
        && obj_lat_vel.is_finite()
        && reaction_time.is_finite()
        && reaction_time >= 0.0
        && mu_lateral_m.is_finite()
        && mu_lateral_m >= 0.0)
    {
        return RSS_FAILSAFE_DISTANCE_M;
    }
    let v = obj_lat_vel.abs();
    let d_reaction = v * reaction_time + 0.5 * lat_accel_max * reaction_time.powi(2);
    let v_after = v + lat_accel_max * reaction_time;
    let d_brake = v_after.powi(2) / (2.0 * lat_brake_min);
    let margin = d_reaction + d_brake + mu_lateral_m;
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
///
/// # Contract: SAME-DIRECTION (lead-ahead) primitive only (tracked: #408 Obs 3)
///
/// `lead_vel` is the lead vehicle's longitudinal velocity in the EGO's direction
/// of travel: this models a lead AHEAD moving the SAME direction. The lead's
/// braking term `lead_vel^2 / (2*brake_max)` SQUARES the velocity, so its SIGN is
/// discarded. Passing an *oncoming* (negative) `lead_vel` would therefore treat
/// the oncoming actor as braking to a stop and SUBTRACT its braking distance,
/// silently UNDER-estimating the required gap. Callers MUST pass a same-direction
/// `lead_vel`; oncoming-actor geometry is out of scope for this primitive and
/// must be handled by a dedicated formula. (No `debug_assert!` enforces the sign:
/// consistent with this module's deliberate fail-closed, panic-free stance —
/// see the note in `lateral_safe_distance` — the contract is by documentation.
/// The pairwise caller `compute_scene_rss` is itself rigorously fail-closed, so
/// this is a primitive-contract note, not an exploited path.)
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

/// Search ceiling for the rule-iv closing-speed inversion (m/s). Far above any
/// ground-vehicle + emerging-actor closing speed; a sightline that admits a
/// closing speed at or beyond this does not bind the ego below the ceiling, so
/// the returned cap is `ceiling - v_emerge_max` — still finite, never `Inf`.
const OCCLUSION_SEARCH_CEILING_MPS: f64 = 150.0;

/// Maximum safe ego speed under RSS rule iv — occlusion / limited sightline
/// (IEEE 2846-2022 §5, occlusion handling).
///
/// Returns the largest ego speed (m/s) at which the ego can still maintain RSS
/// longitudinal safe distance against a worst-case actor that could emerge from
/// the occluded region at the sightline boundary `d_sight`.
///
/// Worst-case-emergence model — a safety-modelling CHOICE; reviewer, read this:
///   * A hidden actor may exist just beyond the visible range and move toward
///     the ego's conflict point at up to `v_emerge_max`. The encounter is
///     modelled as CLOSING: the ego (at `v_ego`) and the actor (at
///     `v_emerge_max`) approach a fixed conflict point at the sightline
///     boundary, so the effective approach speed is `v_ego + v_emerge_max`.
///   * The ego must keep its required longitudinal safe distance against a
///     stationary conflict point (`lead_vel = 0`) at that closing speed within
///     `d_sight`: `longitudinal_safe_distance(v_ego + v_emerge_max, 0, ..) <= d_sight`.
///   * Both knobs move the bound conservatively: a SHORTER sightline or a FASTER
///     possible emerger lowers the permitted ego speed.
///   * `v_emerge_max = 0` reduces this to the classic "stop within the available
///     sightline" (SSD) rule. A caller that cannot bound the emerging speed
///     should pass the largest credible value; the parko-kirra `Absent` path
///     takes this to its fail-closed limit (a full stop).
///
/// Method: `longitudinal_safe_distance(., 0, ..)` is continuous and monotonically
/// increasing in the closing speed, so the largest admissible closing speed is
/// found by bounded bisection and the ego cap is that minus `v_emerge_max`
/// (clamped >= 0).
///
/// FAIL-CLOSED: any invalid input (non-finite; `d_sight <= 0`; negative
/// `v_emerge_max`; or the non-finite / non-positive brake/accel conditions the
/// longitudinal primitive guards) returns `0.0` — the ego must stop. `0.0` is
/// the speed-cap analogue of `RSS_FAILSAFE_DISTANCE_M` (defence in depth, SG9).
// SAFETY: SG1 SG9 | REQ: rss-occlusion-sightline-failsafe | TEST: test_occlusion_nonpositive_dsight_is_stop,test_occlusion_nonfinite_input_is_stop,test_occlusion_invalid_brake_is_stop,test_occlusion_monotonic_in_sightline,test_occlusion_roundtrips_longitudinal,test_occlusion_faster_emerger_lowers_cap
// (≅ Occy SG1 RSS rule iv / occlusion; H9 occlusion trigger -> SG9 fail-closed
//  to a stop. Any invalid input returns 0.0 — a fail-closed speed cap.)
pub fn occlusion_limited_speed(
    d_sight: f64,
    v_emerge_max: f64,
    reaction_time: f64,
    accel_max: f64,
    brake_min: f64,
    brake_max: f64,
) -> f64 {
    // See lateral/longitudinal note: no debug_assert! — the runtime guard is the
    // safety contract, and the fail-closed tests drive these invalid inputs.
    if !(finite_positive(d_sight)
        && v_emerge_max.is_finite()
        && v_emerge_max >= 0.0
        && finite_positive(brake_min)
        && finite_positive(brake_max)
        && reaction_time.is_finite()
        && accel_max.is_finite())
    {
        return 0.0;
    }

    // required(v_close): the gap the ego needs to stop short of a fixed conflict
    // point at worst-case closing speed `v_close`. Monotone increasing in v_close
    // (lead_vel = 0, so the lead-braking term is absent).
    let required = |v_close: f64| -> f64 {
        longitudinal_safe_distance(v_close, 0.0, reaction_time, accel_max, brake_min, brake_max)
    };

    // Even a zero closing speed needs the reaction-phase creep gap; a sightline
    // below that admits no motion → stop.
    if required(0.0) > d_sight {
        return 0.0;
    }

    // Largest admissible closing speed, via monotone bisection.
    let mut lo = 0.0_f64;
    let mut hi = OCCLUSION_SEARCH_CEILING_MPS;
    if required(hi) <= d_sight {
        // Sightline does not bind below the search ceiling.
        lo = hi;
    } else {
        // ~60 iterations drives the interval far below f64 speed resolution.
        for _ in 0..60 {
            let mid = 0.5 * (lo + hi);
            if required(mid) <= d_sight {
                lo = mid;
            } else {
                hi = mid;
            }
        }
    }

    (lo - v_emerge_max).max(0.0)
}

/// Computes the longitudinal RSS safe-distance for two vehicles travelling in
/// **OPPOSITE** directions (a head-on / oncoming encounter) — the dedicated
/// formula `longitudinal_safe_distance` defers to (#408 Obs 3). Per the RSS model
/// (Shalev-Shwartz et al., *On a Formal Model of Safe and Scalable Self-Driving
/// Cars*; IEEE 2846-2022 §5.1 opposite-direction): both vehicles may accelerate
/// through their response window and must then be able to brake to a stop without
/// their paths overlapping, so the required gap is the **sum of both stopping
/// distances**.
///
/// Returns the minimum required gap (metres) along the closing axis. Both speeds
/// are CLOSING magnitudes (≥ 0): `v_ego` is the ego's speed toward the conflict,
/// `v_oncoming` is the oncoming vehicle's speed toward it. (Squaring discards
/// sign, but the contract is closing magnitudes — pass `abs`.)
///
/// Parameters:
///   v_ego         — ego closing speed (m/s); must be finite
///   v_oncoming    — oncoming closing speed (m/s); must be finite
///   reaction_time — response time applied to BOTH vehicles (s); must be finite
///   accel_max     — maximum acceleration during the response phase (m/s²);
///                   applied to both; must be finite (may be 0.0)
///   brake_min_ego      — minimum ego braking after response (m/s²); > 0 or fails safe
///   brake_min_oncoming — minimum oncoming braking after response (m/s²); > 0 or
///                        fails safe
///
/// # The brake asymmetry — a safety-modelling CHOICE; reviewer, read this
///
/// RSS expects the vehicle in its **correct** lane to brake at the normal minimum,
/// but the vehicle in the **wrong** lane (the overtaker crossing into oncoming —
/// i.e. the EGO during an overtake) to apply a *stronger* effort. Exposing both
/// `brake_min_*` lets the caller encode that: during an ego overtake, pass the
/// ego's larger brake. Equal values give the symmetric, more conservative bound.
/// A LARGER brake SHRINKS that vehicle's stopping term, so mis-assigning the
/// stronger brake to the wrong vehicle is non-conservative — the caller owns this.
///
/// On any invalid input (non-finite, or either `brake_min_* <= 0`) returns
/// `RSS_FAILSAFE_DISTANCE_M` — fail-closed, defence in depth (SG9).
// SAFETY: SG1 SG9 | REQ: rss-opposite-direction-distance-failsafe | TEST: test_opposite_equal_closing,test_opposite_sums_both_stopping_distances,test_opposite_monotonic_in_closing,test_opposite_stronger_brake_shrinks_gap,test_opposite_nan_input_is_failsafe,test_opposite_zero_brake_ego_is_failsafe,test_opposite_zero_brake_oncoming_is_failsafe
// (≅ Occy SG1 head-on collision RSS; closes #408 Obs 3 — the oncoming-actor
//  geometry the same-direction primitive declares out of scope. Non-finite or
//  non-positive brake/accel returns RSS_FAILSAFE_DISTANCE_M — fail-closed via SG9.)
pub fn opposite_direction_safe_distance(
    v_ego: f64,
    v_oncoming: f64,
    reaction_time: f64,
    accel_max: f64,
    brake_min_ego: f64,
    brake_min_oncoming: f64,
) -> f64 {
    // See longitudinal/lateral note: no debug_assert! — the runtime guard is the
    // safety contract, exercised by the fail-closed tests.
    if !(finite_positive(brake_min_ego)
        && finite_positive(brake_min_oncoming)
        && v_ego.is_finite()
        && v_oncoming.is_finite()
        && reaction_time.is_finite()
        && accel_max.is_finite())
    {
        return RSS_FAILSAFE_DISTANCE_M;
    }

    // Each vehicle: response-phase travel (creep + acceleration), then brake to a
    // stop from its post-response speed. The full stopping distances sum because
    // the vehicles approach along the same axis from opposite ends.
    let stopping = |v: f64, brake_min: f64| -> f64 {
        let d_response = v * reaction_time + 0.5 * accel_max * reaction_time.powi(2);
        let v_after = v + accel_max * reaction_time;
        d_response + v_after.powi(2) / (2.0 * brake_min)
    };

    let raw = stopping(v_ego, brake_min_ego) + stopping(v_oncoming, brake_min_oncoming);
    if !raw.is_finite() {
        return RSS_FAILSAFE_DISTANCE_M;
    }
    raw.max(0.0)
}

// ---------------------------------------------------------------------------
// Agent-set input model for pairwise RSS (issue #92)
// ---------------------------------------------------------------------------

/// Maximum number of agents evaluated in a single pairwise RSS pass.
///
/// Bounds the worst-case execution time of the checker. A scene carrying more
/// agents than this is treated as fail-closed (unsafe) rather than evaluated
/// partially — a truncated safety check is worse than a conservative stop.
pub const MAX_RSS_AGENTS: usize = 64;

/// Ego RSS profile parameters (vehicle constants) the safe-distance primitives
/// need beyond the per-pair kinematics.
///
/// Invalid values need no validation here: the primitives fail closed
/// (`RSS_FAILSAFE_DISTANCE_M`) on any non-finite / non-positive brake or accel,
/// so a bad profile yields an unreachable *required* distance — never a falsely
/// small one.
#[derive(Debug, Clone, Copy)]
pub struct RssParams {
    /// Actor reaction / response time (s).
    pub reaction_time: f64,
    /// Maximum ego longitudinal acceleration during the response phase (m/s²).
    pub accel_max: f64,
    /// Minimum ego braking deceleration after response (m/s²); must be > 0.
    pub brake_min: f64,
    /// Maximum lead-vehicle braking deceleration (m/s²); must be > 0.
    pub brake_max: f64,
    /// Maximum lateral acceleration / deceleration (m/s²); must be > 0.
    /// With the WP-07 split this is the RESPONSE-PHASE role only
    /// (`a_lat,accel,max` — worst-case drift toward the other actor).
    pub lat_accel_max: f64,
    /// Minimum committed lateral BRAKING deceleration after the response
    /// phase (`a_lat,brake,min`, m/s²); must be > 0 and <= `lat_accel_max`
    /// for the split to be conservative vs the legacy single-parameter form
    /// (WP-07 / #408 Option A). VALIDATION-PENDING at every call site.
    pub lat_brake_min: f64,
    /// Additive lateral-fluctuation margin (IEEE 2846 §5.2 `mu`, metres);
    /// must be finite and >= 0. VALIDATION-PENDING.
    pub mu_lateral_m: f64,
}

/// One perceived agent's measured state for a pairwise RSS check.
///
/// Carries the ACTUAL measured separations alongside the velocities the
/// primitives need, so the checker can compare actual-vs-required per axis.
#[derive(Debug, Clone, Copy)]
pub struct RssAgent {
    /// Ego longitudinal velocity (m/s).
    pub ego_vel: f64,
    /// Lead-vehicle longitudinal velocity (m/s).
    pub lead_vel: f64,
    /// ACTUAL longitudinal gap to the lead (m).
    pub actual_longitudinal_gap_m: f64,
    /// Ego lateral velocity (m/s, signed).
    pub ego_lat_vel: f64,
    /// Object lateral velocity (m/s, signed).
    pub obj_lat_vel: f64,
    /// ACTUAL lateral separation to the object (m).
    pub actual_lateral_separation_m: f64,
    /// Is this agent **oncoming** (opposing traffic / head-on)? When `true`, the
    /// longitudinal axis is checked with [`opposite_direction_safe_distance`] (the
    /// sum of both closing stopping distances) instead of the same-direction lead
    /// primitive — and `ego_vel` / `lead_vel` are then the two closing speeds. The
    /// integrator sets this from the lane graph (an object in an opposing lane;
    /// `kirra_planner::LaneGraph::is_oncoming_at`). Default `false` = same-direction.
    pub oncoming: bool,
}

/// The agent scene the governor sees this tick.
///
/// The ABSENT vs KNOWN-EMPTY distinction is safety-critical: "no agents in the
/// list" must NOT be read as "the scene is clear". Only perception that ran and
/// reported a clear scene (`KnownEmpty`) is RSS-safe; a missing perception
/// update (`Absent`) is fail-closed UNSAFE.
#[derive(Debug, Clone)]
pub enum AgentScene {
    /// No perception data this tick (sensor gap) → fail-closed UNSAFE.
    Absent,
    /// Perception ran and reported a clear scene → RSS-safe.
    KnownEmpty,
    /// One or more agents to check pairwise. An empty vector here is treated
    /// fail-closed (callers must use `KnownEmpty` for a verified-clear scene).
    Agents(Vec<RssAgent>),
}

/// The occlusion / sightline descriptor the governor sees this tick, for RSS
/// rule iv (issue #122). Mirrors [`AgentScene`]'s ABSENT vs KNOWN distinction,
/// which is safety-critical here too: a MISSING sightline assessment must NOT be
/// read as "the road is clear".
#[derive(Debug, Clone, Copy)]
pub enum OcclusionScene {
    /// No occlusion / sightline assessment this tick (perception gap) →
    /// fail-closed: treat as worst-case occlusion (zero sightline → the ego must
    /// stop). ABSENT is NOT `KnownClear`.
    Absent,
    /// Perception ran and the relevant sightline is verified clear → RSS rule iv
    /// imposes no speed bound.
    KnownClear,
    /// A limited sightline: the nearest occluded-region boundary is `d_sight_m`
    /// along the ego path, and an actor could emerge from it at up to
    /// `v_emerge_max_mps`.
    Limited {
        d_sight_m: f64,
        v_emerge_max_mps: f64,
    },
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
        assert_eq!(
            result, 0.0,
            "lead faster: result must clamp to 0.0, got {result}"
        );
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
        assert!(
            result.is_finite(),
            "large velocities must produce finite result, got {result}"
        );
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
        assert!(
            result.is_finite(),
            "large velocities: result must be finite, got {result}"
        );
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

    // ── lateral_safe_distance_split (WP-07 / #408) ─────────────────────────

    /// STRICT CONSERVATISM: the split with `lat_brake_min <= lat_accel_max`
    /// and `mu >= 0` is >= the legacy single-parameter form on every input.
    /// Property-tested over the realistic parameter space; the legacy form is
    /// exactly `split(a, a, rt, 0)`, so equality holds iff brake==accel and
    /// mu==0.
    #[test]
    fn test_lat_split_conservative_vs_legacy() {
        use proptest::prelude::*;
        let mut runner = proptest::test_runner::TestRunner::default();
        runner
            .run(
                &(
                    -40.0f64..40.0, // ego_lat_vel
                    -40.0f64..40.0, // obj_lat_vel
                    0.1f64..10.0,   // lat_accel_max
                    0.05f64..1.0,   // brake fraction (brake_min = f * accel_max)
                    0.05f64..2.0,   // reaction_time
                    0.0f64..1.0,    // mu
                ),
                |(e, o, a, f, rt, mu)| {
                    let legacy = lateral_safe_distance(e, o, a, rt);
                    let split = lateral_safe_distance_split(e, o, a, f * a, rt, mu);
                    prop_assert!(
                        split >= legacy - 1e-12,
                        "split must never be less conservative: split={split} legacy={legacy}"
                    );
                    // And monotone in mu / anti-monotone in brake_min:
                    let weaker_brake = lateral_safe_distance_split(e, o, a, 0.5 * f * a, rt, mu);
                    prop_assert!(weaker_brake >= split - 1e-12);
                    Ok(())
                },
            )
            .unwrap();
    }

    /// `mu` is exactly additive on the finite path.
    #[test]
    fn test_lat_split_mu_is_additive() {
        let base = lateral_safe_distance_split(3.0, 1.0, 4.0, 2.8, 0.5, 0.0);
        let with_mu = lateral_safe_distance_split(3.0, 1.0, 4.0, 2.8, 0.5, 0.2);
        assert!((with_mu - base - 0.2).abs() < 1e-12);
    }

    /// A non-positive / non-finite `lat_brake_min` fails safe — never a small
    /// bound from a corrupt braking parameter.
    #[test]
    fn test_lat_split_invalid_brake_is_failsafe() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                lateral_safe_distance_split(3.0, 1.0, 4.0, bad, 0.5, 0.0),
                RSS_FAILSAFE_DISTANCE_M
            );
        }
    }

    /// The stationary-ego form is exactly the split form minus the
    /// zero-velocity ego term (the response-phase swerve + its braking tail).
    #[test]
    fn test_lat_stationary_ego_is_split_minus_ego_term() {
        for obj_v in [0.0, 0.5, 1.5, 4.0] {
            let (a, b, rho, mu) = (3.5, 2.45, 0.5, 0.2);
            let split = lateral_safe_distance_split(0.0, obj_v, a, b, rho, mu);
            let stationary = lateral_safe_distance_split_stationary_ego(obj_v, a, b, rho, mu);
            let ego_zero_term = 0.5 * a * rho * rho + (a * rho) * (a * rho) / (2.0 * b);
            assert!(
                (split - stationary - ego_zero_term).abs() < 1e-12,
                "obj_v={obj_v}: split {split} - stationary {stationary} != ego term {ego_zero_term}"
            );
            assert!(stationary <= split, "never larger than the split form");
        }
    }

    #[test]
    fn test_lat_stationary_ego_invalid_params_failsafe() {
        assert_eq!(
            lateral_safe_distance_split_stationary_ego(f64::NAN, 3.5, 2.45, 0.5, 0.2),
            RSS_FAILSAFE_DISTANCE_M
        );
        assert_eq!(
            lateral_safe_distance_split_stationary_ego(1.0, 0.0, 2.45, 0.5, 0.2),
            RSS_FAILSAFE_DISTANCE_M
        );
        assert_eq!(
            lateral_safe_distance_split_stationary_ego(1.0, 3.5, 2.45, -0.1, 0.2),
            RSS_FAILSAFE_DISTANCE_M
        );
        assert_eq!(
            lateral_safe_distance_split_stationary_ego(1.0, 3.5, 2.45, 0.5, -0.2),
            RSS_FAILSAFE_DISTANCE_M
        );
    }

    /// A negative or non-finite `mu` fails safe (a margin must only ever ADD).
    #[test]
    fn test_lat_split_negative_mu_is_failsafe() {
        for bad in [-0.1, f64::NAN, f64::NEG_INFINITY] {
            assert_eq!(
                lateral_safe_distance_split(3.0, 1.0, 4.0, 2.8, 0.5, bad),
                RSS_FAILSAFE_DISTANCE_M
            );
        }
    }

    /// A negative-but-finite reaction time fails safe (Copilot #855): it would
    /// otherwise shrink both the response distance and the braking start speed,
    /// producing a SMALLER bound — the opposite of defence-in-depth. The legacy
    /// wrapper delegates through the split, so it is covered too.
    #[test]
    fn test_lat_split_negative_reaction_time_is_failsafe() {
        assert_eq!(
            lateral_safe_distance_split(3.0, 1.0, 4.0, 2.8, -0.5, 0.0),
            RSS_FAILSAFE_DISTANCE_M
        );
        assert_eq!(
            lateral_safe_distance(3.0, 1.0, 4.0, -0.5),
            RSS_FAILSAFE_DISTANCE_M,
            "the legacy wrapper inherits the negative-reaction-time guard"
        );
    }

    /// The legacy wrapper is EXACTLY `split(a, a, rt, 0)` — the delegation
    /// cannot drift.
    #[test]
    fn test_lat_legacy_equals_split_with_equal_roles() {
        for (e, o, a, rt) in [
            (5.0, -5.0, 4.0, 0.5),
            (0.0, 0.0, 4.0, 0.5),
            (30.0, -25.0, 6.0, 0.5),
        ] {
            assert_eq!(
                lateral_safe_distance(e, o, a, rt),
                lateral_safe_distance_split(e, o, a, a, rt, 0.0)
            );
        }
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

    /// SG1 / SG9 / GAP 4: lateral post-arithmetic overflow failsafe.
    /// All inputs are individually finite and `lat_accel_max > 0`, so the
    /// input gate passes — but `reaction_time.powi(2)` overflows the f64
    /// range, producing Inf inside `lateral_stop_distance`. The post-
    /// arithmetic `margin.is_finite()` check at l.83 must catch this and
    /// return the failsafe distance, never silently leak Inf or `0.0`.
    #[test]
    fn test_lat_post_arithmetic_overflow_is_failsafe() {
        let r = lateral_safe_distance(1.0, 0.0, 1.0e-10, 1.0e200);
        assert!(r.is_finite(), "must not leak Inf downstream; got {r}");
        assert!(
            r >= RSS_FAILSAFE_DISTANCE_M,
            "post-arithmetic overflow must fail safe, got {r}"
        );
    }

    /// SG1 / SG9 / GAP 5: longitudinal post-arithmetic overflow failsafe.
    /// All inputs pass the entry gate (`brake_min`, `brake_max` both finite
    /// and > 0; ego/lead velocities, reaction_time, accel_max finite), but
    /// the squared `reaction_time` term overflows mid-calculation. The
    /// `raw.is_finite()` check at l.137 must catch this and return the
    /// failsafe distance.
    #[test]
    fn test_long_post_arithmetic_overflow_is_failsafe() {
        let r = longitudinal_safe_distance(
            1.0, 1.0, 1.0e200, // reaction_time → reaction_time.powi(2) overflows
            1.0e-10, // accel_max  finite-positive
            1.0e-10, // brake_min  finite-positive
            1.0e-10, // brake_max  finite-positive
        );
        assert!(r.is_finite(), "must not leak Inf downstream; got {r}");
        assert!(
            r >= RSS_FAILSAFE_DISTANCE_M,
            "post-arithmetic overflow must fail safe, got {r}"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // MC/DC pair-completion tests (S3 / #115 — KIRRA-OCCY-MCDC-001).
    //
    // The RSS entry guards in `lateral_safe_distance` (l.63–66) and
    // `longitudinal_safe_distance` (l.120–125) are AND-chains of
    // `is_finite()` predicates. The pre-existing tests cover the full
    // pass case and the `finite_positive` (accel/brake) clauses. The
    // remaining independent-effect demonstrations are for each velocity /
    // reaction-time / accel_max `is_finite()` clause taken in isolation —
    // each test below leaves every other clause true and only the named
    // clause becomes false, so the entire decision flips on that clause.
    // ─────────────────────────────────────────────────────────────────────

    /// MC/DC: lateral guard — `ego_lat_vel.is_finite()` (l.64).
    /// Independent-effect: NaN ego_lat_vel with all others valid.
    #[test]
    fn test_lat_nan_ego_lat_vel_is_failsafe() {
        let r = lateral_safe_distance(f64::NAN, 0.0, 4.0, 0.5);
        assert!(
            r >= RSS_FAILSAFE_DISTANCE_M,
            "NaN ego_lat_vel must fail safe, got {r}"
        );
    }

    /// MC/DC: lateral guard — `obj_lat_vel.is_finite()` (l.65).
    /// Independent-effect: Inf obj_lat_vel with all others valid.
    #[test]
    fn test_lat_inf_obj_lat_vel_is_failsafe() {
        let r = lateral_safe_distance(0.0, f64::INFINITY, 4.0, 0.5);
        assert!(
            r >= RSS_FAILSAFE_DISTANCE_M,
            "Inf obj_lat_vel must fail safe, got {r}"
        );
    }

    /// MC/DC: longitudinal guard — `ego_vel.is_finite()` (l.123).
    /// Independent-effect: Inf ego_vel with all others valid.
    #[test]
    fn test_long_inf_ego_vel_is_failsafe() {
        let r = longitudinal_safe_distance(f64::INFINITY, 5.0, 0.5, 3.0, 6.0, 8.0);
        assert!(
            r >= RSS_FAILSAFE_DISTANCE_M,
            "Inf ego_vel must fail safe, got {r}"
        );
    }

    /// MC/DC: longitudinal guard — `lead_vel.is_finite()` (l.124).
    /// Independent-effect: NaN lead_vel with all others valid.
    #[test]
    fn test_long_nan_lead_vel_is_failsafe() {
        let r = longitudinal_safe_distance(10.0, f64::NAN, 0.5, 3.0, 6.0, 8.0);
        assert!(
            r >= RSS_FAILSAFE_DISTANCE_M,
            "NaN lead_vel must fail safe, got {r}"
        );
    }

    /// MC/DC: longitudinal guard — `reaction_time.is_finite()` (l.125).
    /// Independent-effect: NaN reaction_time with all others valid.
    #[test]
    fn test_long_nan_reaction_time_is_failsafe() {
        let r = longitudinal_safe_distance(10.0, 5.0, f64::NAN, 3.0, 6.0, 8.0);
        assert!(
            r >= RSS_FAILSAFE_DISTANCE_M,
            "NaN reaction_time must fail safe, got {r}"
        );
    }

    /// MC/DC: longitudinal guard — `accel_max.is_finite()` (l.125 / accel_max).
    /// Independent-effect: NaN accel_max with all others valid (and the
    /// `finite_positive` checks for brake_min/brake_max already true so
    /// this is the sole determinant).
    #[test]
    fn test_long_nan_accel_max_is_failsafe() {
        let r = longitudinal_safe_distance(10.0, 5.0, 0.5, f64::NAN, 6.0, 8.0);
        assert!(
            r >= RSS_FAILSAFE_DISTANCE_M,
            "NaN accel_max must fail safe, got {r}"
        );
    }

    /// MC/DC: `finite_positive(x)` second clause — `x > 0.0` false arm
    /// while `x.is_finite()` remains true. Already covered by
    /// `test_long_zero_brake_min_is_failsafe_not_zero` (brake_min=0.0) and
    /// `test_long_negative_brake_min_is_failsafe` (brake_min<0). This
    /// explicit pair anchor pins the predicate's independent effect at
    /// the smallest finite positive boundary against a tiny negative.
    #[test]
    fn test_finite_positive_independent_effect_at_zero_boundary() {
        // Tiny positive — finite_positive returns true.
        let r1 = longitudinal_safe_distance(0.0, 0.0, 0.0, 0.0, f64::MIN_POSITIVE, 1.0);
        // Tiny non-positive — finite_positive returns false → failsafe.
        let r2 = longitudinal_safe_distance(0.0, 0.0, 0.0, 0.0, -f64::MIN_POSITIVE, 1.0);
        assert!(
            r1 < RSS_FAILSAFE_DISTANCE_M,
            "tiny positive brake_min passes the guard, got {r1}"
        );
        assert!(
            r2 >= RSS_FAILSAFE_DISTANCE_M,
            "tiny negative brake_min must fail safe, got {r2}"
        );
    }

    // ── occlusion_limited_speed (RSS rule iv, issue #122) ────────────────────
    // Shared ego params for the occlusion tests: rt=0.5, accel=2.0, brakes=6.0.

    /// d_sight <= 0 → ego must stop (0.0). Zero and negative sightlines.
    #[test]
    fn test_occlusion_nonpositive_dsight_is_stop() {
        assert_eq!(
            occlusion_limited_speed(0.0, 0.0, 0.5, 2.0, 6.0, 6.0),
            0.0,
            "zero sightline must fail closed to a stop"
        );
        assert_eq!(
            occlusion_limited_speed(-5.0, 0.0, 0.5, 2.0, 6.0, 6.0),
            0.0,
            "negative sightline must fail closed to a stop"
        );
    }

    /// Non-finite inputs (incl. negative emerge velocity) → 0.0.
    #[test]
    fn test_occlusion_nonfinite_input_is_stop() {
        assert_eq!(
            occlusion_limited_speed(f64::NAN, 0.0, 0.5, 2.0, 6.0, 6.0),
            0.0,
            "NaN d_sight → stop"
        );
        assert_eq!(
            occlusion_limited_speed(50.0, f64::INFINITY, 0.5, 2.0, 6.0, 6.0),
            0.0,
            "Inf v_emerge → stop"
        );
        assert_eq!(
            occlusion_limited_speed(50.0, -1.0, 0.5, 2.0, 6.0, 6.0),
            0.0,
            "negative v_emerge is invalid → stop"
        );
        assert_eq!(
            occlusion_limited_speed(50.0, 0.0, f64::NAN, 2.0, 6.0, 6.0),
            0.0,
            "NaN reaction_time → stop"
        );
    }

    /// The same non-positive/non-finite brake/accel guard as the primitives.
    #[test]
    fn test_occlusion_invalid_brake_is_stop() {
        assert_eq!(
            occlusion_limited_speed(50.0, 0.0, 0.5, 2.0, 0.0, 6.0),
            0.0,
            "zero brake_min → stop"
        );
        assert_eq!(
            occlusion_limited_speed(50.0, 0.0, 0.5, 2.0, 6.0, -1.0),
            0.0,
            "negative brake_max → stop"
        );
    }

    /// Monotonicity: a longer sightline allows a greater-or-equal speed, and a
    /// much longer one strictly more.
    #[test]
    fn test_occlusion_monotonic_in_sightline() {
        let cap = |d: f64| occlusion_limited_speed(d, 0.0, 0.5, 2.0, 6.0, 6.0);
        let (a, b, c) = (cap(10.0), cap(40.0), cap(120.0));
        assert!(
            a <= b && b <= c,
            "more sightline must allow >= speed: {a}, {b}, {c}"
        );
        assert!(
            c > a,
            "a much longer sightline must allow a strictly higher speed: {a} vs {c}"
        );
    }

    /// Hand-anchored via the longitudinal primitive as the ORACLE: take a closing
    /// speed, compute the sightline it requires, and confirm the inverse recovers
    /// it (v_emerge_max = 0, so the ego cap equals the closing speed).
    #[test]
    fn test_occlusion_roundtrips_longitudinal() {
        let (rt, acc, bmin, bmax) = (0.5, 2.0, 6.0, 6.0);
        let v = 12.0_f64;
        let d = longitudinal_safe_distance(v, 0.0, rt, acc, bmin, bmax);
        let cap = occlusion_limited_speed(d, 0.0, rt, acc, bmin, bmax);
        assert!(
            (cap - v).abs() < 1e-3,
            "the inverse of longitudinal_safe_distance must recover {v}, got {cap}"
        );
    }

    /// A faster possible emerger lowers the cap (the conservative direction).
    #[test]
    fn test_occlusion_faster_emerger_lowers_cap() {
        let slow = occlusion_limited_speed(60.0, 0.0, 0.5, 2.0, 6.0, 6.0);
        let fast = occlusion_limited_speed(60.0, 5.0, 0.5, 2.0, 6.0, 6.0);
        assert!(
            fast < slow,
            "a faster possible emerger must lower the cap: slow={slow}, fast={fast}"
        );
        assert!(fast >= 0.0, "the cap is never negative, got {fast}");
    }

    // ── opposite-direction (head-on) primitive (#408 Obs 3) ─────────────────

    /// Both at rest needs zero gap; both at equal closing speed gives the sum of
    /// two identical stopping distances. (`accel_max = 0` so a stationary vehicle
    /// contributes exactly 0 — with worst-case response acceleration even a
    /// stationary vehicle carries a nonzero creep+brake term, exercised separately.)
    #[test]
    fn test_opposite_equal_closing() {
        // Both stationary, no reaction creep (ρ=0) → zero required gap.
        assert!(opposite_direction_safe_distance(0.0, 0.0, 0.0, 0.0, 6.0, 6.0).abs() < EPS);
        // Symmetric closing: result is exactly twice one vehicle's stopping distance.
        let d = opposite_direction_safe_distance(10.0, 10.0, 0.5, 0.0, 6.0, 6.0);
        let one = opposite_direction_safe_distance(10.0, 0.0, 0.5, 0.0, 6.0, 6.0);
        assert!(
            (d - 2.0 * one).abs() < EPS,
            "equal closing = 2× one side: d={d}, one={one}"
        );
    }

    /// The required gap is the SUM of both vehicles' stopping distances — it must
    /// exceed the ego-against-a-stationary-actor distance alone, because the
    /// oncoming vehicle adds its own stopping distance. (`accel_max = 0` so the
    /// additive identity is exact; the worst-case response term cancels.)
    #[test]
    fn test_opposite_sums_both_stopping_distances() {
        let head_on = opposite_direction_safe_distance(12.0, 8.0, 0.5, 0.0, 6.0, 6.0);
        let ego_only = opposite_direction_safe_distance(12.0, 0.0, 0.5, 0.0, 6.0, 6.0);
        let onc_only = opposite_direction_safe_distance(0.0, 8.0, 0.5, 0.0, 6.0, 6.0);
        assert!(
            head_on > ego_only,
            "oncoming motion adds to the required gap"
        );
        assert!(
            (head_on - (ego_only + onc_only)).abs() < EPS,
            "gap is additive across both"
        );
    }

    /// With worst-case response acceleration (`accel_max > 0`) even a *stationary*
    /// oncoming vehicle contributes a nonzero creep+brake term — the conservative
    /// RSS response model (matches `longitudinal_safe_distance`).
    #[test]
    fn test_opposite_stationary_oncoming_still_contributes_under_accel() {
        let onc_only = opposite_direction_safe_distance(0.0, 0.0, 0.5, 2.0, 6.0, 6.0);
        // 0.5·a·ρ² + (a·ρ)²/(2·b) per side, ×2 = 2·(0.25 + 1/12) ≈ 0.667.
        assert!(
            onc_only > 0.6 && onc_only < 0.7,
            "worst-case response term, got {onc_only}"
        );
    }

    /// Monotonic: a faster oncoming closing speed strictly increases the gap.
    #[test]
    fn test_opposite_monotonic_in_closing() {
        let slow = opposite_direction_safe_distance(10.0, 5.0, 0.5, 2.0, 6.0, 6.0);
        let fast = opposite_direction_safe_distance(10.0, 15.0, 0.5, 2.0, 6.0, 6.0);
        assert!(
            fast > slow,
            "faster oncoming → larger required gap: slow={slow}, fast={fast}"
        );
        assert!(slow.is_finite() && slow >= 0.0);
    }

    /// A stronger brake on a vehicle SHRINKS its stopping term (the asymmetry knob).
    #[test]
    fn test_opposite_stronger_brake_shrinks_gap() {
        let weak = opposite_direction_safe_distance(10.0, 10.0, 0.5, 2.0, 4.0, 6.0);
        let strong = opposite_direction_safe_distance(10.0, 10.0, 0.5, 2.0, 9.0, 6.0);
        assert!(
            strong < weak,
            "a stronger ego brake reduces the gap: weak={weak}, strong={strong}"
        );
    }

    /// NaN input must fail safe (an unreachable gap), not collapse to a small value.
    #[test]
    fn test_opposite_nan_input_is_failsafe() {
        let r = opposite_direction_safe_distance(f64::NAN, 8.0, 0.5, 2.0, 6.0, 6.0);
        assert!(
            r >= RSS_FAILSAFE_DISTANCE_M,
            "NaN closing speed must fail safe, got {r}"
        );
    }

    /// Zero ego brake with a stationary ego (raw numerator 0) must NOT collapse to
    /// 0.0 via a NaN→0 sink — must fail safe.
    #[test]
    fn test_opposite_zero_brake_ego_is_failsafe() {
        let r = opposite_direction_safe_distance(0.0, 0.0, 0.5, 2.0, 0.0, 6.0);
        assert!(
            r >= RSS_FAILSAFE_DISTANCE_M,
            "zero ego brake must fail safe, got {r}"
        );
    }

    /// Zero oncoming brake (divisor → NaN otherwise) must fail safe.
    #[test]
    fn test_opposite_zero_brake_oncoming_is_failsafe() {
        let r = opposite_direction_safe_distance(10.0, 5.0, 0.5, 2.0, 6.0, 0.0);
        assert!(
            r >= RSS_FAILSAFE_DISTANCE_M,
            "zero oncoming brake must fail safe, got {r}"
        );
    }
}
