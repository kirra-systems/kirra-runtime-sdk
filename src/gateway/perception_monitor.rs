// src/gateway/perception_monitor.rs
//
// Track-C Perception Monitor — Phase-0 first slice.
// Doc: docs/safety/OCCY_PERCEPTION_MONITOR_PHASE0.md (KIRRA-OCCY-PMON-001).
//
// Two bounded, deterministic, STATELESS analytic guards over Track-A
// perception OUTPUT (object lists, observed detection range). Neither is
// learned inference; neither sits on the Nominal verdict path. Both emit a
// homogeneous `DerateDecision` (a permitted-speed cap + a `DerateCode`
// reason) that composes into the ADR-0002 cap `min()` and surfaces — when the
// cap forces commanded speed below the proposed value — as the EXISTING
// `EnforceAction::ClampLinear(cap)`. `DenyCode` and `validate_vehicle_command`
// are UNTOUCHED (C2 byte-stable discipline).
//
// Templated on `gateway/containment.rs`: `is_healthy()`-style conservative
// gating, `*_is_finite` fail-closed, `MAX_*` bounding, scalar f64 math, no
// heap allocation, no recursion, and stable audit-token tests.
//
// PENDING-WIRING (staged follow-ups, NOT built in this slice):
//   - Live `R_obs` feed from Track-B detection-range profiling (the one
//     cross-track coupling; OCCY_SPEED_CAP_VALIDATION.md Item B / #120). This
//     slice unit-tests against synthetic/constructed `R_obs`, exactly as the
//     SG2 containment check was unit-tested against constructed corridors.
//   - Verdict-path composition wiring: feeding these caps into the ADR-0002
//     `min()`. Today the only composition point is
//     `VehicleKinematicsContract::effective_max_speed_mps()` =
//     `min(max_speed_mps, odd_speed_cap_mps)`; the full multi-input min is
//     not yet a single function (see doc §5.2 / §7).
//   - `wcet_gate` budget registration for both guards.

use crate::audit_chain::PerceptionDerateEvent;

// ---------------------------------------------------------------------------
// Bounds & constants
// ---------------------------------------------------------------------------

/// Maximum number of tracked objects inspected per cycle. Bounds the
/// per-call WCET of [`kinematic_plausibility_derate`] (the single per-object
/// loop). Over-cap → conservative MRC-floor derate, never silent truncation.
pub const MAX_TRACKED_OBJECTS: usize = 256;

/// Maximum credible object ground speed (m/s), GROUND/MAP-FRAME.
///
/// **Derived value — KIRRA-OCCY-PMON-KIN-MARGIN-001.** The contract reports
/// each object's own **map-frame** velocity (confirmed against the Autoware
/// adapter — this is the object's absolute ground speed, NOT an ego-relative
/// / closing speed), so the ceiling is a single absolute bound, not a sum of
/// ego + object speeds.
///
/// `60.0 m/s` (216 km/h) is the rounded value of:
///   - max credible object-class ground speed near the deployment ODD road
///     network (highway-adjacent vehicle, ~50 m/s / 180 km/h)            ≈ 50 m/s
///   - + margin for measurement noise / transient tracker overshoot       ≈ 10 m/s
///   →                                                                    = 60 m/s
/// bounded ABOVE by the sensors' reliable-velocity measurement range (a
/// reported speed past this is a sensor/tracker artifact, not a real actor).
///
/// **Disposition is a DERATE, not a hard reject** (doc §2): a tight ceiling
/// therefore costs at most a conservative slowdown, and the teleport
/// implied-speed check (which uses the SAME constant) backstops a velocity
/// field that under-reports while the position field jumps. Both failure
/// modes mark the object implausible and feed the graded derate; they never
/// emit a `DenyCode` on the hot path.
///
/// See docs/safety/OCCY_PERCEPTION_MONITOR_PHASE0.md §4 + §10.
pub const V_OBJECT_MAX_MPS: f64 = 60.0;

/// Monotone fixed step table for Guard 1: implausible-fraction → cap factor
/// of the nominal cap. Each row is `(max_inclusive_fraction, cap_factor)`.
///
/// **Monotone non-increasing in `cap_factor`** by construction (auditable,
/// O(1) lookup, deterministic — NOT a clamped-linear map). A fraction above
/// the last row's threshold falls through to the MRC floor (handled in
/// [`kinematic_plausibility_derate`], not the table).
const KIN_DERATE_TABLE: &[(f64, f64)] = &[
    (0.00, 1.00), // no implausible objects → no derate
    (0.10, 0.75), // up to 10% implausible → 75% cap
    (0.25, 0.50), // up to 25% implausible → 50% cap
    (0.50, 0.25), // up to 50% implausible → 25% cap
    // > 0.50 implausible → MRC floor (table tail; see fn body)
];

// ---------------------------------------------------------------------------
// Reason codes (mirror `DenyCode`: Copy + 'static, SCREAMING_SNAKE_CASE,
// const reason(), Display) — feed the Ed25519 audit chain as the event_type.
// ---------------------------------------------------------------------------

/// Reason codes for a [`DerateDecision`]. Each variant maps to a fixed
/// `&'static str` token (via [`DerateCode::reason`]) that is byte-stable for
/// audit-chain hashing — exactly the `DenyCode` convention in
/// `gateway/kinematics_contract.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DerateCode {
    /// No derate applied — the guard passed cleanly (cap unchanged).
    NominalNoDerate,
    /// Guard 1: ≥ 1 object reported a ground speed above `V_OBJECT_MAX_MPS`.
    ObjectVelocityImplausible,
    /// Guard 1: ≥ 1 object's frame-to-frame position jump implies a speed
    /// above `V_OBJECT_MAX_MPS` (teleport / track-ID swap / detection flicker).
    ObjectFrameTeleport,
    /// Guard 1: the perception snapshot is structurally unusable —
    /// absent / stale / low-confidence / over-`MAX_TRACKED_OBJECTS` /
    /// a non-finite or `dt_s <= 0` object. Conservative MRC-floor derate.
    PerceptionSnapshotUnhealthy,
    /// Guard 2: observed detection range is fresh and trusted but supports a
    /// cap below the nominal cap (`range_supported(R_obs) < nominal`).
    DetectionRangeDegraded,
    /// Guard 2: observed detection range is untrusted — low confidence /
    /// stale / non-finite / negative. Treated as `R_obs = 0` → controlled
    /// stop (cap 0). Conservative.
    DetectionRangeUntrusted,
}

impl DerateCode {
    /// Byte-stable audit/log token (e.g. `"OBJECT_VELOCITY_IMPLAUSIBLE"`).
    /// Identical SCREAMING_SNAKE_CASE rendering to the serde representation —
    /// preserving audit-chain hash stability.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::NominalNoDerate            => "NOMINAL_NO_DERATE",
            Self::ObjectVelocityImplausible  => "OBJECT_VELOCITY_IMPLAUSIBLE",
            Self::ObjectFrameTeleport        => "OBJECT_FRAME_TELEPORT",
            Self::PerceptionSnapshotUnhealthy => "PERCEPTION_SNAPSHOT_UNHEALTHY",
            Self::DetectionRangeDegraded     => "DETECTION_RANGE_DEGRADED",
            Self::DetectionRangeUntrusted    => "DETECTION_RANGE_UNTRUSTED",
        }
    }
}

impl std::fmt::Display for DerateCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason())
    }
}

/// Homogeneous output of both Phase-0 guards: a permitted-speed cap (m/s) and
/// the reason for it. A cap of `0.0` is a controlled stop (the MRC-floor
/// disposition expressed through the derate channel).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DerateDecision {
    pub cap_mps: f64,
    pub reason: DerateCode,
}

impl DerateDecision {
    /// True iff this decision is a controlled-stop / MRC-floor disposition.
    #[must_use]
    pub fn is_controlled_stop(&self) -> bool {
        self.cap_mps <= 0.0
    }

    /// Build the audit-chain event payload for this decision. The audit
    /// `event_type` is `self.reason.reason()` (the byte-stable token); the
    /// JSON payload carries the cap. Feeds `AuditChainLinker::append_*`.
    #[must_use]
    pub fn to_audit_event(&self, timestamp_ms: u64) -> PerceptionDerateEvent {
        PerceptionDerateEvent {
            reason: self.reason.reason().to_string(),
            cap_mps: self.cap_mps,
            timestamp_ms,
        }
    }
}

// ---------------------------------------------------------------------------
// Perception-output contract (doc §3) — KIRRA-side view of #126.
// ---------------------------------------------------------------------------

/// 2D vector in the map/world frame (meters or m/s depending on use).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

impl Vec2 {
    #[inline]
    #[must_use]
    pub fn magnitude(&self) -> f64 {
        self.x.hypot(self.y)
    }
    #[inline]
    fn is_finite(&self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
    #[inline]
    fn dist_to(&self, other: &Vec2) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

/// A single tracked object from Track-A perception output. STATELESS guard:
/// the contract supplies `prev_pos_m` + `dt_s` so the teleport check needs no
/// KIRRA-held frame and no `SystemTime::now()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackedObject {
    pub id: u64,
    /// Current position (map frame, meters).
    pub pos_m: Vec2,
    /// Reported ground-frame velocity (map frame, m/s).
    pub vel_mps: Vec2,
    /// Position in the previous frame (map frame, meters).
    pub prev_pos_m: Vec2,
    /// Time delta between `prev_pos_m` and `pos_m` (seconds).
    pub dt_s: f64,
}

impl TrackedObject {
    /// Fail-closed finite/structural validity: all geometry finite and a
    /// strictly positive `dt_s`. Returns false → the object is structurally
    /// invalid → Guard 1 fails closed to the MRC floor.
    #[inline]
    fn is_structurally_valid(&self) -> bool {
        self.pos_m.is_finite()
            && self.vel_mps.is_finite()
            && self.prev_pos_m.is_finite()
            && self.dt_s.is_finite()
            && self.dt_s > 0.0
    }

    /// True iff this object's reported velocity exceeds the ground-frame
    /// ceiling (Guard 1, velocity-ceiling check). Caller has already
    /// confirmed structural validity.
    #[inline]
    fn velocity_implausible(&self, v_max: f64) -> bool {
        self.vel_mps.magnitude() > v_max
    }

    /// True iff the implied frame-to-frame speed exceeds the ceiling
    /// (Guard 1, teleport check). Caller has already confirmed structural
    /// validity (so `dt_s > 0`).
    #[inline]
    fn teleport_implausible(&self, v_max: f64) -> bool {
        self.pos_m.dist_to(&self.prev_pos_m) / self.dt_s > v_max
    }
}

/// Bounded snapshot of perception output consumed by Guard 1, with health
/// metadata. `is_healthy()` mirrors `Corridor::is_healthy` — absent / stale /
/// low-confidence / over-cap → conservative failure.
#[derive(Debug, Clone, Copy)]
pub struct PerceptionOutput<'a> {
    pub objects: &'a [TrackedObject],
    /// Source confidence in `[0.0, 1.0]`. Below `min_confidence` → unhealthy.
    pub confidence: f32,
    /// Age (ms) of the snapshot vs. now. Above `max_age_ms` → unhealthy.
    pub age_ms: u64,
    pub min_confidence: f32,
    pub max_age_ms: u64,
}

impl PerceptionOutput<'_> {
    /// True iff the snapshot is present, fresh, confident, and within the
    /// object-count bound. Failure → conservative MRC-floor derate.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.confidence.is_finite()
            && self.confidence >= self.min_confidence
            && self.age_ms <= self.max_age_ms
            && self.objects.len() <= MAX_TRACKED_OBJECTS
    }
}

// ---------------------------------------------------------------------------
// Contracts (tunables) for the two guards.
// ---------------------------------------------------------------------------

/// Tunables for [`kinematic_plausibility_derate`].
#[derive(Debug, Clone, Copy)]
pub struct KinematicPlausibilityContract {
    /// Nominal permitted cap when no derate applies (m/s).
    pub nominal_cap_mps: f64,
    /// MRC floor cap for the conservative/structural-failure disposition
    /// (m/s). `0.0` = controlled stop.
    pub mrc_floor_mps: f64,
    /// Ground-frame object-speed ceiling (m/s). Defaults to `V_OBJECT_MAX_MPS`.
    pub v_object_max_mps: f64,
}

impl KinematicPlausibilityContract {
    /// Reference urban-ODD contract: nominal = `URBAN_ODD_SPEED_CAP_MPS`,
    /// MRC floor = controlled stop, ceiling = `V_OBJECT_MAX_MPS`.
    #[must_use]
    pub fn urban_reference() -> Self {
        Self {
            nominal_cap_mps: crate::gateway::kinematics_contract::URBAN_ODD_SPEED_CAP_MPS,
            mrc_floor_mps: 0.0,
            v_object_max_mps: V_OBJECT_MAX_MPS,
        }
    }
}

/// Tunables for [`range_supported_derate`] — the SSD model parameters.
#[derive(Debug, Clone, Copy)]
pub struct RangeDerateContract {
    /// Nominal permitted cap (m/s); the range cap is `min(nominal, v_max(R))`.
    pub nominal_cap_mps: f64,
    /// Reaction / actuation-latency term in the SSD model (seconds). The
    /// FTTI chain (S3 / #115) is the authoritative source — PENDING-WIRING.
    pub t_react_s: f64,
    /// Braking deceleration basis in the SSD model (m/s²). Comfortable basis
    /// (3.0) per OCCY_SPEED_CAP_VALIDATION.md item 3.
    pub a_brake_mps2: f64,
    /// Minimum acceptable `range_confidence`; below → untrusted.
    pub min_confidence: f32,
    /// Maximum acceptable snapshot age (ms); above → untrusted.
    pub max_age_ms: u64,
}

impl RangeDerateContract {
    /// Reference urban-ODD contract. With these values
    /// `range_supported(94.4 m) ≈ 22.35 m/s`, reproducing the ADR-0001
    /// "50 mph presumes R ≥ 94 m look-ahead" figure (SPEED_ENVELOPE.md §5–6).
    #[must_use]
    pub fn urban_reference() -> Self {
        Self {
            nominal_cap_mps: crate::gateway::kinematics_contract::URBAN_ODD_SPEED_CAP_MPS,
            t_react_s: 0.5,
            a_brake_mps2: 3.0,
            min_confidence: 0.5,
            max_age_ms: 500,
        }
    }
}

// ---------------------------------------------------------------------------
// GUARD 1 — kinematic-plausibility derate (KIRRA-OCCY-PMON-KIN-001).
// ---------------------------------------------------------------------------

// SAFETY: PMON-KIN | REQ: kinematic-plausibility-monitor | TEST: kin_passes_clean_snapshot,kin_velocity_ceiling_trips_derate,kin_teleport_trips_derate,kin_nonfinite_object_mrc_floor,kin_nonpositive_dt_mrc_floor,kin_unhealthy_snapshot_mrc_floor,kin_over_cap_object_count_mrc_floor,kin_step_table_each_bin,kin_velocity_ceiling_priority_over_teleport
/// Guard 1 — kinematic-plausibility derate.
///
/// STATELESS, pure, `O(MAX_TRACKED_OBJECTS)`. For each object: a finite /
/// structural check (any non-finite field or `dt_s <= 0` → MRC floor), then a
/// velocity-ceiling check and a teleport (implied-speed) check against
/// `contract.v_object_max_mps`. The implausible **fraction** over the bounded
/// slice maps through the monotone [`KIN_DERATE_TABLE`] to a cap; a fraction
/// above the table's last threshold → MRC floor.
///
/// Conservative gates (each → `PerceptionSnapshotUnhealthy`, MRC floor):
/// unhealthy snapshot, over-`MAX_TRACKED_OBJECTS`, or any structurally invalid
/// object. An empty (but healthy) snapshot → no derate (nothing to distrust).
///
/// Reason precedence when ≥ 1 object is implausible: `ObjectVelocityImplausible`
/// wins over `ObjectFrameTeleport` (deterministic "first-class cause wins",
/// mirroring `EnforceAction`'s first-triggered-check-wins).
#[must_use]
pub fn kinematic_plausibility_derate(
    perception: &PerceptionOutput,
    contract: &KinematicPlausibilityContract,
) -> DerateDecision {
    let mrc = DerateDecision {
        cap_mps: contract.mrc_floor_mps,
        reason: DerateCode::PerceptionSnapshotUnhealthy,
    };

    // Conservative gate: snapshot health (covers over-MAX_TRACKED_OBJECTS).
    if !perception.is_healthy() {
        return mrc;
    }

    let total = perception.objects.len();
    if total == 0 {
        // Healthy, nothing to distrust → no derate.
        return DerateDecision {
            cap_mps: contract.nominal_cap_mps,
            reason: DerateCode::NominalNoDerate,
        };
    }

    let v_max = contract.v_object_max_mps;
    let mut implausible = 0usize;
    let mut any_velocity = false;
    let mut any_teleport = false;

    for obj in perception.objects.iter() {
        // Fail-closed on any structurally invalid object.
        if !obj.is_structurally_valid() {
            return mrc;
        }
        let vel_bad = obj.velocity_implausible(v_max);
        let tel_bad = obj.teleport_implausible(v_max);
        if vel_bad || tel_bad {
            implausible += 1;
            any_velocity |= vel_bad;
            any_teleport |= tel_bad;
        }
    }

    if implausible == 0 {
        return DerateDecision {
            cap_mps: contract.nominal_cap_mps,
            reason: DerateCode::NominalNoDerate,
        };
    }

    let fraction = implausible as f64 / total as f64;

    // Velocity-ceiling cause takes precedence over teleport.
    let reason = if any_velocity {
        DerateCode::ObjectVelocityImplausible
    } else {
        debug_assert!(any_teleport);
        DerateCode::ObjectFrameTeleport
    };

    let cap = match step_table_factor(fraction) {
        Some(factor) => (contract.nominal_cap_mps * factor).max(contract.mrc_floor_mps),
        // Fraction past the last table threshold → MRC floor.
        None => contract.mrc_floor_mps,
    };

    DerateDecision { cap_mps: cap, reason }
}

/// Monotone step-table lookup: returns the cap factor for the first row whose
/// inclusive fraction threshold is `>= fraction`, or `None` if `fraction`
/// exceeds the last threshold (caller applies the MRC floor). Deterministic,
/// O(table length).
#[inline]
fn step_table_factor(fraction: f64) -> Option<f64> {
    for (threshold, factor) in KIN_DERATE_TABLE.iter() {
        if fraction <= *threshold {
            return Some(*factor);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// GUARD 2 — range-based derate (KIRRA-OCCY-PMON-RNG-001).
// ---------------------------------------------------------------------------

// SAFETY: PMON-RNG | REQ: range-based-derate | TEST: range_full_range_no_derate,range_degraded_below_nominal,range_zero_is_controlled_stop,range_untrusted_low_confidence,range_untrusted_stale,range_untrusted_nonfinite,range_ssd_inversion_reproduces_adr0001,range_monotone_proptest
/// Guard 2 — range-based derate. Realizes ADR-0001 rule 1 (cap = f(R)) and the
/// `range_supported(R_obs)` term of the ADR-0002 composition.
///
/// `cap = min(nominal_cap, v_max(R_obs))` where `v_max` inverts the
/// stopping-sight-distance model `SSD(v) = v·t_react + v²/(2·a_brake) ≤ R_obs`:
///
/// ```text
///   v_max(R) = a_brake · ( sqrt(t_react² + 2·R/a_brake) − t_react )
/// ```
///
/// Monotone non-decreasing in `R_obs`; condition-agnostic (no weather
/// classifier — rain/fog/night/dirt all manifest as a shorter `R_obs` and are
/// handled uniformly); downward-only with no hysteresis on the drop.
///
/// Conservative: low confidence / stale / non-finite / negative `R_obs` →
/// treated as `R_obs = 0` → `v_max = 0` → controlled stop
/// (`DetectionRangeUntrusted`).
///
/// PENDING-WIRING: `observed_range_m` is supplied here as a constructed input;
/// the live feed is Track-B profiling (#120 Item B).
#[must_use]
pub fn range_supported_derate(
    observed_range_m: f64,
    range_confidence: f32,
    snapshot_age_ms: u64,
    contract: &RangeDerateContract,
) -> DerateDecision {
    // Conservative trust gate → treat as R_obs = 0 (controlled stop).
    let untrusted = !range_confidence.is_finite()
        || range_confidence < contract.min_confidence
        || snapshot_age_ms > contract.max_age_ms
        || !observed_range_m.is_finite()
        || observed_range_m < 0.0;

    if untrusted {
        return DerateDecision {
            cap_mps: 0.0,
            reason: DerateCode::DetectionRangeUntrusted,
        };
    }

    let v_max = range_supported_speed_mps(
        observed_range_m,
        contract.t_react_s,
        contract.a_brake_mps2,
    );

    if v_max >= contract.nominal_cap_mps {
        DerateDecision {
            cap_mps: contract.nominal_cap_mps,
            reason: DerateCode::NominalNoDerate,
        }
    } else {
        DerateDecision {
            cap_mps: v_max,
            reason: DerateCode::DetectionRangeDegraded,
        }
    }
}

/// Inverts `SSD(v) = v·t_react + v²/(2·a_brake) = R` for the largest `v ≥ 0`.
/// Monotone non-decreasing in `R`. Returns `0.0` for degenerate parameters
/// (fail-closed).
#[inline]
#[must_use]
pub fn range_supported_speed_mps(range_m: f64, t_react_s: f64, a_brake_mps2: f64) -> f64 {
    if !(range_m.is_finite() && t_react_s.is_finite() && a_brake_mps2.is_finite())
        || a_brake_mps2 <= 0.0
        || t_react_s < 0.0
        || range_m <= 0.0
    {
        return 0.0;
    }
    // v = a·( sqrt(t² + 2R/a) − t )
    let disc = t_react_s * t_react_s + 2.0 * range_m / a_brake_mps2;
    let v = a_brake_mps2 * (disc.sqrt() - t_react_s);
    if v.is_finite() && v > 0.0 {
        v
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f64, y: f64) -> Vec2 {
        Vec2 { x, y }
    }

    /// A plausible object: small velocity, small frame step.
    fn ok_object(id: u64) -> TrackedObject {
        TrackedObject {
            id,
            pos_m: v(10.0, 0.0),
            vel_mps: v(5.0, 0.0),
            prev_pos_m: v(9.5, 0.0), // 0.5 m step
            dt_s: 0.1,               // → 5 m/s implied
        }
    }

    fn healthy<'a>(objects: &'a [TrackedObject]) -> PerceptionOutput<'a> {
        PerceptionOutput {
            objects,
            confidence: 0.95,
            age_ms: 10,
            min_confidence: 0.5,
            max_age_ms: 500,
        }
    }

    // ---- Guard 1 ----

    #[test]
    fn kin_passes_clean_snapshot() {
        let objs = [ok_object(1), ok_object(2)];
        let p = healthy(&objs);
        let d = kinematic_plausibility_derate(&p, &KinematicPlausibilityContract::urban_reference());
        assert_eq!(d.reason, DerateCode::NominalNoDerate);
        assert_eq!(d.cap_mps, crate::gateway::kinematics_contract::URBAN_ODD_SPEED_CAP_MPS);
    }

    #[test]
    fn kin_empty_healthy_snapshot_no_derate() {
        let objs: [TrackedObject; 0] = [];
        let p = healthy(&objs);
        let d = kinematic_plausibility_derate(&p, &KinematicPlausibilityContract::urban_reference());
        assert_eq!(d.reason, DerateCode::NominalNoDerate);
    }

    #[test]
    fn kin_velocity_ceiling_trips_derate() {
        // 1 of 2 objects over the 60 m/s ceiling → fraction 0.5 → factor 0.25.
        let mut bad = ok_object(2);
        bad.vel_mps = v(70.0, 0.0); // > 60
        let objs = [ok_object(1), bad];
        let p = healthy(&objs);
        let c = KinematicPlausibilityContract::urban_reference();
        let d = kinematic_plausibility_derate(&p, &c);
        assert_eq!(d.reason, DerateCode::ObjectVelocityImplausible);
        assert!((d.cap_mps - c.nominal_cap_mps * 0.25).abs() < 1e-9);
        assert!(d.cap_mps < c.nominal_cap_mps);
    }

    #[test]
    fn kin_teleport_trips_derate() {
        // Velocity field looks fine but position jumped 10 m in 0.1 s → 100 m/s.
        let mut bad = ok_object(2);
        bad.vel_mps = v(3.0, 0.0); // plausible velocity
        bad.prev_pos_m = v(0.0, 0.0);
        bad.pos_m = v(10.0, 0.0); // 10 m
        bad.dt_s = 0.1; // → 100 m/s implied
        let objs = [ok_object(1), bad];
        let p = healthy(&objs);
        let d = kinematic_plausibility_derate(&p, &KinematicPlausibilityContract::urban_reference());
        assert_eq!(d.reason, DerateCode::ObjectFrameTeleport);
    }

    #[test]
    fn kin_velocity_ceiling_priority_over_teleport() {
        // One object trips velocity, another trips teleport → velocity wins.
        let mut vbad = ok_object(1);
        vbad.vel_mps = v(70.0, 0.0);
        let mut tbad = ok_object(2);
        tbad.prev_pos_m = v(0.0, 0.0);
        tbad.pos_m = v(20.0, 0.0);
        tbad.dt_s = 0.1; // 200 m/s implied
        let objs = [vbad, tbad];
        let p = healthy(&objs);
        let d = kinematic_plausibility_derate(&p, &KinematicPlausibilityContract::urban_reference());
        assert_eq!(d.reason, DerateCode::ObjectVelocityImplausible);
    }

    #[test]
    fn kin_nonfinite_object_mrc_floor() {
        let mut bad = ok_object(1);
        bad.vel_mps = v(f64::NAN, 0.0);
        let objs = [bad];
        let p = healthy(&objs);
        let c = KinematicPlausibilityContract::urban_reference();
        let d = kinematic_plausibility_derate(&p, &c);
        assert_eq!(d.reason, DerateCode::PerceptionSnapshotUnhealthy);
        assert_eq!(d.cap_mps, c.mrc_floor_mps);
        assert!(d.is_controlled_stop());
    }

    #[test]
    fn kin_nonpositive_dt_mrc_floor() {
        let mut bad = ok_object(1);
        bad.dt_s = 0.0; // teleport check undefined → fail closed
        let objs = [bad];
        let p = healthy(&objs);
        let d = kinematic_plausibility_derate(&p, &KinematicPlausibilityContract::urban_reference());
        assert_eq!(d.reason, DerateCode::PerceptionSnapshotUnhealthy);
    }

    #[test]
    fn kin_unhealthy_snapshot_mrc_floor() {
        let objs = [ok_object(1)];
        let mut p = healthy(&objs);
        p.confidence = 0.1; // below min_confidence
        let d = kinematic_plausibility_derate(&p, &KinematicPlausibilityContract::urban_reference());
        assert_eq!(d.reason, DerateCode::PerceptionSnapshotUnhealthy);
    }

    #[test]
    fn kin_stale_snapshot_mrc_floor() {
        let objs = [ok_object(1)];
        let mut p = healthy(&objs);
        p.age_ms = 10_000; // > max_age_ms
        let d = kinematic_plausibility_derate(&p, &KinematicPlausibilityContract::urban_reference());
        assert_eq!(d.reason, DerateCode::PerceptionSnapshotUnhealthy);
    }

    #[test]
    fn kin_over_cap_object_count_mrc_floor() {
        // MAX_TRACKED_OBJECTS + 1 → unhealthy → MRC floor.
        let objs: Vec<TrackedObject> =
            (0..(MAX_TRACKED_OBJECTS + 1) as u64).map(ok_object).collect();
        let p = healthy(&objs);
        let d = kinematic_plausibility_derate(&p, &KinematicPlausibilityContract::urban_reference());
        assert_eq!(d.reason, DerateCode::PerceptionSnapshotUnhealthy);
    }

    #[test]
    fn kin_full_max_slice_runs_bounded() {
        // Exactly MAX_TRACKED_OBJECTS healthy objects: passes, bounded WCET shape.
        let objs: Vec<TrackedObject> =
            (0..MAX_TRACKED_OBJECTS as u64).map(ok_object).collect();
        let p = healthy(&objs);
        let d = kinematic_plausibility_derate(&p, &KinematicPlausibilityContract::urban_reference());
        assert_eq!(d.reason, DerateCode::NominalNoDerate);
    }

    #[test]
    fn kin_step_table_each_bin() {
        let c = KinematicPlausibilityContract::urban_reference();
        let nominal = c.nominal_cap_mps;

        // Helper: build `total` objects with `bad` of them velocity-implausible.
        let build = |total: usize, bad: usize| -> DerateDecision {
            let mut objs: Vec<TrackedObject> = (0..total as u64).map(ok_object).collect();
            for o in objs.iter_mut().take(bad) {
                o.vel_mps = v(70.0, 0.0);
            }
            // Leak into a slice for the borrow; use a local binding instead.
            let p = PerceptionOutput {
                objects: &objs,
                confidence: 0.95,
                age_ms: 10,
                min_confidence: 0.5,
                max_age_ms: 500,
            };
            kinematic_plausibility_derate(&p, &c)
        };

        // fraction 0.0 → 1.00
        assert!((build(10, 0).cap_mps - nominal * 1.00).abs() < 1e-9);
        // fraction 0.10 → 0.75
        assert!((build(10, 1).cap_mps - nominal * 0.75).abs() < 1e-9);
        // fraction 0.20 (≤0.25) → 0.50
        assert!((build(10, 2).cap_mps - nominal * 0.50).abs() < 1e-9);
        // fraction 0.40 (≤0.50) → 0.25
        assert!((build(10, 4).cap_mps - nominal * 0.25).abs() < 1e-9);
        // fraction 0.60 (>0.50) → MRC floor
        assert!((build(10, 6).cap_mps - c.mrc_floor_mps).abs() < 1e-9);
        // monotone non-increasing across the bins
        assert!(build(10, 0).cap_mps >= build(10, 1).cap_mps);
        assert!(build(10, 1).cap_mps >= build(10, 2).cap_mps);
        assert!(build(10, 2).cap_mps >= build(10, 4).cap_mps);
        assert!(build(10, 4).cap_mps >= build(10, 6).cap_mps);
    }

    // ---- Guard 2 ----

    #[test]
    fn range_full_range_no_derate() {
        let c = RangeDerateContract::urban_reference();
        // Plenty of range → cap at nominal.
        let d = range_supported_derate(1000.0, 0.95, 10, &c);
        assert_eq!(d.reason, DerateCode::NominalNoDerate);
        assert_eq!(d.cap_mps, c.nominal_cap_mps);
    }

    #[test]
    fn range_degraded_below_nominal() {
        let c = RangeDerateContract::urban_reference();
        // 50 m range → well under the ~94 m needed for nominal → degraded.
        let d = range_supported_derate(50.0, 0.95, 10, &c);
        assert_eq!(d.reason, DerateCode::DetectionRangeDegraded);
        assert!(d.cap_mps > 0.0 && d.cap_mps < c.nominal_cap_mps);
    }

    #[test]
    fn range_zero_is_controlled_stop() {
        let c = RangeDerateContract::urban_reference();
        let d = range_supported_derate(0.0, 0.95, 10, &c);
        // R=0 → v_max=0 → degraded cap of 0 (controlled stop).
        assert!(d.is_controlled_stop());
    }

    #[test]
    fn range_untrusted_low_confidence() {
        let c = RangeDerateContract::urban_reference();
        let d = range_supported_derate(1000.0, 0.1, 10, &c);
        assert_eq!(d.reason, DerateCode::DetectionRangeUntrusted);
        assert!(d.is_controlled_stop());
    }

    #[test]
    fn range_untrusted_stale() {
        let c = RangeDerateContract::urban_reference();
        let d = range_supported_derate(1000.0, 0.95, 10_000, &c);
        assert_eq!(d.reason, DerateCode::DetectionRangeUntrusted);
    }

    #[test]
    fn range_untrusted_nonfinite() {
        let c = RangeDerateContract::urban_reference();
        let d = range_supported_derate(f64::NAN, 0.95, 10, &c);
        assert_eq!(d.reason, DerateCode::DetectionRangeUntrusted);
        let d2 = range_supported_derate(-5.0, 0.95, 10, &c);
        assert_eq!(d2.reason, DerateCode::DetectionRangeUntrusted);
    }

    #[test]
    fn range_ssd_inversion_reproduces_adr0001() {
        // ADR-0001: 50 mph (22.35 m/s) presumes R ≥ ~94 m look-ahead.
        // With t_react=0.5, a_brake=3.0: SSD(22.35) = 11.175 + 83.25 ≈ 94.4 m.
        let c = RangeDerateContract::urban_reference();
        let required = 22.35 * c.t_react_s + 22.35 * 22.35 / (2.0 * c.a_brake_mps2);
        assert!((required - 94.4).abs() < 0.5, "SSD(22.35) ≈ 94.4 m, got {required}");
        // Inverting that range yields ~22.35 m/s.
        let v_max = range_supported_speed_mps(required, c.t_react_s, c.a_brake_mps2);
        assert!((v_max - 22.35).abs() < 1e-3, "inverted v_max = {v_max}");
    }

    #[test]
    fn range_monotone_proptest() {
        // R_obs ↓ ⇒ cap ↓ (never up). Deterministic sweep stands in for a
        // proptest harness; the property is the same one proptest would assert.
        let c = RangeDerateContract::urban_reference();
        let mut prev_cap = -1.0;
        let mut r = 0.0;
        while r <= 400.0 {
            let cap = range_supported_derate(r, 0.95, 10, &c).cap_mps;
            assert!(
                cap >= prev_cap - 1e-9,
                "cap must be non-decreasing in R: at R={r} cap={cap} < prev={prev_cap}"
            );
            prev_cap = cap;
            r += 1.0;
        }
    }

    // ---- DerateCode stable-token (audit) ----

    #[test]
    fn derate_code_stable_tokens() {
        for (code, token) in [
            (DerateCode::NominalNoDerate, "NOMINAL_NO_DERATE"),
            (DerateCode::ObjectVelocityImplausible, "OBJECT_VELOCITY_IMPLAUSIBLE"),
            (DerateCode::ObjectFrameTeleport, "OBJECT_FRAME_TELEPORT"),
            (DerateCode::PerceptionSnapshotUnhealthy, "PERCEPTION_SNAPSHOT_UNHEALTHY"),
            (DerateCode::DetectionRangeDegraded, "DETECTION_RANGE_DEGRADED"),
            (DerateCode::DetectionRangeUntrusted, "DETECTION_RANGE_UNTRUSTED"),
        ] {
            assert_eq!(code.reason(), token);
            assert_eq!(code.to_string(), token);
            let json = serde_json::to_string(&code).expect("serialize");
            assert_eq!(json, format!("\"{token}\""));
        }
    }
}
