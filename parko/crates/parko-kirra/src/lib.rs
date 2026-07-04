// crates/parko-kirra/src/lib.rs
//
// Adapter from parko-core's SafetyGovernor trait to the
// kirra-runtime-sdk vehicle kinematics contract.
//
// AXIS COVERAGE:
//
// parko's ControlCommand uses a differential-drive Twist model
// (linear_velocity, angular_velocity in m/s and rad/s respectively).
// Kirra's ProposedVehicleCommand uses a bicycle/Ackermann model
// (linear_velocity_mps, steering_angle_deg) — semantically different
// control representations.
//
// (clippy doc-list lints are allowed below: `angular_bound.rs` carries
// column-aligned ASCII parameter-derivation tables in its doc-comments that the
// markdown-nesting lints would misalign.)
#![allow(clippy::doc_lazy_continuation, clippy::doc_overindented_list_items)]
//
//   - Linear axis is bridged through Kirra's vehicle kinematics contract
//     (acceleration, deceleration, lateral-accel; see
//     `validate_vehicle_command`).
//   - Angular (yaw-rate) axis is bounded NATIVELY here against
//     `max_angular_velocity_rad_s` — appropriate for differential drive
//     where in-place rotation has no bicycle-steering equivalent and
//     uncapped spin can tip the platform or sweep into a person (H1).
//
// The result is the most-restrictive verdict across the two axes:
//   linear → ClampLinear,  angular → ClampAngularVelocity,
//   both   → ClampMotion { linear: Some, angular: Some },
//   either deny → Deny.
//
// CAVEAT — bound value derivation:
//   The angular-velocity bound is SOTIF-derived as of #136 (see
//   `angular_bound::AngularVelocityBound`, `PlatformParams`, and
//   `docs/safety/ANGULAR_VELOCITY_SOTIF.md`). The H1 placeholder
//   constants are gone. The new bound is `ω_max(v) = min(rollover(v),
//   sweep, ftti)`. **Status: DRAFT — pending formal safety-engineer
//   review.**

// The kinematics-contract talisman + FleetPosture now live in the lean `kirra-core`
// crate (de-monolith Stages 1/3/5) — same types, no heavy verifier-service tree pulled
// for them. (parko-kirra keeps `kirra-runtime-sdk` for the SQLite `VerifierStore` it
// persists clearances/audit through — see audit_sink / clearance_delivery.)
use kirra_core::kinematics_contract::{
    validate_vehicle_command, EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
    STOP_EPSILON_MPS,
};
// `DenyCode` is reached via `EnforceAction::DenyBreach` — see the Nominal branch below.
use kirra_core::FleetPosture;

use parko_core::commands::ControlCommand;
use parko_core::rss::{
    lateral_safe_distance, longitudinal_safe_distance, occlusion_limited_speed,
    opposite_direction_safe_distance, RSS_LATERAL_MOTION_EPS_MPS, RSS_LONGITUDINAL_CONFLICT_M,
    RSS_LONGITUDINAL_OVERLAP_M,
};
use kirra_core::frame_integrity::{resolve_frame_trust, FrameIntegrity, FrameIntegrityCfg, FrameTrust};
use parko_core::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};
use parko_core::{
    commit_zone_blocked, gate_commit_zone_scene, gate_water_scene, water_untraversable_veto,
    AgentScene, ClearanceLoop, CommitZoneCfg, CommitZoneScene, ImpactEvidence, ImpactLatch,
    OcclusionScene, RssParams, RssState, VanishedCfg, VanishedObjectDetector, WaterScene,
    WaterVetoConfig, MAX_RSS_AGENTS,
};

pub mod angular_bound;
// R5: the verifier-backed durable sinks are opt-in (feature `verifier-sink`); the
// governor/comparator core below depends only on `kirra-core`.
#[cfg(feature = "verifier-sink")]
pub mod audit_sink;
#[cfg(feature = "verifier-sink")]
pub mod clearance_delivery;
pub mod comparator;
pub mod diverse;
pub mod platform;
pub use angular_bound::{AngularVelocityBound, PlatformParams, ROLLOVER_MIN_LINEAR_VELOCITY_MPS};
#[cfg(feature = "verifier-sink")]
pub use clearance_delivery::{ClearanceDelivery, DeliveryOutcome};
#[cfg(feature = "verifier-sink")]
pub use audit_sink::{
    select_audit_client, select_divergence_sink, select_impact_sink, AuditChainLinkerAuditClient,
    AuditChainLinkerDivergenceSink, FatalAuditConfig, ImpactAuditSink,
    ImpactClearanceRejectedPayload, ImpactClearedPayload, ImpactDetectedPayload,
    ImpactEscalationPayload, ImpactEventSink, InMemoryImpactSink, RecordedClearanceLoop,
    RecordedImpactLatch, IMPACT_CLEARANCE_REJECTED_EVENT_TYPE, IMPACT_CLEARED_EVENT_TYPE,
    IMPACT_DETECTED_EVENT_TYPE, IMPACT_ESCALATION_RAISED_EVENT_TYPE, PARKO_DECISION_EVENT_TYPE,
    PARKO_FAULT_EVENT_TYPE, PARKO_HEALTH_EVENT_TYPE, PARKO_OVERRIDE_EVENT_TYPE,
};
pub use comparator::{GovernorComparator, RssAwareGovernor};
pub use diverse::DiverseKirraGovernor;

/// MRC (Minimum Risk Condition) velocity ceiling — the decel-trajectory
/// bound applied to a *decelerating* command under Degraded / RSS-unsafe.
/// NOT a crawl set-point: as of Issue #70, Degraded is decel-to-stop-and-HOLD
/// (see `apply_mrc_profile`), so this ceiling bounds how fast a still-moving,
/// converging-to-zero command may be — it never licenses re-initiation or a
/// speed increase. NOT applied to LockedOut — LockedOut is a hard stop (0.0).
/// Single source of truth. Per ADL-001.
pub const MRC_VELOCITY_CEILING_MPS: f64 = 5.0;

/// Angular-velocity magnitude (rad/s) at or below which the platform is
/// treated as **not rotating** for the Issue #70 Degraded no-re-initiation
/// invariant on the angular channel (≈ 1.1 deg/s — above gyro noise, below
/// any deliberate turn). The angular analogue of `STOP_EPSILON_MPS`.
///
/// JUDGMENT-CALL (Issue #70 FLAG #2): differential-drive platforms have an
/// independent angular-velocity actuator, so the converge-to-zero /
/// no-re-initiation rule is applied to the angular channel natively here
/// (unlike the Ackermann bicycle model, where yaw rate `ω = v·tan δ/L`
/// vanishes with linear speed and needs no separate angular gate).
pub const STOP_EPSILON_RAD_S: f64 = 0.02;

/// Issue #70 Degraded single-channel gate: returns `Some(reason)` when a
/// proposed value on one motion channel (linear or angular) violates the
/// decel-to-stop-and-HOLD invariant relative to the current value, else
/// `None`. The `reason` tokens match the Kirra-SDK `DenyCode` audit strings
/// (`DEGRADED_REINITIATION_DENIED` / `DEGRADED_SPEED_INCREASE_DENIED`) so the
/// cross-crate deny vocabulary stays identical.
///
/// `current`/`proposed` are signed; `eps` is the channel's stop floor.
/// Fails closed on non-finite input.
fn degraded_channel_violation(current: f64, proposed: f64, eps: f64) -> Option<&'static str> {
    if !proposed.is_finite() || !current.is_finite() {
        return Some("DEGRADED_REINITIATION_DENIED");
    }
    let cur_mag = current.abs();
    let prop_mag = proposed.abs();
    // (c) no re-initiation from a stop / hold.
    if cur_mag <= eps && prop_mag > eps {
        return Some("DEGRADED_REINITIATION_DENIED");
    }
    // (c') no direction reversal through a stop while moving.
    if proposed.signum() != current.signum() && cur_mag > eps && prop_mag > eps {
        return Some("DEGRADED_REINITIATION_DENIED");
    }
    // (b) non-increasing magnitude.
    if prop_mag > cur_mag + 1e-9 {
        return Some("DEGRADED_SPEED_INCREASE_DENIED");
    }
    None
}

// Angular-velocity bound — SOTIF-derived (issue #136).
//
// The H1 placeholder constants (`MAX_ANGULAR_VELOCITY_RAD_S_PLACEHOLDER = 1.5`,
// `MRC_ANGULAR_VELOCITY_CEILING_RAD_S = 0.5`) are removed. The bound is now
// computed by `crate::angular_bound::AngularVelocityBound::omega_max(v)` from
// platform parameters (`PlatformParams`):
//
//   ω_max(v) = min(rollover(v), sweep, ftti)
//
// with rollover masked below `ROLLOVER_MIN_LINEAR_VELOCITY_MPS` to handle the
// v=0 singularity. See `crate::angular_bound` and
// `docs/safety/ANGULAR_VELOCITY_SOTIF.md` for the derivation, assumptions, and
// worked reference numbers.
//
// Status: DRAFT — pending formal safety-engineer review. The improvement over
// the H1 placeholders is real (reasoning + defensible values where there were
// none), but treating these numbers as a validated safety claim requires
// sign-off.

/// CHECKER-OVER-DOER pairwise RSS evaluation (issue #92).
///
/// The trusted checker (this crate) computes the worst-case RSS verdict over
/// the agent set ITSELF, via the vetted parko-core safe-distance primitives —
/// it never trusts a `safe: bool` pushed by an upstream doer. For each agent it
/// computes the required longitudinal AND lateral safe-distance and compares to
/// the agent's ACTUAL measured gap / separation; the pair is unsafe if actual <
/// required on either axis. The scene verdict is the WORST case: `safe` only if
/// every pair is safe.
///
/// Fail-closed handling:
///   - `Absent` (no perception update) → UNSAFE. "No agents" is not "clear".
///   - `KnownEmpty` (perception ran, clear) → safe.
///   - more than `MAX_RSS_AGENTS`, or an empty `Agents` vector → UNSAFE.
///   - A non-finite agent is NOT skipped: the parko-core primitive returns
///     `RSS_FAILSAFE_DISTANCE_M` (1e6 m, an unreachable separation), so
///     `actual < required` holds and the pair is unsafe. A non-finite ACTUAL
///     gap likewise makes the `>=` comparison false → unsafe.
///
/// Margins on the returned [`RssState`] are the worst (minimum actual−required)
/// across all pairs; `KnownEmpty` reports `f64::MAX` margins (the no-threat
/// convention used by `KirraGovernor::new`).
pub fn compute_scene_rss(scene: &AgentScene, params: &RssParams) -> RssState {
    // Fail-closed verdict reused for Absent / over-cap / empty-set.
    let unsafe_state = || RssState {
        safe: false,
        longitudinal_margin: 0.0,
        lateral_margin: 0.0,
    };

    let agents = match scene {
        AgentScene::Absent => return unsafe_state(),
        AgentScene::KnownEmpty => {
            return RssState {
                safe: true,
                longitudinal_margin: f64::MAX,
                lateral_margin: f64::MAX,
            }
        }
        AgentScene::Agents(agents) => agents,
    };

    // Bounded WCET: a scene larger than the cap is fail-closed, not truncated.
    // An empty `Agents` vector is ambiguous vs `KnownEmpty` → fail-closed.
    if agents.is_empty() || agents.len() > MAX_RSS_AGENTS {
        return unsafe_state();
    }

    let mut all_safe = true;
    let mut min_long_margin = f64::INFINITY;
    let mut min_lat_margin = f64::INFINITY;

    for a in agents {
        // ONCOMING agents take the head-on bound (sum of both closing stopping
        // distances); a same-direction lead takes the classic primitive. Routing
        // an oncoming actor through the same-direction primitive would discard the
        // closing sign and UNDER-estimate the gap (the #408 Obs 3 hazard) — the
        // directionality the lane graph supplies is what prevents that here.
        let required_long = if a.oncoming {
            // Both speeds are closing magnitudes. Symmetric brake_min: the ego is
            // in its correct lane (brakes at brake_min) and the oncoming actor is
            // assumed to brake no harder than brake_min too (conservative). The
            // wrong-lane overtake asymmetry is the deferred maneuver, not this path.
            opposite_direction_safe_distance(
                a.ego_vel.abs(),
                a.lead_vel.abs(),
                params.reaction_time,
                params.accel_max,
                params.brake_min,
                params.brake_min,
            )
        } else {
            longitudinal_safe_distance(
                a.ego_vel,
                a.lead_vel,
                params.reaction_time,
                params.accel_max,
                params.brake_min,
                params.brake_max,
            )
        };
        let required_lat = lateral_safe_distance(
            a.ego_lat_vel,
            a.obj_lat_vel,
            params.lat_accel_max,
            params.reaction_time,
        );

        // NaN-safe: a non-finite ACTUAL gap makes `>=` false (NaN comparisons
        // are false); a non-finite agent VELOCITY drove `required_*` to the
        // 1e6 failsafe, so a realistic finite actual gap is `< required` → the
        // pair is unsafe. Either way the agent is evaluated, never skipped.
        //
        // RSS danger is the longitudinal∧lateral conjunction; each axis bounds
        // safety only when the OTHER axis is in conflict, so each is gated on the
        // other's proximity (§4). Each gate is fail-closed FIRST on its own
        // non-finite input (a NaN must never read as "safe").
        //
        // Longitudinal (rear-end / head-on) bounds safety only when the footprints
        // laterally OVERLAP — an object the ego is laterally clear of (passed, or
        // oncoming in the next lane) cannot be a longitudinal collision.
        let lon_safe = if !a.actual_longitudinal_gap_m.is_finite() {
            false
        } else if a.actual_lateral_separation_m.is_finite()
            && a.actual_lateral_separation_m >= RSS_LONGITUDINAL_OVERLAP_M
        {
            true
        } else {
            a.actual_longitudinal_gap_m >= required_long
        };
        // Lateral defence-in-depth bounds safety only when the object is also
        // longitudinally CLOSE — a lead well ahead / oncoming traffic safely
        // passing in the next lane is longitudinally distant, so its lateral gap
        // does not bound safety (it over-rejected before — §4). And WITHIN that
        // proximity it bounds safety only when a side collision is actually possible:
        // the pair ABREAST (longitudinally unsafe) OR closing laterally (a cut-in —
        // either actor has lateral velocity). A longitudinally-SAFE, laterally-
        // STATIONARY object — a stopped queue member, or a stopped lead the ego halts
        // behind — is neither, so it is admitted instead of spuriously vetoed by the
        // reaction-time swerve term in `lateral_safe_distance` (the §4 over-rejection of
        // a safe same-lane stop). This strictly NARROWS the lateral veto (an added
        // precondition), so it can only admit longitudinally-safe + laterally-still pairs
        // — never a state with lateral motion or abreast danger. Mirrors the kirra-core
        // checker's RSS-conjunction fix.
        let lat_safe = if !a.actual_lateral_separation_m.is_finite() {
            false
        // #683/#684: the lateral-conflict longitudinal window is the FLOOR
        // `RSS_LONGITUDINAL_CONFLICT_M` scaled up by closing speed via `required_long`
        // (the diverse-governor analog of the trajectory checker's `lon_required`). A
        // FIXED 8 m ceiling deemed a high-speed cut-in originating farther ahead than
        // 8 m "laterally safe" before `required_lat` was consulted — at the 22.35 m/s
        // ODD cap reaction-time travel alone is ~11 m. No over-rejection: the
        // `(lon_unsafe || lateral_cut_in)` precondition still admits a longitudinally-
        // safe, laterally-still object inside the wider window.
        } else if a.actual_longitudinal_gap_m > RSS_LONGITUDINAL_CONFLICT_M.max(required_long) {
            true
        } else {
            let lon_unsafe = a.actual_longitudinal_gap_m < required_long;
            let lateral_cut_in = a.obj_lat_vel.abs() > RSS_LATERAL_MOTION_EPS_MPS
                || a.ego_lat_vel.abs() > RSS_LATERAL_MOTION_EPS_MPS;
            if lon_unsafe || lateral_cut_in {
                a.actual_lateral_separation_m >= required_lat
            } else {
                true // longitudinally-safe + laterally-stationary → no side collision possible
            }
        };
        let pair_safe = lon_safe && lat_safe;
        if !pair_safe {
            all_safe = false;
        }

        let long_margin = a.actual_longitudinal_gap_m - required_long;
        let lat_margin = a.actual_lateral_separation_m - required_lat;
        // `min` propagates NaN-safely here: a NaN margin (from a NaN actual)
        // is replaced by the running minimum, but `all_safe` is already false.
        if long_margin < min_long_margin {
            min_long_margin = long_margin;
        }
        if lat_margin < min_lat_margin {
            min_lat_margin = lat_margin;
        }
    }

    RssState {
        safe: all_safe,
        longitudinal_margin: min_long_margin,
        lateral_margin: min_lat_margin,
    }
}

/// CHECKER-OVER-DOER occlusion speed cap — RSS rule iv (issue #122).
///
/// The trusted checker computes the maximum admissible ego speed from the
/// sightline scene ITSELF, via the vetted parko-core primitive
/// [`occlusion_limited_speed`] — it never trusts a caller-pushed verdict.
///
/// Fail-closed scene handling mirrors [`compute_scene_rss`]'s ABSENT-vs-KNOWN
/// distinction on the occlusion axis:
///   - `Absent` (no sightline assessment) → `0.0`: worst-case occlusion, the ego
///     must stop. ABSENT is NOT `KnownClear`.
///   - `KnownClear` (sightline verified clear) → `f64::INFINITY`: rule iv does
///     not bind.
///   - `Limited { d_sight_m, v_emerge_max_mps }` → `occlusion_limited_speed`,
///     which itself fails closed to `0.0` on any invalid input.
///
/// The returned value is a MAX EGO SPEED (m/s); the governor treats a proposed
/// speed above it as RSS-unsafe (override → MRC profile).
pub fn compute_occlusion_cap(occlusion: &OcclusionScene, params: &RssParams) -> f64 {
    match *occlusion {
        OcclusionScene::Absent => 0.0,
        OcclusionScene::KnownClear => f64::INFINITY,
        OcclusionScene::Limited {
            d_sight_m,
            v_emerge_max_mps,
        } => occlusion_limited_speed(
            d_sight_m,
            v_emerge_max_mps,
            params.reaction_time,
            params.accel_max,
            params.brake_min,
            params.brake_max,
        ),
    }
}

/// Where the pushed-state RSS verdict for `evaluate()` comes from — and,
/// critically, what "nothing was ever pushed" means.
///
/// **Fail-closed default (#G2 / WS-0.1):** a governor that has NEVER been fed
/// an RSS verdict must not assert one. The prior implementation seeded
/// `rss_state` with `safe: true`, so a call site that forgot (or didn't know)
/// to feed the governor got a silently fail-open RSS tier — the exact
/// "accept by default" class the doer-checker invariants forbid. `NeverFed`
/// gates as UNSAFE (→ the MRC decel-to-stop-and-HOLD profile: no motion from
/// standstill), so an unfed governor is immobile, not permissive.
///
/// The two ways OUT of `NeverFed` are both explicit and greppable:
///   - [`KirraGovernor::update_rss_state`] → `Fed(state)`: a control loop
///     pushes a per-cycle verdict (the comparator/shadow + test path).
///   - [`KirraGovernor::with_external_rss_gate`] → `ExternallyGated`: the
///     integrator DECLARES that the scene-RSS verdict is enforced OUTSIDE this
///     governor instance — e.g. parko-ros2's publication-seam
///     `apply_object_rss_gate` (ADR-0029 Phase 3b), which bounds the exact
///     twist about to be published — or that the operator has explicitly
///     accepted motion without a scene-RSS producer (logged loudly by the
///     node). The RSS tier inside `evaluate()` is then intentionally
///     quiescent; every OTHER tier (non-finite reject, LockedOut, Degraded
///     MRC, kinematic envelope, angular bound) still enforces.
///
/// The authoritative `evaluate_scene*` family bypasses this state entirely —
/// it computes the verdict from the scene per call and never trusts a pushed
/// value.
#[derive(Debug, Clone)]
pub enum RssFeed {
    /// No RSS verdict has ever been supplied. Gates as UNSAFE (fail-closed).
    NeverFed,
    /// The most recent verdict pushed via `update_rss_state`.
    Fed(RssState),
    /// The integrator explicitly declared that scene-RSS enforcement happens
    /// outside this governor (publication-seam gate) or was explicitly
    /// waived. The pushed-state RSS tier does not gate.
    ExternallyGated,
}

impl RssFeed {
    /// The RSS-safe verdict this feed contributes to the three-tier gate.
    fn rss_safe(&self) -> bool {
        match self {
            RssFeed::NeverFed => false,
            RssFeed::Fed(state) => state.safe,
            RssFeed::ExternallyGated => true,
        }
    }
}

/// A safety governor backed by the Kirra runtime SDK's vehicle kinematics
/// contract.
///
/// Holds both nominal and MRC fallback contract profiles and selects
/// between them per-call based on the posture passed to `evaluate()`.
pub struct KirraGovernor {
    nominal_contract: VehicleKinematicsContract,
    #[allow(dead_code)]
    fallback_contract: VehicleKinematicsContract,
    rss_feed: RssFeed,
    /// SOTIF-derived angular-velocity bound for the Nominal posture.
    /// Default = `AngularVelocityBound::nominal(PlatformParams::conservative_default())`.
    /// Override per platform via `with_platform_params` or
    /// `with_angular_bounds`.
    nominal_angular_bound: AngularVelocityBound,
    /// SOTIF-derived angular-velocity bound for the MRC (Degraded /
    /// RSS-unsafe) posture. Default = `AngularVelocityBound::mrc(...)`
    /// with `mrc_posture_factor = 0.5`. Override per platform similarly.
    mrc_angular_bound: AngularVelocityBound,
}

impl Default for KirraGovernor {
    fn default() -> Self {
        Self::new()
    }
}

impl KirraGovernor {
    /// Construct a governor that holds both nominal and MRC fallback
    /// contract profiles and selects between them per-call based on
    /// the posture passed to `evaluate()`.
    ///
    /// Angular-velocity bounds default to the SOTIF-derived bound
    /// from `PlatformParams::conservative_default()` — produces a
    /// tight bound that fails toward safe for an uncharacterised
    /// platform. Override with `with_platform_params(params)` to
    /// pass platform-specific geometry + FTTI, or
    /// `with_angular_bounds(nom, mrc)` for a direct scalar override.
    /// See `crate::angular_bound` for the derivation;
    /// `docs/safety/ANGULAR_VELOCITY_SOTIF.md` is the safety case
    /// (DRAFT — pending formal safety-engineer review).
    /// **Fail-closed RSS default:** the constructed governor starts
    /// [`RssFeed::NeverFed`] — its pushed-state RSS tier gates as UNSAFE (MRC
    /// decel-to-stop-and-HOLD; no motion from standstill) until either a
    /// verdict is fed (`update_rss_state`) or external gating is explicitly
    /// declared (`with_external_rss_gate`). See [`RssFeed`].
    pub fn new() -> Self {
        Self {
            nominal_contract: VehicleKinematicsContract::nominal_reference_profile(),
            fallback_contract: VehicleKinematicsContract::mrc_fallback_profile(),
            rss_feed: RssFeed::NeverFed,
            nominal_angular_bound: AngularVelocityBound::nominal(PlatformParams::conservative_default()),
            mrc_angular_bound:     AngularVelocityBound::mrc    (PlatformParams::conservative_default()),
        }
    }

    /// Overrides the angular-velocity bounds with **v-independent
    /// scalar** values. Use this when the SOTIF derivation has already
    /// been done elsewhere and you want a single number per posture.
    /// For per-platform v-dependent bounds (rollover, sweep, FTTI),
    /// use `with_platform_params` instead.
    ///
    /// Panics if either bound is non-finite or non-positive.
    pub fn with_angular_bounds(mut self, nominal_rad_s: f64, mrc_rad_s: f64) -> Self {
        assert!(
            nominal_rad_s.is_finite() && nominal_rad_s > 0.0,
            "nominal angular bound must be a finite positive value, got {}",
            nominal_rad_s
        );
        assert!(
            mrc_rad_s.is_finite() && mrc_rad_s > 0.0,
            "MRC angular bound must be a finite positive value, got {}",
            mrc_rad_s
        );
        self.nominal_angular_bound = AngularVelocityBound::Scalar(nominal_rad_s);
        self.mrc_angular_bound     = AngularVelocityBound::Scalar(mrc_rad_s);
        self
    }

    /// **Issue #136 SOTIF** — overrides the angular-velocity bounds
    /// with platform-parameter-driven `ω_max(v) = min(rollover(v),
    /// sweep, ftti)` derivations. The Nominal bound uses
    /// `posture_factor = 1.0`; the MRC bound uses
    /// `params.mrc_posture_factor` (default 0.5).
    ///
    /// See `crate::angular_bound::PlatformParams` for the field
    /// catalog and `docs/safety/ANGULAR_VELOCITY_SOTIF.md` for the
    /// derivation. DRAFT — pending formal safety-engineer review.
    pub fn with_platform_params(mut self, params: PlatformParams) -> Self {
        params.validate().expect(
            "PlatformParams failed validation; check geometry > 0 and \
             mrc_posture_factor in (0, 1]"
        );
        self.nominal_angular_bound = AngularVelocityBound::nominal(params.clone());
        self.mrc_angular_bound     = AngularVelocityBound::mrc(params);
        self
    }

    /// Sets the per-ODD operational speed cap on the nominal contract. The
    /// effective nominal ceiling then becomes `min(max_speed_mps, cap)` via
    /// `VehicleKinematicsContract::effective_max_speed_mps`. Mirrors
    /// `DiverseKirraGovernor::with_odd_speed_cap` so a `GovernorComparator`
    /// can pair the two on the SAME ODD cap and they agree by construction;
    /// without it the cap could not be configured and the diverse governor's
    /// ODD-cap arm was unreachable (quality-hardening finding).
    ///
    /// Panics if `cap_mps` is non-finite or non-positive.
    pub fn with_odd_speed_cap(mut self, cap_mps: f64) -> Self {
        assert!(
            cap_mps.is_finite() && cap_mps > 0.0,
            "ODD speed cap must be a finite positive value, got {}",
            cap_mps
        );
        self.nominal_contract.odd_speed_cap_mps = Some(cap_mps);
        self
    }

    /// Updates the RSS safe-distance state.
    /// Called by the control loop after each RSS evaluation cycle.
    pub fn update_rss_state(&mut self, state: RssState) {
        self.rss_feed = RssFeed::Fed(state);
    }

    /// EXPLICITLY declare that the scene-RSS verdict is enforced OUTSIDE this
    /// governor instance — at the publication seam (parko-ros2's
    /// `apply_object_rss_gate` bounds the exact twist about to publish,
    /// ADR-0029 Phase 3b) — or that the operator has explicitly accepted
    /// motion without a scene-RSS producer. The pushed-state RSS tier inside
    /// `evaluate()` is then intentionally quiescent; all other tiers
    /// (non-finite reject, LockedOut, Degraded MRC, kinematic envelope,
    /// angular bound) still enforce.
    ///
    /// This is the ONLY way to get pre-#G2 pass-through behaviour from a
    /// governor that is never fed — and it is intent-visible at the call
    /// site, unlike the removed `safe: true` construction default.
    pub fn with_external_rss_gate(mut self) -> Self {
        self.rss_feed = RssFeed::ExternallyGated;
        self
    }

    /// Construct a governor that uses the nominal profile regardless of
    /// the posture passed to evaluate(). Kept for convenience and
    /// backward compatibility.
    /// Starts [`RssFeed::NeverFed`] like `new()` — feed or explicitly gate.
    pub fn nominal() -> Self {
        let profile = VehicleKinematicsContract::nominal_reference_profile();
        Self {
            nominal_contract: profile,
            fallback_contract: profile,
            rss_feed: RssFeed::NeverFed,
            nominal_angular_bound: AngularVelocityBound::nominal(PlatformParams::conservative_default()),
            mrc_angular_bound:     AngularVelocityBound::mrc    (PlatformParams::conservative_default()),
        }
    }

    /// Construct a governor that uses the MRC fallback profile regardless
    /// of the posture passed to evaluate(). Kept for convenience and
    /// backward compatibility.
    /// Starts [`RssFeed::NeverFed`] like `new()` — feed or explicitly gate.
    pub fn mrc_fallback() -> Self {
        let profile = VehicleKinematicsContract::mrc_fallback_profile();
        Self {
            nominal_contract: profile,
            fallback_contract: profile,
            rss_feed: RssFeed::NeverFed,
            nominal_angular_bound: AngularVelocityBound::nominal(PlatformParams::conservative_default()),
            mrc_angular_bound:     AngularVelocityBound::mrc    (PlatformParams::conservative_default()),
        }
    }

    /// Backward-compatible posture-based constructor. Equivalent to
    /// new() but kept for callers using the older API.
    pub fn for_posture(posture: FleetPosture) -> Self {
        match posture {
            FleetPosture::Nominal => Self::nominal(),
            // Degraded uses the MRC fallback profile as its nominal contract.
            FleetPosture::Degraded => Self::mrc_fallback(),
            // LockedOut: evaluate() always returns Deny (0.0) regardless of
            // the contract stored here; use the full profile so the struct is
            // valid and the Nominal branch works if posture changes.
            FleetPosture::LockedOut => Self::new(),
        }
    }
}

impl KirraGovernor {
    /// Applies the Degraded / RSS-unsafe enforcement: **controlled
    /// decel-to-stop-and-HOLD** (Issue #70) on BOTH axes, with the MRC
    /// envelope as the decel-trajectory bound. Used by both the Degraded
    /// posture branch and the RSS unsafe gate. NOT used for LockedOut (which
    /// is a hard stop returning 0.0).
    ///
    /// Two stages, gate then clamp:
    ///   1. **Stop-and-hold gate** — per channel (linear and angular,
    ///      `degraded_channel_violation`): deny any speed increase or
    ///      re-initiation from a stop. `current` is taken from `previous`
    ///      (the last commanded value; `None` ⇒ treated as stopped, so a
    ///      first-cycle motion command fails closed). A violation on either
    ///      channel → `Deny` (the actuator falls to the controlled stop);
    ///      the Governor does not author a replacement command.
    ///   2. **MRC envelope clamp** (unchanged) — for a command that passed
    ///      the gate (converging toward zero), most-restrictive-wins:
    ///        - linear within cap + angular within cap  → Allow
    ///        - linear over cap   + angular within cap  → ClampLinearVelocity
    ///        - linear within cap + angular over cap    → ClampAngularVelocity
    ///        - both over cap                            → ClampMotion { Some, Some }
    // SAFETY: SG8 | REQ: degraded-decel-to-stop-and-hold-multiaxis | TEST: degraded_above_cap_clamps_to_mrc_ceiling,rss_unsafe_above_ceiling_clamps_to_mrc,degraded_angular_above_bound_clamps_to_mrc_angular_ceiling,degraded_both_axes_above_bound_returns_clampmotion,degraded_reinitiation_from_stop_is_denied,degraded_angular_reinitiation_from_stop_is_denied
    // (Issue #70: Degraded is decel-to-stop-and-HOLD, not an MRC crawl —
    //  no autonomous re-initiation on either axis; clamps a converging
    //  command to the MRC envelope.)
    fn apply_mrc_profile(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
    ) -> EnforcementAction {
        // Stage 1 — Issue #70 stop-and-hold gate (both channels).
        let cur_lin = previous.map(|p| p.linear_velocity).unwrap_or(0.0);
        let cur_ang = previous.map(|p| p.angular_velocity).unwrap_or(0.0);
        if let Some(reason) =
            degraded_channel_violation(cur_lin, proposed.linear_velocity, STOP_EPSILON_MPS)
        {
            return EnforcementAction::Deny { reason: reason.to_string() };
        }
        if let Some(reason) =
            degraded_channel_violation(cur_ang, proposed.angular_velocity, STOP_EPSILON_RAD_S)
        {
            return EnforcementAction::Deny { reason: reason.to_string() };
        }

        // Stage 2 — MRC envelope clamp. `MRC_VELOCITY_CEILING_MPS` is a MAGNITUDE
        // ceiling (ADL-001), so clamp symmetrically: `.min(ceiling)` bounded only
        // the forward direction, admitting a reverse command past the ceiling
        // (e.g. -8 m/s convergent from -10, which passes the Stage-1 non-increasing
        // gate) unclamped (#407). The angular channel below already clamps by
        // magnitude; the linear channel must match.
        let safe_linear =
            proposed.linear_velocity.clamp(-MRC_VELOCITY_CEILING_MPS, MRC_VELOCITY_CEILING_MPS);
        let linear_clamped = safe_linear != proposed.linear_velocity;
        // SOTIF-derived: ω_max evaluated at the COMMAND's linear
        // velocity (clamped to the post-linear-cap value so the
        // rollover constraint is consistent with what'll actually
        // be commanded).
        let v_for_bound = safe_linear.abs();
        let mrc_omega_max = self.mrc_angular_bound.omega_max(v_for_bound);
        let angular_clamped = proposed.angular_velocity.abs() > mrc_omega_max;
        let safe_angular = if angular_clamped {
            mrc_omega_max * proposed.angular_velocity.signum()
        } else {
            proposed.angular_velocity
        };
        match (linear_clamped, angular_clamped) {
            (false, false) => EnforcementAction::Allow,
            (true, false)  => EnforcementAction::ClampLinearVelocity(safe_linear),
            (false, true)  => EnforcementAction::ClampAngularVelocity(safe_angular),
            (true, true)   => EnforcementAction::ClampMotion {
                linear:  Some(safe_linear),
                angular: Some(safe_angular),
            },
        }
    }

    /// WS-0.4 — the MRC angular-velocity ceiling `ω_max(v)` (rad/s) at a
    /// given linear velocity: the SAME SOTIF-derived bound
    /// `apply_mrc_profile` enforces (#136), exposed for the comparator's
    /// divergence reconciliation, which caps the reconciled yaw rate with
    /// the primary arm's MRC envelope. Always non-negative and finite
    /// (`AngularVelocityBound::omega_max` masks the v→0 singularity).
    pub(crate) fn mrc_omega_max(&self, linear_velocity_mps: f64) -> f64 {
        self.mrc_angular_bound.omega_max(linear_velocity_mps.abs())
    }

    /// Applies the Nominal angular-velocity ceiling to a proposed command,
    /// returning the clamped magnitude (sign-preserved) when the bound is
    /// exceeded, else `None`.
    ///
    /// **#136 SOTIF:** the ceiling is `ω_max(v) = min(rollover(v),
    /// sweep, ftti)` from `AngularVelocityBound::omega_max`, evaluated
    /// at the proposed command's linear velocity.
    // SAFETY: SG8 | REQ: angular-velocity-bound-sotif | TEST: nominal_angular_above_bound_clamps_to_max,nominal_angular_below_bound_passes_through,in_place_rotation_above_bound_is_clamped,linear_and_angular_both_above_bound_returns_clampmotion,locked_out_dominates_high_angular_velocity,reverse_spin_above_bound_clamps_with_correct_sign
    fn nominal_angular_clamp(&self, proposed: &ControlCommand) -> Option<f64> {
        let omega_max = self.nominal_angular_bound.omega_max(proposed.linear_velocity.abs());
        if proposed.angular_velocity.abs() > omega_max {
            Some(omega_max * proposed.angular_velocity.signum())
        } else {
            None
        }
    }
}

impl KirraGovernor {
    /// The three-tier gate (LockedOut → RSS → kinematic, PARK-016),
    /// parameterized on the RSS-safe verdict so the SAME logic serves both the
    /// pushed-state path (`evaluate`, used by the comparator/shadow + tests) and
    /// the authoritative pairwise path (`evaluate_scene`).
    fn gate(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
        posture: SafetyPosture,
        rss_safe: bool,
    ) -> EnforcementAction {
        // Priority 0 (fail-closed): non-finite on EITHER channel. NaN compares
        // false against every bound, so an unguarded NaN — notably on the Nominal
        // angular axis (`nominal_angular_clamp`), where `NaN.abs() > omega_max` is
        // false → `None` → forwarded as `Allow` — slips through as a "safe" command
        // (the same silent-NaN class as #404). Reject both axes at the boundary,
        // for every posture. (Defense-in-depth: the Degraded/RSS path also guards in
        // `degraded_channel_violation`, and the linear axis in
        // `validate_vehicle_command`; this closes the Nominal angular gap.)
        if !proposed.linear_velocity.is_finite() || !proposed.angular_velocity.is_finite() {
            return EnforcementAction::Deny {
                reason: "NONFINITE_COMMAND_REJECTED".to_string(),
            };
        }

        // LockedOut check first — hard stop takes absolute priority.
        if posture == SafetyPosture::LockedOut {
            return EnforcementAction::Deny {
                reason: "LockedOut: hard stop".to_string(),
            };
        }

        // RSS gate second — unsafe state applies Degraded semantics
        // (decel-to-stop-and-HOLD, Issue #70). Per ADL-001: a sensor gap is
        // recoverable; hard stop (0.0) is not.
        if !rss_safe {
            return self.apply_mrc_profile(proposed, previous);
        }

        match posture {
            SafetyPosture::LockedOut => unreachable!("handled above"),
            SafetyPosture::Degraded => self.apply_mrc_profile(proposed, previous),
            SafetyPosture::Nominal => {
                let current_velocity = previous.map(|p| p.linear_velocity).unwrap_or(0.0);
                let kirra_input = ProposedVehicleCommand {
                    linear_velocity_mps: proposed.linear_velocity,
                    current_velocity_mps: current_velocity,
                    delta_time_s,
                    // Steering angle dimension is not bridged from parko's
                    // angular_velocity (differential drive ↔ Ackermann mismatch;
                    // see module doc). The angular axis is bounded natively
                    // below via `nominal_angular_clamp`.
                    steering_angle_deg: 0.0,
                    current_steering_angle_deg: 0.0,
                };
                // Linear axis — Kirra kinematics contract.
                let linear_action = validate_vehicle_command(&kirra_input, &self.nominal_contract);
                // Angular axis — native parko-side bound (H1 closeout).
                let angular_clamp = self.nominal_angular_clamp(proposed);

                match linear_action {
                    // Hard deny on the linear axis dominates — angular bound is
                    // moot if the command is already being rejected outright.
                    // `DenyCode -> String` here is the single per-deny allocation
                    // permitted at this cross-crate adapter boundary: `EnforcementAction::Deny`
                    // owns a `String` reason field by its public contract. The
                    // Governor hot path itself stayed alloc-free (S3 / #115).
                    EnforceAction::DenyBreach(code) => EnforcementAction::Deny {
                        reason: code.reason().to_string(),
                    },
                    // ClampBoth carries a steering correction too (review H1), but
                    // steering is meaningless for differential drive (see below),
                    // so only the linear channel is honored — identical handling
                    // to ClampLinear.
                    EnforceAction::ClampLinear(safe_linear)
                    | EnforceAction::ClampBoth { linear: safe_linear, .. } => match angular_clamp {
                        // Both axes need clamping → multi-axis enforcement.
                        Some(safe_angular) => EnforcementAction::ClampMotion {
                            linear:  Some(safe_linear),
                            angular: Some(safe_angular),
                        },
                        // Only the linear axis needs clamping.
                        None => EnforcementAction::ClampLinearVelocity(safe_linear),
                    },
                    // ClampSteering coming out of the Kirra Ackermann pipeline
                    // is meaningless for differential drive (steering is always
                    // hardcoded to 0.0 in the bridge), so we ignore that channel
                    // and let the angular-axis verdict speak for itself.
                    EnforceAction::Allow | EnforceAction::ClampSteering(_) => match angular_clamp {
                        Some(safe_angular) => EnforcementAction::ClampAngularVelocity(safe_angular),
                        None => EnforcementAction::Allow,
                    },
                }
            }
        }
    }

    /// AUTHORITATIVE pairwise RSS path (issue #92).
    ///
    /// The governor COMPUTES the RSS verdict from the agent scene it sees
    /// (`compute_scene_rss`, checker-over-doer) and runs the three-tier gate on
    /// THAT computed verdict — it never trusts a caller-pushed `safe: bool`.
    /// `update_rss_state` remains for the comparator/shadow + tests, but the
    /// production verdict path is this method.
    pub fn evaluate_scene(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
        posture: SafetyPosture,
        scene: &AgentScene,
        params: &RssParams,
    ) -> EnforcementAction {
        let computed = compute_scene_rss(scene, params);
        self.gate(proposed, previous, delta_time_s, posture, computed.safe)
    }

    /// AUTHORITATIVE pairwise RSS + occlusion path (issues #92 + #122).
    ///
    /// Extends [`evaluate_scene`] with RSS rule iv: alongside the pairwise agent
    /// verdict, the governor computes the occlusion speed cap from the sightline
    /// scene it sees ([`compute_occlusion_cap`]) and OVERRIDES the verdict to
    /// unsafe when the proposed ego speed exceeds that cap. Checker-over-doer: a
    /// caller-pushed `safe: true` that violates the occlusion bound is overridden
    /// (→ MRC profile), never trusted.
    ///
    /// Fail-closed: an `Absent` occlusion scene caps at `0.0` (any motion is
    /// unsafe) — DISTINCT from `KnownClear`, where rule iv does not bind. A
    /// non-finite proposed speed fails the `<=` comparison (NaN) → unsafe.
    ///
    /// NOTE: producing the [`OcclusionScene`] from perception (sightline ranging
    /// / occluded-region extraction) is OUT OF SCOPE here, exactly as the agent
    /// set's perception ingestion is for `evaluate_scene` — the scene is a check
    /// INPUT supplied by the caller.
    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_scene_with_occlusion(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
        posture: SafetyPosture,
        scene: &AgentScene,
        occlusion: &OcclusionScene,
        params: &RssParams,
    ) -> EnforcementAction {
        let pairwise_safe = compute_scene_rss(scene, params).safe;
        let cap = compute_occlusion_cap(occlusion, params);
        // The occlusion bound is on SPEED magnitude. `<=` is NaN-safe: a
        // non-finite proposed speed makes it false → unsafe; an `Absent` cap of
        // 0.0 makes any |v| > 0 unsafe; a `KnownClear` cap of +inf never binds.
        let occlusion_ok = proposed.linear_velocity.abs() <= cap;
        self.gate(
            proposed,
            previous,
            delta_time_s,
            posture,
            pairwise_safe && occlusion_ok,
        )
    }

    /// AUTHORITATIVE pairwise RSS + SG4 water path (issues #92 + #98).
    ///
    /// Extends [`evaluate_scene`] with the SG4 WATER_UNTRAVERSABLE veto: alongside
    /// the pairwise agent verdict, the governor evaluates the water scene it sees
    /// (`water_untraversable_veto`, checker-over-doer) and OVERRIDES the verdict
    /// to unsafe when the water is the unbounded / fail-closed signature — forcing
    /// the gate's MRC decel-to-stop profile (stop short of water), regardless of
    /// the planner's `safe: true`.
    ///
    /// The governor vetoes only the clearly-dangerous unbounded signature; a
    /// bounded-safe puddle is NOT vetoed (the planner drives it — no over-stop in
    /// rain). `Unknown` water (no healthy detector update) fails closed to a veto,
    /// DISTINCT from `Clear`.
    ///
    /// NOTE: producing the [`WaterScene`] from perception (water-surface anomaly
    /// detection) is OUT OF SCOPE here — the scene is a check INPUT supplied by
    /// the caller, exactly as the agent set and occlusion scene are. Depth is
    /// never taken or computed (it is unrangeable).
    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_scene_with_water(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
        posture: SafetyPosture,
        scene: &AgentScene,
        water: &WaterScene,
        water_cfg: &WaterVetoConfig,
        params: &RssParams,
    ) -> EnforcementAction {
        let pairwise_safe = compute_scene_rss(scene, params).safe;
        // SG4: a WATER_UNTRAVERSABLE veto forces the verdict unsafe → the gate
        // applies the MRC decel-to-stop profile (stop short of water). A
        // bounded-safe puddle / Clear / EarnedTraversable does not veto.
        let water_veto = water_untraversable_veto(water, water_cfg);
        self.gate(
            proposed,
            previous,
            delta_time_s,
            posture,
            pairwise_safe && !water_veto,
        )
    }

    /// AUTHORITATIVE pairwise RSS + SG5 commit-zone path (issues #92 + #106).
    ///
    /// Extends [`evaluate_scene`] with the SG5 map-anchored COMMIT_ZONE_BLOCKED
    /// veto: alongside the pairwise agent verdict, the governor evaluates the
    /// commit-zone scene it sees (`commit_zone_blocked`, checker-over-doer) and
    /// OVERRIDES the verdict to unsafe — forcing the gate's MRC decel-to-stop
    /// (STOP SHORT of the zone) — when entry is blocked, regardless of the
    /// planner's `safe: true`.
    ///
    /// "Reject fires from MAP ALONE": an `Unknown` (absent / unhealthy) map, or a
    /// known zone with degraded inputs / missing clearance / unverified exit,
    /// blocks at the integration layer without live perception of the hazard. A
    /// `NoZone` or a healthy, clearance-confirmed, exit-verified zone passes
    /// through (no over-block).
    ///
    /// NOTE: producing the [`CommitZoneScene`] from a map source is OUT OF SCOPE
    /// here — the scene is a check INPUT supplied by the caller. The supplied
    /// `clearance_confirmed` / `exit_verified` booleans are derived from
    /// geometry/kinematics (#107) and agent arrival (#108) on top of this brick;
    /// stop-inside-zone prevention is part of #107.
    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_scene_with_commit_zone(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
        posture: SafetyPosture,
        scene: &AgentScene,
        commit_zone: &CommitZoneScene,
        cz_cfg: &CommitZoneCfg,
        params: &RssParams,
    ) -> EnforcementAction {
        let pairwise_safe = compute_scene_rss(scene, params).safe;
        // SG5: a COMMIT_ZONE_BLOCKED veto forces the verdict unsafe → the gate
        // applies the MRC decel-to-stop profile (stop short of the zone).
        let cz_veto = commit_zone_blocked(commit_zone, cz_cfg);
        self.gate(
            proposed,
            previous,
            delta_time_s,
            posture,
            pairwise_safe && !cz_veto,
        )
    }

    /// SG5 commit-zone path GATED by localization integrity (#123, runtime half).
    ///
    /// The map-anchored commit-zone trust is only as sound as the integrator's
    /// pose. When the localization-integrity report says the G2 AoU (≤ 0.10 m
    /// 95th-pct lateral error) does NOT currently hold, the commit-zone scene is
    /// degraded to [`CommitZoneScene::Unknown`] BEFORE the check — so a clean
    /// `NoZone` (or a confirmed zone) under a bad pose still vetoes via the #260
    /// fail-closed path. Then delegates to [`evaluate_scene_with_commit_zone`].
    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_scene_with_commit_zone_localized(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
        posture: SafetyPosture,
        scene: &AgentScene,
        commit_zone: &CommitZoneScene,
        cz_cfg: &CommitZoneCfg,
        localization: &FrameIntegrity,
        loc_cfg: &FrameIntegrityCfg,
        params: &RssParams,
    ) -> EnforcementAction {
        // STRICT view: a map-anchored commit-zone location may only be trusted
        // under full `Trusted` (≤ 0.10 m). The `Degraded` fallback band is good
        // enough for graduated containment but NOT for trusting a mapped zone's
        // placement, so anything short of `Trusted` gates the scene fail-closed.
        let trusted = matches!(resolve_frame_trust(localization, loc_cfg), FrameTrust::Trusted);
        let gated = gate_commit_zone_scene(*commit_zone, trusted);
        self.evaluate_scene_with_commit_zone(
            proposed, previous, delta_time_s, posture, scene, &gated, cz_cfg, params,
        )
    }

    /// SG4 water path GATED by localization integrity (#123, runtime half).
    ///
    /// Under an untrusted pose, only the MAP-DERIVED water trust is stripped: a
    /// `MapKnownSafe` ford earn-back falls back to the fail-closed veto state,
    /// while an `OperatorAuthorized` grant SURVIVES (operator authority is not
    /// map-frame-dependent) and perception-derived states are untouched. Then
    /// delegates to [`evaluate_scene_with_water`].
    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_scene_with_water_localized(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
        posture: SafetyPosture,
        scene: &AgentScene,
        water: &WaterScene,
        water_cfg: &WaterVetoConfig,
        localization: &FrameIntegrity,
        loc_cfg: &FrameIntegrityCfg,
        params: &RssParams,
    ) -> EnforcementAction {
        // STRICT view (see commit-zone wrapper): a map-frame ford earn-back may
        // only be trusted under full `Trusted`; the `Degraded` band gates it.
        let trusted = matches!(resolve_frame_trust(localization, loc_cfg), FrameTrust::Trusted);
        let gated = gate_water_scene(*water, trusted);
        self.evaluate_scene_with_water(
            proposed, previous, delta_time_s, posture, scene, &gated, water_cfg, params,
        )
    }

    /// AUTHORITATIVE post-collision path (SG6 / #102).
    ///
    /// While the [`ImpactLatch`] is latched, OVERRIDE any proposed motion →
    /// **immobilize** (a motion-veto `Deny`), regardless of the planner's verdict
    /// or the posture: SG6 requires no further motion after a detected collision
    /// until clearance is confirmed. Not latched → normal pushed-state evaluation.
    ///
    /// The latch is sticky-toward-safe and cleared only by an explicit clearance
    /// signal (`ImpactLatch::clear`) — wiring that to the #103 authenticated
    /// clearance is a deferred follow-up.
    pub fn evaluate_with_impact_latch(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
        posture: SafetyPosture,
        latch: &ImpactLatch,
    ) -> EnforcementAction {
        if latch.is_latched() {
            return EnforcementAction::Deny {
                reason: "SG6: post-collision impact latch — immobilize until clearance"
                    .to_string(),
            };
        }
        self.evaluate(proposed, previous, delta_time_s, posture)
    }

    /// AUTHORITATIVE post-collision path through the SG6 CLEARANCE LOOP (#103).
    ///
    /// While the [`ClearanceLoop`] is immobilized — in EITHER `Latched` OR
    /// `EscalationRaised` — OVERRIDE any proposed motion → immobilize, exactly as
    /// [`evaluate_with_impact_latch`](Self::evaluate_with_impact_latch) does for
    /// the bare latch. The difference is the EXIT: the loop returns to `Normal`
    /// only via `ClearanceLoop::try_clear` with a well-formed operator grant (the
    /// SS-003 structural no-resume), never on clean evidence.
    pub fn evaluate_with_clearance_loop(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
        posture: SafetyPosture,
        clearance: &ClearanceLoop,
    ) -> EnforcementAction {
        if clearance.is_immobilized() {
            return EnforcementAction::Deny {
                reason: "SG6: post-collision clearance loop — immobilize until operator clearance"
                    .to_string(),
            };
        }
        self.evaluate(proposed, previous, delta_time_s, posture)
    }
}

/// SG6 — derive `vanished_object` from the agent scene and fold it into this
/// tick's [`ImpactEvidence`], BEFORE the existing [`ImpactLatch`] /
/// [`ClearanceLoop`] consumes it (#102 follow-up). Thin by design: it only runs
/// the [`VanishedObjectDetector`] and sets the flag — the latch, the clearance
/// loop, and the `is_impact` fusion are UNCHANGED (they already handle the flag,
/// which latches ALONE per SG6). `imu_accel_spike_mps2` and `contact_sensor` are
/// the caller's other evidence for the tick; `now_ms` follows the same
/// caller-supplied-clock convention as the detector and the clearance loop.
#[allow(clippy::too_many_arguments)]
pub fn impact_evidence_with_vanished(
    detector: &mut VanishedObjectDetector,
    scene: &AgentScene,
    now_ms: u64,
    vanished_cfg: &VanishedCfg,
    imu_accel_spike_mps2: f64,
    contact_sensor: bool,
) -> ImpactEvidence {
    let vanished_object = detector.observe(scene, now_ms, vanished_cfg);
    ImpactEvidence {
        imu_accel_spike_mps2,
        contact_sensor,
        vanished_object,
    }
}

impl SafetyGovernor for KirraGovernor {
    fn evaluate(
        &self,
        proposed: &ControlCommand,
        previous: Option<&ControlCommand>,
        delta_time_s: f64,
        posture: SafetyPosture,
    ) -> EnforcementAction {
        // Pushed-state path (comparator/shadow + tests): the gate runs on the
        // RSS verdict last set via `update_rss_state`. The production verdict
        // comes from `evaluate_scene`, which computes RSS pairwise — or from
        // the publication-seam gate when `with_external_rss_gate` was
        // declared. A governor that was NEVER fed gates as UNSAFE
        // (RssFeed::NeverFed → MRC/HOLD): "no RSS input" is never "safe".
        self.gate(proposed, previous, delta_time_s, posture, self.rss_feed.rss_safe())
    }
}

#[cfg(test)]
mod tests {
    use super::{KirraGovernor, MRC_VELOCITY_CEILING_MPS, PlatformParams};
    use parko_core::commands::ControlCommand;
    use parko_core::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};
    use parko_core::RssState;

    fn effective_velocity(action: EnforcementAction, proposed: f64) -> f64 {
        match action {
            EnforcementAction::Allow => proposed,
            EnforcementAction::ClampLinearVelocity(v) => v,
            EnforcementAction::ClampAngularVelocity(_) => proposed,
            EnforcementAction::ClampMotion { linear, .. } => linear.unwrap_or(proposed),
            EnforcementAction::Deny { .. } => 0.0,
        }
    }

    fn cmd(v: f64) -> ControlCommand {
        ControlCommand { linear_velocity: v, angular_velocity: 0.0, timestamp_ms: 0 }
    }

    // Test 1 — LockedOut is a hard stop across the full input range.
    #[test]
    fn locked_out_is_hard_stop_for_all_inputs() {
        let gov = KirraGovernor::new();
        for &v in &[0.0_f64, 1.0, 3.0, 5.0, 10.0, 35.0, 100.0] {
            let action = gov.evaluate(&cmd(v), None, 0.05, SafetyPosture::LockedOut);
            assert_eq!(
                effective_velocity(action, v),
                0.0,
                "LockedOut must always return 0.0 — hard stop (input {})",
                v
            );
        }
    }

    // Test 2 — Degraded applies the MRC cap to a DECELERATING command.
    // (Issue #70: `previous` is a moving vehicle bleeding speed toward the
    //  command; without a moving history the gate would deny re-initiation —
    //  see `degraded_reinitiation_from_stop_is_denied`.)
    #[test]
    fn degraded_above_cap_clamps_to_mrc_ceiling() {
        let gov = KirraGovernor::new();
        let prev = cmd(10.0);
        let action = gov.evaluate(&cmd(10.0), Some(&prev), 0.05, SafetyPosture::Degraded);
        assert_eq!(
            effective_velocity(action, 10.0),
            MRC_VELOCITY_CEILING_MPS,
            "Degraded: a decelerating command above the MRC ceiling must be capped"
        );
    }

    #[test]
    fn degraded_below_cap_allows_through() {
        let gov = KirraGovernor::new();
        let prev = cmd(3.0);
        let action = gov.evaluate(&cmd(3.0), Some(&prev), 0.05, SafetyPosture::Degraded);
        assert_eq!(
            effective_velocity(action, 3.0),
            3.0,
            "Degraded: a non-increasing command below the MRC ceiling must pass through"
        );
    }

    // #407 Finding 1 — the MRC ceiling is a MAGNITUDE bound: a REVERSE command
    // above it (convergent from -10, so it passes the Stage-1 non-increasing gate)
    // must be clamped to -ceiling, not admitted. `.min(ceiling)` only bounded the
    // forward direction, so reverse over-ceiling slipped through unclamped.
    #[test]
    fn degraded_reverse_above_ceiling_clamps_to_negative_mrc() {
        let gov = KirraGovernor::new();
        let prev = cmd(-10.0); // reversing, bleeding speed toward the command
        let action = gov.evaluate(&cmd(-8.0), Some(&prev), 0.05, SafetyPosture::Degraded);
        assert!(
            matches!(action, EnforcementAction::ClampLinearVelocity(_)),
            "reverse over-ceiling must clamp, got {action:?}"
        );
        assert_eq!(
            effective_velocity(action, -8.0),
            -MRC_VELOCITY_CEILING_MPS,
            "a reverse command above the MRC magnitude ceiling must clamp to -ceiling, not pass at -8"
        );
    }

    // #407 Finding 2 — a non-finite angular velocity must fail-closed to Deny in
    // the Nominal arm (it previously forwarded as Allow, since `NaN.abs() > ω_max`
    // is false). The finite linear axis would otherwise admit the command.
    #[test]
    fn nominal_nonfinite_angular_is_denied() {
        let gov = KirraGovernor::new();
        for &ang in &[f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let proposed = ControlCommand { linear_velocity: 1.0, angular_velocity: ang, timestamp_ms: 0 };
            let action = gov.evaluate(&proposed, None, 0.05, SafetyPosture::Nominal);
            assert!(
                matches!(action, EnforcementAction::Deny { .. }),
                "Nominal: non-finite angular ({ang}) must Deny, got {action:?}"
            );
        }
    }

    // The boundary guard rejects a non-finite command on EITHER channel, in EVERY
    // posture (defense-in-depth).
    #[test]
    fn nonfinite_command_is_denied_in_all_postures() {
        let gov = KirraGovernor::new();
        for posture in [SafetyPosture::Nominal, SafetyPosture::Degraded, SafetyPosture::LockedOut] {
            let nan_lin = ControlCommand { linear_velocity: f64::NAN, angular_velocity: 0.0, timestamp_ms: 0 };
            let nan_ang = ControlCommand { linear_velocity: 1.0, angular_velocity: f64::NAN, timestamp_ms: 0 };
            assert!(
                matches!(gov.evaluate(&nan_lin, None, 0.05, posture), EnforcementAction::Deny { .. }),
                "posture {posture:?}: NaN linear must Deny"
            );
            assert!(
                matches!(gov.evaluate(&nan_ang, None, 0.05, posture), EnforcementAction::Deny { .. }),
                "posture {posture:?}: NaN angular must Deny"
            );
        }
    }

    // Test 3 — LockedOut and Degraded must produce different outputs for non-zero input.
    #[test]
    fn locked_out_and_degraded_produce_different_outputs() {
        let gov = KirraGovernor::new();
        let prev = cmd(3.0);
        let locked_out = effective_velocity(
            gov.evaluate(&cmd(3.0), Some(&prev), 0.05, SafetyPosture::LockedOut),
            3.0,
        );
        let degraded = effective_velocity(
            gov.evaluate(&cmd(3.0), Some(&prev), 0.05, SafetyPosture::Degraded),
            3.0,
        );
        assert_ne!(
            locked_out, degraded,
            "LockedOut and Degraded must never produce the same output \
             for non-zero input — they are different code paths"
        );
    }

    // Test 4 — Nominal passes through valid input.
    #[test]
    fn nominal_steady_state_below_ceiling_allows_through() {
        let gov = KirraGovernor::new();
        // Use steady-state previous to suppress rate-of-change clamping.
        let prev = cmd(3.0);
        let action = gov.evaluate(&cmd(3.0), Some(&prev), 0.05, SafetyPosture::Nominal);
        assert_eq!(
            effective_velocity(action, 3.0),
            3.0,
            "Nominal: input within envelope must pass through unchanged"
        );
    }

    // -------------------------------------------------------------------------
    // Tests A–E: RSS pre-actuator gate (PARK-016)
    // -------------------------------------------------------------------------

    fn unsafe_rss() -> RssState {
        RssState { safe: false, longitudinal_margin: 1.0, lateral_margin: 0.3 }
    }

    fn safe_rss() -> RssState {
        RssState { safe: true, longitudinal_margin: 12.0, lateral_margin: 5.0 }
    }

    // Test A — RSS unsafe, input above MRC ceiling: exact MRC contract — ADL-001
    #[test]
    fn rss_unsafe_above_ceiling_clamps_to_mrc() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(unsafe_rss());
        let commanded = MRC_VELOCITY_CEILING_MPS + 5.0;
        let prev = cmd(commanded);
        let action = gov.evaluate(&cmd(commanded), Some(&prev), 0.05, SafetyPosture::Nominal);
        assert_eq!(
            effective_velocity(action, commanded),
            commanded.min(MRC_VELOCITY_CEILING_MPS),
            "RSS unsafe: exact MRC contract — ADL-001"
        );
    }

    // Test B — RSS safe, input within nominal envelope: passes through.
    #[test]
    fn rss_safe_nominal_input_passes_through() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(safe_rss());
        let prev = cmd(3.0);
        let action = gov.evaluate(&cmd(3.0), Some(&prev), 0.05, SafetyPosture::Nominal);
        assert_eq!(
            effective_velocity(action, 3.0),
            3.0,
            "RSS safe: input within nominal envelope must pass through"
        );
    }

    // Test C — RSS unsafe, input below MRC ceiling: cap not triggered, passes through.
    #[test]
    fn rss_unsafe_below_ceiling_passes_through() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(unsafe_rss());
        let commanded = MRC_VELOCITY_CEILING_MPS - 1.0;
        let prev = cmd(commanded);
        let action = gov.evaluate(&cmd(commanded), Some(&prev), 0.05, SafetyPosture::Nominal);
        assert_eq!(
            effective_velocity(action, commanded),
            commanded,
            "RSS unsafe: input below MRC ceiling must pass through unchanged"
        );
    }

    // Test D — RSS unsafe and Degraded share one code path (apply_mrc_profile).
    #[test]
    fn rss_unsafe_and_degraded_share_mrc_code_path() {
        let mut gov = KirraGovernor::new();
        let prev = cmd(10.0);

        // Degraded with RSS safe
        gov.update_rss_state(safe_rss());
        let output_degraded = effective_velocity(
            gov.evaluate(&cmd(10.0), Some(&prev), 0.05, SafetyPosture::Degraded),
            10.0,
        );

        // Nominal with RSS unsafe
        gov.update_rss_state(unsafe_rss());
        let output_rss_unsafe = effective_velocity(
            gov.evaluate(&cmd(10.0), Some(&prev), 0.05, SafetyPosture::Nominal),
            10.0,
        );

        assert_eq!(
            output_degraded, output_rss_unsafe,
            "Degraded and RSS-unsafe must produce identical output — single apply_mrc_profile path"
        );
    }

    // Test E — LockedOut hard stop takes priority over RSS gate.
    #[test]
    fn locked_out_dominates_rss_unsafe() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(unsafe_rss());
        let action = gov.evaluate(&cmd(10.0), None, 0.05, SafetyPosture::LockedOut);
        assert_eq!(
            effective_velocity(action, 10.0),
            0.0,
            "LockedOut hard stop must dominate RSS gate — LockedOut always returns 0.0"
        );
    }

    // -------------------------------------------------------------------------
    // #G2 / WS-0.1 — the fail-closed RssFeed default. These pin the semantic
    // change: an UNFED governor never asserts RSS-safe; the pre-fix
    // `safe: true` construction default (silent fail-open) is gone.
    // -------------------------------------------------------------------------

    /// THE DoD TEST — "default posture without RSS input is not-safe": a
    /// freshly constructed governor (never fed an RSS verdict) must not admit
    /// motion from standstill in Nominal. The RSS tier gates UNSAFE →
    /// MRC/HOLD → re-initiation from stop is denied.
    #[test]
    fn unfed_governor_is_immobile_from_standstill_fail_closed() {
        let gov = KirraGovernor::new();
        let action = gov.evaluate(&cmd(2.0), None, 0.05, SafetyPosture::Nominal);
        assert!(
            matches!(action, EnforcementAction::Deny { .. }),
            "an unfed governor must HOLD at zero (deny re-initiation), got {action:?}"
        );
    }

    /// An unfed governor bounds an already-moving platform by the MRC
    /// decel-to-stop profile: a speed INCREASE is denied (never authored),
    /// while a converging (non-increasing) command within the MRC envelope
    /// is admitted — a controlled stop, not a slammed one.
    #[test]
    fn unfed_governor_applies_mrc_decel_profile_when_moving() {
        let gov = KirraGovernor::new();
        let prev = cmd(4.0);
        let increase = gov.evaluate(&cmd(6.0), Some(&prev), 0.05, SafetyPosture::Nominal);
        assert!(
            matches!(increase, EnforcementAction::Deny { .. }),
            "an unfed governor must deny a speed increase, got {increase:?}"
        );
        let converge = gov.evaluate(&cmd(3.9), Some(&prev), 0.05, SafetyPosture::Nominal);
        assert!(
            !matches!(converge, EnforcementAction::Deny { .. }),
            "a converging command within the MRC envelope decelerates under control, got {converge:?}"
        );
    }

    /// The explicit opt-out restores envelope pass-through — and is the ONLY
    /// way to get it without feeding a verdict.
    #[test]
    fn externally_gated_governor_restores_envelope_passthrough() {
        let gov = KirraGovernor::new().with_external_rss_gate();
        let prev = cmd(3.0);
        let action = gov.evaluate(&cmd(3.0), Some(&prev), 0.05, SafetyPosture::Nominal);
        assert!(
            matches!(action, EnforcementAction::Allow),
            "externally-gated governor passes an in-envelope command, got {action:?}"
        );
    }

    /// Feeding a safe verdict exits NeverFed; feeding an unsafe one after a
    /// safe one re-gates — the feed is live state, not a one-shot unlock.
    #[test]
    fn feeding_transitions_never_fed_to_live_verdicts() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(safe_rss());
        let prev = cmd(3.0);
        assert!(matches!(
            gov.evaluate(&cmd(3.0), Some(&prev), 0.05, SafetyPosture::Nominal),
            EnforcementAction::Allow
        ));
        gov.update_rss_state(unsafe_rss());
        let after_unsafe = gov.evaluate(&cmd(6.0), Some(&cmd(4.0)), 0.05, SafetyPosture::Nominal);
        assert!(
            matches!(after_unsafe, EnforcementAction::Deny { .. }),
            "an unsafe feed after a safe one must re-gate, got {after_unsafe:?}"
        );
    }

    /// Lockstep: an unfed primary and an unfed diverse shadow reach the SAME
    /// physical verdict (both MRC/HOLD) — the fail-closed default cannot be a
    /// source of false divergence.
    #[test]
    fn unfed_primary_and_diverse_agree_fail_closed() {
        let primary = KirraGovernor::new();
        let diverse = crate::DiverseKirraGovernor::new();
        let proposed = cmd(2.0);
        let pa = primary.evaluate(&proposed, None, 0.05, SafetyPosture::Nominal);
        let da = diverse.evaluate(&proposed, None, 0.05, SafetyPosture::Nominal);
        assert!(
            matches!(pa, EnforcementAction::Deny { .. }),
            "unfed primary holds, got {pa:?}"
        );
        assert!(
            matches!(da, EnforcementAction::Deny { .. }),
            "unfed diverse holds, got {da:?}"
        );
    }

    // -------------------------------------------------------------------------
    // Tests for H1 — angular-velocity enforcement (Approach A)
    // -------------------------------------------------------------------------
    //
    // The fix adds a NATIVE angular-velocity bound to the parko-kirra
    // bridge. Differential drive has independent linear + angular axes;
    // these tests cover the matrix:
    //
    //   posture   | linear     | angular        | expected
    //   ----------+------------+----------------+----------------------
    //   Nominal   | within     | within         | Allow
    //   Nominal   | within     | above bound    | ClampAngularVelocity
    //   Nominal   | above lin  | above bound    | ClampMotion(Some,Some)
    //   Nominal   | linear=0   | above bound    | ClampAngularVelocity   (in-place rotation)
    //   Degraded  | within     | above MRC bnd  | ClampAngularVelocity   (tighter cap)
    //   Degraded  | above lin  | above MRC bnd  | ClampMotion(Some,Some)
    //   LockedOut | any        | above bound    | Deny (hard stop)
    //   Nominal   | within     | reverse above  | ClampAngularVelocity with sign preserved
    //
    // The H1 enforcement-logic tests pin the angular bound at the
    // pre-SOTIF placeholder values (1.5 / 0.5 rad/s) via the back-
    // compat `with_angular_bounds` (Scalar) overlay. This keeps the
    // tests focused on the enforcement LOGIC (sign preservation,
    // multi-axis ClampMotion, sticky behaviour) without coupling them
    // to the SOTIF derivation's specific numbers — the derivation
    // tests live in `crate::angular_bound::tests` and the
    // `derived_*` tests below.

    /// Local constant — pins the H1 numeric expectations against a
    /// known scalar bound, NOT the SOTIF derivation default. Lets
    /// the existing enforcement-logic assertions read the same way
    /// they did before #136 landed.
    const H1_NOMINAL_RAD_S: f64 = 1.5;
    const H1_MRC_RAD_S:     f64 = 0.5;

    /// Helper: build a governor with the legacy scalar bounds so the
    /// enforcement-logic tests below have a known reference. Tests
    /// of the SOTIF derivation itself construct their own governors
    /// via `with_platform_params`.
    fn legacy_scalar_gov() -> KirraGovernor {
        // These tests exercise the ANGULAR/kinematic tier; the RSS tier is out
        // of scope, so declare it externally gated (the unfed default would
        // route every command to the MRC/HOLD profile — see `RssFeed`).
        KirraGovernor::new()
            .with_angular_bounds(H1_NOMINAL_RAD_S, H1_MRC_RAD_S)
            .with_external_rss_gate()
    }

    fn cmd_twist(linear: f64, angular: f64) -> ControlCommand {
        ControlCommand { linear_velocity: linear, angular_velocity: angular, timestamp_ms: 0 }
    }

    fn effective_angular(action: &EnforcementAction, proposed: f64) -> f64 {
        match action {
            EnforcementAction::Allow => proposed,
            EnforcementAction::ClampLinearVelocity(_) => proposed,
            EnforcementAction::ClampAngularVelocity(a) => *a,
            EnforcementAction::ClampMotion { angular, .. } => angular.unwrap_or(proposed),
            EnforcementAction::Deny { .. } => 0.0,
        }
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// linear within envelope, angular within bound → Allow.
    #[test]
    fn nominal_angular_below_bound_passes_through() {
        let gov = legacy_scalar_gov();
        let prev = cmd_twist(3.0, 0.0);
        let proposed = cmd_twist(3.0, H1_NOMINAL_RAD_S - 0.1);
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        assert!(
            matches!(action, EnforcementAction::Allow),
            "in-envelope command must pass through; got {action:?}"
        );
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// linear within envelope, angular above bound → ClampAngularVelocity
    /// (the linear axis is untouched; only the angular axis is clamped).
    #[test]
    fn nominal_angular_above_bound_clamps_to_max() {
        let gov = legacy_scalar_gov();
        let prev = cmd_twist(3.0, 0.0);
        let proposed = cmd_twist(3.0, H1_NOMINAL_RAD_S + 1.0);
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert!(
                    (a - H1_NOMINAL_RAD_S).abs() < 1e-12,
                    "expected angular clamped to {H1_NOMINAL_RAD_S}, got {a}"
                );
            }
            other => panic!("expected ClampAngularVelocity, got {other:?}"),
        }
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// In-place rotation (linear=0, angular above bound) — the case
    /// Approach B (diff-drive → bicycle conversion) could not represent
    /// (infinite curvature). Approach A handles it cleanly.
    #[test]
    fn in_place_rotation_above_bound_is_clamped() {
        let gov = legacy_scalar_gov();
        let prev = cmd_twist(0.0, 0.0);
        let proposed = cmd_twist(0.0, H1_NOMINAL_RAD_S * 2.0);
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert_eq!(a, H1_NOMINAL_RAD_S);
            }
            other => panic!(
                "in-place rotation with excess spin must clamp angular axis; got {other:?}"
            ),
        }
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// Reverse spin (negative angular) above bound — clamps with sign preserved.
    #[test]
    fn reverse_spin_above_bound_clamps_with_correct_sign() {
        let gov = legacy_scalar_gov();
        let prev = cmd_twist(0.0, 0.0);
        let proposed = cmd_twist(0.0, -(H1_NOMINAL_RAD_S + 0.5));
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert_eq!(a, -H1_NOMINAL_RAD_S);
            }
            other => panic!("expected ClampAngularVelocity with negative sign; got {other:?}"),
        }
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// Both axes above their respective bounds → ClampMotion with both
    /// fields populated. The linear axis is exceeding the nominal
    /// kinematics-contract velocity ceiling (35 m/s); the angular axis
    /// is exceeding the placeholder bound.
    #[test]
    fn linear_and_angular_both_above_bound_returns_clampmotion() {
        let gov = legacy_scalar_gov();
        let prev = cmd_twist(30.0, 0.0);
        let proposed = cmd_twist(40.0, H1_NOMINAL_RAD_S + 0.5);
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        match action {
            EnforcementAction::ClampMotion { linear, angular } => {
                let lin = linear.expect("linear must be Some when over the linear ceiling");
                let ang = angular.expect("angular must be Some when over the angular bound");
                assert!(
                    lin <= 35.0 + 1e-9,
                    "linear must be clamped at or below the vehicle max (35 m/s), got {lin}"
                );
                assert_eq!(ang, H1_NOMINAL_RAD_S);
            }
            other => panic!("expected ClampMotion {{Some,Some}}, got {other:?}"),
        }
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// Degraded posture — angular above MRC bound (tighter than Nominal)
    /// is clamped. Mirror of the linear MRC ceiling philosophy.
    #[test]
    fn degraded_angular_above_bound_clamps_to_mrc_angular_ceiling() {
        let gov = legacy_scalar_gov();
        // linear is within MRC linear cap so only the angular axis fires.
        let proposed = cmd_twist(
            MRC_VELOCITY_CEILING_MPS - 0.5,
            H1_MRC_RAD_S + 0.3,
        );
        // Issue #70: a moving, non-increasing `previous` so the decel-to-stop
        // gate passes and the angular MRC clamp is what the test exercises.
        let prev = proposed.clone();
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Degraded);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert!(
                    (a - H1_MRC_RAD_S).abs() < 1e-12,
                    "expected angular clamped to MRC ceiling {H1_MRC_RAD_S}, got {a}"
                );
            }
            other => panic!("expected ClampAngularVelocity under Degraded; got {other:?}"),
        }
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// Degraded — both axes over their MRC caps → ClampMotion.
    #[test]
    fn degraded_both_axes_above_bound_returns_clampmotion() {
        let gov = legacy_scalar_gov();
        let proposed = cmd_twist(
            MRC_VELOCITY_CEILING_MPS + 2.0,
            H1_MRC_RAD_S + 0.4,
        );
        // Issue #70: moving, non-increasing `previous` so the gate passes and
        // the dual-axis MRC clamp is what the test exercises.
        let prev = proposed.clone();
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Degraded);
        match action {
            EnforcementAction::ClampMotion { linear, angular } => {
                assert_eq!(linear, Some(MRC_VELOCITY_CEILING_MPS));
                assert_eq!(angular, Some(H1_MRC_RAD_S));
            }
            other => panic!("expected ClampMotion {{Some,Some}} under Degraded; got {other:?}"),
        }
    }

    /// SAFETY: SG8 | REQ: angular-velocity-bound
    /// LockedOut dominates the angular check — no matter what the angular
    /// value is, the verdict is Deny / hard-stop.
    #[test]
    fn locked_out_dominates_high_angular_velocity() {
        let gov = legacy_scalar_gov();
        let proposed = cmd_twist(0.0, H1_NOMINAL_RAD_S * 10.0);
        let action = gov.evaluate(&proposed, None, 0.05, SafetyPosture::LockedOut);
        assert!(
            matches!(action, EnforcementAction::Deny { .. }),
            "LockedOut must Deny regardless of angular value; got {action:?}"
        );
        assert_eq!(effective_angular(&action, proposed.angular_velocity), 0.0);
    }

    /// `with_angular_bounds` overrides the placeholder defaults — verifies
    /// the configurable parameter actually changes the verdict.
    #[test]
    fn with_angular_bounds_override_changes_verdict() {
        // Tighter platform: 0.3 rad/s nominal cap. A command at 0.5 rad/s
        // that would be Allow under the placeholder default now clamps.
        let gov = KirraGovernor::new().with_angular_bounds(0.3, 0.1).with_external_rss_gate();
        let prev = cmd_twist(2.0, 0.0);
        let proposed = cmd_twist(2.0, 0.5);
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => assert_eq!(a, 0.3),
            other => panic!("expected ClampAngularVelocity(0.3) with tightened bound; got {other:?}"),
        }
    }

    #[test]
    #[should_panic(expected = "nominal angular bound must be a finite positive value")]
    fn with_angular_bounds_rejects_non_positive() {
        let _ = KirraGovernor::new().with_angular_bounds(0.0, 0.5);
    }

    #[test]
    #[should_panic(expected = "MRC angular bound must be a finite positive value")]
    fn with_angular_bounds_rejects_nan() {
        let _ = KirraGovernor::new().with_angular_bounds(1.0, f64::NAN);
    }

    // -----------------------------------------------------------------------
    // #136 — SOTIF derivation INTEGRATION tests (governor side)
    // -----------------------------------------------------------------------
    //
    // Pure ω_max(v) math is in `crate::angular_bound::tests`. These
    // tests verify the governor's evaluate path uses the derived
    // bound at the proposed command's linear velocity — swapping
    // PlatformParams changes the verdict.

    /// SOTIF derivation drives the governor verdict. 0.5 rad/s
    /// passes under urban-reference (sweep ≈ 0.833) but clamps under
    /// conservative default (sweep ≈ 0.2). Same command, different
    /// config, different verdict — proves the derivation isn't a no-op.
    #[test]
    fn derived_bound_changes_verdict_between_platforms() {
        let proposed = cmd_twist(1.0, 0.5);
        let urban = KirraGovernor::new()
            .with_platform_params(PlatformParams::urban_service_robot_reference())
            .with_external_rss_gate();
        let action_urban = urban.evaluate(
            &proposed, Some(&cmd_twist(1.0, 0.0)), 0.05, SafetyPosture::Nominal);
        assert!(matches!(action_urban, EnforcementAction::Allow),
            "urban-reference: 0.5 rad/s at v=1 m/s must Allow; got {action_urban:?}");
        let cons = KirraGovernor::new().with_external_rss_gate();
        let action_cons = cons.evaluate(
            &proposed, Some(&cmd_twist(1.0, 0.0)), 0.05, SafetyPosture::Nominal);
        match action_cons {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert!((a - 0.2_f64).abs() < 1e-9,
                    "conservative default: expected 0.2 rad/s, got {a}");
            }
            other => panic!("conservative default must clamp; got {other:?}"),
        }
    }

    /// v=0 in-place rotation under the derived bound — singularity
    /// masked, sweep + FTTI bind. Urban reference: clamps to ~0.833.
    #[test]
    fn derived_in_place_rotation_clamps_to_sweep_bound() {
        let gov = KirraGovernor::new()
            .with_platform_params(PlatformParams::urban_service_robot_reference())
            .with_external_rss_gate();
        let proposed = cmd_twist(0.0, 1.0);
        let action = gov.evaluate(&proposed, None, 0.05, SafetyPosture::Nominal);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert!((a - 0.833_f64).abs() < 1e-2,
                    "in-place: expected ~0.833 (sweep), got {a}");
            }
            other => panic!("expected ClampAngularVelocity at v=0; got {other:?}"),
        }
    }

    /// Degraded MRC tightens the derived bound — sweep budget halves
    /// to ~0.4167 under the 0.5 posture factor.
    #[test]
    fn derived_mrc_in_place_rotation_is_tighter_than_nominal() {
        let gov = KirraGovernor::new()
            .with_platform_params(PlatformParams::urban_service_robot_reference());
        let proposed = cmd_twist(0.0, 1.0);
        // Issue #70: already rotating at the commanded rate (non-increasing),
        // so the angular decel-to-stop gate passes and the MRC sweep clamp is
        // what this test exercises. Re-initiating rotation from a standstill
        // under Degraded is denied — see `degraded_angular_reinitiation_from_stop_is_denied`.
        let prev = proposed.clone();
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Degraded);
        match action {
            EnforcementAction::ClampAngularVelocity(a) => {
                assert!((a - 0.4167_f64).abs() < 1e-2,
                    "MRC in-place: expected ~0.4167, got {a}");
            }
            other => panic!("expected ClampAngularVelocity under MRC; got {other:?}"),
        }
    }

    /// `with_angular_bounds` (Scalar) is v-independent — back-compat
    /// confirmation for the H1 enforcement-logic tests.
    #[test]
    fn with_angular_bounds_scalar_back_compat_is_v_independent() {
        let gov = KirraGovernor::new().with_angular_bounds(0.7, 0.3).with_external_rss_gate();
        for v in [0.0_f64, 1.0, 5.0] {
            let proposed = cmd_twist(v, 0.8);
            let action = gov.evaluate(
                &proposed, Some(&cmd_twist(v, 0.0)), 0.05, SafetyPosture::Nominal);
            match action {
                EnforcementAction::ClampAngularVelocity(a) => {
                    assert!((a - 0.7_f64).abs() < 1e-9, "v={v}: expected 0.7, got {a}");
                }
                other => panic!("v={v}: expected ClampAngularVelocity, got {other:?}"),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Issue #70 — Degraded decel-to-stop-and-HOLD (Cruise Oct-2023 SF lesson)
    // -----------------------------------------------------------------------

    /// A STOPPED platform must not re-initiate LINEAR motion under Degraded.
    #[test]
    fn degraded_reinitiation_from_stop_is_denied() {
        let gov = KirraGovernor::new();
        let prev = cmd(0.0); // stopped
        let action = gov.evaluate(&cmd(3.0), Some(&prev), 0.05, SafetyPosture::Degraded);
        assert!(
            matches!(action, EnforcementAction::Deny { .. }),
            "Degraded must deny linear re-initiation from a stop; got {action:?}"
        );
    }

    /// `previous = None` (no history / cold start) is treated as stopped —
    /// a first-cycle Degraded motion command fails closed.
    #[test]
    fn degraded_no_history_treated_as_stopped() {
        let gov = KirraGovernor::new();
        let action = gov.evaluate(&cmd(3.0), None, 0.05, SafetyPosture::Degraded);
        assert!(
            matches!(action, EnforcementAction::Deny { .. }),
            "Degraded with no history must fail closed (hold); got {action:?}"
        );
    }

    /// A LINEAR speed increase while moving is denied under Degraded.
    #[test]
    fn degraded_speed_increase_is_denied() {
        let gov = KirraGovernor::new();
        let prev = cmd(2.0);
        let action = gov.evaluate(&cmd(4.0), Some(&prev), 0.05, SafetyPosture::Degraded);
        assert!(
            matches!(action, EnforcementAction::Deny { .. }),
            "Degraded must deny a linear speed increase; got {action:?}"
        );
    }

    /// A STOPPED platform must not re-initiate ANGULAR motion under Degraded
    /// (Issue #70 FLAG #2 — angular channel gated natively for diff-drive).
    #[test]
    fn degraded_angular_reinitiation_from_stop_is_denied() {
        let gov = legacy_scalar_gov();
        let prev = cmd_twist(0.0, 0.0); // stopped, not rotating
        let proposed = cmd_twist(0.0, 0.3); // re-initiate in-place rotation
        let action = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Degraded);
        assert!(
            matches!(action, EnforcementAction::Deny { .. }),
            "Degraded must deny angular re-initiation from a standstill; got {action:?}"
        );
    }

    /// A decelerating (non-increasing) LINEAR command is admitted under
    /// Degraded — the vehicle is permitted to bleed speed to a stop.
    #[test]
    fn degraded_decel_toward_zero_is_admitted() {
        let gov = KirraGovernor::new();
        let prev = cmd(4.0);
        let action = gov.evaluate(&cmd(2.0), Some(&prev), 0.05, SafetyPosture::Degraded);
        assert!(
            !matches!(action, EnforcementAction::Deny { .. }),
            "Degraded must admit a decelerating command; got {action:?}"
        );
        assert_eq!(effective_velocity(action, 2.0), 2.0,
            "a within-MRC decelerating command passes through unchanged");
    }

    /// Holding at a standstill (0 → 0) on both channels is admitted — the
    /// safe state itself.
    #[test]
    fn degraded_hold_at_stop_is_admitted() {
        let gov = KirraGovernor::new();
        let prev = cmd_twist(0.0, 0.0);
        let action = gov.evaluate(&cmd_twist(0.0, 0.0), Some(&prev), 0.05, SafetyPosture::Degraded);
        assert!(
            matches!(action, EnforcementAction::Allow),
            "Degraded must admit holding at a standstill; got {action:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// #92 — pairwise RSS (checker-over-doer) tests.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod scene_rss_tests {
    use super::{compute_occlusion_cap, compute_scene_rss, impact_evidence_with_vanished, KirraGovernor};
    use parko_core::commands::ControlCommand;
    use parko_core::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};
    use kirra_core::frame_integrity::{FrameIntegrity, FrameIntegrityCfg, LocalizationChannel};
    use parko_core::{
        non_yielding_clearance, AgentScene, ClearanceLoop, ClearanceState, CommitZoneCfg,
        CommitZoneMap, CommitZoneScene, ImpactCfg, ImpactEvidence, ImpactLatch, NonYieldingAgent,
        NonYieldingScene, OcclusionScene, OperatorClearanceGrant, RssAgent, RssParams, RssState,
        TraversalEvidence, VanishedCfg, VanishedObjectDetector, WaterScene, WaterVetoConfig,
        MAX_RSS_AGENTS,
    };

    fn params() -> RssParams {
        RssParams {
            reaction_time: 0.5,
            accel_max: 2.0,
            brake_min: 6.0,
            brake_max: 6.0,
            lat_accel_max: 2.0,
        }
    }

    /// A comfortably-safe IN-LANE lead: a huge longitudinal gap, laterally aligned
    /// (within the footprint-overlap band so the longitudinal axis is live) but with
    /// a side gap above the lateral requirement. No lateral motion. Derived
    /// "longitudinally unsafe" agents inherit this in-lane separation so their
    /// violation is a real same-lane conflict, not an object the ego is clear of.
    fn safe_agent() -> RssAgent {
        RssAgent {
            ego_vel: 10.0,
            lead_vel: 10.0,
            actual_longitudinal_gap_m: 1000.0,
            ego_lat_vel: 0.0,
            obj_lat_vel: 0.0,
            actual_lateral_separation_m: 2.0,
            oncoming: false,
        }
    }

    fn long_unsafe_agent() -> RssAgent {
        RssAgent { actual_longitudinal_gap_m: 0.1, ..safe_agent() }
    }

    fn lat_unsafe_agent() -> RssAgent {
        RssAgent {
            // Stationary longitudinally (tiny required gap) and CLOSE — within
            // RSS_LONGITUDINAL_CONFLICT_M — so the lateral shortfall is a genuine
            // alongside conflict, not a distant object the gate now ignores.
            ego_vel: 0.0,
            lead_vel: 0.0,
            actual_longitudinal_gap_m: 4.0,
            ego_lat_vel: 2.0,
            obj_lat_vel: 2.0,
            actual_lateral_separation_m: 0.0,
            ..safe_agent()
        }
    }

    fn cmd(linear: f64) -> ControlCommand {
        ControlCommand { linear_velocity: linear, angular_velocity: 0.0, timestamp_ms: 0 }
    }

    // --- compute_scene_rss: scene semantics ---

    #[test]
    fn known_empty_is_safe() {
        assert!(compute_scene_rss(&AgentScene::KnownEmpty, &params()).safe);
    }

    #[test]
    fn absent_is_fail_closed_unsafe() {
        assert!(!compute_scene_rss(&AgentScene::Absent, &params()).safe,
            "no perception data must be fail-closed unsafe (absent != clear)");
    }

    #[test]
    fn empty_agents_vector_is_fail_closed() {
        assert!(!compute_scene_rss(&AgentScene::Agents(vec![]), &params()).safe,
            "an empty Agents vector is ambiguous vs KnownEmpty → fail-closed");
    }

    #[test]
    fn all_safe_scene_is_safe() {
        let scene = AgentScene::Agents(vec![safe_agent(), safe_agent(), safe_agent()]);
        assert!(compute_scene_rss(&scene, &params()).safe);
    }

    #[test]
    fn rss_conjunction_admits_a_safe_stationary_queue() {
        // The §4 RSS-conjunction fix (mirroring the kirra-core checker): a stopped ego a safe
        // longitudinal distance behind a stopped object — within the 8 m proximity band,
        // dead-center — is now SAFE. The lateral side-RSS no longer vetoes a longitudinally-safe,
        // laterally-stationary pair via the reaction-time swerve term.
        let queue = RssAgent {
            ego_vel: 0.0,
            lead_vel: 0.0,
            actual_longitudinal_gap_m: 4.0, // safe for two stopped vehicles, within 8 m
            ego_lat_vel: 0.0,
            obj_lat_vel: 0.0,
            actual_lateral_separation_m: 0.0, // dead center
            oncoming: false,
        };
        assert!(
            compute_scene_rss(&AgentScene::Agents(vec![queue]), &params()).safe,
            "a stopped queue (longitudinally safe, laterally still) must be admitted"
        );

        // The veto only NARROWS for genuine stillness — the same close, dead-center pair is
        // STILL unsafe if it is closing laterally (a cut-in) ...
        let cut_in = RssAgent { ego_lat_vel: 2.0, obj_lat_vel: 2.0, ..queue };
        assert!(
            !compute_scene_rss(&AgentScene::Agents(vec![cut_in]), &params()).safe,
            "the same close dead-center pair CLOSING laterally is still vetoed"
        );
        // ... or longitudinally unsafe (abreast).
        let abreast = RssAgent { actual_longitudinal_gap_m: 0.2, ..queue };
        assert!(
            !compute_scene_rss(&AgentScene::Agents(vec![abreast]), &params()).safe,
            "a longitudinally-unsafe (abreast) pair is still vetoed"
        );
    }

    #[test]
    fn scene_rss_catches_a_high_speed_cut_in_beyond_8m_dc2() {
        // #684 in the diverse scene-RSS checker: a high-speed cut-in MORE than the
        // old fixed 8 m ceiling ahead must be unsafe. ego 18 m/s; object 12 m ahead,
        // 3 m to the side (lat_sep ≥ overlap → longitudinal axis clear), closing
        // laterally. Pre-fix `gap > 8` short-circuited `lat_safe = true` → SAFE;
        // post-fix the window is `max(8, required_long)` (~39 m at 18 m/s) → the
        // lateral RSS binds → UNSAFE.
        let cut_in = RssAgent {
            ego_vel: 18.0,
            lead_vel: 0.0,
            actual_longitudinal_gap_m: 12.0, // beyond the old 8 m ceiling
            ego_lat_vel: 0.0,
            obj_lat_vel: 3.0,                 // closing laterally (cut-in)
            actual_lateral_separation_m: 3.0, // 2.5–4 band; ≥ overlap → no rear-end
            oncoming: false,
        };
        assert!(
            !compute_scene_rss(&AgentScene::Agents(vec![cut_in]), &params()).safe,
            "an 18 m/s ego must flag a lateral cut-in 12 m ahead as unsafe (beyond the old 8 m ceiling)"
        );
    }

    #[test]
    fn scene_rss_admits_a_stationary_side_object_beyond_8m_683() {
        // #683 guard: the widened window must NOT over-reject a longitudinally-unsafe
        // but laterally-STILL object in the 2.5–4 band. Zero lateral velocity → small
        // required_lat (= 2·a_lat·ρ² = 1.0 m here) < the 3 m separation → still safe.
        // Same geometry as the cut-in test with zero object velocity.
        let still = RssAgent {
            ego_vel: 18.0,
            lead_vel: 0.0,
            actual_longitudinal_gap_m: 12.0,
            ego_lat_vel: 0.0,
            obj_lat_vel: 0.0, // laterally still
            actual_lateral_separation_m: 3.0,
            oncoming: false,
        };
        assert!(
            compute_scene_rss(&AgentScene::Agents(vec![still]), &params()).safe,
            "a stationary object 3 m to the side stays admitted inside the widened window"
        );
    }

    #[test]
    fn one_unsafe_among_safe_is_worst_case_unsafe() {
        let scene = AgentScene::Agents(vec![safe_agent(), long_unsafe_agent(), safe_agent()]);
        assert!(!compute_scene_rss(&scene, &params()).safe,
            "a single unsafe agent makes the whole-scene verdict unsafe (worst case)");
    }

    // --- oncoming (head-on) agents take the opposite-direction bound ---

    #[test]
    fn oncoming_agent_requires_a_larger_gap_than_a_same_direction_lead() {
        // Same kinematics (both moving 10 m/s), same actual gap — but one is a
        // same-direction lead and one is oncoming. The oncoming head-on bound (sum
        // of both stopping distances) requires a far larger gap, so at a gap that
        // is SAFE for a lead it is UNSAFE for an oncoming vehicle.
        // ~7 m suffices for a 10 m/s same-direction lead; the head-on bound needs
        // ~31 m. 20 m sits between → safe as a lead, unsafe as oncoming.
        let gap = 20.0;
        // In-lane (laterally overlapping) so the longitudinal axis is live for both.
        let lead = RssAgent { ego_vel: 10.0, lead_vel: 10.0, actual_longitudinal_gap_m: gap,
            ego_lat_vel: 0.0, obj_lat_vel: 0.0, actual_lateral_separation_m: 1.5, oncoming: false };
        let onc = RssAgent { oncoming: true, ..lead };

        let lead_state = compute_scene_rss(&AgentScene::Agents(vec![lead]), &params());
        let onc_state = compute_scene_rss(&AgentScene::Agents(vec![onc]), &params());
        assert!(lead_state.safe, "a same-direction lead at {gap} m is safe");
        assert!(!onc_state.safe, "the SAME gap is unsafe for an oncoming (head-on) vehicle");
        assert!(onc_state.longitudinal_margin < lead_state.longitudinal_margin,
            "the head-on bound leaves a smaller (here negative) margin");
    }

    #[test]
    fn oncoming_agent_with_ample_gap_is_safe() {
        // A wide gap clears even the (larger) head-on requirement. In-lane
        // (laterally overlapping) so the head-on longitudinal bound actually applies.
        let onc = RssAgent { ego_vel: 10.0, lead_vel: 10.0, actual_longitudinal_gap_m: 1000.0,
            ego_lat_vel: 0.0, obj_lat_vel: 0.0, actual_lateral_separation_m: 1.5, oncoming: true };
        assert!(compute_scene_rss(&AgentScene::Agents(vec![onc]), &params()).safe,
            "an oncoming vehicle far enough away is safe under the head-on bound");
    }

    // --- both axes evaluated per pair ---

    #[test]
    fn longitudinal_axis_is_evaluated() {
        let scene = AgentScene::Agents(vec![long_unsafe_agent()]);
        assert!(!compute_scene_rss(&scene, &params()).safe, "longitudinal violation must be caught");
    }

    #[test]
    fn lateral_axis_is_evaluated() {
        let scene = AgentScene::Agents(vec![lat_unsafe_agent()]);
        assert!(!compute_scene_rss(&scene, &params()).safe, "lateral violation must be caught");
    }

    /// The lateral defence-in-depth is GATED on longitudinal proximity (the RSS
    /// conjunction, §4): the SAME lateral shortfall that is unsafe when the object
    /// is alongside is SAFE when it is longitudinally far — two vehicles cannot
    /// collide laterally unless also longitudinally close. Before the gate this
    /// over-rejected a lead well ahead / oncoming traffic in the next lane.
    #[test]
    fn a_distant_lateral_shortfall_is_not_a_conflict() {
        // Same lateral shortfall as `lat_unsafe_agent`, but longitudinally FAR
        // (well beyond RSS_LONGITUDINAL_CONFLICT_M) → longitudinally safe → not
        // dangerous.
        let far = RssAgent { actual_longitudinal_gap_m: 50.0, ..lat_unsafe_agent() };
        assert!(compute_scene_rss(&AgentScene::Agents(vec![far]), &params()).safe,
            "a lateral shortfall 50 m ahead is not a collision risk (RSS conjunction)");
        // Control: the SAME shortfall when alongside is still caught.
        assert!(!compute_scene_rss(&AgentScene::Agents(vec![lat_unsafe_agent()]), &params()).safe,
            "the shortfall WHEN ALONGSIDE is still caught (defence-in-depth intact)");
    }

    // --- NaN / non-finite → failsafe → unsafe (agent evaluated, not skipped) ---

    #[test]
    fn nan_velocity_agent_is_failsafe_unsafe() {
        // ego_vel NaN → longitudinal_safe_distance returns RSS_FAILSAFE_DISTANCE_M
        // (1e6 m); the agent's realistic 1000 m gap is < 1e6 → unsafe.
        let agent = RssAgent { ego_vel: f64::NAN, ..safe_agent() };
        assert!(!compute_scene_rss(&AgentScene::Agents(vec![agent]), &params()).safe);
    }

    #[test]
    fn nan_actual_gap_agent_is_unsafe() {
        // A NaN actual gap makes `actual >= required` false → unsafe.
        let agent = RssAgent { actual_longitudinal_gap_m: f64::NAN, ..safe_agent() };
        assert!(!compute_scene_rss(&AgentScene::Agents(vec![agent]), &params()).safe);
    }

    // --- bounded WCET ---

    #[test]
    fn over_max_agents_is_fail_closed() {
        let over = AgentScene::Agents(vec![safe_agent(); MAX_RSS_AGENTS + 1]);
        assert!(!compute_scene_rss(&over, &params()).safe, "more than MAX_RSS_AGENTS → fail-closed");
        let at_cap = AgentScene::Agents(vec![safe_agent(); MAX_RSS_AGENTS]);
        assert!(compute_scene_rss(&at_cap, &params()).safe, "exactly MAX_RSS_AGENTS is still evaluated");
    }

    // --- THE KEY TEST: checker overrides doer ---

    #[test]
    fn checker_over_doer_pairwise_overrides_pushed_safe_bool() {
        let mut gov = KirraGovernor::new();
        // The doer PUSHES safe:true via the old trusted path.
        gov.update_rss_state(RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX });

        // 8 m/s steady: within the Nominal envelope (max 35) but above the MRC
        // ceiling (5) — so a Nominal-safe verdict is `Allow`, while an RSS-unsafe
        // verdict applies the MRC profile (NOT `Allow`).
        let proposed = cmd(8.0);
        let prev = cmd(8.0);
        let unsafe_scene = AgentScene::Agents(vec![long_unsafe_agent()]);

        // The doer's pushed safe:true → trusts the bool → Nominal Allow.
        let doer_pushed = gov.evaluate(&proposed, Some(&prev), 0.05, SafetyPosture::Nominal);
        assert!(matches!(doer_pushed, EnforcementAction::Allow),
            "pushed safe:true yields Nominal Allow, got {doer_pushed:?}");

        // SAME pushed state, but evaluate_scene COMPUTES the verdict from the
        // scene. A clear scene agrees (Allow); the unsafe scene OVERRIDES the
        // pushed bool and applies the MRC profile (not Allow).
        let computed_clear = gov.evaluate_scene(
            &proposed, Some(&prev), 0.05, SafetyPosture::Nominal, &AgentScene::KnownEmpty, &params());
        assert!(matches!(computed_clear, EnforcementAction::Allow),
            "clear computed scene matches the safe verdict, got {computed_clear:?}");

        let computed_unsafe = gov.evaluate_scene(
            &proposed, Some(&prev), 0.05, SafetyPosture::Nominal, &unsafe_scene, &params());
        assert!(!matches!(computed_unsafe, EnforcementAction::Allow),
            "the governor's OWN pairwise computation must override the doer's safe:true \
             and apply the RSS-unsafe MRC profile, got {computed_unsafe:?}");
    }

    // --- RSS rule iv: occlusion speed bound (issue #122) ---

    /// ABSENT occlusion data must fail closed — DISTINCT from KnownClear. Even a
    /// clear agent scene and a pushed safe:true cannot make it Allow.
    #[test]
    fn occlusion_absent_is_fail_closed_unsafe() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX });
        let proposed = cmd(8.0);
        let prev = cmd(8.0);
        let action = gov.evaluate_scene_with_occlusion(
            &proposed, Some(&prev), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &OcclusionScene::Absent, &params());
        assert!(!matches!(action, EnforcementAction::Allow),
            "absent occlusion data must fail closed (not Allow), got {action:?}");
    }

    /// KNOWN-CLEAR sightline imposes no rule-iv bound: with a clear agent scene
    /// the Nominal verdict is Allow (the absent-vs-known distinction matters).
    #[test]
    fn occlusion_known_clear_does_not_bind() {
        let gov = KirraGovernor::new();
        let proposed = cmd(8.0);
        let prev = cmd(8.0);
        let action = gov.evaluate_scene_with_occlusion(
            &proposed, Some(&prev), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &OcclusionScene::KnownClear, &params());
        assert!(matches!(action, EnforcementAction::Allow),
            "a verified-clear sightline must not bind rule iv, got {action:?}");
    }

    /// THE KEY TEST (rule-iv analogue of the pairwise checker-over-doer): a tight
    /// sightline whose cap is below the proposed speed OVERRIDES a pushed
    /// safe:true → MRC profile (not Allow).
    #[test]
    fn checker_over_doer_occlusion_overrides_pushed_safe_bool() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX });
        let proposed = cmd(8.0);
        let prev = cmd(8.0);
        let p = params();
        // 5 m of sightline → cap well below 8 m/s (fixture self-check).
        let tight = OcclusionScene::Limited { d_sight_m: 5.0, v_emerge_max_mps: 0.0 };
        assert!(compute_occlusion_cap(&tight, &p) < 8.0,
            "fixture: the occlusion cap must bind below the proposed 8 m/s");

        let action = gov.evaluate_scene_with_occlusion(
            &proposed, Some(&prev), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &tight, &p);
        assert!(!matches!(action, EnforcementAction::Allow),
            "a proposed speed above the occlusion cap must override pushed safe:true → MRC, got {action:?}");
    }

    /// The override is CONDITIONAL, not blanket: a sightline long enough that the
    /// cap exceeds the proposed speed does not bind → Allow.
    #[test]
    fn occlusion_generous_sightline_allows() {
        let gov = KirraGovernor::new();
        let proposed = cmd(8.0);
        let prev = cmd(8.0);
        let p = params();
        let clear_enough = OcclusionScene::Limited { d_sight_m: 500.0, v_emerge_max_mps: 0.0 };
        assert!(compute_occlusion_cap(&clear_enough, &p) > 8.0,
            "fixture: a 500 m sightline cap must exceed 8 m/s");
        let action = gov.evaluate_scene_with_occlusion(
            &proposed, Some(&prev), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &clear_enough, &p);
        assert!(matches!(action, EnforcementAction::Allow),
            "a sightline long enough must not bind rule iv, got {action:?}");
    }

    /// The bound is never larger than the no-occlusion (KnownClear) case — for
    /// any limited sightline, the cap is <= the unbounded cap.
    #[test]
    fn occlusion_cap_never_exceeds_no_occlusion() {
        let p = params();
        let no_occ = compute_occlusion_cap(&OcclusionScene::KnownClear, &p);
        for d in [5.0_f64, 20.0, 100.0, 1000.0] {
            let lim = compute_occlusion_cap(
                &OcclusionScene::Limited { d_sight_m: d, v_emerge_max_mps: 0.0 }, &p);
            assert!(lim <= no_occ, "limited cap {lim} (d={d}) must be <= no-occlusion {no_occ}");
        }
    }

    // --- SG4 water veto (issue #98) ---

    /// A bounded-safe puddle the planner can drive (short, near visible dry exit,
    /// geometry intact, no flow).
    fn bounded_safe_puddle() -> WaterScene {
        WaterScene::Detected {
            extent_m: 2.0,
            exit_distance_m: Some(4.0),
            flow_detected: false,
            geometry_confirmed: true,
        }
    }

    /// An unbounded water signature (no visible dry exit) — the dangerous case.
    fn unbounded_water() -> WaterScene {
        WaterScene::Detected {
            extent_m: 40.0,
            exit_distance_m: None,
            flow_detected: false,
            geometry_confirmed: true,
        }
    }

    /// THE KEY TEST: a pushed `safe: true` over unbounded water is OVERRIDDEN to
    /// the MRC profile (not Allow) — the governor vetoes the planner's verdict.
    #[test]
    fn checker_over_doer_water_overrides_pushed_safe_bool() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX });
        let proposed = cmd(8.0);
        let prev = cmd(8.0);

        let action = gov.evaluate_scene_with_water(
            &proposed, Some(&prev), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &unbounded_water(), &WaterVetoConfig::default(), &params());
        assert!(!matches!(action, EnforcementAction::Allow),
            "unbounded water must override the planner's safe:true → MRC, got {action:?}");
    }

    /// NO-OVER-STOP proof: a pushed `safe: true` over a bounded-safe puddle is NOT
    /// overridden — the planner proceeds (the governor does not over-stop in rain).
    #[test]
    fn water_bounded_safe_puddle_does_not_override() {
        let gov = KirraGovernor::new();
        let proposed = cmd(8.0);
        let prev = cmd(8.0);
        let action = gov.evaluate_scene_with_water(
            &proposed, Some(&prev), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &bounded_safe_puddle(), &WaterVetoConfig::default(), &params());
        assert!(matches!(action, EnforcementAction::Allow),
            "a bounded-safe puddle must NOT be vetoed (planner drives it), got {action:?}");
    }

    /// The #98 named negative test — "no false-traverse without evidence":
    /// Detected-unbounded water WITHOUT an EarnedTraversable grant must veto
    /// (the untraversable default holds), while the SAME scene with an explicit
    /// map/operator grant is allowed.
    #[test]
    fn no_false_traverse_without_evidence() {
        let gov = KirraGovernor::new();
        let proposed = cmd(8.0);
        let prev = cmd(8.0);

        // Unbounded water, no evidence → veto (not Allow).
        let vetoed = gov.evaluate_scene_with_water(
            &proposed, Some(&prev), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &unbounded_water(), &WaterVetoConfig::default(), &params());
        assert!(!matches!(vetoed, EnforcementAction::Allow),
            "unbounded water without evidence must NOT traverse, got {vetoed:?}");

        // Same situation, but an EXPLICIT earned-traversable grant → allowed.
        let earned = WaterScene::EarnedTraversable { evidence: TraversalEvidence::OperatorAuthorized };
        let allowed = gov.evaluate_scene_with_water(
            &proposed, Some(&prev), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &earned, &WaterVetoConfig::default(), &params());
        assert!(matches!(allowed, EnforcementAction::Allow),
            "an explicit map/operator grant earns traversal, got {allowed:?}");
    }

    /// Unknown water (no healthy detector update) fails closed to a veto — and is
    /// DISTINCT from Clear (which does not veto).
    #[test]
    fn water_unknown_fail_closed_distinct_from_clear() {
        let gov = KirraGovernor::new();
        let proposed = cmd(8.0);
        let prev = cmd(8.0);
        let unknown = gov.evaluate_scene_with_water(
            &proposed, Some(&prev), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &WaterScene::Unknown, &WaterVetoConfig::default(), &params());
        assert!(!matches!(unknown, EnforcementAction::Allow),
            "Unknown water must fail closed (not Allow), got {unknown:?}");
        let clear = gov.evaluate_scene_with_water(
            &proposed, Some(&prev), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &WaterScene::Clear, &WaterVetoConfig::default(), &params());
        assert!(matches!(clear, EnforcementAction::Allow),
            "Clear water must not veto, got {clear:?}");
    }

    // --- SG6 post-collision impact latch (issue #102) ---

    fn latched() -> ImpactLatch {
        let mut l = ImpactLatch::new();
        l.observe(
            &ImpactEvidence { imu_accel_spike_mps2: 0.0, contact_sensor: true, vanished_object: false },
            &ImpactCfg::default(),
        );
        l
    }

    /// THE KEY TEST: while latched, a pushed-safe motion is OVERRIDDEN to
    /// immobilize (Deny) — regardless of the planner's Nominal verdict.
    #[test]
    fn impact_latch_overrides_pushed_safe_to_immobilize() {
        let gov = KirraGovernor::new().with_external_rss_gate();
        let action = gov.evaluate_with_impact_latch(
            &cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal, &latched());
        assert!(matches!(action, EnforcementAction::Deny { .. }),
            "a latched impact must immobilize (Deny), got {action:?}");
    }

    /// Not latched → motion passes through (normal evaluation).
    #[test]
    fn impact_not_latched_passes_through() {
        let gov = KirraGovernor::new().with_external_rss_gate();
        let action = gov.evaluate_with_impact_latch(
            &cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal, &ImpactLatch::new());
        assert!(matches!(action, EnforcementAction::Allow),
            "an un-latched governor passes motion through, got {action:?}");
    }

    /// The SG6 named test — "no resume without clearance": a latch that has seen
    /// further CLEAN evidence (sticky) still immobilizes a pushed-safe motion;
    /// only an explicit clearance releases it.
    #[test]
    fn impact_no_resume_without_clearance() {
        let gov = KirraGovernor::new().with_external_rss_gate();
        let mut l = latched();
        // More clean ticks must NOT release the latch (sticky-toward-safe).
        l.observe(
            &ImpactEvidence { imu_accel_spike_mps2: 0.1, contact_sensor: false, vanished_object: false },
            &ImpactCfg::default(),
        );
        let still = gov.evaluate_with_impact_latch(
            &cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal, &l);
        assert!(matches!(still, EnforcementAction::Deny { .. }),
            "no resume without clearance — still immobilized, got {still:?}");

        // Only an explicit clearance permits motion again.
        l.clear(true);
        let resumed = gov.evaluate_with_impact_latch(
            &cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal, &l);
        assert!(matches!(resumed, EnforcementAction::Allow),
            "after explicit clearance, motion resumes, got {resumed:?}");
    }

    // --- SG6 clearance loop (issue #103) ---

    /// The clearance-loop motion veto immobilizes in BOTH Latched and
    /// EscalationRaised, and releases ONLY after a well-formed operator grant —
    /// the SS-003 structural no-resume at the governor boundary.
    #[test]
    fn clearance_loop_vetoes_in_both_states_and_releases_only_on_grant() {
        let gov = KirraGovernor::new().with_external_rss_gate();
        let mut l = ClearanceLoop::new();
        let contact = ImpactEvidence { imu_accel_spike_mps2: 0.5, contact_sensor: true, vanished_object: false };
        let clean = ImpactEvidence { imu_accel_spike_mps2: 0.5, contact_sensor: false, vanished_object: false };

        // Latching observe → immobilized (post-#328: EscalationRaised in one step) → Deny.
        l.observe(&contact, &ImpactCfg::default(), 1_000);
        assert_eq!(l.state(), ClearanceState::EscalationRaised);
        let d1 = gov.evaluate_with_clearance_loop(&cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal, &l);
        assert!(matches!(d1, EnforcementAction::Deny { .. }), "the latching tick must immobilize, got {d1:?}");

        // Still EscalationRaised on a clean tick → still Deny; clean evidence never resumes.
        l.observe(&clean, &ImpactCfg::default(), 1_001);
        assert_eq!(l.state(), ClearanceState::EscalationRaised);
        let d2 = gov.evaluate_with_clearance_loop(&cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal, &l);
        assert!(matches!(d2, EnforcementAction::Deny { .. }), "EscalationRaised must immobilize, got {d2:?}");

        // A well-formed grant — and ONLY that — releases motion.
        let now = 5_000u64;
        let grant = OperatorClearanceGrant { operator_id: "op-1".to_string(), granted_at_ms: now - 100 };
        l.try_clear(&grant, now, 60_000).expect("well-formed grant clears");
        let resumed = gov.evaluate_with_clearance_loop(&cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal, &l);
        assert!(matches!(resumed, EnforcementAction::Allow), "after the grant, motion resumes, got {resumed:?}");
    }

    /// SG6 end-to-end (#102 follow-up): a close agent, then a next valid frame
    /// empty within the band → DERIVED `vanished_object` → the impact latch fires
    /// ALONE (no IMU spike, no contact) → the motion veto immobilizes.
    #[test]
    fn vanished_object_derivation_latches_alone_and_vetoes() {
        let gov = KirraGovernor::new();
        let mut detector = VanishedObjectDetector::new();
        let vcfg = VanishedCfg::default();
        let mut latch = ImpactLatch::new();

        let close = AgentScene::Agents(vec![RssAgent {
            ego_vel: 0.0, lead_vel: 0.0, actual_longitudinal_gap_m: 1.0,
            ego_lat_vel: 0.0, obj_lat_vel: 0.0, actual_lateral_separation_m: 100.0,
            oncoming: false,
        }]);

        // Tick 1 — close agent: derives vanished=false (obligation opens), no latch.
        let ev1 = impact_evidence_with_vanished(&mut detector, &close, 0, &vcfg, 0.0, false);
        assert!(!ev1.vanished_object);
        latch.observe(&ev1, &ImpactCfg::default());
        assert!(!latch.is_latched());

        // Tick 2 — KnownEmpty within the band: derives vanished=true with NO IMU
        // spike and NO contact → the latch fires on the vanished flag ALONE.
        let ev2 = impact_evidence_with_vanished(&mut detector, &AgentScene::KnownEmpty, 100, &vcfg, 0.0, false);
        assert!(ev2.vanished_object, "small-band empty after a close agent must derive vanished");
        assert!(!ev2.contact_sensor && ev2.imu_accel_spike_mps2 == 0.0, "latches on vanished ALONE");
        latch.observe(&ev2, &ImpactCfg::default());
        assert!(latch.is_latched(), "vanished_object latches alone per SG6");

        // The motion veto immobilizes a pushed-safe command.
        let action = gov.evaluate_with_impact_latch(&cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal, &latch);
        assert!(matches!(action, EnforcementAction::Deny { .. }),
            "a derived vanished-object latch must immobilize, got {action:?}");
    }

    // --- SG5 commit-zone veto (issue #106) ---

    fn healthy_zone_map(distance_m: f64) -> CommitZoneMap {
        CommitZoneMap {
            zone_ahead: true,
            distance_to_zone_m: distance_m,
            confidence: 0.95,
            age_ms: 50,
            min_confidence: 0.5,
            max_age_ms: 1_000,
        }
    }

    /// A blocked commit zone (within horizon, clearance not confirmed).
    fn blocked_zone() -> CommitZoneScene {
        CommitZoneScene::ZoneAhead {
            map: healthy_zone_map(50.0),
            clearance_confirmed: false,
            exit_verified: true,
            zone_length_m: 30.0,
            proposed_stop_distance_m: None,
        }
    }

    /// THE KEY TEST: a pushed `safe: true` into a blocked commit zone is
    /// OVERRIDDEN to the MRC profile (not Allow) — stop short of the zone.
    #[test]
    fn checker_over_doer_commit_zone_overrides_pushed_safe_bool() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX });
        let action = gov.evaluate_scene_with_commit_zone(
            &cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &blocked_zone(), &CommitZoneCfg::default(), &params());
        assert!(!matches!(action, EnforcementAction::Allow),
            "a blocked commit zone must override the planner's safe:true → MRC, got {action:?}");
    }

    /// A healthy, clearance-confirmed, exit-verified zone passes through (no
    /// over-block — entry is permitted).
    #[test]
    fn commit_zone_confirmed_clear_passes_through() {
        let gov = KirraGovernor::new();
        let confirmed = CommitZoneScene::ZoneAhead {
            map: healthy_zone_map(50.0), clearance_confirmed: true, exit_verified: true,
            zone_length_m: 30.0, proposed_stop_distance_m: None,
        };
        let action = gov.evaluate_scene_with_commit_zone(
            &cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &confirmed, &CommitZoneCfg::default(), &params());
        assert!(matches!(action, EnforcementAction::Allow),
            "a confirmed-clear zone must permit entry, got {action:?}");
    }

    /// SG5 stop-inside at the integration layer: a confirmed, healthy zone whose
    /// proposed plan STOPS inside it overrides a pushed safe:true → MRC.
    #[test]
    fn commit_zone_stop_inside_overrides_pushed_safe() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX });
        // zone [50, 80]; plan stops at 65 — inside.
        let stop_inside = CommitZoneScene::ZoneAhead {
            map: healthy_zone_map(50.0), clearance_confirmed: true, exit_verified: true,
            zone_length_m: 30.0, proposed_stop_distance_m: Some(65.0),
        };
        let action = gov.evaluate_scene_with_commit_zone(
            &cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &stop_inside, &CommitZoneCfg::default(), &params());
        assert!(!matches!(action, EnforcementAction::Allow),
            "a plan that stops inside the zone must override safe:true → MRC, got {action:?}");
    }

    /// Reject-from-map-alone at the integration layer: an `Unknown` (absent /
    /// unhealthy) map overrides a pushed safe:true, no live perception needed.
    #[test]
    fn commit_zone_unknown_map_overrides_pushed_safe() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX });
        let action = gov.evaluate_scene_with_commit_zone(
            &cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &CommitZoneScene::Unknown, &CommitZoneCfg::default(), &params());
        assert!(!matches!(action, EnforcementAction::Allow),
            "an absent/unhealthy map must override pushed safe:true (Reject from map alone), got {action:?}");
    }

    // --- #123 localization-integrity gate over the map-anchored checks ------

    /// Commit-zone path: a pushed safe:true with a HEALTHY, confirmed zone that
    /// would normally pass — but under UNTRUSTED localization the scene degrades
    /// to Unknown and the verdict is overridden (stop short). The G2 AoU complement.
    #[test]
    fn localization_untrusted_overrides_healthy_commit_zone() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX });
        // A confirmed, healthy, exit-verified zone — passes through when trusted.
        let confirmed = CommitZoneScene::ZoneAhead {
            map: healthy_zone_map(50.0), clearance_confirmed: true, exit_verified: true,
            zone_length_m: 30.0, proposed_stop_distance_m: None,
        };
        // Sanity: trusted localization → the confirmed zone passes through.
        let trusted = FrameIntegrity::Reported {
            localization: LocalizationChannel { lateral_error_95_m: 0.05, age_ms: 50 },
        };
        let ok = gov.evaluate_scene_with_commit_zone_localized(
            &cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &confirmed, &CommitZoneCfg::default(),
            &trusted, &FrameIntegrityCfg::default(), &params());
        assert!(matches!(ok, EnforcementAction::Allow),
            "a confirmed zone under trusted localization must pass, got {ok:?}");
        // Untrusted (absent report) → scene degrades to Unknown → overridden.
        let untrusted = FrameIntegrity::Unknown;
        let action = gov.evaluate_scene_with_commit_zone_localized(
            &cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &confirmed, &CommitZoneCfg::default(),
            &untrusted, &FrameIntegrityCfg::default(), &params());
        assert!(!matches!(action, EnforcementAction::Allow),
            "untrusted localization must override a healthy commit zone → MRC, got {action:?}");
    }

    /// Water path: a mapped-ford earn-back (MapKnownSafe) that normally permits
    /// traversal is STRIPPED under untrusted localization → the veto fires.
    #[test]
    fn localization_untrusted_vetoes_mapknownsafe_ford() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX });
        let ford = WaterScene::EarnedTraversable { evidence: TraversalEvidence::MapKnownSafe };
        let wcfg = WaterVetoConfig { max_exit_distance_m: 5.0, max_puddle_extent_m: 5.0 };
        // Trusted → the mapped ford permits traversal.
        let trusted = FrameIntegrity::Reported {
            localization: LocalizationChannel { lateral_error_95_m: 0.05, age_ms: 50 },
        };
        let ok = gov.evaluate_scene_with_water_localized(
            &cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &ford, &wcfg, &trusted, &FrameIntegrityCfg::default(), &params());
        assert!(matches!(ok, EnforcementAction::Allow),
            "a mapped ford under trusted localization must pass, got {ok:?}");
        // Untrusted → MapKnownSafe stripped → veto.
        let untrusted = FrameIntegrity::Unknown;
        let action = gov.evaluate_scene_with_water_localized(
            &cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &ford, &wcfg, &untrusted, &FrameIntegrityCfg::default(), &params());
        assert!(!matches!(action, EnforcementAction::Allow),
            "untrusted localization must strip the MapKnownSafe ford → veto, got {action:?}");
    }

    /// The asymmetry crux at the integration layer: an OperatorAuthorized grant
    /// SURVIVES untrusted localization (it is not map-frame-dependent).
    #[test]
    fn localization_untrusted_preserves_operator_authorized_ford() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX });
        let ford = WaterScene::EarnedTraversable { evidence: TraversalEvidence::OperatorAuthorized };
        let wcfg = WaterVetoConfig { max_exit_distance_m: 5.0, max_puddle_extent_m: 5.0 };
        let untrusted = FrameIntegrity::Unknown;
        let action = gov.evaluate_scene_with_water_localized(
            &cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &ford, &wcfg, &untrusted, &FrameIntegrityCfg::default(), &params());
        assert!(matches!(action, EnforcementAction::Allow),
            "operator authority must survive untrusted localization, got {action:?}");
    }

    /// SG5 trio end-to-end (#260 map-anchored block + #107 exit-clearance + #108
    /// non-yielding clearance): a train that arrives before the ego clears →
    /// DERIVED clearance_confirmed=false → blocked → overrides pushed safe:true.
    #[test]
    fn commit_zone_non_yielding_train_overrides_pushed_safe() {
        let mut gov = KirraGovernor::new();
        gov.update_rss_state(RssState { safe: true, longitudinal_margin: f64::MAX, lateral_margin: f64::MAX });
        let cfg = CommitZoneCfg::default();
        let map = healthy_zone_map(50.0);
        // train: arrival 60/10 = 6.0 s < ego clear (8.45 s) → NOT clear.
        let scene = NonYieldingScene::Agents(vec![NonYieldingAgent {
            approach_velocity_mps: 10.0, distance_to_conflict_m: 60.0,
        }]);
        let clearance = non_yielding_clearance(&scene, &map, 30.0, 10.0, &cfg);
        assert!(!clearance, "the train must defeat clearance (derived false)");
        // exit verified independently, but clearance is false → blocked.
        let zone = CommitZoneScene::ZoneAhead {
            map, clearance_confirmed: clearance, exit_verified: true,
            zone_length_m: 30.0, proposed_stop_distance_m: None,
        };
        let action = gov.evaluate_scene_with_commit_zone(
            &cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &zone, &cfg, &params());
        assert!(!matches!(action, EnforcementAction::Allow),
            "a non-yielding train must override safe:true → MRC (stop short), got {action:?}");
    }

    /// Inverse: train arrives well AFTER the ego clears + exit verified → DERIVED
    /// clearance_confirmed=true → permitted (no over-block).
    #[test]
    fn commit_zone_non_yielding_clear_passes_through() {
        let gov = KirraGovernor::new();
        let cfg = CommitZoneCfg::default();
        let map = healthy_zone_map(50.0);
        // train: arrival 200/10 = 20.0 s > 8.45 + 2.0 = 10.45 s → clear.
        let scene = NonYieldingScene::Agents(vec![NonYieldingAgent {
            approach_velocity_mps: 10.0, distance_to_conflict_m: 200.0,
        }]);
        let clearance = non_yielding_clearance(&scene, &map, 30.0, 10.0, &cfg);
        assert!(clearance, "an agent arriving well after the ego clears must be clear");
        let zone = CommitZoneScene::ZoneAhead {
            map, clearance_confirmed: clearance, exit_verified: true,
            zone_length_m: 30.0, proposed_stop_distance_m: None,
        };
        let action = gov.evaluate_scene_with_commit_zone(
            &cmd(8.0), Some(&cmd(8.0)), 0.05, SafetyPosture::Nominal,
            &AgentScene::KnownEmpty, &zone, &cfg, &params());
        assert!(matches!(action, EnforcementAction::Allow),
            "a non-yielding-clear, exit-verified zone must permit entry, got {action:?}");
    }
}
