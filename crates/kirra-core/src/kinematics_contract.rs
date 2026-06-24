// crates/kirra-core/src/kinematics_contract.rs (de-monolith Stage 3: the FROZEN talisman, relocated verbatim)
//
// Deterministic vehicle kinematics safety contract for Kirra AV flight envelope protection.
//
// This module answers exactly one question: "Is this proposed vehicle command physically
// safe to execute on this platform, given the current kinematic state?"
//
// The verification pipeline runs checks in strict priority order. A check that fires
// returns immediately. See docs/kinematics_envelope_protection.md for the full spec.
//
// Security invariants respected:
//   - No interaction with KIRRA_ADMIN_TOKEN or any auth primitives (wrong layer)
//   - No DDS/ROS2 publishing (ros2_adapter.rs owns NaN/Inf rejection for that path)
//   - LockedOut handling belongs to the calling policy layer
//   - All arithmetic is deterministic; no RNG, no I/O, no async

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// ODD Operational Speed Cap
// ---------------------------------------------------------------------------

/// Urban Occy deployment ODD operational speed cap (m/s).
///
/// Source: ADR-0001 (`docs/adr/0001-occy-odd-speed-cap.md`) /
/// SPEED_ENVELOPE.md — derived from the RSS stopping-distance chain with
/// ~28% margin on the worst-case 130 m sensor detection range.
///
/// This is the **operational ODD ceiling**, NOT the vehicle physical
/// maximum (`nominal_reference_profile().max_speed_mps = 35.0`). The
/// two concepts are intentionally kept distinct:
///
///   - `max_speed_mps`        — vehicle mechanical / drivetrain ceiling
///   - `odd_speed_cap_mps`    — safety-case operational ceiling (per ODD)
///
/// The enforced ceiling at runtime is `min(max_speed_mps,
/// odd_speed_cap_mps)`. See `VehicleKinematicsContract::effective_max_speed_mps`.
///
/// Exact value: 50 mph = 22.352 m/s; rounded to **22.35 m/s** per
/// SPEED_ENVELOPE.md line 116 and ADR-0001 line 29. Validated unchanged
/// in S8 Item C (`docs/safety/OCCY_SPEED_CAP_VALIDATION.md`,
/// KIRRA-OCCY-SPEED-VAL-001).
pub const URBAN_ODD_SPEED_CAP_MPS: f64 = 22.35;

// ---------------------------------------------------------------------------
// Contract Profiles
// ---------------------------------------------------------------------------

/// All physical limits that govern whether a proposed vehicle command is admissible.
///
/// Two canonical constructors are provided for Nominal and MRC posture states.
/// Custom profiles may be constructed for non-standard platforms.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
pub struct VehicleKinematicsContract {
    /// Maximum allowable forward/reverse speed (m/s). Hard upper bound.
    pub max_speed_mps: f64,
    /// Maximum allowable linear acceleration rate (m/s²).
    pub max_accel_mps2: f64,
    /// Maximum allowable linear deceleration rate (m/s²). Service braking only;
    /// emergency braking is handled by a separate hardware interlock layer.
    pub max_brake_mps2: f64,
    /// Maximum allowable absolute steering angle (degrees). Physical rack limit.
    pub max_steering_deg: f64,
    /// Maximum allowable steering angle rate-of-change (degrees/second).
    pub max_steering_rate_deg_s: f64,
    /// Minimum required following distance (meters). Stored for profile completeness;
    /// not evaluated in `validate_vehicle_command`.
    pub min_follow_distance_m: f64,
    /// Maximum allowable lateral acceleration from the bicycle model (m/s²).
    /// `a_lat = (v² × |tan(δ)|) / L ≤ max_lateral_accel_mps2`
    pub max_lateral_accel_mps2: f64,
    /// Vehicle wheelbase (meters). Used in the bicycle model denominator.
    /// Must match the physical platform.
    pub wheelbase_m: f64,
    /// Bumper-to-bumper width (meters). Used by the SG2 drivable-space
    /// containment check (`gateway::containment::validate_trajectory_containment`)
    /// to compute the vehicle's per-pose footprint. Same dimension across
    /// Nominal/MRC profiles — the vehicle does not shrink in degraded posture.
    pub width_m: f64,
    /// Bumper-to-bumper length (meters); equals
    /// `wheelbase_m + overhang_front_m + overhang_rear_m`.
    pub length_m: f64,
    /// Distance from front wheel axle to front bumper (meters).
    pub overhang_front_m: f64,
    /// Distance from rear wheel axle to rear bumper (meters).
    pub overhang_rear_m: f64,
    /// Optional ODD operational speed cap (m/s). Enforced as a separate
    /// ceiling from `max_speed_mps`; the effective max =
    /// `min(max_speed_mps, odd_speed_cap_mps)`.
    ///
    /// `None` means no ODD cap is applied (vehicle physical max only) —
    /// integrators that deploy into an ODD with a defined cap (e.g. the
    /// urban Occy ODD, 22.35 m/s per ADR-0001) MUST populate this; a
    /// startup warning fires if it is missing on a deployment that
    /// should have it. See `URBAN_ODD_SPEED_CAP_MPS`.
    #[serde(default)]
    pub odd_speed_cap_mps: Option<f64>,
}

impl VehicleKinematicsContract {
    /// Full operational profile for a standard reference vehicle platform.
    /// Suitable for `FleetPosture::Nominal`.
    ///
    /// Note: `odd_speed_cap_mps` is `None` because this is a *reference*
    /// vehicle capability profile, not a deployment-specific ODD profile.
    /// Deployments must set the ODD cap explicitly via the integrator
    /// config (e.g. `VehicleConfig::default_urban`).
    pub fn nominal_reference_profile() -> Self {
        Self {
            max_speed_mps: 35.0,
            max_accel_mps2: 2.5,
            max_brake_mps2: 4.5,
            max_steering_deg: 35.0,
            max_steering_rate_deg_s: 45.0,
            min_follow_distance_m: 2.0,
            max_lateral_accel_mps2: 3.5,
            wheelbase_m: 2.8,
            // Reference mid-size vehicle footprint (sedan / small SUV). 2.8
            // wheelbase + 0.9 front + 1.1 rear = 4.8 m length, 1.85 m width.
            width_m: 1.85,
            length_m: 4.8,
            overhang_front_m: 0.9,
            overhang_rear_m: 1.1,
            odd_speed_cap_mps: None,
        }
    }

    /// Minimal Risk Condition (MRC) fallback profile for degraded fleet posture.
    /// Suitable for `FleetPosture::Degraded`.
    pub fn mrc_fallback_profile() -> Self {
        Self {
            max_speed_mps: 5.0,
            max_accel_mps2: 1.0,
            max_brake_mps2: 3.0,
            max_steering_deg: 15.0,
            max_steering_rate_deg_s: 20.0,
            min_follow_distance_m: 5.0,
            max_lateral_accel_mps2: 1.5,
            wheelbase_m: 2.8,
            // Footprint dimensions are platform geometry — same as Nominal.
            width_m: 1.85,
            length_m: 4.8,
            overhang_front_m: 0.9,
            overhang_rear_m: 1.1,
            // MRC speed (5.0) is already well below any ODD cap; we leave
            // odd_speed_cap_mps = None so the min() simply selects 5.0.
            odd_speed_cap_mps: None,
        }
    }

    /// Effective maximum forward speed enforced by the kinematics
    /// pipeline. The Priority-2 hard ceiling and the P3/P4 clamping
    /// bounds use this, not `max_speed_mps` directly.
    ///
    /// SAFETY: SG1 | REQ: odd-speed-cap-enforcement
    /// (ADR-0001, SPEED_ENVELOPE.md, KIRRA-OCCY-SPEED-VAL-001)
    #[inline]
    #[must_use]
    pub fn effective_max_speed_mps(&self) -> f64 {
        match self.odd_speed_cap_mps {
            Some(cap) if cap < self.max_speed_mps => cap,
            _ => self.max_speed_mps,
        }
    }
}

// ---------------------------------------------------------------------------
// Command Input
// ---------------------------------------------------------------------------

/// A proposed actuator command from the motion planning stack, with the current
/// kinematic state required to compute rate-of-change invariants.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProposedVehicleCommand {
    /// Desired forward velocity at end of this time step (m/s).
    /// Negative values indicate reverse motion.
    pub linear_velocity_mps: f64,
    /// Actual forward velocity at start of this time step (m/s).
    pub current_velocity_mps: f64,
    /// Duration of this planning time step (seconds). Must be > 0.
    pub delta_time_s: f64,
    /// Desired steering angle at end of this time step (degrees).
    /// Sign convention: positive = left turn (ISO 8855).
    pub steering_angle_deg: f64,
    /// Actual steering angle at start of this time step (degrees).
    pub current_steering_angle_deg: f64,
}

// ---------------------------------------------------------------------------
// Enforcement Result
// ---------------------------------------------------------------------------

/// Result of `validate_vehicle_command`.
///
/// - `Allow`         → forward to actuator
/// - `ClampLinear`   → replace linear velocity with provided safe value
/// - `ClampSteering` → replace steering angle with provided safe value
/// - `DenyBreach`    → drop command, log reason, emit posture event
///
/// Only one action is returned per call. The first-triggered check wins.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum EnforceAction {
    Allow,
    ClampLinear(f64),
    ClampSteering(f64),
    DenyBreach(DenyCode),
}

/// Reason codes for [`EnforceAction::DenyBreach`].
///
/// Each variant maps to a fixed `&'static str` rendering (via [`DenyCode::reason`]
/// and the `Display` impl) and serializes to the same `SCREAMING_SNAKE_CASE`
/// token the previous `DenyBreach(String)` form carried — audit-chain hashes
/// and JSON deny-reason fields are byte-identical across this refactor.
///
/// Per-variant `Safety:` tags link the breach to the safety goal it enforces.
/// The enum is `Copy + 'static` so a per-verdict deny carries no heap
/// allocation on the Governor check path (S3 / #115).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DenyCode {
    /// Safety: SG-004 (AEGIS-SG-001) ≅ SG9 (OCCY_SAFETY_GOALS).
    /// Linear velocity (`linear_velocity_mps`) is NaN or Inf.
    NanInfLinearVelocity,
    /// Safety: SG-004 ≅ SG9. Current velocity is NaN or Inf.
    NanInfCurrentVelocity,
    /// Safety: SG-004 ≅ SG9. Steering angle is NaN or Inf.
    NanInfSteeringAngle,
    /// Safety: SG-004 ≅ SG9. Current steering angle is NaN or Inf.
    NanInfCurrentSteering,
    /// Safety: SG-004 ≅ SG9. Time delta (`delta_time_s`) is NaN or Inf.
    NanInfDeltaTime,
    /// Safety: SG-003 ≅ SG3. Zero or negative dt makes rate-of-change undefined.
    InvalidTimeDelta,
    /// Safety: SG-007 ≅ SG8. Asset is under LockedOut fleet posture in the fabric.
    AssetLockedOut,
    /// Safety: SG-002 ≅ SG2. Trajectory pose / vehicle footprint departs the
    /// drivable-space corridor (with margin), or the corridor input is
    /// absent/stale/low-confidence (conservative containment failure per
    /// OCCY_FAULT_MODEL §3 sensor-availability rule). Issued by
    /// `gateway::containment::validate_trajectory_containment`.
    DrivableSpaceDeparture,
    /// Safety: SG-007 ≅ SG8. Degraded posture: a stopped vehicle
    /// (`|current_velocity_mps| <= STOP_EPSILON_MPS`) was commanded to
    /// re-initiate motion (`|linear_velocity_mps| > STOP_EPSILON_MPS`), or
    /// to reverse direction through a stop. In Degraded the safe state is
    /// **decel-to-stop-and-HOLD**: the Governor never authorizes autonomous
    /// re-initiation of motion from a standstill. Issued by
    /// `enforce_degraded_decel_to_stop`. (Issue #70 — Cruise Oct-2023 SF
    /// post-stop pullover-drag lesson: a stopped AV must not re-initiate
    /// motion under a degraded safety posture.)
    DegradedReinitiationDenied,
    /// Safety: SG-007 ≅ SG8. Degraded posture: the proposed speed magnitude
    /// exceeds the current speed magnitude (`|linear_velocity_mps| >
    /// |current_velocity_mps|`). Degraded permits only a **non-increasing**
    /// (decelerating-toward-zero) speed profile; any acceleration is denied
    /// and the actuator falls to the MRC controlled-stop. Issued by
    /// `enforce_degraded_decel_to_stop`. (Issue #70.)
    DegradedSpeedIncreaseDenied,
    /// Safety: SG-002 ≅ SG2. Frame/localization integrity is UNTRUSTED this tick
    /// (absent / stale / non-finite / 95th-pct lateral error beyond the
    /// conservative-fallback bound), so the map-anchored corridor cannot be
    /// trusted to be correctly placed relative to the ego. Containment refuses to
    /// validate (it does not reason about geometry in an untrusted frame) and the
    /// actuator falls to the MRC controlled-stop — the frame-trust-minimal
    /// maneuver. Issued by `containment::validate_trajectory_containment`
    /// when the [`crate::frame_integrity::FrameTrust`] verdict is `Untrusted`.
    /// (Stage S-FI1 — behind AOU-LOCALIZATION-001.)
    ///
    /// APPENDED LAST deliberately: the bincode variant index is the wire tag
    /// (see `kirra-wire-client` `ClientDenyCode`), so existing indices 0–9 must
    /// not shift.
    FrameIntegrityUntrusted,
}

impl DenyCode {
    /// Returns the audit/log token for this code (e.g. `"NAN_INF_LINEAR_VELOCITY"`).
    ///
    /// The text is byte-identical to the previous `DenyBreach(String)` rendering,
    /// preserving audit-chain hash stability across this refactor.
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::NanInfLinearVelocity  => "NAN_INF_LINEAR_VELOCITY",
            Self::NanInfCurrentVelocity => "NAN_INF_CURRENT_VELOCITY",
            Self::NanInfSteeringAngle   => "NAN_INF_STEERING_ANGLE",
            Self::NanInfCurrentSteering => "NAN_INF_CURRENT_STEERING",
            Self::NanInfDeltaTime       => "NAN_INF_DELTA_TIME",
            Self::InvalidTimeDelta      => "INVALID_TIME_DELTA",
            Self::AssetLockedOut        => "ASSET_LOCKED_OUT",
            Self::DrivableSpaceDeparture => "DRIVABLE_SPACE_DEPARTURE",
            Self::DegradedReinitiationDenied  => "DEGRADED_REINITIATION_DENIED",
            Self::DegradedSpeedIncreaseDenied => "DEGRADED_SPEED_INCREASE_DENIED",
            Self::FrameIntegrityUntrusted     => "FRAME_INTEGRITY_UNTRUSTED",
        }
    }
}

impl std::fmt::Display for DenyCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason())
    }
}

// ---------------------------------------------------------------------------
// Degraded-posture stop-and-hold gate (Issue #70)
// ---------------------------------------------------------------------------

/// Speed magnitude (m/s) at or below which the vehicle is treated as
/// **stopped** for the Degraded decel-to-stop-and-HOLD invariant.
///
/// At a standstill, wheel-odometry / GNSS-velocity estimates carry a few
/// cm/s of noise; this floor (5 cm/s) sits above that noise band yet far
/// below any meaningful crawl, so it denies any *commanded* re-initiation of
/// motion from a stop while not flapping on standstill sensor jitter.
///
/// JUDGMENT-CALL (Issue #70 FLAG #1): 0.05 m/s is the recommended value.
/// Integrators with a noisier velocity estimate may raise it; it must stay
/// well below the slowest deliberate maneuver speed so a genuine creep is
/// never silently admitted.
pub const STOP_EPSILON_MPS: f64 = 0.05;

/// Degraded-posture enforcement: **controlled decel-to-stop-and-HOLD** with
/// NO autonomous re-initiation of motion.
///
/// This is the Issue #70 Degraded behavior. It is a *narrower* allow than the
/// Nominal envelope and a *wider* allow than LockedOut (which denies
/// everything): in Degraded the vehicle is permitted to keep moving only so
/// long as it is converging toward a standstill under the MRC kinematic
/// envelope, and once stopped it must HOLD. It must never be commanded to
/// speed up or to re-initiate motion from a stop.
///
/// A proposed command is permitted **only if ALL** hold:
///
/// - **(a) within the MRC kinematic envelope** — delegated to
///   [`validate_vehicle_command`] against `mrc_contract`, which is the
///   decel-trajectory bound (speed ceiling, brake/accel rate, steering,
///   lateral-accel). A clamp from the envelope is still a permitted
///   (decelerating) command, returned as `ClampLinear`/`ClampSteering`.
/// - **(b) non-increasing speed magnitude** — `|proposed| <= |current|`. Any
///   speed increase → [`DenyCode::DegradedSpeedIncreaseDenied`].
/// - **(c) no re-initiation from a stop** — if `|current| <= STOP_EPSILON_MPS`,
///   any `|proposed| > STOP_EPSILON_MPS` → [`DenyCode::DegradedReinitiationDenied`]
///   (hold at zero). A direction reversal through a stop (sign flip while both
///   magnitudes exceed the stop floor) is likewise treated as re-initiation of
///   opposite-direction motion → [`DenyCode::DegradedReinitiationDenied`]
///   (Issue #70 FLAG #3: converge-toward-zero by magnitude, no reverse
///   re-initiation).
///
/// On a (b)/(c) violation the function returns `DenyBreach(..)`; the caller
/// drops the command and the actuator falls to the MRC controlled-stop — the
/// Governor does **not** author a replacement decel command (parallel to the
/// LockedOut deny→MRC fallback, but with the narrower Degraded allow above).
///
/// The angular channel is intentionally NOT gated here for the Ackermann
/// bicycle model: a vehicle's yaw rate is `ω = v·tan(δ)/L`, so the linear
/// no-re-initiation / non-increasing invariant already forces `ω → 0` as
/// `v → 0`. Steering *geometry* at a standstill is not motion and is left to
/// the envelope's steering-rate/angle clamps. Platforms with an *independent*
/// angular-velocity actuator (e.g. differential drive in `parko-kirra`) apply
/// the same converge-to-zero / no-re-initiation rule to the angular channel
/// natively (Issue #70 FLAG #2).
///
/// WCET (Issue #70 STEP 5 / S3): the gate adds only a fixed, branch-bounded
/// set of finite-checks, `abs`, sign and magnitude comparisons before
/// delegating to the already-characterized [`validate_vehicle_command`]. It is
/// O(1), allocation-free, and only on the Degraded path — the Nominal verdict
/// path is unchanged.
// SAFETY: SG8 | REQ: degraded-decel-to-stop-and-hold | TEST: test_degraded_reinitiation_from_stop_is_denied,test_degraded_speed_increase_is_denied,test_degraded_decel_toward_zero_is_allowed,test_degraded_hold_at_stop_is_allowed,test_degraded_reverse_through_stop_is_denied
#[must_use]
pub fn enforce_degraded_decel_to_stop(
    cmd: &ProposedVehicleCommand,
    mrc_contract: &VehicleKinematicsContract,
) -> EnforceAction {
    // Priority 0: fail closed on non-finite linear/current velocity before any
    // magnitude comparison. NaN compares false against every threshold, which
    // would silently mask a re-initiation; reject explicitly. (The full
    // NaN/Inf + dt guard is re-run inside validate_vehicle_command for the
    // envelope check; these two are the fields the gate itself reads.)
    if !cmd.linear_velocity_mps.is_finite() {
        return EnforceAction::DenyBreach(DenyCode::NanInfLinearVelocity);
    }
    if !cmd.current_velocity_mps.is_finite() {
        return EnforceAction::DenyBreach(DenyCode::NanInfCurrentVelocity);
    }

    let proposed = cmd.linear_velocity_mps.abs();
    let current = cmd.current_velocity_mps.abs();

    // (c) No autonomous re-initiation from a stop — HOLD at zero.
    if current <= STOP_EPSILON_MPS && proposed > STOP_EPSILON_MPS {
        return EnforceAction::DenyBreach(DenyCode::DegradedReinitiationDenied);
    }

    // (c') No direction reversal through a stop while moving — commanding the
    // opposite sign at a non-trivial magnitude re-initiates motion in reverse.
    let reversing = cmd.linear_velocity_mps.signum() != cmd.current_velocity_mps.signum();
    if reversing && current > STOP_EPSILON_MPS && proposed > STOP_EPSILON_MPS {
        return EnforceAction::DenyBreach(DenyCode::DegradedReinitiationDenied);
    }

    // (b) Non-increasing speed magnitude (decelerating-toward-zero only).
    // A tiny tolerance absorbs float jitter on a steady-state hold; anything
    // meaningfully above the current magnitude is a speed increase → deny.
    if proposed > current + 1e-9 {
        return EnforceAction::DenyBreach(DenyCode::DegradedSpeedIncreaseDenied);
    }

    // (a) Within the MRC kinematic envelope — the decel-trajectory bound.
    validate_vehicle_command(cmd, mrc_contract)
}

// ---------------------------------------------------------------------------
// Validation Pipeline
// ---------------------------------------------------------------------------

/// Evaluates a proposed vehicle command against a kinematics contract.
///
/// Checks run in strict priority order (Priority 0 → 6). Returns the first
/// violation found, or `EnforceAction::Allow` if all checks pass.
#[must_use]
pub fn validate_vehicle_command(
    cmd: &ProposedVehicleCommand,
    contract: &VehicleKinematicsContract,
) -> EnforceAction {
    // ------------------------------------------------------------------
    // SAFETY: SG9 | REQ: fail-closed-nonfinite | TEST: test_nan_linear_velocity_is_denied_before_any_arithmetic,test_nan_current_velocity_is_denied_with_specific_code,test_nan_steering_angle_is_denied_with_specific_code,prop_nan_in_any_field_produces_deny_breach
    // (≅ AEGIS SG-004 — cross-map per OCCY_SAFETY_GOALS.md §6.2.)
    // Priority 0: NaN/Inf guard — must run before ANY arithmetic.
    //
    // IEEE 754 NaN/Inf values poison every subsequent computation silently:
    //   - NaN comparisons always return false → branch logic becomes unsafe
    //   - NaN * finite = NaN → clamping silently produces NaN output
    //   - Inf - Inf = NaN → acceleration check produces NaN, passes as 0.0
    //   - NaN > threshold = false → bicycle model lateral check never fires
    //
    // None of these produce a panic in Rust. They silently pass invalid
    // commands to the actuator — an AV-class safety failure mode.
    //
    // Each field gets a distinct denial code for audit forensics: a NaN in
    // steering_angle_deg implies a different upstream bug than NaN in
    // linear_velocity_mps. Infinity is rejected alongside NaN.
    // ------------------------------------------------------------------
    if !cmd.linear_velocity_mps.is_finite() {
        return EnforceAction::DenyBreach(DenyCode::NanInfLinearVelocity);
    }
    if !cmd.current_velocity_mps.is_finite() {
        return EnforceAction::DenyBreach(DenyCode::NanInfCurrentVelocity);
    }
    if !cmd.steering_angle_deg.is_finite() {
        return EnforceAction::DenyBreach(DenyCode::NanInfSteeringAngle);
    }
    if !cmd.current_steering_angle_deg.is_finite() {
        return EnforceAction::DenyBreach(DenyCode::NanInfCurrentSteering);
    }
    if !cmd.delta_time_s.is_finite() {
        return EnforceAction::DenyBreach(DenyCode::NanInfDeltaTime);
    }

    // ------------------------------------------------------------------
    // SAFETY: SG3 | REQ: reject-non-physical-dt | TEST: test_zero_time_delta_is_denied,test_negative_time_delta_is_denied,test_time_delta_check_fires_before_speed_check,prop_non_positive_dt_always_denied
    // (≅ AEGIS SG-003.)
    // Priority 1: Non-physical time delta
    // Zero or negative dt makes rate-of-change calculations undefined.
    // ------------------------------------------------------------------
    if cmd.delta_time_s <= 0.0 {
        return EnforceAction::DenyBreach(DenyCode::InvalidTimeDelta);
    }

    // ------------------------------------------------------------------
    // SAFETY: SG1 SG3 | REQ: velocity-hard-ceiling,odd-speed-cap-enforcement | TEST: test_speed_above_ceiling_triggers_clamp_linear,test_reverse_speed_above_ceiling_clamps_with_correct_sign,prop_clamp_linear_value_within_speed_contract,prop_allow_result_satisfies_speed_contract,test_command_above_odd_cap_below_vehicle_max_clamps_to_odd_cap,test_command_above_both_clamps_to_odd_cap,test_command_below_odd_cap_passes,test_no_odd_cap_falls_back_to_vehicle_max
    // (≅ AEGIS SG-001.)
    // Priority 2: Linear velocity hard ceiling
    // Checked before acceleration rate — a velocity-over-limit command
    // implies an over-limit acceleration; no need to compute it.
    //
    // The enforced ceiling is `effective_max_speed_mps()`:
    //   min(max_speed_mps, odd_speed_cap_mps.unwrap_or(+∞))
    // This separates the vehicle physical max (capability) from the ODD
    // operational cap (safety case). See `URBAN_ODD_SPEED_CAP_MPS` for
    // the urban Occy ODD value (22.35 m/s per ADR-0001).
    // ------------------------------------------------------------------
    let effective_max_speed = contract.effective_max_speed_mps();
    if cmd.linear_velocity_mps.abs() > effective_max_speed {
        let clamped = effective_max_speed * cmd.linear_velocity_mps.signum();
        return EnforceAction::ClampLinear(clamped);
    }

    // ------------------------------------------------------------------
    // Priorities 3–6: apply corrections progressively; evaluate P6 last.
    //
    // The pipeline accumulates corrections into `v` and `delta` rather
    // than returning at the first triggered check. This ensures:
    //
    //   - A P3/P4 velocity clamp does not suppress a P6 lateral-accel
    //     violation that would appear in the resulting state (Bug G).
    //   - A P5a/P5b steering clamp does not suppress a P6 lateral-accel
    //     violation in the rate-limited result (Bug C).
    //
    // P6 uses cmd.linear_velocity_mps (original commanded speed): when
    // ClampSteering is returned the caller applies the clamped steering
    // with the original velocity, so the safe angle is back-solved for
    // that speed. This also satisfies the proptest invariant which checks
    // lateral accel using the original commanded velocity.
    //
    // Return priority: steering corrections take precedence because a
    // lateral-accel violation is the most acute physical safety concern.
    // When only velocity needs correction, ClampLinear is returned.
    // ------------------------------------------------------------------
    let mut v = cmd.linear_velocity_mps;
    let mut v_clamped = false;
    let mut delta = cmd.steering_angle_deg;
    let mut delta_clamped = false;

    // SAFETY: SG3 | REQ: accel-ceiling | TEST: test_excessive_acceleration_triggers_linear_clamping,prop_clamp_linear_value_within_speed_contract
    // Priority 3: Implied acceleration ceiling
    let implied_accel =
        (cmd.linear_velocity_mps - cmd.current_velocity_mps) / cmd.delta_time_s;

    if implied_accel > 0.0 && implied_accel > contract.max_accel_mps2 + 1e-9 {
        v = (cmd.current_velocity_mps + contract.max_accel_mps2 * cmd.delta_time_s)
            .clamp(-effective_max_speed, effective_max_speed);
        v_clamped = true;
    }

    // SAFETY: SG3 | REQ: brake-ceiling | TEST: test_excessive_braking_triggers_linear_clamping
    // Priority 4: Implied deceleration ceiling
    // Asymmetric from acceleration: braking limit is typically higher.
    if implied_accel < 0.0 && implied_accel.abs() > contract.max_brake_mps2 + 1e-9 {
        v = (cmd.current_velocity_mps - contract.max_brake_mps2 * cmd.delta_time_s)
            .clamp(-effective_max_speed, effective_max_speed);
        v_clamped = true;
    }

    // SAFETY: SG3 | REQ: steering-hard-limit | TEST: test_high_speed_lateral_acceleration_forces_steering_clamp,prop_clamp_steering_value_is_finite
    // Priority 5a: Absolute steering angle hard limit
    if delta.abs() > contract.max_steering_deg {
        delta = contract.max_steering_deg * delta.signum();
        delta_clamped = true;
    }

    // SAFETY: SG3 | REQ: steering-rate-ceiling | TEST: test_excessive_steering_rate_triggers_steering_clamp
    // Priority 5b: Steering rate ceiling
    // Rate is measured from current_steering to the (possibly P5a-clamped)
    // delta so that a bounded target is never inflated back up by the rate.
    let steering_delta = delta - cmd.current_steering_angle_deg;
    let implied_steering_rate = steering_delta.abs() / cmd.delta_time_s;

    if implied_steering_rate > contract.max_steering_rate_deg_s {
        let max_delta_deg = contract.max_steering_rate_deg_s * cmd.delta_time_s;
        delta = (cmd.current_steering_angle_deg + max_delta_deg * steering_delta.signum())
            .clamp(-contract.max_steering_deg, contract.max_steering_deg);
        delta_clamped = true;
    }

    // SAFETY: SG3 | REQ: lateral-accel-envelope | TEST: test_high_speed_lateral_acceleration_forces_steering_clamp,prop_clamp_steering_satisfies_lateral_accel_invariant,test_mrc_lateral_limit_is_tighter_than_nominal
    // (≅ AEGIS SG-002.)
    // Priority 6: Dynamic lateral acceleration envelope (bicycle model)
    //
    //   a_lat = (v² × |tan(δ)|) / L
    //
    // Guard: skip at near-zero velocity to avoid division by v² ≈ 0.
    let v2 = cmd.linear_velocity_mps.powi(2);
    if v2 > 1e-6 {
        let delta_rad = delta.to_radians();
        let implied_lat_accel = (v2 * delta_rad.tan().abs()) / contract.wheelbase_m;

        if implied_lat_accel > contract.max_lateral_accel_mps2 {
            let max_safe_tan =
                (contract.max_lateral_accel_mps2 * contract.wheelbase_m) / v2;
            delta = max_safe_tan.atan().to_degrees() * delta.signum();
            delta_clamped = true;
        }
    }

    match (v_clamped, delta_clamped) {
        (_, true) => EnforceAction::ClampSteering(delta),
        (true, false) => EnforceAction::ClampLinear(v),
        (false, false) => EnforceAction::Allow,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod kinematics_contract_tests {
    use super::*;

    // --- Allow cases --------------------------------------------------------

    #[test]
    fn test_nominal_command_passes_unhindered() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 9.5,
            delta_time_s: 0.2,
            steering_angle_deg: 5.0,
            current_steering_angle_deg: 4.5,
        };
        assert_eq!(validate_vehicle_command(&cmd, &contract), EnforceAction::Allow);
    }

    #[test]
    fn test_zero_motion_command_passes() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 0.0,
            current_velocity_mps: 0.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(validate_vehicle_command(&cmd, &contract), EnforceAction::Allow);
    }

    #[test]
    fn test_mrc_command_within_mrc_profile_passes() {
        let contract = VehicleKinematicsContract::mrc_fallback_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 3.0,
            current_velocity_mps: 2.8,
            delta_time_s: 0.2,
            steering_angle_deg: 5.0,
            current_steering_angle_deg: 4.0,
        };
        assert_eq!(validate_vehicle_command(&cmd, &contract), EnforceAction::Allow);
    }

    #[test]
    fn test_mild_reverse_command_passes() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: -2.0,
            current_velocity_mps: -1.5,
            delta_time_s: 0.5,
            steering_angle_deg: -3.0,
            current_steering_angle_deg: -2.5,
        };
        assert_eq!(validate_vehicle_command(&cmd, &contract), EnforceAction::Allow);
    }

    // --- Deny cases ---------------------------------------------------------

    #[test]
    fn test_zero_time_delta_is_denied() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,
            delta_time_s: 0.0,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::InvalidTimeDelta)
        );
    }

    #[test]
    fn test_negative_time_delta_is_denied() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,
            delta_time_s: -0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::InvalidTimeDelta)
        );
    }

    // --- Linear velocity clamping -------------------------------------------

    #[test]
    fn test_speed_above_ceiling_triggers_clamp_linear() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 40.0,
            current_velocity_mps: 34.0,
            delta_time_s: 0.5,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::ClampLinear(35.0)
        );
    }

    #[test]
    fn test_reverse_speed_above_ceiling_clamps_with_correct_sign() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: -40.0,
            current_velocity_mps: -20.0,
            delta_time_s: 0.5,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::ClampLinear(-35.0)
        );
    }

    #[test]
    fn test_excessive_acceleration_triggers_linear_clamping() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 25.0,
            current_velocity_mps: 10.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        match validate_vehicle_command(&cmd, &contract) {
            EnforceAction::ClampLinear(clamped) => {
                let expected = 10.0_f64 + (2.5 * 0.1);
                assert!((clamped - expected).abs() < 1e-9, "expected {expected}, got {clamped}");
            }
            other => panic!("Expected ClampLinear, got {other:?}"),
        }
    }

    #[test]
    fn test_excessive_braking_triggers_linear_clamping() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 0.0,
            current_velocity_mps: 30.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        match validate_vehicle_command(&cmd, &contract) {
            EnforceAction::ClampLinear(clamped) => {
                let expected = 30.0_f64 - (4.5 * 0.1);
                assert!(clamped > 0.0, "should not allow instant stop");
                assert!((clamped - expected).abs() < 1e-9, "expected {expected}, got {clamped}");
            }
            other => panic!("Expected ClampLinear for over-deceleration, got {other:?}"),
        }
    }

    // --- Steering clamping --------------------------------------------------

    #[test]
    fn test_excessive_steering_rate_triggers_steering_clamp() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,
            delta_time_s: 0.1,
            steering_angle_deg: 30.0,
            current_steering_angle_deg: 0.0,
        };
        match validate_vehicle_command(&cmd, &contract) {
            EnforceAction::ClampSteering(safe) => {
                assert!((safe - 4.5_f64).abs() < 1e-9, "expected 4.5, got {safe}");
            }
            other => panic!("Expected ClampSteering for excessive rate, got {other:?}"),
        }
    }

    #[test]
    fn test_high_speed_lateral_acceleration_forces_steering_clamp() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 30.0,
            current_velocity_mps: 30.0,
            delta_time_s: 0.5,
            steering_angle_deg: 20.0,
            current_steering_angle_deg: 0.0,
        };
        match validate_vehicle_command(&cmd, &contract) {
            EnforceAction::ClampSteering(safe) => {
                assert!(safe < 20.0, "must reduce steering angle");
                assert!(safe > 0.0, "sign must be preserved");
                assert!(safe < 2.0, "at 30 m/s, safe steering must be very small");
            }
            other => panic!("Expected ClampSteering for lateral accel breach, got {other:?}"),
        }
    }

    #[test]
    fn test_near_zero_velocity_skips_bicycle_model_division() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 0.001,
            current_velocity_mps: 0.001,
            delta_time_s: 0.1,
            steering_angle_deg: 30.0,
            current_steering_angle_deg: 27.0,
        };
        assert_eq!(validate_vehicle_command(&cmd, &contract), EnforceAction::Allow);
    }

    // --- MRC profile enforcement --------------------------------------------

    #[test]
    fn test_nominal_speed_breaches_mrc_profile() {
        let contract = VehicleKinematicsContract::mrc_fallback_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 15.0,
            current_velocity_mps: 14.0,
            delta_time_s: 0.5,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::ClampLinear(5.0)
        );
    }

    #[test]
    fn test_mrc_lateral_limit_is_tighter_than_nominal() {
        // 18° at 4 m/s: a_lat ≈ 1.857 m/s² — passes nominal (3.5), breaches MRC (1.5)
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 4.0,
            current_velocity_mps: 4.0,
            delta_time_s: 1.0,
            steering_angle_deg: 18.0,
            current_steering_angle_deg: 0.0,
        };
        let nominal = VehicleKinematicsContract::nominal_reference_profile();
        let mrc = VehicleKinematicsContract::mrc_fallback_profile();
        assert_eq!(validate_vehicle_command(&cmd, &nominal), EnforceAction::Allow);
        match validate_vehicle_command(&cmd, &mrc) {
            EnforceAction::ClampSteering(s) => assert!(s < 18.0),
            other => panic!("MRC should clamp lateral breach, got {other:?}"),
        }
    }

    // --- Priority ordering --------------------------------------------------

    #[test]
    fn test_time_delta_check_fires_before_speed_check() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 999.0,
            current_velocity_mps: 0.0,
            delta_time_s: 0.0,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::InvalidTimeDelta)
        );
    }

    #[test]
    fn test_speed_check_fires_before_accel_check() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 50.0,
            current_velocity_mps: 5.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::ClampLinear(35.0)
        );
    }

    // --- NaN/Inf guard (Priority 0) ----------------------------------------

    #[test]
    fn test_nan_linear_velocity_is_denied_before_any_arithmetic() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: f64::NAN,
            current_velocity_mps: 10.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::NanInfLinearVelocity)
        );
    }

    #[test]
    fn test_inf_linear_velocity_is_denied() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: f64::INFINITY,
            current_velocity_mps: 10.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::NanInfLinearVelocity)
        );
    }

    #[test]
    fn test_neg_inf_linear_velocity_is_denied() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: f64::NEG_INFINITY,
            current_velocity_mps: 0.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::NanInfLinearVelocity)
        );
    }

    #[test]
    fn test_nan_current_velocity_is_denied_with_specific_code() {
        // NaN current_velocity_mps poisons acceleration calc:
        //   implied_accel = (v_cmd - NaN) / dt = NaN
        //   NaN > max_accel = false → accel check silently passes
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: f64::NAN,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::NanInfCurrentVelocity)
        );
    }

    #[test]
    fn test_nan_steering_angle_is_denied_with_specific_code() {
        // NaN steering_angle_deg passed to tan() produces NaN lateral accel.
        // NaN > max_lateral_accel = false → bicycle model silently passes.
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,
            delta_time_s: 0.1,
            steering_angle_deg: f64::NAN,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::NanInfSteeringAngle)
        );
    }

    #[test]
    fn test_nan_current_steering_is_denied_with_specific_code() {
        // NaN current_steering_angle_deg poisons steering rate:
        //   steering_delta = angle - NaN = NaN; NaN / dt = NaN
        //   NaN > max_rate = false → rate check silently passes
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,
            delta_time_s: 0.1,
            steering_angle_deg: 5.0,
            current_steering_angle_deg: f64::NAN,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::NanInfCurrentSteering)
        );
    }

    #[test]
    fn test_nan_delta_time_is_denied_with_specific_code() {
        // NaN delta_time_s: NaN <= 0.0 = false → Priority 1 does NOT fire.
        // Without Priority 0: v_delta / NaN = NaN, silently passes all checks.
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,
            delta_time_s: f64::NAN,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::NanInfDeltaTime)
        );
    }

    #[test]
    fn test_inf_delta_time_is_denied_before_zero_check() {
        // f64::INFINITY > 0.0 is true → Priority 1 would NOT catch it.
        // v_delta / INFINITY = 0.0 → accel check sees zero, passes everything.
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,
            delta_time_s: f64::INFINITY,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::NanInfDeltaTime)
        );
    }

    #[test]
    fn test_nan_guard_fires_before_time_delta_check() {
        // Both NaN dt AND zero dt present. Priority 0 must fire first.
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: f64::NAN,
            current_velocity_mps: 0.0,
            delta_time_s: 0.0,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::NanInfLinearVelocity),
            "NaN guard (priority 0) must fire before zero-dt check (priority 1)"
        );
    }

    /// SG3 / GAP 6: Priority-3 implied-acceleration guard at the zero boundary.
    /// Exercises the FALSE arm of `implied_accel > 0.0` (l.279). When
    /// commanded == current velocity, implied_accel = 0.0, so neither P3 nor
    /// P4 should clamp; the command should Allow.
    #[test]
    fn test_implied_accel_at_zero_boundary_treated_as_no_clamp() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::Allow,
            "implied_accel == 0.0 must NOT trigger the P3 acceleration clamp"
        );
    }

    /// SG3 / GAP 7: Priority-4 implied-deceleration guard at the zero boundary.
    /// Exercises the FALSE arm of `implied_accel < 0.0` (l.288). A negligible
    /// positive accel (just below the max-accel threshold) keeps implied_accel
    /// strictly above zero so P4 does not consider it; the command must Allow.
    #[test]
    fn test_implied_decel_at_zero_boundary_treated_as_no_clamp() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        // implied_accel = (10.0 - 10.000001) / 0.1 = -1e-5 m/s² — well below
        // max_brake_mps2 (4.5), exercises the P4 false arm where the
        // computed |implied_accel| is non-zero but below the brake ceiling.
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.000_001,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::Allow,
            "tiny negative implied_accel must NOT trigger the P4 brake clamp"
        );
    }

    // --- ODD speed cap enforcement (Option B, H2 fix) -----------------------
    //
    // The ODD operational speed cap (URBAN_ODD_SPEED_CAP_MPS = 22.35 m/s,
    // per ADR-0001) is enforced as a separate ceiling from the vehicle
    // physical max (35 m/s). The four tests below cover the matrix:
    //
    //   cmd  | vehicle_max | odd_cap | expected
    //   -----+-------------+---------+----------------------
    //   30   |     35      |  22.35  | ClampLinear(22.35)
    //   20   |     35      |  22.35  | Allow
    //   40   |     35      |  22.35  | ClampLinear(22.35)
    //   30   |     35      |  None   | Allow            (falls back to 35)
    //
    // Source-of-truth: SPEED_ENVELOPE.md line 116; ADR-0001 line 29;
    // S8 Item C (KIRRA-OCCY-SPEED-VAL-001).

    /// Helper: nominal-reference profile with the urban ODD cap applied.
    /// Mirrors what `VehicleConfig::default_urban()` produces at runtime.
    fn nominal_with_urban_odd_cap() -> VehicleKinematicsContract {
        VehicleKinematicsContract {
            odd_speed_cap_mps: Some(URBAN_ODD_SPEED_CAP_MPS),
            ..VehicleKinematicsContract::nominal_reference_profile()
        }
    }

    #[test]
    fn test_urban_odd_speed_cap_constant_matches_speed_envelope_doc() {
        // SPEED_ENVELOPE.md line 116 / ADR-0001 line 29 / OCCY_SPEED_CAP_VALIDATION.md
        // line 13 all state the cap as 22.35 m/s (the rounded value of
        // 50 mph = 22.352 m/s). If this constant drifts, the safety case
        // derivation no longer matches the enforced code path.
        assert_eq!(URBAN_ODD_SPEED_CAP_MPS, 22.35);
    }

    /// SAFETY: SG1 | REQ: odd-speed-cap-enforcement
    /// Command at 30 m/s — below the 35 m/s vehicle physical max but ABOVE
    /// the 22.35 m/s ODD cap → must clamp to the ODD cap.
    #[test]
    fn test_command_above_odd_cap_below_vehicle_max_clamps_to_odd_cap() {
        let contract = nominal_with_urban_odd_cap();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 30.0,
            current_velocity_mps: 29.5,
            delta_time_s: 0.5,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::ClampLinear(URBAN_ODD_SPEED_CAP_MPS),
            "30 m/s is below 35 m/s vehicle max but above the 22.35 m/s ODD cap; must clamp to ODD cap"
        );
    }

    /// SAFETY: SG1 | REQ: odd-speed-cap-enforcement
    /// Command at 20 m/s — below the ODD cap → allowed (no clamp).
    #[test]
    fn test_command_below_odd_cap_passes() {
        let contract = nominal_with_urban_odd_cap();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 20.0,
            current_velocity_mps: 19.5,
            delta_time_s: 0.5,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::Allow,
            "20 m/s is below the 22.35 m/s ODD cap; must pass without clamp"
        );
    }

    /// SAFETY: SG1 | REQ: odd-speed-cap-enforcement
    /// Command at 40 m/s — above BOTH the 35 m/s vehicle max and the
    /// 22.35 m/s ODD cap → most restrictive wins (clamps to the ODD cap).
    #[test]
    fn test_command_above_both_clamps_to_odd_cap() {
        let contract = nominal_with_urban_odd_cap();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 40.0,
            current_velocity_mps: 34.0,
            delta_time_s: 0.5,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::ClampLinear(URBAN_ODD_SPEED_CAP_MPS),
            "40 m/s exceeds both ceilings; min(35, 22.35) = 22.35 wins"
        );
    }

    /// SAFETY: SG1 | REQ: odd-speed-cap-enforcement
    /// `odd_speed_cap_mps = None` → falls back to vehicle physical max.
    /// Same 30 m/s command that clamps with the cap now passes without
    /// one. (The deployment startup warning is the surfacing mechanism
    /// for the missing-cap case; the contract itself remains permissive.)
    #[test]
    fn test_no_odd_cap_falls_back_to_vehicle_max() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        // Sanity: the reference profile carries no ODD cap.
        assert!(contract.odd_speed_cap_mps.is_none());
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 30.0,
            current_velocity_mps: 29.5,
            delta_time_s: 0.5,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::Allow,
            "With no ODD cap set, 30 m/s is below the 35 m/s vehicle max and must pass"
        );
    }

    /// `effective_max_speed_mps` returns the more restrictive of
    /// `max_speed_mps` and `odd_speed_cap_mps`. Verify the boundary cases.
    #[test]
    fn test_effective_max_speed_picks_more_restrictive() {
        let with_cap = nominal_with_urban_odd_cap();
        let no_cap   = VehicleKinematicsContract::nominal_reference_profile();
        assert_eq!(with_cap.effective_max_speed_mps(), URBAN_ODD_SPEED_CAP_MPS);
        assert_eq!(no_cap.effective_max_speed_mps(),   35.0);

        // An ODD cap above the vehicle max is silently ignored (vehicle
        // physical max remains the binding ceiling).
        let cap_above = VehicleKinematicsContract {
            odd_speed_cap_mps: Some(50.0),
            ..VehicleKinematicsContract::nominal_reference_profile()
        };
        assert_eq!(cap_above.effective_max_speed_mps(), 35.0);
    }

    /// SG9 / GAP 8: `Display for DenyCode` must render byte-identical to
    /// `DenyCode::reason()`. Audit-chain hash stability depends on this
    /// (every variant must match its SCREAMING_SNAKE_CASE token).
    #[test]
    fn test_deny_code_display_matches_reason() {
        let all = [
            DenyCode::NanInfLinearVelocity,
            DenyCode::NanInfCurrentVelocity,
            DenyCode::NanInfSteeringAngle,
            DenyCode::NanInfCurrentSteering,
            DenyCode::NanInfDeltaTime,
            DenyCode::InvalidTimeDelta,
            DenyCode::AssetLockedOut,
            // SG2 — merged in from sg2-drivable-space (#128); the union of
            // SG2 + GAP 8 means the corridor-departure variant must also
            // pin its Display token for audit-hash stability.
            DenyCode::DrivableSpaceDeparture,
            // Issue #70 — Degraded decel-to-stop-and-hold reason codes.
            DenyCode::DegradedReinitiationDenied,
            DenyCode::DegradedSpeedIncreaseDenied,
            // Stage S-FI1 — frame/localization-integrity gate (AOU-LOCALIZATION-001).
            DenyCode::FrameIntegrityUntrusted,
        ];
        for code in all {
            assert_eq!(
                format!("{code}"),
                code.reason(),
                "Display for {code:?} must equal reason() token"
            );
        }
    }

    // --- Issue #70: Degraded decel-to-stop-and-hold gate -------------------

    fn degraded_cmd(current: f64, proposed: f64) -> ProposedVehicleCommand {
        ProposedVehicleCommand {
            linear_velocity_mps: proposed,
            current_velocity_mps: current,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        }
    }

    /// Cruise lesson (Cruise Oct-2023 SF): a STOPPED vehicle must not
    /// re-initiate motion under Degraded posture.
    #[test]
    fn test_degraded_reinitiation_from_stop_is_denied() {
        let mrc = VehicleKinematicsContract::mrc_fallback_profile();
        // Stopped (0.0), commanded to crawl forward at 3 m/s — the exact
        // class of maneuver the Cruise pullover-drag executed from a stop.
        let cmd = degraded_cmd(0.0, 3.0);
        assert_eq!(
            enforce_degraded_decel_to_stop(&cmd, &mrc),
            EnforceAction::DenyBreach(DenyCode::DegradedReinitiationDenied)
        );
    }

    /// A speed INCREASE while moving is denied (only decel-toward-zero allowed).
    #[test]
    fn test_degraded_speed_increase_is_denied() {
        let mrc = VehicleKinematicsContract::mrc_fallback_profile();
        let cmd = degraded_cmd(2.0, 4.0); // 2 → 4 m/s, accelerating
        assert_eq!(
            enforce_degraded_decel_to_stop(&cmd, &mrc),
            EnforceAction::DenyBreach(DenyCode::DegradedSpeedIncreaseDenied)
        );
    }

    /// A decelerating-toward-zero command within the MRC envelope is ALLOWED.
    #[test]
    fn test_degraded_decel_toward_zero_is_allowed() {
        let mrc = VehicleKinematicsContract::mrc_fallback_profile();
        // 3 → 2.9 m/s over 0.1 s: decel of 1 m/s², within MRC max_brake (3.0).
        let cmd = degraded_cmd(3.0, 2.9);
        assert_eq!(
            enforce_degraded_decel_to_stop(&cmd, &mrc),
            EnforceAction::Allow
        );
    }

    /// Holding at a standstill (0 → 0) is ALLOWED — that IS the safe state.
    #[test]
    fn test_degraded_hold_at_stop_is_allowed() {
        let mrc = VehicleKinematicsContract::mrc_fallback_profile();
        let cmd = degraded_cmd(0.0, 0.0);
        assert_eq!(
            enforce_degraded_decel_to_stop(&cmd, &mrc),
            EnforceAction::Allow
        );
    }

    /// A direction reversal through a stop (forward → reverse) is treated as
    /// re-initiation of opposite-direction motion and denied.
    #[test]
    fn test_degraded_reverse_through_stop_is_denied() {
        let mrc = VehicleKinematicsContract::mrc_fallback_profile();
        let cmd = degraded_cmd(2.0, -1.0); // moving forward, commanded reverse
        assert_eq!(
            enforce_degraded_decel_to_stop(&cmd, &mrc),
            EnforceAction::DenyBreach(DenyCode::DegradedReinitiationDenied)
        );
    }

    /// An over-MRC-ceiling but still-decelerating command is clamped by the
    /// envelope (NOT denied) — the gate defers (a) to validate_vehicle_command.
    #[test]
    fn test_degraded_overspeed_but_decelerating_is_clamped_by_envelope() {
        let mrc = VehicleKinematicsContract::mrc_fallback_profile();
        // Current 20, proposed 8: decelerating (non-increasing) but 8 > 5.0
        // MRC ceiling → envelope clamps to 5.0.
        let cmd = degraded_cmd(20.0, 8.0);
        assert_eq!(
            enforce_degraded_decel_to_stop(&cmd, &mrc),
            EnforceAction::ClampLinear(5.0)
        );
    }

    /// Non-finite inputs fail closed even on the Degraded gate.
    #[test]
    fn test_degraded_gate_rejects_nan_linear() {
        let mrc = VehicleKinematicsContract::mrc_fallback_profile();
        let cmd = degraded_cmd(2.0, f64::NAN);
        assert_eq!(
            enforce_degraded_decel_to_stop(&cmd, &mrc),
            EnforceAction::DenyBreach(DenyCode::NanInfLinearVelocity)
        );
    }
}
