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

// Imported from the lean foundation, not the heavy `audit_chain` (Stage 2): the
// gateway/governor surface stays free of the verifier service's deps.
use crate::PerceptionDerateEvent;

// ---------------------------------------------------------------------------
// Bounds & constants
// ---------------------------------------------------------------------------

/// Maximum number of tracked objects inspected per cycle. Bounds the
/// per-call WCET of [`kinematic_plausibility_derate`] (the single per-object
/// loop). Over-cap → conservative MRC-floor derate, never silent truncation.
pub const MAX_TRACKED_OBJECTS: usize = 256;

/// Maximum credible object ground speed (m/s), GROUND/MAP-FRAME.
///
/// **Derived value — KIRRA-OCCY-PMON-KIN-MARGIN-001.** The ceiling is checked
/// against each object's own **map-frame absolute ground speed** (not an
/// ego-relative / closing speed), so it is a single absolute bound, not a sum
/// of ego + object speeds. **This map/world-frame assumption rests on the
/// upstream Autoware message contract** (`PredictedObjects` twist), NOT on any
/// transform inside the KIRRA adapter — the adapter performs no frame
/// conversion on object twist (see KIRRA-OCCY-PMON-003 §4 / D4). **Confirm per
/// deployment** that the target Autoware version emits object twist as
/// map/world-frame absolute velocity before enabling the derate on a vehicle;
/// if it does not, the ingest shim must transform to map frame first.
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
            nominal_cap_mps: crate::kinematics_contract::URBAN_ODD_SPEED_CAP_MPS,
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
            nominal_cap_mps: crate::kinematics_contract::URBAN_ODD_SPEED_CAP_MPS,
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

// ===========================================================================
// Verdict-path composition (KIRRA-OCCY-PMON-002).
//
// Makes the guards ENFORCE without touching the byte-stable verdict path.
// Option B: a perception-monitor worker evaluates the guards at perception-tick
// rate and PUBLISHES a cap to `SharedPerceptionCap`; the verdict surfaces read
// that cap O(1), resolve the 3-state lifecycle, and TIGHTEN the contract they
// pass to `validate_vehicle_command` via `apply_perception_cap` — so
// `validate_vehicle_command` and `effective_max_speed_mps` stay byte-identical.
// Derate-only: never touches `DenyCode` or the deny path.
// ===========================================================================

use std::sync::{Arc, RwLock};
use crate::kinematics_contract::VehicleKinematicsContract;

/// The MRC-floor cap (m/s). A published cap of `0.0` composes into
/// `effective_max_speed = min(…, 0.0) = 0.0` and surfaces as the EXISTING
/// `ClampLinear(0.0)` → controlled stop. No new code path.
pub const MRC_FLOOR_CAP_MPS: f64 = 0.0;

/// Atomic published snapshot of the perception-derived speed cap. Mirrors
/// `CachedFleetPosture`: the worker owns `generated_at_ms`/`ttl_ms`; the
/// verdict-path resolver reads them to evaluate staleness.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CachedPerceptionCap {
    /// Published permitted-speed cap (m/s). `0.0` = controlled stop.
    pub cap_mps: f64,
    /// Absolute timestamp (ms since UNIX epoch) when the worker published this.
    pub generated_at_ms: u64,
    /// Staleness TTL (ms). After `generated_at_ms + ttl_ms < now`, the resolver
    /// treats this entry as stale and fails closed to the MRC floor.
    pub ttl_ms: u64,
    /// The `DerateCode` that produced this cap (audit/diagnostics).
    pub reason: DerateCode,
}

impl CachedPerceptionCap {
    /// Returns true if this entry has exceeded its TTL relative to `now_ms`.
    /// Saturating subtraction tolerates clock skew without panic — identical
    /// to `CachedFleetPosture::is_stale`.
    #[must_use]
    pub fn is_stale(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.generated_at_ms) >= self.ttl_ms
    }
}

/// Shared perception-cap cache. `Arc<RwLock<Option<CachedPerceptionCap>>>` —
/// the same shape as `SharedPostureCache`. `None` = the enabled worker has not
/// published yet (cold start) → the resolver fails closed.
pub type SharedPerceptionCap = Arc<RwLock<Option<CachedPerceptionCap>>>;

/// Constructs an empty (cold-start) shared perception cap.
#[must_use]
pub fn empty_perception_cap() -> SharedPerceptionCap {
    Arc::new(RwLock::new(None))
}

/// The 3-state cap resolver (KIRRA-OCCY-PMON-002 §4) — the enabled-gate.
///
/// Pure function of `(enabled, cache, now_ms)` → the effective perception cap to
/// compose, or `None` for "no cap, no derate":
///
/// - **State 1 — NOT enabled** → `None`. The monitor is an *optional* layer;
///   its absence is NOT a fault and must not derate. This is what lets the
///   mechanism land as a pure no-op on main before any ingest exists.
/// - **State 2 — enabled + FRESH** (`now − generated_at ≤ ttl`) → `Some(cap_mps)`.
/// - **State 3 — enabled + STALE / `None` / poisoned RwLock** → `Some(MRC_FLOOR_CAP_MPS)`.
///   A configured monitor going silent IS a fault → controlled stop. Mirrors
///   `resolve_posture → LockedOut` exactly.
#[must_use]
pub fn resolve_perception_cap(
    enabled: bool,
    cache: &SharedPerceptionCap,
    now_ms: u64,
) -> Option<f64> {
    // State 1 — layer not deployed/enabled. No-op (NOT a fault).
    if !enabled {
        return None;
    }
    // Enabled: read O(1). Poison → fail closed (state 3).
    match cache.read() {
        Ok(guard) => match guard.as_ref() {
            // State 2 — fresh.
            Some(entry) if !entry.is_stale(now_ms) => Some(entry.cap_mps),
            // State 3 — stale or never-published.
            _ => Some(MRC_FLOOR_CAP_MPS),
        },
        // State 3 — poisoned lock.
        Err(_) => Some(MRC_FLOOR_CAP_MPS),
    }
}

/// Composition via call-site contract tightening (KIRRA-OCCY-PMON-002 §1, §6).
///
/// Returns a copy of `contract` whose `odd_speed_cap_mps` is tightened to
/// `min(existing_effective_cap, cap)` when `effective_cap` is `Some`; returns
/// the contract unchanged when `None`. Because
/// `effective_max_speed_mps() = min(max_speed_mps, odd_speed_cap_mps)`, the
/// result yields `min(max_speed, odd_cap, perception_cap)` — the ADR-0002
/// most-conservative-wins multi-input `min` — **without** touching
/// `validate_vehicle_command` or `effective_max_speed_mps`. The verdict fn
/// stays byte-identical; the only per-command cost is the O(1) resolver read at
/// the call site.
///
/// WHY call-site tightening (not threading an `Option<f64>` through
/// `validate_vehicle_command`): keeping the verdict fn + `effective_max_speed_mps`
/// byte-identical preserves their verified-unchanged status (WCET, MC/DC, the
/// whole P0..P6 pipeline) and confines this change to the composition layer.
#[must_use]
pub fn apply_perception_cap(
    contract: &VehicleKinematicsContract,
    effective_cap: Option<f64>,
) -> VehicleKinematicsContract {
    let mut out = *contract;
    if let Some(cap) = effective_cap {
        let existing = out.effective_max_speed_mps();
        out.odd_speed_cap_mps = Some(existing.min(cap));
    }
    out
}

/// Publishes perception-derate caps into a `SharedPerceptionCap` at tick rate.
/// Stateless w.r.t. its own history; mirrors the posture-engine worker's
/// "compute then atomically replace the cache" shape.
///
/// First cut is KINEMATIC-ONLY (`range_supported_derate` is STAGED — no `R_obs`
/// producer exists, #120 Item B). When the range guard lands, `on_tick` composes
/// `min(kinematic_cap, range_cap)` here before publishing.
pub struct PerceptionCapPublisher {
    cache: SharedPerceptionCap,
    contract: KinematicPlausibilityContract,
    ttl_ms: u64,
}

impl PerceptionCapPublisher {
    #[must_use]
    pub fn new(
        cache: SharedPerceptionCap,
        contract: KinematicPlausibilityContract,
        ttl_ms: u64,
    ) -> Self {
        Self { cache, contract, ttl_ms }
    }

    /// Run the kinematic guard over a fresh perception snapshot and publish the
    /// resulting cap. Called once per perception tick.
    pub fn on_tick(&self, perception: &PerceptionOutput, now_ms: u64) {
        let decision = kinematic_plausibility_derate(perception, &self.contract);
        self.publish(decision.cap_mps, decision.reason, now_ms);
    }

    /// Staleness sweep: if no fresh cap exists within the TTL, publish the
    /// MRC-floor cap (a configured monitor going silent IS a fault). Mirrors
    /// the telemetry watchdog's timeout sweep.
    pub fn sweep_staleness(&self, now_ms: u64) {
        let stale = match self.cache.read() {
            Ok(guard) => guard.as_ref().map(|e| e.is_stale(now_ms)).unwrap_or(true),
            Err(_) => true,
        };
        if stale {
            self.publish(MRC_FLOOR_CAP_MPS, DerateCode::PerceptionSnapshotUnhealthy, now_ms);
        }
    }

    fn publish(&self, cap_mps: f64, reason: DerateCode, now_ms: u64) {
        let entry = CachedPerceptionCap {
            cap_mps,
            generated_at_ms: now_ms,
            ttl_ms: self.ttl_ms,
            reason,
        };
        // Poison-tolerant write: if the lock is poisoned we still replace the
        // entry (the resolver fails closed on poison regardless).
        match self.cache.write() {
            Ok(mut guard) => *guard = Some(entry),
            Err(poisoned) => *poisoned.into_inner() = Some(entry),
        }
    }
}

// ===========================================================================
// Ingest helpers (KIRRA-OCCY-PMON-003 slice-1) — PURE, default-features-tested.
//
// These are the *safety-relevant* transforms of the perception ingest. They
// operate on KERNEL types + plain scalars only (NOT on any ROS / r2r /
// adapter type), so they compile and are unit-tested under default features
// (CI `Test`). The ROS2 adapter's `ros2`-gated wiring is a thin extractor that
// pulls `(id, pos, vel)` out of its message type and calls these — it holds no
// safety decision logic.
// ===========================================================================

/// Sentinel inter-frame Δt (seconds) used by [`tracked_object_from_parts`] so
/// the teleport (implied-speed) check is a **no-op** in slice-1 (D2a): with
/// `prev_pos_m == pos_m`, the implied speed `|pos − prev_pos| / dt = 0` for any
/// positive `dt`, so only the reported-velocity ceiling can fire. The real
/// inter-frame Δt arrives with the teleport check itself (D2b, deferred).
pub const TELEPORT_NOOP_DT_S: f64 = 1.0;

/// Build a kernel [`TrackedObject`] from ingest parts (KIRRA-OCCY-PMON-003 §3).
///
/// `vel_mps` is the object's **reported map-frame ground velocity vector**
/// (D2a: reported-velocity ceiling). `prev_pos_m` is set to `pos_m` and `dt_s`
/// to [`TELEPORT_NOOP_DT_S`], making the teleport check inert for slice-1.
///
/// Pure; no allocation; the caller (the ROS2 adapter shim) supplies already-
/// extracted scalars — no tracking/association happens here (ADR-0004).
#[must_use]
pub fn tracked_object_from_parts(id: u64, pos_m: Vec2, vel_mps: Vec2) -> TrackedObject {
    TrackedObject {
        id,
        pos_m,
        vel_mps,
        prev_pos_m: pos_m,
        dt_s: TELEPORT_NOOP_DT_S,
    }
}

/// Assemble a [`PerceptionOutput`] for the ingest path from a bounded slice of
/// shimmed objects (KIRRA-OCCY-PMON-003 §3).
///
/// Upstream-tracked objects are treated as trusted-present (`confidence = 1.0`,
/// `min_confidence = 0.0`), so the snapshot-confidence gate does not itself
/// derate; **object-stream staleness is enforced one layer up** by the cap's
/// `generated_at_ms`/`ttl_ms` + `resolve_perception_cap` (the freshness
/// authority for the ingest). The structural fail-closed gates that DO remain
/// active here are the per-object finite/`dt>0` checks inside the guard and the
/// `objects.len() <= MAX_TRACKED_OBJECTS` bound (over-cap → MRC floor).
#[must_use]
pub fn ingest_perception_output(objects: &[TrackedObject]) -> PerceptionOutput<'_> {
    PerceptionOutput {
        objects,
        confidence: 1.0,
        age_ms: 0,
        min_confidence: 0.0,
        max_age_ms: u64::MAX,
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
        assert_eq!(d.cap_mps, crate::kinematics_contract::URBAN_ODD_SPEED_CAP_MPS);
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

    // -----------------------------------------------------------------------
    // Composition (KIRRA-OCCY-PMON-002): resolver, apply_perception_cap,
    // end-to-end resolve→apply→validate, and the publisher worker.
    // -----------------------------------------------------------------------

    use crate::kinematics_contract::{
        validate_vehicle_command, EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
        URBAN_ODD_SPEED_CAP_MPS,
    };

    fn fresh_cap(cache: &SharedPerceptionCap, cap_mps: f64, now_ms: u64, ttl_ms: u64) {
        *cache.write().unwrap() = Some(CachedPerceptionCap {
            cap_mps,
            generated_at_ms: now_ms,
            ttl_ms,
            reason: DerateCode::ObjectVelocityImplausible,
        });
    }

    /// A steady-state command at `v` m/s (no accel/steering corrections), so the
    /// only thing that can act is the P2 velocity ceiling.
    fn cmd_at(v: f64) -> ProposedVehicleCommand {
        ProposedVehicleCommand {
            linear_velocity_mps: v,
            current_velocity_mps: v,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        }
    }

    // ---- 3-state resolver ----

    #[test]
    fn resolver_state1_not_enabled_is_none() {
        let cache = empty_perception_cap();
        fresh_cap(&cache, 5.0, 1000, 500); // even with a fresh cap present…
        assert_eq!(resolve_perception_cap(false, &cache, 1100), None, "disabled → no cap");
    }

    #[test]
    fn resolver_state2_enabled_fresh_returns_cap() {
        let cache = empty_perception_cap();
        fresh_cap(&cache, 7.5, 1000, 500);
        assert_eq!(resolve_perception_cap(true, &cache, 1200), Some(7.5));
    }

    #[test]
    fn resolver_state3_enabled_stale_is_mrc_floor() {
        let cache = empty_perception_cap();
        fresh_cap(&cache, 7.5, 1000, 500);
        // now - generated = 600 > ttl 500 → stale → MRC floor.
        assert_eq!(resolve_perception_cap(true, &cache, 1600), Some(MRC_FLOOR_CAP_MPS));
    }

    #[test]
    fn resolver_state3_enabled_none_is_mrc_floor() {
        let cache = empty_perception_cap(); // never published
        assert_eq!(resolve_perception_cap(true, &cache, 1000), Some(MRC_FLOOR_CAP_MPS));
    }

    // ---- apply_perception_cap ----

    #[test]
    fn apply_none_returns_unchanged_contract() {
        let base = VehicleKinematicsContract::nominal_reference_profile();
        let out = apply_perception_cap(&base, None);
        assert_eq!(out.effective_max_speed_mps(), base.effective_max_speed_mps());
        assert_eq!(out.odd_speed_cap_mps, base.odd_speed_cap_mps);
    }

    #[test]
    fn apply_some_tightens_to_min() {
        let base = VehicleKinematicsContract::nominal_reference_profile(); // max 35, no odd cap
        // Perception cap 12 < 35 → effective becomes 12.
        let out = apply_perception_cap(&base, Some(12.0));
        assert_eq!(out.effective_max_speed_mps(), 12.0);
        // A looser perception cap never raises the ceiling.
        let out2 = apply_perception_cap(&base, Some(100.0));
        assert_eq!(out2.effective_max_speed_mps(), base.max_speed_mps);
    }

    #[test]
    fn apply_composes_with_existing_odd_cap_most_conservative_wins() {
        let mut base = VehicleKinematicsContract::nominal_reference_profile();
        base.odd_speed_cap_mps = Some(URBAN_ODD_SPEED_CAP_MPS); // 22.35
        // Perception cap 10 < 22.35 → 10 wins.
        assert_eq!(apply_perception_cap(&base, Some(10.0)).effective_max_speed_mps(), 10.0);
        // Perception cap 30 > 22.35 → existing odd cap still wins.
        assert!(
            (apply_perception_cap(&base, Some(30.0)).effective_max_speed_mps() - URBAN_ODD_SPEED_CAP_MPS).abs() < 1e-9
        );
    }

    #[test]
    fn apply_zero_floor_stops() {
        let base = VehicleKinematicsContract::nominal_reference_profile();
        let out = apply_perception_cap(&base, Some(MRC_FLOOR_CAP_MPS));
        assert_eq!(out.effective_max_speed_mps(), 0.0);
    }

    // ---- end-to-end: resolve → apply → validate (the composition outcome) ----

    #[test]
    fn compose_state1_identical_to_baseline() {
        let cache = empty_perception_cap();
        let base = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = cmd_at(20.0); // under the 35 m/s vehicle max → baseline Allow
        let baseline = validate_vehicle_command(&cmd, &base);

        let eff = resolve_perception_cap(false, &cache, 1000); // disabled
        let composed = validate_vehicle_command(&cmd, &apply_perception_cap(&base, eff));
        assert_eq!(composed, baseline);
        assert_eq!(composed, EnforceAction::Allow);
    }

    #[test]
    fn compose_state2_cap_below_command_clamps() {
        let cache = empty_perception_cap();
        fresh_cap(&cache, 8.0, 1000, 500);
        let base = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = cmd_at(20.0); // 20 > published cap 8 → clamp to 8

        let eff = resolve_perception_cap(true, &cache, 1200);
        let composed = validate_vehicle_command(&cmd, &apply_perception_cap(&base, eff));
        assert_eq!(composed, EnforceAction::ClampLinear(8.0));
    }

    #[test]
    fn compose_state2_cap_above_command_allows() {
        let cache = empty_perception_cap();
        fresh_cap(&cache, 25.0, 1000, 500);
        let base = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = cmd_at(20.0); // 20 < published cap 25 → Allow

        let eff = resolve_perception_cap(true, &cache, 1200);
        let composed = validate_vehicle_command(&cmd, &apply_perception_cap(&base, eff));
        assert_eq!(composed, EnforceAction::Allow);
    }

    #[test]
    fn compose_state3_stale_is_controlled_stop() {
        let cache = empty_perception_cap();
        fresh_cap(&cache, 25.0, 1000, 500);
        let base = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = cmd_at(20.0);

        // Stale (now - gen = 600 > ttl 500) → MRC floor → ClampLinear(0.0).
        let eff = resolve_perception_cap(true, &cache, 1600);
        let composed = validate_vehicle_command(&cmd, &apply_perception_cap(&base, eff));
        assert_eq!(composed, EnforceAction::ClampLinear(0.0));
    }

    // ---- publisher worker (synthetic TrackedObjects → published cap) ----

    fn ok_obj(id: u64) -> TrackedObject {
        TrackedObject {
            id,
            pos_m: Vec2 { x: 10.0, y: 0.0 },
            vel_mps: Vec2 { x: 5.0, y: 0.0 },
            prev_pos_m: Vec2 { x: 9.5, y: 0.0 },
            dt_s: 0.1,
        }
    }

    #[test]
    fn worker_on_tick_publishes_nominal_cap_for_clean_snapshot() {
        let cache = empty_perception_cap();
        let pubr = PerceptionCapPublisher::new(
            cache.clone(),
            KinematicPlausibilityContract::urban_reference(),
            500,
        );
        let objs = [ok_obj(1), ok_obj(2)];
        let p = PerceptionOutput {
            objects: &objs,
            confidence: 0.95,
            age_ms: 10,
            min_confidence: 0.5,
            max_age_ms: 500,
        };
        pubr.on_tick(&p, 1000);
        // Enabled+fresh read returns the nominal cap (no implausible objects).
        assert_eq!(
            resolve_perception_cap(true, &cache, 1100),
            Some(URBAN_ODD_SPEED_CAP_MPS)
        );
    }

    #[test]
    fn worker_on_tick_publishes_mrc_floor_for_implausible_snapshot() {
        let cache = empty_perception_cap();
        let pubr = PerceptionCapPublisher::new(
            cache.clone(),
            KinematicPlausibilityContract::urban_reference(),
            500,
        );
        // A non-finite object → structural failure → MRC floor (0.0).
        let mut bad = ok_obj(1);
        bad.vel_mps = Vec2 { x: f64::NAN, y: 0.0 };
        let objs = [bad];
        let p = PerceptionOutput {
            objects: &objs,
            confidence: 0.95,
            age_ms: 10,
            min_confidence: 0.5,
            max_age_ms: 500,
        };
        pubr.on_tick(&p, 1000);
        assert_eq!(resolve_perception_cap(true, &cache, 1100), Some(0.0));
    }

    #[test]
    fn worker_sweep_publishes_mrc_floor_when_no_tick() {
        let cache = empty_perception_cap(); // never ticked
        let pubr = PerceptionCapPublisher::new(
            cache.clone(),
            KinematicPlausibilityContract::urban_reference(),
            500,
        );
        pubr.sweep_staleness(2000);
        // Sweep published an MRC-floor cap; a fresh read now sees 0.0.
        let entry = cache.read().unwrap().unwrap();
        assert_eq!(entry.cap_mps, 0.0);
        assert_eq!(resolve_perception_cap(true, &cache, 2050), Some(0.0));
    }

    #[test]
    fn worker_sweep_leaves_fresh_cap_untouched() {
        let cache = empty_perception_cap();
        fresh_cap(&cache, 9.0, 2000, 500);
        let pubr = PerceptionCapPublisher::new(
            cache.clone(),
            KinematicPlausibilityContract::urban_reference(),
            500,
        );
        pubr.sweep_staleness(2100); // within TTL → no-op
        assert_eq!(resolve_perception_cap(true, &cache, 2100), Some(9.0));
    }

    // -----------------------------------------------------------------------
    // Ingest helpers (KIRRA-OCCY-PMON-003 slice-1): tracked_object_from_parts
    // + ingest_perception_output. Pure, default-features (CI `Test`).
    // -----------------------------------------------------------------------

    #[test]
    fn shim_from_parts_carries_vector_and_neutralizes_teleport() {
        let obj = tracked_object_from_parts(7, Vec2 { x: 10.0, y: 2.0 }, Vec2 { x: 3.0, y: 4.0 });
        assert_eq!(obj.id, 7);
        assert_eq!(obj.pos_m, Vec2 { x: 10.0, y: 2.0 });
        assert_eq!(obj.vel_mps, Vec2 { x: 3.0, y: 4.0 }); // vector preserved
        // D2a: prev == pos and dt > 0 → implied speed is exactly 0 (teleport no-op).
        assert_eq!(obj.prev_pos_m, obj.pos_m);
        assert_eq!(obj.dt_s, TELEPORT_NOOP_DT_S);
        assert!(obj.dt_s > 0.0);
        let implied = obj.pos_m.dist_to(&obj.prev_pos_m) / obj.dt_s;
        assert_eq!(implied, 0.0, "teleport implied-speed must be 0 in slice-1");
    }

    #[test]
    fn shim_object_only_velocity_ceiling_can_fire() {
        // |vel| = 5 (3,4) < 60 → plausible; the teleport check is inert.
        let ok = tracked_object_from_parts(1, Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 3.0, y: 4.0 });
        // |vel| = 65 > 60 → velocity ceiling trips (teleport still inert).
        let bad = tracked_object_from_parts(2, Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 65.0, y: 0.0 });
        let objs = [ok, bad];
        let p = ingest_perception_output(&objs);
        let d = kinematic_plausibility_derate(&p, &KinematicPlausibilityContract::urban_reference());
        assert_eq!(d.reason, DerateCode::ObjectVelocityImplausible);
    }

    #[test]
    fn ingest_output_passes_health_gate_and_keeps_count_bound() {
        let objs: Vec<TrackedObject> = (0..3)
            .map(|i| tracked_object_from_parts(i, Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 1.0, y: 0.0 }))
            .collect();
        assert!(ingest_perception_output(&objs).is_healthy());
        // Over MAX_TRACKED_OBJECTS → unhealthy → guard returns MRC floor.
        let too_many: Vec<TrackedObject> = (0..(MAX_TRACKED_OBJECTS as u64 + 1))
            .map(|i| tracked_object_from_parts(i, Vec2 { x: 0.0, y: 0.0 }, Vec2 { x: 1.0, y: 0.0 }))
            .collect();
        let p = ingest_perception_output(&too_many);
        assert!(!p.is_healthy());
        let d = kinematic_plausibility_derate(&p, &KinematicPlausibilityContract::urban_reference());
        assert_eq!(d.reason, DerateCode::PerceptionSnapshotUnhealthy);
        assert_eq!(d.cap_mps, 0.0);
    }
}
