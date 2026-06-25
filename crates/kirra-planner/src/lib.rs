//! kirra-planner — Occy autonomy planner, **Phase-0 interface lock** (#89 / Occy 0.A).
//!
//! This crate is the **scaffold** that locks the Phase-0 planner interfaces so the
//! Occy Phase-1 chain (#90–#93, CARLA-blocked) can build against a stable shape.
//! It is **not** a real planner.
//!
//! # Derivation, not invention
//!
//! The #89 issue body predates a checker side that now fully exists on main. The
//! interfaces here are therefore **derived from current main, never copied from the
//! issue**. The load-bearing fact: the planner's job is to **propose** a trajectory
//! that the **existing checker** consumes — it does not check, and it does not
//! redefine the checker's types.
//!
//! - The checker entry is [`kirra_ros2_adapter::validate_trajectory_slow`] (the
//!   **#131** per-trajectory containment path), which consumes `&[TrajectoryPoint]`.
//!   So [`PlanOutput`] carries exactly `Vec<TrajectoryPoint>` — the same type,
//!   imported, never redefined.
//! - Posture is [`kirra_core::FleetPosture`].
//! - **The planner does NOT produce scenes.** Scenes are perception-side inputs
//!   (`parko_kirra::…evaluate_scene*`); the planner consumes a world-state.
//!
//! # Phase-0 finding (surfaced, not fixed)
//!
//! The checked trajectory type (`TrajectoryPoint`) and the validation entry live in
//! the `kirra-ros2-adapter` crate — a downstream integration layer. A planner
//! depending on the adapter inverts the natural direction and pulls the whole SDK +
//! adapter. **Proposal (NOT done here):** promote the trajectory contract + the
//! validation entry to a lean shared home (e.g. a `kirra-trajectory` crate, or the
//! SDK gateway) so the planner depends on the *contract*, not the integration crate.
//! Until then we **import** the real type — the held line: no parallel redefinition.

// Import (never redefine) the locked upstream types. Re-exported so a Phase-1
// consumer names them from one place — but they remain the adapter's / SDK's
// definitions.
pub use kirra_core::trajectory::{PerceivedObject, Pose, TrajectoryPoint, TrajectoryVerdict};
// FleetPosture + the containment cap now live in the lean `kirra-core` crate
// (de-monolith Stage 4) — same types, no heavy verifier-service tree pulled directly.
pub use kirra_core::FleetPosture;

// Build hygiene (review M3): import the corridor seam from the lean `kirra-core`
// (Stage 6a) rather than the heavy adapter, so the planner's library no longer
// pulls `kirra-ros2-adapter` (and its ros2/tokio tree). The adapter stays a
// dev-dependency — its `validate_trajectory_slow` checker entry is used only by tests.
use kirra_core::corridor::{CorridorSource, Point};
// Derive (never guess) the checker's hard trajectory-length cap: the #131
// containment gate rejects `len > MAX_TRAJECTORY_HORIZON`, so a proposal must
// stay within it (including the terminal stop point) to be admissible.
use kirra_core::containment::MAX_TRAJECTORY_HORIZON;

pub mod behavior;
pub use behavior::{
    Behavioral, BehaviorConfig, LaneBoundary, LineType, SignalState, TrafficControl,
};

pub mod lanemap;
pub use lanemap::{
    JunctionContext, Lane, LaneControl, LaneCorridor, LaneEdge, LaneGraph, Occluder,
    MAX_ROUTE_LANES,
};

pub mod lanelet2;
pub use lanelet2::{parse_lanelet2_osm, Lanelet2ParseError};

pub mod mick;
pub use mick::{
    mick_drive_once, plan_for_intent, MickBrain, MickDriver, MickError, MickIntent, ObjectView,
    ScriptedBrain, TurnDirection, WorldContext, DEFAULT_DECIDE_INTERVAL_MS,
    DEFAULT_INTENT_STALENESS_MS, MICK_MAX_OBJECTS,
};

/// Mick's model-agnostic LLM brain (prompt render + reply parse behind the `MickBrain`
/// seam). Pure + testable with a `MockModel`; a concrete `ModelClient` (local Gemma via
/// Ollama, etc.) plugs in behind a feature/crate.
pub mod mick_llm;

/// Mick decision capture — the eval side-channel logging intent → grounding → verdict to
/// JSONL for offline scoring of the brain against the checker. Observability only.
pub mod mick_capture;
pub use mick_capture::{IntentStats, MickDecisionRecord, MickEvalLog, MickEvalSummary};
pub use mick_llm::{build_prompt, intent_schema, LlmBrain, MockModel, ModelClient, ModelError};

pub mod learned;
pub use learned::{LearnedManeuverPlanner, LearnedPlanner, Teacher};

/// Fast-loop trajectory tracker — the System-1 conformance side of the dual-rate loop, which
/// follows one admitted trajectory by elapsed time across many fast ticks.
pub mod fast_loop;
pub use fast_loop::{FastLoopTracker, TrackedCommand};

/// What set the binding travel limit `s_limit` — selects the speed/brake policy:
/// `Lead` matches & follows (rolling gap, no brake-to-zero); `ObjectStop`,
/// `Behavioral` and `Yield` decelerate to a hard stop; `Goal` cruises to the goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LimitKind {
    Goal,
    ObjectStop,
    Lead,
    Behavioral,
    /// Predicted-conflict yield: a crossing object will enter the lane ahead.
    Yield,
    /// Commanded pull-over: decel to a controlled stop at the road edge.
    PullOver,
}

/// Ego world-state the planner consumes.
///
/// `// PHASE-0 LOCKED` — derived from `kirra_ros2_adapter::state::EgoOdom`
/// (`linear_x_mps`, `yaw_rate_rads`, `stamp_ms`), plus the ego `pose`. The pose is
/// **integrator / localization sourced** (the SDK localization-integrity gate,
/// AOU-LOCALIZATION-001, owns its trustworthiness — not this crate).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EgoState {
    pub pose: Pose,
    pub linear_x_mps: f64,
    pub yaw_rate_rads: f64,
    pub stamp_ms: u64,
}

/// The planning goal.
///
/// `// PHASE-0 LOCKED` — Phase-0 shape is a target pose; **integrator / mission
/// sourced**. Richer goal forms (route, behavior intent) are later-slice work.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Goal {
    pub target: Pose,
}

/// Per-object kinematic motion state the checker contract ([`PerceivedObject`])
/// doesn't carry. Keyed by the object's `id`; supplies the **yaw rate** so the
/// planner can predict a turning object on the CTRV model (matches the Taj
/// tracker's estimate). `yaw_rate_rad_s = 0` reduces to constant-velocity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionState {
    pub id: u64,
    pub yaw_rate_rad_s: f64,
}

/// Per-object **intention-aware predicted path** the tracker/integrator supplies
/// for predictive yielding. Keyed by the object's `id`; `points` is the object's
/// expected future positions in world frame (e.g. its **lane centerline ahead** —
/// the lane-following intent — derived from the map the tracker holds). When
/// present, the planner rolls the object ALONG this path at its current speed
/// instead of extrapolating its instantaneous velocity (CTRV/CV): a vehicle keeping
/// its own (adjacent / opposing / curving) lane is no longer mis-predicted as
/// drifting into the ego lane, so it does not trigger a spurious yield. An object
/// with no entry falls back to the CTRV kinematic rollout, as before. `< 2` points
/// is treated as "no path" (fail-safe → CTRV). The planner only *consumes* this;
/// the intent reasoning lives where the map does, and KIRRA still backstops.
#[derive(Debug, Clone, Copy)]
pub struct PredictedPath<'a> {
    pub id: u64,
    pub points: &'a [Point],
}

/// World-state input to [`Planner::plan`].
///
/// `// PHASE-0 LOCKED` — derived from the checker's own consumed inputs: ego
/// state, the drivable-space handle (the **same** [`CorridorSource`] trait
/// `validate_trajectory_slow` consumes), and the fleet posture. Borrowed `map`
/// keeps it allocation-free and lets the planner and the checker read one corridor.
///
/// `Clone` is cheap — every field is a `Copy` reference or a small `Copy`/`Clone`
/// value — and lets the Mick intent bridge build a plan that overrides only the
/// goal/maneuver while re-borrowing the same perception-derived world.
#[derive(Clone)]
pub struct PlanInput<'a> {
    pub ego: EgoState,
    pub goal: Goal,
    /// Drivable-space handle — the same `CorridorSource` the checker re-reads.
    pub map: &'a dyn CorridorSource,
    /// Perceived obstacles — the **same** [`PerceivedObject`] slice the checker
    /// runs RSS against. Phase-1 perception input (the Phase-0 lock predated an
    /// obstacle-aware planner): [`GeometricPlanner`] decelerates to a controlled
    /// stop short of the nearest in-path object. An empty slice = no obstacles.
    pub objects: &'a [PerceivedObject],
    /// Active traffic controls (signs / signals) the planner must OBEY — the
    /// behavioral/legal layer (distinct from KIRRA's physical authority). An
    /// empty slice = no controls. See [`behavior`].
    pub controls: &'a [TrafficControl],
    /// Lane-line boundaries (lateral offsets from the path centerline) whose
    /// crossing rules gate the lateral-avoidance maneuver: Occy will not route
    /// around an object across a solid line. An empty slice = unconstrained.
    pub lane_boundaries: &'a [LaneBoundary],
    /// Per-object motion state (yaw rate) the checker contract can't carry — lets
    /// predictive yielding roll objects forward on the turn-aware CTRV model
    /// (from the Taj tracker) instead of constant-velocity. An object with no
    /// entry (or an empty slice) predicts straight-line (CV), as before.
    pub motion: &'a [MotionState],
    /// Per-object intention-aware predicted paths (see [`PredictedPath`]) — when an
    /// object has one, predictive yielding rolls it along that path (lane-following
    /// intent) instead of the CTRV kinematic tangent, suppressing spurious yields to
    /// vehicles keeping their own lane. An object with no entry (or an empty slice)
    /// uses the CTRV rollout, as before.
    pub predicted_paths: &'a [PredictedPath<'a>],
    /// **Junction right-of-way**: object ids that must yield TO the ego at the
    /// junction the ego is negotiating (the ego has priority over them). The
    /// predictive yield SKIPS these — the ego asserts its right-of-way and proceeds
    /// rather than waiting for traffic that must cede. Right-of-way is determined
    /// upstream by the behavioral/map layer (stop/yield signs + lane priority), like
    /// the other legal rules; Occy only executes it. The ego still yields (space-time)
    /// to every other crossing agent, and KIRRA still backstops any agent that is an
    /// imminent collision. Empty = no priority asserted (yield to all crossings).
    pub cedes_to_ego_ids: &'a [u64],
    /// Commanded lane change: a target lateral offset from the current path
    /// centerline to shift to (e.g. the adjacent lane center). Honored only if the
    /// lane-line rules permit crossing to that side and the corridor fits;
    /// otherwise the planner stays in lane. `None` = no lane change.
    pub lane_change_to_m: Option<f64>,
    /// Object ids that must **NOT be passed** — chiefly a **stopped school bus**
    /// actively loading/unloading (red lights flashing, stop arm extended). US law
    /// (MUTCD §7D; every state's stop-for-school-bus statute) requires traffic to
    /// **stop and not overtake** such a bus — on an undivided road in *both*
    /// directions. This is a **LEGAL** constraint, so it lives in Occy's behavioral
    /// layer, not KIRRA: passing a bus is not a *collision*, so the physical
    /// governor never enforces it (cf. running a red light). The integrator /
    /// perception flags the id; Occy then refuses any route-around or overtake of
    /// it and holds behind it (stop-short). An empty slice = no such restriction.
    ///
    /// Scope (honest): this gates the *pass*; resuming when the bus's lights clear
    /// is the integrator dropping the id. The divided-highway exception (oncoming
    /// traffic may proceed past a median) is not modeled — fail-safe is to stop.
    pub no_overtake_ids: &'a [u64],
    /// Optional **drivable area** distinct from the reference corridor `map` — the
    /// wider space an **overtake** may briefly borrow (e.g. the full undivided road
    /// when `map` is just the ego lane). `map` stays the reference the path follows
    /// and within-lane route-arounds fit; `drivable`, when present, is the extra
    /// area a cross-centerline pass may use. `None` → no overtake is attempted and
    /// behavior is identical to before (the reference corridor is the only space).
    /// This is the planner-side of Autoware's *reference-path vs drivable-area*
    /// split; the integrator must pass the SAME corridor to the checker so KIRRA
    /// independently bounds the oncoming traffic the pass exposes (head-on RSS).
    pub drivable: Option<&'a dyn CorridorSource>,
    /// Fleet posture → planner mode (see [`planner_mode`]).
    pub posture: FleetPosture,
    /// **Requested cruise speed** (m/s), if any — Mick's "ease off here" / "you can go up
    /// to the limit" knob. A `Some` value can only LOWER the posture-derived cruise
    /// ceiling (`min(ceiling, request)`): the caller can slow the chauffeur but can NEVER
    /// raise it above the configured envelope, and KIRRA still caps independently. A
    /// non-finite or negative request is ignored (fail-safe → the ceiling stands). In
    /// `Degraded` it only lowers an already non-increasing target, preserving the
    /// decel-only invariant. `None` = use the posture ceiling (byte-for-byte prior behavior).
    pub target_speed_mps: Option<f64>,
    /// **Requested overtake** — Mick's "pass the slow/stopped lead ahead" knob. When `true`
    /// AND a `drivable` area is supplied, the planner attempts the cross-centerline pass
    /// (`compute_overtake_bump`) *discretionarily* — not only when a within-lane route-around
    /// fails. The pass still must fit the drivable area, cross a crossable lane line, and
    /// clear the checker's lateral band; if it can't, the planner falls back to the
    /// within-lane behavior. KIRRA independently bounds the pass (head-on RSS), so a request
    /// to overtake into oncoming traffic is refused downstream regardless. `false` =
    /// byte-for-byte prior behavior (overtake fires only when within-lane can't clear it).
    pub request_overtake: bool,
    /// **Requested pull-over** — Mick's "get to the road edge and stop" knob (e.g. to
    /// yield to an emergency vehicle, or a commanded curb stop). When `true` the planner
    /// shifts as far **right** as containment admits — onto the `drivable` shoulder if one
    /// is supplied, else to the reference corridor's right edge — and decelerates to a
    /// controlled stop there (`compute_pull_over_bump` + a `PullOver` stop limit). Honored
    /// only if the lane line permits the rightward move and the shifted footprint fits the
    /// corridor; otherwise the ego stays in lane. A nearer object/behavioral stop still
    /// binds first (never drives past a hazard to finish parking), and KIRRA independently
    /// bounds the maneuver. `false` = byte-for-byte prior behavior.
    pub request_pull_over: bool,
    /// **Lane graph** for junction routing, if available — the substrate Mick's `TurnAt`
    /// intent grounds against. When `Some`, `plan_for_intent` can resolve the ego lane from
    /// its pose, pick the direction's turn branch (successor by heading), route through it,
    /// and follow the materialized route corridor (`LaneGraph::route_corridor`) through the
    /// turn. `None` = no junction routing (a `TurnAt` intent then fails closed to HOLD), and
    /// byte-for-byte prior behavior for every other intent. The planner's own corridor-
    /// following / containment is unchanged; this only supplies the route corridor a turn
    /// needs, which KIRRA bounds exactly as it bounds any corridor.
    pub lane_graph: Option<&'a LaneGraph>,
    /// **Live traffic-signal states** by governed lane id `(lane_id, state)` — the dynamic
    /// input a `LaneControl::TrafficLight` needs (perception / V2X). When the ego's lane
    /// carries a traffic light, its state is looked up here; **absent → red (stop),
    /// fail-closed**. Empty = no signal info (a light with no state then reads red). Ignored
    /// for lanes with no light. Only consulted when `lane_graph` is set and the integrator
    /// did not hand-supply `controls`.
    pub signal_states: &'a [(u64, behavior::SignalState)],
}

/// Intent label on a proposal.
///
/// **AUDIT-ONLY.** Like #89's `command_source`, it MUST NOT relax the checker —
/// the checker never sees it (`validate_trajectory_slow` takes only the
/// trajectory). It records what the planner *intended*, nothing more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalKind {
    Motion,
    SafeStop,
}

/// A trajectory proposal — **exactly** the shape the #131 checker consumes.
///
/// `// PHASE-0 LOCKED` — `trajectory` is `Vec<TrajectoryPoint>`, the input type of
/// [`kirra_ros2_adapter::validate_trajectory_slow`]. No curvature / accel / metadata
/// fields are added: the checked `TrajectoryPoint` is `{pose, velocity_mps,
/// time_from_start_s}`, and the checker derives per-pose deltas itself. (The #89
/// "Trajectory {…curvature, accel, horizon, metadata}" shape is **not** the checked
/// shape — main wins; see the PR divergence table.)
#[derive(Debug, Clone, PartialEq)]
pub struct PlanOutput {
    pub trajectory: Vec<TrajectoryPoint>,
    pub kind: ProposalKind,
}

impl PlanOutput {
    // SAFETY: occy planner stop-proposal invariant | REQ: Occy-0.A (#89) | TEST: kirra_planner::tests::{safe_stop_is_valid_stop_proposal, stop_planner_output_feeds_the_checker}
    /// The always-available safe-stop / MRC proposal.
    ///
    /// `// PHASE-0 LOCKED — the stop-proposal invariant.` A planner MUST always be
    /// able to propose stopping: the checker may veto every *motion* proposal, but
    /// the architecture needs a safe-stop proposal to fall back to — **a planner
    /// with no stop output deadlocks it.** This constructor guarantees one exists.
    ///
    /// Produces ≥ 2 zero-velocity points holding `at` (the checker requires ≥ 2
    /// points; a held pose at 0 m/s is the controlled stop-and-hold).
    #[must_use]
    pub fn safe_stop(at: Pose) -> Self {
        let trajectory = vec![
            TrajectoryPoint { pose: at, velocity_mps: 0.0, time_from_start_s: 0.0 },
            TrajectoryPoint { pose: at, velocity_mps: 0.0, time_from_start_s: 0.1 },
        ];
        PlanOutput { trajectory, kind: ProposalKind::SafeStop }
    }
}

/// The planner contract.
///
/// `// PHASE-0 LOCKED` — derived from the checker consumer
/// (`validate_trajectory_slow`): a planner takes a world-state and **proposes** a
/// trajectory; the checker decides. Object-safe so Phase-1 may hold `Box<dyn
/// Planner>`.
pub trait Planner {
    fn plan(&mut self, input: &PlanInput<'_>) -> PlanOutput;
}

/// Planner operating mode, derived from fleet posture (#89 "FleetPosture →
/// planner-mode").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannerMode {
    /// `Nominal` → full planning.
    Full,
    /// `Degraded` → conservative planning.
    Conservative,
    /// `LockedOut` → MRC-only: the planner may only propose safe-stop.
    MrcOnly,
}

// PHASE-0 LOCKED — derived from kirra_core::FleetPosture.
/// Map fleet posture to planner mode.
#[must_use]
pub fn planner_mode(posture: FleetPosture) -> PlannerMode {
    match posture {
        FleetPosture::Nominal => PlannerMode::Full,
        FleetPosture::Degraded => PlannerMode::Conservative,
        FleetPosture::LockedOut => PlannerMode::MrcOnly,
    }
}

/// Trivial reference planner: **always** proposes safe-stop.
///
/// NOT a real planner — it exists to prove the locked interfaces are constructible
/// and consumable: it compiles against the trait, feeds the real checker, and
/// satisfies the stop-proposal invariant.
#[derive(Debug, Default, Clone, Copy)]
pub struct StopPlanner;

impl Planner for StopPlanner {
    fn plan(&mut self, input: &PlanInput<'_>) -> PlanOutput {
        // Always able to stop — holds the ego pose at zero velocity.
        PlanOutput::safe_stop(input.ego.pose)
    }
}

// ---------------------------------------------------------------------------
// Phase-1 geometric reference planner (#90 — Occy 1.A).
// ---------------------------------------------------------------------------

/// Speed at/under which a proposal is "stopped" — mirrors the SDK
/// `STOP_EPSILON_MPS` Degraded HOLD threshold. A terminal point at or below
/// this is the controlled stop-and-hold.
const STOP_EPSILON_MPS: f64 = 0.05;

/// Arc-length resolution (m) of the forward–backward velocity profile. Fine enough
/// to resolve a curve's deceleration, coarse enough to keep the two passes cheap.
const VELOCITY_PROFILE_DS: f64 = 0.5;

/// Tunables for [`GeometricPlanner`].
///
/// Defaults stay **inside** `VehicleConfig::default_urban` kinematic limits
/// (accel ≤ 2.5, decel ≤ 4.5, speed ≤ 35 m/s) so a nominal in-corridor proposal
/// is *checker-admissible* (`Accept`/`Clamp`), not merely consumable. The
/// planner still PROPOSES — the checker is the authority — but a planner whose
/// nominal output the real checker refuses is not a useful reference.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeometricPlannerConfig {
    /// Nominal (`Full`) cruise speed target.
    pub cruise_speed_mps: f64,
    /// `Degraded`/`Conservative` derate: target = `cruise * factor`, additionally
    /// clamped to be **non-increasing** vs. the ego's current speed (decel-only).
    pub conservative_factor: f64,
    /// Acceleration cap used to ramp toward the target speed.
    pub max_accel_mps2: f64,
    /// Deceleration cap used to taper to a controlled stop at the goal.
    pub max_decel_mps2: f64,
    /// Time spacing between emitted trajectory points.
    pub sample_dt_s: f64,
    /// Horizon cap — bounds the proposal allocation (rolling-horizon planning).
    pub max_points: usize,
    /// Travel remaining at/under which the goal is "reached" → controlled stop.
    pub goal_tolerance_m: f64,
    /// Lateral distance from the path centerline within which an object counts as
    /// "in my path" (→ stop short of it). Objects farther off-axis are ignored by
    /// the planner (the checker's RSS still backstops them).
    pub object_lane_tolerance_m: f64,
    /// Longitudinal gap left between the controlled stop and the nearest in-path
    /// object — the planner stops this far short of it.
    pub object_stop_gap_m: f64,
    /// Longitudinal gap left before a PREDICTED crossing / cut-in conflict (the
    /// [`predict_yield_s`](GeometricPlanner::predict_yield_s) stop-line), distinct from the
    /// static `object_stop_gap_m`. It is larger because a crossing object is still *laterally*
    /// approaching the yield point: a stop only `object_stop_gap_m` short can leave the ego —
    /// especially mid-turn, where the curving heading rotates the crosser to the edge of the
    /// checker's lateral-alignment band — inside the checker's longitudinal-conflict window,
    /// where its lateral RSS against a cutting-in object binds and MRC-rejects the yield. Set
    /// to the checker's longitudinal-conflict distance (`RSS_LONGITUDINAL_CONFLICT_M` = 8 m) so
    /// the stopped ego sits OUTSIDE that window — making a predicted-crossing yield
    /// checker-ADMISSIBLE (a smooth doer yield) across straight AND curved geometry, instead of
    /// fail-closing to the checker's safe-stop. On a straight road the outcome is unchanged
    /// (already admissible); the ego merely yields a few metres earlier.
    pub predictive_yield_gap_m: f64,
    /// Speed cap while an in-path object limits travel: the planner approaches a
    /// hazard slowly so the RSS following distance stays satisfied the whole way
    /// in (a planner that brakes only geometrically still over-speeds mid-approach
    /// and the checker rejects it).
    pub object_approach_speed_mps: f64,
    /// Lateral clearance the planner steers for when routing around an off-path
    /// object: it offsets the path so the object ends up at least this far from
    /// it. Must exceed the checker's lateral-alignment band (4 m) so the cleared
    /// object is RSS-filtered (#451).
    pub lateral_clearance_target_m: f64,
    /// Cap on the lateral offset the planner will take to route around an object.
    pub lateral_offset_max_m: f64,
    /// Max lateral metres per longitudinal metre while ramping the offset in/out
    /// (a gentle slope keeps the maneuver kinematically admissible).
    pub lateral_ramp_slope: f64,
    /// The planner's model of the vehicle half-width + containment margin, used to
    /// keep an offset path inside the corridor boundaries (the checker uses the
    /// real footprint; this is the planner's conservative assumption).
    pub vehicle_half_width_m: f64,
    pub containment_margin_m: f64,
    /// The planner's model of the vehicle LENGTH (m), used by the joint optimizer's
    /// **oriented-footprint** containment check: a candidate racing line's rotated footprint
    /// corners (front/back reach ±`vehicle_length_m`/2 along heading) are projected onto the
    /// corridor centerline and must stay within the half-width — so the line stays inside KIRRA's
    /// oriented containment on a curve, where a centered half-width check would miss the swing.
    pub vehicle_length_m: f64,
    /// Speed cap while routing around an object (the lateral pass), low enough to
    /// keep the maneuver inside the steering-rate / lateral-accel envelope.
    pub lateral_pass_speed_mps: f64,
    /// Longitudinal speed above which an in-path object is treated as a moving
    /// LEAD (matched/followed) rather than a static hazard (stopped short of).
    pub lead_speed_threshold_mps: f64,
    /// Lateral band within which a moving object is a lead to match speed with
    /// (wider than `object_lane_tolerance_m`: a slightly off-center slower vehicle
    /// is overtaken at its speed, not blown past at cruise → RSS reject).
    pub lead_lateral_band_m: f64,
    /// Following gap kept behind a dead-ahead lead (cruise at the lead's speed up
    /// to this gap; rolling-horizon, not a hard stop).
    pub lead_following_gap_m: f64,
    /// How far ahead (s) to predict object motion for conflict yielding.
    pub prediction_horizon_s: f64,
    /// Time step for the constant-velocity prediction rollout.
    pub prediction_dt_s: f64,
    /// Lane half-width used to decide when a predicted object has entered the path.
    pub prediction_lane_half_m: f64,
    /// Min object speed to treat it as a moving "crosser" worth predicting.
    pub crossing_speed_threshold_mps: f64,
    /// Cap on the lateral offset an **overtake** will take (a pass legitimately
    /// crosses a full lane, so this exceeds `lateral_offset_max_m`). The overtake
    /// reuses `lateral_clearance_target_m` for the pass clearance: that clearance
    /// must exceed the checker's 4 m lateral-alignment band so the passed object is
    /// RSS-filtered (#451) — which is also why a *narrow* road can't admit a pass
    /// (>4 m clearance won't fit one oncoming lane). The DRIVABLE corridor fit is
    /// the real bound; this is a sanity ceiling.
    pub overtake_offset_max_m: f64,
    /// Temporal-overlap margin (s) for predictive yielding. The planner yields to a
    /// predicted crosser UNLESS the object provably clears the conflict band more
    /// than this margin before the ego's SOONEST possible arrival there. Covers
    /// reaction + a safety buffer; bigger = more conservative (yields more often).
    pub yield_temporal_margin_s: f64,
    /// Maximum jerk (m/s³) for the speed profile. Bounds the rate of change of
    /// acceleration so the profile is an S-curve (comfort), not the bang-bang
    /// trapezoid that steps instantly between full accel / cruise / full brake. The
    /// brake trigger is made jerk-aware so the vehicle still stops by the limit
    /// despite ramping the deceleration in. Within the accel/decel envelope the
    /// checker already enforces, so this is a comfort refinement, not a safety one.
    pub max_jerk_mps3: f64,
    /// Chaikin corner-cutting iterations applied to the guide centerline before
    /// sampling — rounds the corners of a coarse / curved corridor so the path's
    /// curvature (and the steering it implies) is bounded (comfort), instead of the
    /// heading jumping at each polyline vertex. `0` = off; the new vertices lie on
    /// the original centerline segments, so it stays within the corridor
    /// (containment still backstops). A straight guide is unaffected. Bounded for
    /// WCET: the vertex count grows by ≤ 2× per iteration, so keep this small.
    pub path_smoothing_iterations: usize,
    /// Longitudinal distance (m) past the end of the pull-over lateral ramp at which
    /// the commanded pull-over comes to its controlled stop — room to settle at the
    /// edge before halting. A nearer object/behavioral stop still binds first (the
    /// ego never drives past a hazard to finish parking). The decel itself is the
    /// speed profile's job; this only places the stop station.
    pub pull_over_stop_margin_m: f64,
    /// Lateral clearance (m) the pull-over keeps between the footprint CENTER and the
    /// right boundary at the parked pose. Larger than the static footprint half-width
    /// on purpose: while the long vehicle ramps right it angles up to
    /// `atan(lateral_ramp_slope)` and its nose swings toward the edge, so this clearance
    /// covers that transient excursion (~2 m for an urban car at the default slope) and
    /// keeps the *angled* footprint inside the corridor — which the planner's straight
    /// hold-station fit check alone would miss. Consequence: a road must be wider than
    /// this clearance plus the footprint for a pull-over to fit (a narrow lane can't
    /// admit a full edge-park, mirroring the narrow-road overtake bound); a real shoulder
    /// supplied via `drivable` gives the room. A tighter curb-hug would need a slow final
    /// straightening pass the single-ramp maneuver does not author (future work).
    pub pull_over_edge_clearance_m: f64,
    /// Comfort lateral-acceleration limit (m/s²) for curvature-aware speed. The
    /// target speed is capped to `sqrt(comfort_lateral_accel / κ)` for the path's
    /// upcoming curvature κ, so the ego SLOWS for curves (looking ahead far enough
    /// to decelerate in time) instead of taking them at cruise — which the checker
    /// would otherwise clamp (steering rate / lateral accel) and which is
    /// uncomfortable. Below the checker's own lateral-accel ceiling, so this is a
    /// comfort refinement; `0` (or a huge value) effectively disables it.
    pub comfort_lateral_accel_mps2: f64,
    /// Comfort steering-RATE limit (rad/s) for the curvature-transition speed cap. The
    /// curvature-aware cap above bounds steering *angle* (lateral accel) for a curve's κ, but a
    /// sharp **transition** — κ changing fast along the path (entering/exiting a bend, an S) —
    /// demands steering *rate* ∝ `v·dκ/ds`, which it does not bound. This caps the speed where the
    /// curvature is changing fast so the steering rate stays within the envelope, via the
    /// bicycle relation `δ = atan(L·κ)` ([`wheelbase_m`](Self::wheelbase_m)). Kept below the
    /// checker's hard steering-rate ceiling, so a plan capped here is checker-admissible (the doer
    /// slows the transition instead of being clamped). `0` disables it; a straight or
    /// constant-curvature path is unaffected (`dκ/ds = 0` ⇒ no cap).
    pub max_steering_rate_rads: f64,
    /// Wheelbase (m) for the bicycle-model steering relation `δ = atan(L·κ)` the steering-rate cap
    /// uses. A larger wheelbase implies more steering angle per curvature, so a tighter cap.
    pub wheelbase_m: f64,
    /// Enable the **joint path+speed** optimizer: a sampling-based spatiotemporal search that, after
    /// the reference guide is built, tries a bounded vocabulary of ramped lateral-offset candidate
    /// paths (the "racing / comfort line") within the corridor and keeps the one with the lowest
    /// **traversal time** (scored through the same velocity profile, so a flatter path's higher
    /// achievable speed is captured) plus a small deviation penalty. Co-optimizes path SHAPE and
    /// SPEED instead of fixing the centerline then speeding it. `false` (default) ⇒ the centerline
    /// guide is used unchanged (byte-identical). KIRRA still bounds containment + kinematics; a
    /// candidate is bounded to keep the footprint inside the corridor.
    pub joint_path_optimize: bool,
}

impl Default for GeometricPlannerConfig {
    fn default() -> Self {
        Self {
            cruise_speed_mps: 8.0,
            conservative_factor: 0.5,
            max_accel_mps2: 2.0,
            max_decel_mps2: 2.5,
            sample_dt_s: 0.1,
            max_points: 50,
            goal_tolerance_m: 0.5,
            object_lane_tolerance_m: 2.0,
            object_stop_gap_m: 5.0,
            predictive_yield_gap_m: 8.0,
            object_approach_speed_mps: 2.0,
            lateral_clearance_target_m: 4.5,
            lateral_offset_max_m: 3.0,
            lateral_ramp_slope: 0.35,
            vehicle_half_width_m: 1.0,
            containment_margin_m: 0.45,
            vehicle_length_m: 4.5,
            lateral_pass_speed_mps: 4.0,
            lead_speed_threshold_mps: 0.5,
            lead_lateral_band_m: 3.5,
            lead_following_gap_m: 8.0,
            prediction_horizon_s: 3.0,
            prediction_dt_s: 0.2,
            prediction_lane_half_m: 2.0,
            crossing_speed_threshold_mps: 0.5,
            overtake_offset_max_m: 7.0,
            yield_temporal_margin_s: 1.0,
            max_jerk_mps3: 2.5,
            path_smoothing_iterations: 2,
            pull_over_stop_margin_m: 2.0,
            pull_over_edge_clearance_m: 2.2,
            comfort_lateral_accel_mps2: 2.0,
            max_steering_rate_rads: 0.4,
            wheelbase_m: 2.7,
            joint_path_optimize: false,
        }
    }
}

/// A deterministic geometric go-to-goal planner: it follows the drivable
/// **corridor centerline** toward the goal with a trapezoidal speed profile that
/// tapers to a controlled stop at the goal.
///
/// **It PROPOSES; the checker decides.** Containment is respected *by
/// construction* (the centerline is the laterally-safest path), and the speed
/// profile stays within urban kinematic limits, but the planner is never the
/// safety authority — `validate_trajectory_slow` is. Posture-mode gated:
/// - `Full` → cruise to the goal.
/// - `Conservative` (`Degraded`) → derated **and non-increasing** speed
///   (decel-only; never re-accelerates), mirroring the SDK Degraded semantics.
/// - `MrcOnly` (`LockedOut`) → only ever proposes [`PlanOutput::safe_stop`].
///
/// If the corridor boundaries don't pair into a usable centerline (need ≥ 2
/// vertices each), it falls back to a straight ego→goal guide. If the goal is
/// already within tolerance, or the mode admits no forward speed, it HOLDs
/// (safe-stop) — the planner never authors re-acceleration.
#[derive(Debug, Clone, Copy, Default)]
pub struct GeometricPlanner {
    pub cfg: GeometricPlannerConfig,
}

impl GeometricPlanner {
    #[must_use]
    pub fn new(cfg: GeometricPlannerConfig) -> Self {
        Self { cfg }
    }

    /// Lateral-avoidance solver: for the nearest off-path object a centered path
    /// could not clear, compute a trapezoidal offset bump that routes around it —
    /// IF the offset both fits the corridor (with footprint + margin) and has room
    /// to ramp in before the object. Otherwise [`LateralBump::NONE`] (the caller
    /// then stops short instead — never an unsafe squeeze).
    #[allow(clippy::too_many_arguments)]
    fn compute_bump(
        &self,
        guide: &[(f64, f64)],
        left: &[Point],
        right: &[Point],
        objects: &[PerceivedObject],
        lane_boundaries: &[LaneBoundary],
        no_overtake: &[u64],
        s_ego: f64,
    ) -> LateralBump {
        let ct = self.cfg.lateral_clearance_target_m;

        // Nearest object that is ahead and NOT already clear of the centerline.
        // A moving LEAD is followed (speed-matched), not routed around — skip it.
        let mut best: Option<(f64, f64, f64)> = None; // (s_obj, signed_lateral, obj_x)
        for obj in objects {
            // A no-pass object (stopped school bus) is never routed around — the
            // ego must hold behind it (the per-object stop-short does that).
            if no_overtake.contains(&obj.id) {
                continue;
            }
            let (s_obj, signed) = project_signed(guide, obj.pos.x_m, obj.pos.y_m);
            if s_obj <= s_ego || signed.abs() >= ct {
                continue;
            }
            let h = heading_at(guide, s_obj);
            let lon_v = obj.vel.x_m * h.cos() + obj.vel.y_m * h.sin();
            if lon_v > self.cfg.lead_speed_threshold_mps {
                continue; // a lead — handled by speed-matching, not avoidance
            }
            if best.is_none_or(|(bs, _, _)| s_obj < bs) {
                best = Some((s_obj, signed, obj.pos.x_m));
            }
        }
        let (s_obj, signed, obj_x) = match best {
            Some(v) => v,
            None => return LateralBump::NONE,
        };

        // Offset to the FAR side, minimal magnitude to reach `ct` clearance.
        let y_off = signed - ct * if signed >= 0.0 { 1.0 } else { -1.0 };
        if y_off.abs() > self.cfg.lateral_offset_max_m {
            return LateralBump::NONE;
        }
        // Lane-line rule: never route around across a non-crossable line (e.g. a
        // solid centerline). The legal constraint is Occy's; the collision shadow
        // (oncoming traffic) stays KIRRA's. Falls back to stop-short.
        if !behavior::lateral_move_permitted(lane_boundaries, 0.0, y_off) {
            return LateralBump::NONE;
        }

        // Corridor fit at the object's x: offset path + footprint inside boundaries.
        let cl = 0.5 * (boundary_y_at(left, obj_x) + boundary_y_at(right, obj_x));
        let path_y = cl + y_off;
        let fh = self.cfg.vehicle_half_width_m + self.cfg.containment_margin_m;
        if path_y + fh > boundary_y_at(left, obj_x) || path_y - fh < boundary_y_at(right, obj_x) {
            return LateralBump::NONE;
        }

        // Room to ramp the offset in before reaching the object.
        let ramp_len = (1.5 * y_off.abs() / self.cfg.lateral_ramp_slope.max(1e-3)).max(1.0);
        let hold_half = 1.0;
        let hold_start = (s_obj - s_ego) - hold_half;
        if hold_start - ramp_len < 0.0 {
            return LateralBump::NONE;
        }
        LateralBump { y_off, ramp_len, hold_start, hold_end: (s_obj - s_ego) + hold_half }
    }

    /// **Overtake** a stopped / slow in-lane object by crossing **left** (the
    /// lawful pass side on a right-driving road) into the adjacent lane, holding
    /// alongside, then returning — a route-around bump, but it specifically passes
    /// on the left across a *crossable* centerline, uses a tight object-relative
    /// clearance (vehicle + gap, not the wide roadside berth), and fits the
    /// **drivable** area (the full road), not just the reference lane. Returns
    /// `NONE` (→ stop short) unless the lane line permits the left crossing AND the
    /// offset path + footprint stays inside `left`/`right` (the drivable boundaries).
    ///
    /// This is what Occy *proposes*; it never reasons about the oncoming traffic
    /// the pass exposes — KIRRA's head-on RSS is the sole authority on whether the
    /// oncoming lane is clear enough (the doer-checker split). A moving lead is
    /// followed (speed-matched), never overtaken here.
    #[allow(clippy::too_many_arguments)]
    fn compute_overtake_bump(
        &self,
        guide: &[(f64, f64)],
        left: &[Point],
        right: &[Point],
        objects: &[PerceivedObject],
        lane_boundaries: &[LaneBoundary],
        no_overtake: &[u64],
        s_ego: f64,
    ) -> LateralBump {
        let band = self.cfg.object_lane_tolerance_m;
        // Nearest stopped/slow object ahead and IN our lane (a pass candidate).
        let mut best: Option<(f64, f64)> = None; // (s_obj, signed)
        for obj in objects {
            // A stopped school bus (or any flagged no-pass object) is NEVER
            // overtaken — it is illegal. Hold behind it (stop-short) instead.
            if no_overtake.contains(&obj.id) {
                continue;
            }
            let (s_obj, signed) = project_signed(guide, obj.pos.x_m, obj.pos.y_m);
            if s_obj <= s_ego || signed.abs() > band {
                continue;
            }
            let h = heading_at(guide, s_obj);
            let lon_v = obj.vel.x_m * h.cos() + obj.vel.y_m * h.sin();
            if lon_v > self.cfg.lead_speed_threshold_mps {
                continue; // a moving lead → follow, don't overtake
            }
            if best.is_none_or(|(bs, _)| s_obj < bs) {
                best = Some((s_obj, signed));
            }
        }
        let (s_obj, signed) = match best {
            Some(v) => v,
            None => return LateralBump::NONE,
        };

        // Pass on the LEFT (+y): path goes to the object's left by the clearance
        // target. It must exceed the checker's 4 m lateral band so the passed
        // object is RSS-filtered (#451) — the same berth a within-lane route-around
        // uses, here taken across the centerline into the drivable area.
        let clearance = self.cfg.lateral_clearance_target_m;
        let y_off = signed + clearance;
        if y_off <= 0.0 || y_off.abs() > self.cfg.overtake_offset_max_m {
            return LateralBump::NONE;
        }
        // Lane-line rule: the centerline must be crossable to the left.
        if !behavior::lateral_move_permitted(lane_boundaries, 0.0, y_off) {
            return LateralBump::NONE;
        }
        // Drivable fit at the object, GUIDE-relative: the path follows the guide
        // (the ego lane), offset by `y_off`, and must stay inside the drivable area.
        let (gx, gy) = point_at(guide, s_obj);
        let path_y = gy + y_off;
        let fh = self.cfg.vehicle_half_width_m + self.cfg.containment_margin_m;
        if path_y + fh > boundary_y_at(left, gx) || path_y - fh < boundary_y_at(right, gx) {
            return LateralBump::NONE;
        }
        // Commit EARLY: ramp from the ego over the whole run-up to the object, so
        // the lateral clearance has cleared the checker's 4 m band BEFORE the ego
        // closes within its longitudinal gap. A late, slope-fixed ramp (as the
        // within-lane route-around uses) would still be sweeping through the band at
        // close range and KIRRA would MRC the pass. Hold alongside; the return ramp
        // runs past the horizon (a later cycle completes it — receding-horizon).
        let hold_half = 1.5;
        let hold_start = (s_obj - s_ego) - hold_half;
        let ramp_len = hold_start; // start the lateral move at the ego (up0 = 0)
        // Reject if the run-up is too short for a gentle (in-envelope) ramp.
        if ramp_len < y_off.abs() / self.cfg.lateral_ramp_slope.max(1e-3) {
            return LateralBump::NONE;
        }
        LateralBump { y_off, ramp_len, hold_start, hold_end: (s_obj - s_ego) + hold_half }
    }

    /// Commanded lane change: a SUSTAINED lateral shift to `target` (ramp in, then
    /// hold — unlike the route-around bump, which returns to center). Honored only
    /// if the lane-line rules permit crossing to that side and the shifted path +
    /// footprint stays inside the corridor; else `None` (stay in lane).
    fn compute_lane_change_bump(
        &self,
        target: f64,
        left: &[Point],
        right: &[Point],
        lane_boundaries: &[LaneBoundary],
        s_ego: f64,
    ) -> Option<LateralBump> {
        if target.abs() <= 1e-3 {
            return None;
        }
        // Lane-line rule: may we cross to the target side?
        if !behavior::lateral_move_permitted(lane_boundaries, 0.0, target) {
            return None;
        }
        // Corridor fit: the shifted path + footprint must stay inside, checked
        // across the held region (a few stations past the ramp).
        let ramp_len = (1.5 * target.abs() / self.cfg.lateral_ramp_slope.max(1e-3)).max(1.0);
        let fh = self.cfg.vehicle_half_width_m + self.cfg.containment_margin_m;
        for k in 0..=4 {
            let x = s_ego + ramp_len + k as f64 * 2.0;
            let cl = 0.5 * (boundary_y_at(left, x) + boundary_y_at(right, x));
            let path_y = cl + target;
            if path_y + fh > boundary_y_at(left, x) || path_y - fh < boundary_y_at(right, x) {
                return None;
            }
        }
        // Ramp in, then hold to the end of the horizon (hold_end → effectively ∞).
        Some(LateralBump { y_off: target, ramp_len, hold_start: ramp_len, hold_end: 1e9 })
    }

    /// Commanded **pull-over**: a SUSTAINED rightward shift that brings the footprint
    /// center to `pull_over_edge_clearance_m` inside the right boundary (the clearance
    /// covers the angled-ramp nose excursion — see the field doc), held to the end of
    /// the horizon; the longitudinal stop is authored separately by the caller.
    /// `left`/`right` are the boundaries the move is fitted against (the `drivable`
    /// shoulder if supplied, else the reference corridor), so a pull-over reaches a real
    /// shoulder when there is one and otherwise pulls to the right of the lane.
    ///
    /// Returns `None` (→ stay in lane) if the lane line forbids the rightward move, the
    /// corridor is degenerate (no room right of center), or the shifted footprint will
    /// not fit. Like every maneuver here it only PROPOSES — KIRRA bounds the result.
    fn compute_pull_over_bump(
        &self,
        left: &[Point],
        right: &[Point],
        lane_boundaries: &[LaneBoundary],
        s_ego: f64,
    ) -> Option<LateralBump> {
        let fh = self.cfg.vehicle_half_width_m + self.cfg.containment_margin_m;
        // Target path: bring the footprint center to `pull_over_edge_clearance_m` inside
        // the right boundary. That clearance is deliberately larger than the static
        // footprint half-width: while the (long) vehicle ramps over it angles up to
        // ~atan(lateral_ramp_slope) and its nose swings toward the edge — the clearance
        // covers that transient excursion so the angled footprint still fits (a tighter
        // curb-hug would need a slow final straightening pass; see the field doc). Read
        // at the ego station (exact for a straight edge). Negative target = rightward.
        let rb0 = boundary_y_at(right, s_ego);
        let cl0 = 0.5 * (boundary_y_at(left, s_ego) + rb0);
        let target = (rb0 + self.cfg.pull_over_edge_clearance_m) - cl0;
        if target >= -1e-3 {
            return None; // clearance ≥ half-corridor (no room right of center) → stay put
        }
        // Lane-line rule: pulling right (e.g. onto a shoulder) must be permitted — a
        // solid right edge / barrier line forbids it. The legal constraint is Occy's.
        if !behavior::lateral_move_permitted(lane_boundaries, 0.0, target) {
            return None;
        }
        // Corridor fit across the held region past the ramp (footprint stays inside).
        let ramp_len = (1.5 * target.abs() / self.cfg.lateral_ramp_slope.max(1e-3)).max(1.0);
        for k in 0..=4 {
            let x = s_ego + ramp_len + k as f64 * 2.0;
            let cl = 0.5 * (boundary_y_at(left, x) + boundary_y_at(right, x));
            let path_y = cl + target;
            if path_y + fh > boundary_y_at(left, x) || path_y - fh < boundary_y_at(right, x) {
                return None;
            }
        }
        Some(LateralBump { y_off: target, ramp_len, hold_start: ramp_len, hold_end: 1e9 })
    }

    /// Predictive yield (SPACE-TIME): roll a moving object forward (CTRV if a yaw
    /// rate is supplied via `motion`, else constant-velocity) and, if it is
    /// currently OUT of the lane but will ENTER the path ahead within the horizon
    /// (a crossing or turning-in vehicle/pedestrian), return the arc-length at
    /// which it crosses in — so the planner yields short of it. Objects already in
    /// the lane are handled by stop-short/lead (skipped here).
    ///
    /// TEMPORAL OVERLAP: a yield is returned only if the object still occupies the
    /// conflict band when the ego could reach it. The object's occupancy window
    /// `[t_enter, t_exit]` is compared against `t_ego`, the SOONEST the ego could
    /// arrive at the conflict (accelerate to cruise, then cruise — an upper-bound
    /// speed, so the test is yield-biased / fail-safe). If the object clears more
    /// than `yield_temporal_margin_s` before `t_ego`, the crosser is long gone and
    /// the planner does NOT yield. This only DROPS provably-unnecessary yields — it
    /// is never less cautious than a purely spatial yield, and KIRRA still backstops.
    /// Returns `None` if there is no predicted space-time conflict.
    #[allow(clippy::too_many_arguments)]
    fn predict_yield_s(
        &self,
        obj: &PerceivedObject,
        motion: &[MotionState],
        predicted_paths: &[PredictedPath],
        guide: &[(f64, f64)],
        s_ego: f64,
        ego_speed: f64,
        ego_target: f64,
    ) -> Option<f64> {
        let (_, signed_now) = project_signed(guide, obj.pos.x_m, obj.pos.y_m);
        if signed_now.abs() <= self.cfg.object_lane_tolerance_m {
            return None; // already in-lane → stop-short / lead handles it
        }
        let speed = obj.vel.x_m.hypot(obj.vel.y_m);
        if speed < self.cfg.crossing_speed_threshold_mps {
            return None; // not a mover worth predicting
        }
        // INTENTION prior + MULTI-MODAL: an object may carry SEVERAL predicted modes
        // (e.g. lane-follow vs. cut-in vs. turn). Yield against the WORST CASE — the
        // nearest conflict over all modes — so a single dangerous hypothesis is enough
        // to slow the ego, while a mode that stays clear cannot relax that. One mode →
        // the single intention path; no modes → the turn-aware CTRV kinematic rollout.
        // This is the planner-side complement to KIRRA's multi-modal predictive RSS:
        // Occy proposes conservatively over modes, the checker bounds over modes.
        let yaw_rate = motion
            .iter()
            .find(|m| m.id == obj.id)
            .map_or(0.0, |m| m.yaw_rate_rad_s);
        let modes: Vec<&PredictedPath> = predicted_paths
            .iter()
            .filter(|p| p.id == obj.id && p.points.len() >= 2)
            .collect();
        if modes.is_empty() {
            return self.yield_for_mode(obj, None, speed, yaw_rate, guide, s_ego, ego_speed, ego_target);
        }
        modes
            .iter()
            .filter_map(|m| {
                self.yield_for_mode(obj, Some(m), speed, yaw_rate, guide, s_ego, ego_speed, ego_target)
            })
            .min_by(|a, b| a.total_cmp(b))
    }

    /// Yield arc-length for ONE predicted mode: `Some(path)` rolls the object along it
    /// at its speed; `None` is the turn-aware CTRV kinematic rollout (yaw rate 0 → CV).
    /// Returns the conflict arc-length, or `None` if this mode produces no space-time
    /// conflict (including the temporal-overlap relaxation below). The per-mode core of
    /// [`predict_yield_s`]; multi-mode worst-casing happens in the caller.
    #[allow(clippy::too_many_arguments)]
    fn yield_for_mode(
        &self,
        obj: &PerceivedObject,
        path: Option<&PredictedPath>,
        speed: f64,
        yaw_rate: f64,
        guide: &[(f64, f64)],
        s_ego: f64,
        ego_speed: f64,
        ego_target: f64,
    ) -> Option<f64> {
        let dt = self.cfg.prediction_dt_s.max(0.05);
        let steps = (self.cfg.prediction_horizon_s / dt).ceil() as usize;
        let mut heading = obj.vel.y_m.atan2(obj.vel.x_m);
        let (mut kx, mut ky) = (obj.pos.x_m, obj.pos.y_m);
        // Track the FIRST in-lane-ahead entry and the time the object then LEAVES
        // the band — only a real exit (within the horizon) counts.
        let mut conflict: Option<(f64, f64)> = None; // (s_conflict, t_enter)
        let mut exit_t: Option<f64> = None;
        for i in 1..=steps {
            let t = i as f64 * dt;
            let (fx, fy) = if let Some(p) = path {
                // Lane-following: position after travelling `speed*t` along the path.
                point_on_path(p.points, speed * t)
            } else {
                heading += yaw_rate * dt;
                kx += speed * heading.cos() * dt;
                ky += speed * heading.sin() * dt;
                (kx, ky)
            };
            let (s_f, lateral_f) = project_signed(guide, fx, fy);
            let in_band = s_f > s_ego && lateral_f.abs() <= self.cfg.prediction_lane_half_m;
            match conflict {
                None if in_band => conflict = Some((s_f, t)), // first entry
                Some(_) if !in_band => {
                    exit_t = Some(t); // first exit after entry
                    break;
                }
                _ => {}
            }
        }
        let (s_conflict, _t_enter) = conflict?;

        // Relax the yield ONLY if the object DEFINITELY clears the band within the
        // horizon, and does so more than `yield_temporal_margin_s` before the ego's
        // SOONEST arrival (accelerate to cruise, then cruise — upper-bound speed, so
        // the test is yield-biased). If the object enters and STAYS through the
        // horizon (a stall / slow crosser still present at the end of prediction),
        // we cannot confirm it clears → yield (conservative). KIRRA still backstops.
        if let Some(t_exit) = exit_t {
            let t_ego = self.time_to_distance(s_conflict - s_ego, ego_speed, ego_target);
            if t_exit + self.cfg.yield_temporal_margin_s < t_ego {
                return None;
            }
        }
        Some(s_conflict)
    }

    /// Time (s) for the ego to cover `dist` starting at `v0`, accelerating at
    /// `max_accel_mps2` to `v_target`, then cruising. The SOONEST plausible arrival
    /// (used yield-biased). `dist <= 0` → 0.
    fn time_to_distance(&self, dist: f64, v0: f64, v_target: f64) -> f64 {
        if dist <= 0.0 {
            return 0.0;
        }
        let a = self.cfg.max_accel_mps2.max(1e-3);
        let v0 = v0.clamp(0.0, v_target.max(0.0));
        let t_accel = ((v_target - v0) / a).max(0.0);
        let d_accel = v0 * t_accel + 0.5 * a * t_accel * t_accel;
        if dist <= d_accel {
            // Still accelerating: solve v0*t + 0.5*a*t^2 = dist.
            (-v0 + (v0 * v0 + 2.0 * a * dist).sqrt()) / a
        } else {
            let v = v_target.max(1e-3);
            t_accel + (dist - d_accel) / v
        }
    }

    /// Curvature-aware speed cap (m/s) at guide arc-length `s_now`: the highest
    /// speed the ego may hold NOW so it can still decelerate (at `decel`) to each
    /// upcoming curve's comfort limit `sqrt(comfort_lat / κ)` by the time it gets
    /// there. Looks ahead over the braking distance + a fixed horizon. A straight
    /// path (κ ≈ 0) → no cap (`f64::INFINITY`).
    /// Full forward–backward VELOCITY PROFILE over the travel window `[0, dist]`
    /// (arc-length from the ego), sampled every `VELOCITY_PROFILE_DS`.
    ///
    /// One principled pass that subsumes the curvature speed cap AND the brake
    /// trigger: the static limit at each station is `min(target, √(comfort_lat/κ))`
    /// (cruise + each curve); a `stop_at_end` window pins the terminal to 0. A
    /// single BACKWARD pass then enforces deceleration feasibility —
    /// `v[i] = min(limit[i], √(v[i+1]² + 2·decel·ds))` — which propagates every
    /// downstream constraint (a stop, each curve) upstream so the ego slows in
    /// time, handling SEQUENCES (a curve then a stop) that an incremental
    /// look-ahead can get wrong. The forward jerk-limited integration in `plan`
    /// then executes this cap (its own accel limit is the forward feasibility pass).
    ///
    /// `decel` is reduced to leave headroom for the jerk-limited ramp, so the ego
    /// reaches each limit BEFORE it bites rather than lagging into it.
    fn velocity_profile(
        &self,
        guide: &[(f64, f64)],
        s_ego: f64,
        dist: f64,
        target: f64,
        decel: f64,
        stop_at_end: bool,
    ) -> Vec<f64> {
        let ds = VELOCITY_PROFILE_DS;
        let n = ((dist / ds).ceil() as usize).max(1) + 1;
        let lat = self.cfg.comfort_lateral_accel_mps2;
        // Static speed limit: cruise/target, tightened by each curve's comfort speed.
        let mut v: Vec<f64> = (0..n)
            .map(|i| {
                let s = (i as f64 * ds).min(dist);
                let mut lim = target;
                let k = curvature_at(guide, s_ego + s, 3.0);
                // Curvature (lateral-accel) cap — slow for a curve's κ.
                if lat > 0.0 && k > 1e-4 {
                    lim = lim.min((lat / k).sqrt());
                }
                // Curvature-TRANSITION (steering-rate) cap — slow where κ is changing fast (a sharp
                // entry/exit or S-bend), so the steering rate stays within the comfort envelope and
                // the plan is checker-admissible. A no-op on a straight / constant-curvature path.
                if self.cfg.max_steering_rate_rads > 0.0 {
                    let k_next = curvature_at(guide, s_ego + s + ds, 3.0);
                    let dk_ds = (k_next - k) / ds;
                    lim = lim.min(steering_rate_speed_cap(
                        k,
                        dk_ds,
                        self.cfg.wheelbase_m,
                        self.cfg.max_steering_rate_rads,
                    ));
                }
                lim
            })
            .collect();
        if stop_at_end {
            v[n - 1] = 0.0;
        }
        // Backward pass: enforce decel feasibility to every downstream limit.
        let decel_eff = (0.6 * decel).max(1e-3);
        for i in (0..n - 1).rev() {
            let feasible = (v[i + 1] * v[i + 1] + 2.0 * decel_eff * ds).sqrt();
            v[i] = v[i].min(feasible);
        }
        v
    }

    /// **Joint path+speed optimization** (sampling-based; opt-in via `joint_path_optimize`). Given
    /// the reference `guide` and the drivable corridor, try a bounded vocabulary of ramped
    /// lateral-offset candidate paths (the "racing / comfort line") and return the one with the
    /// lowest **traversal time** — scored through [`velocity_profile`](Self::velocity_profile), so a
    /// flatter path's higher achievable speed is what wins — plus a small deviation penalty. This
    /// co-optimizes path SHAPE and SPEED jointly instead of fixing the centerline then speeding it.
    ///
    /// Determinism + WCET: a fixed `2·JOINT_OFFSET_SAMPLES_PER_SIDE+1` candidate set, each scored by
    /// one velocity-profile pass. The centerline (offset 0) is always a candidate, so the result is
    /// never worse than the baseline and is a no-op on a straight road (equal times ⇒ zero-penalty
    /// centerline wins). Each candidate is bounded so the footprint stays inside the corridor;
    /// KIRRA still bounds containment + kinematics independently.
    fn optimize_guide(
        &self,
        guide: &[(f64, f64)],
        map: &dyn CorridorSource,
        s_ego: f64,
        goal: (f64, f64),
        target: f64,
        decel: f64,
    ) -> Vec<(f64, f64)> {
        let (left, right) = (map.left_boundary(), map.right_boundary());
        let guide_len = polyline_len(guide);
        // The bend's peak curvature drives the apex offset (zero where straight) and a κ≈0 path makes
        // the optimizer a no-op (a straight road can't be improved by a corner cut).
        let mut kappa_max = 0.0_f64;
        let mut s = s_ego;
        while s <= guide_len {
            kappa_max = kappa_max.max(signed_curvature_at(guide, s, 3.0).abs());
            s += VELOCITY_PROFILE_DS;
        }
        if kappa_max <= 1e-4 {
            return guide.to_vec();
        }
        // Oriented-footprint containment: a candidate is admissible iff every pose's rotated footprint
        // corner projects within the corridor's narrowest half-width (less the margin) — the angled
        // footprint on a curve, checked via `project_signed` (robust where an x-indexed scan is not).
        let half_width = corridor_half_width(guide, left, right);
        let (half_len, half_wid) = (0.5 * self.cfg.vehicle_length_m, self.cfg.vehicle_half_width_m);
        let lateral_limit = half_width - self.cfg.containment_margin_m;
        if lateral_limit <= half_wid {
            return guide.to_vec(); // corridor barely fits the footprint — no room to deviate
        }
        let admissible = |cand: &[(f64, f64)]| -> bool {
            let len = polyline_len(cand);
            let mut s = s_ego;
            while s <= len {
                let (x, y) = point_at(cand, s);
                let h = heading_at(cand, s);
                for (cx, cy) in footprint_corners(x, y, h, half_len, half_wid) {
                    if project_signed(guide, cx, cy).1.abs() > lateral_limit {
                        return false;
                    }
                }
                s += VELOCITY_PROFILE_DS;
            }
            true
        };
        // Offset amplitude is bounded by the lateral room; sample symmetric apex amplitudes.
        let max_off = lateral_limit - half_wid;
        let step = max_off / JOINT_OFFSET_SAMPLES_PER_SIDE as f64;
        let n = JOINT_OFFSET_SAMPLES_PER_SIDE as i64;
        let mut best = guide.to_vec(); // the centerline (offset 0) is always an admissible candidate
        let mut best_cost = f64::INFINITY;
        for k in -n..=n {
            let delta = k as f64 * step;
            let cand = offset_guide(guide, s_ego, delta, kappa_max);
            if !admissible(&cand) {
                continue; // never propose a line KIRRA's oriented containment would reject
            }
            // Objective = time to reach the GOAL along this candidate (a flatter AND/OR shorter line
            // reaches it sooner), comparable across candidates of different length.
            let goal_s = project_arc_length(&cand, goal.0, goal.1);
            let dist = (goal_s - s_ego).max(VELOCITY_PROFILE_DS);
            let prof = self.velocity_profile(&cand, s_ego, dist, target, decel, true);
            let time: f64 = prof.iter().map(|v| VELOCITY_PROFILE_DS / v.max(0.1)).sum();
            let cost = time + JOINT_DEVIATION_WEIGHT_S_PER_M * delta.abs();
            if cost < best_cost {
                best_cost = cost;
                best = cand;
            }
        }
        best
    }
}

/// Linear interpolation of a `velocity_profile` (sampled every `VELOCITY_PROFILE_DS`
/// from arc-length 0) at distance `s`; clamped to the endpoints.
fn sample_profile(profile: &[f64], s: f64) -> f64 {
    if profile.is_empty() {
        return 0.0;
    }
    let x = (s / VELOCITY_PROFILE_DS).max(0.0);
    let i = x.floor() as usize;
    if i + 1 >= profile.len() {
        return *profile.last().unwrap();
    }
    let f = x - i as f64;
    profile[i] * (1.0 - f) + profile[i + 1] * f
}

// SAFETY: occy planner proposes within corridor + urban kinematic limits; checker decides | REQ: Occy-1.A (#90) | TEST: kirra_planner::tests::{geometric_planner_proposes_motion_toward_goal, geometric_planner_output_is_checker_admissible, geometric_planner_locked_out_only_stops, geometric_planner_degraded_is_non_increasing, geometric_planner_at_goal_holds, geometric_planner_respects_horizon_cap}
impl Planner for GeometricPlanner {
    fn plan(&mut self, input: &PlanInput<'_>) -> PlanOutput {
        // LockedOut → the planner may only ever propose safe-stop.
        let mode = planner_mode(input.posture.clone());
        if mode == PlannerMode::MrcOnly {
            return PlanOutput::safe_stop(input.ego.pose);
        }

        let cur = input.ego.linear_x_mps.abs();
        let target = match mode {
            PlannerMode::Full => self.cfg.cruise_speed_mps,
            // Degraded: derated AND non-increasing (decel-only; no re-accel).
            PlannerMode::Conservative => {
                (self.cfg.cruise_speed_mps * self.cfg.conservative_factor).min(cur)
            }
            PlannerMode::MrcOnly => unreachable!("handled above"),
        };

        // A requested cruise speed (Mick's chauffeur knob) may only SLOW the planner —
        // `min(ceiling, request)` — never raise it above the posture-derived ceiling. A
        // non-finite / negative request is ignored (fail-safe). In Degraded this only
        // lowers an already non-increasing target, so the decel-only invariant holds.
        let target = match input.target_speed_mps {
            Some(req) if req.is_finite() && req >= 0.0 => target.min(req),
            _ => target,
        };

        // Guide path: corridor centerline if usable, else a straight ego→goal line.
        let center = centerline_from(input.map.left_boundary(), input.map.right_boundary());
        let raw_guide: Vec<(f64, f64)> = if center.len() >= 2 {
            center
        } else {
            vec![
                (input.ego.pose.x_m, input.ego.pose.y_m),
                (input.goal.target.x_m, input.goal.target.y_m),
            ]
        };
        // Round the guide's corners (Chaikin) so a coarse / curved corridor yields a
        // bounded-curvature path (comfort) instead of a heading jump at each vertex.
        // A straight guide is unchanged; containment still backstops the result.
        let guide = chaikin_smooth(&raw_guide, self.cfg.path_smoothing_iterations);

        // Joint path+speed optimization (opt-in): replace the centerline guide with the time-optimal
        // ramped-offset candidate within the corridor (a flatter "racing line" that admits more
        // speed). Off ⇒ the guide is unchanged (byte-identical). KIRRA still bounds the result.
        let guide = if self.cfg.joint_path_optimize {
            let s0 = project_arc_length(&guide, input.ego.pose.x_m, input.ego.pose.y_m);
            let goal = (input.goal.target.x_m, input.goal.target.y_m);
            self.optimize_guide(&guide, input.map, s0, goal, target, self.cfg.max_decel_mps2.max(1e-3))
        } else {
            guide
        };

        // Travel window: ego projection → goal projection along the guide.
        let s_ego = project_arc_length(&guide, input.ego.pose.x_m, input.ego.pose.y_m);
        let s_goal = project_arc_length(&guide, input.goal.target.x_m, input.goal.target.y_m);

        // Lateral maneuver. A COMMANDED lane change (sustained shift to the target
        // lane) takes precedence over route-around; otherwise, if an off-path
        // object can be routed around within the corridor, compute the avoidance
        // bump. Both are smooth lateral offsets applied per-sample below; NONE →
        // stay centered (objects handled by stop-short).
        // A commanded PULL-OVER takes precedence over every other lateral maneuver:
        // shift as far right as containment admits (onto the drivable shoulder if one
        // was supplied, else the reference corridor's edge) and — below — stop there.
        let bump = if input.request_pull_over {
            let (edge_left, edge_right) = match input.drivable {
                Some(d) => (d.left_boundary(), d.right_boundary()),
                None => (input.map.left_boundary(), input.map.right_boundary()),
            };
            self.compute_pull_over_bump(edge_left, edge_right, input.lane_boundaries, s_ego)
                .unwrap_or(LateralBump::NONE)
        } else {
            match input.lane_change_to_m {
            Some(target) => self
                .compute_lane_change_bump(
                    target,
                    input.map.left_boundary(),
                    input.map.right_boundary(),
                    input.lane_boundaries,
                    s_ego,
                )
                .unwrap_or(LateralBump::NONE),
            None => {
                // Within-lane route-around first (stays inside the reference
                // corridor). If that can't clear the object AND the integrator has
                // supplied a wider drivable area (an undivided road's full width),
                // try an overtake that crosses the centerline into it.
                let within = self.compute_bump(
                    &guide,
                    input.map.left_boundary(),
                    input.map.right_boundary(),
                    input.objects,
                    input.lane_boundaries,
                    input.no_overtake_ids,
                    s_ego,
                );
                match input.drivable {
                    // Fire the cross-centerline overtake when the doer REQUESTS it (Mick's
                    // `Overtake` intent) OR when the within-lane route-around couldn't clear
                    // the object on its own. `compute_overtake_bump` enforces the drivable
                    // fit, the crossable lane line, and the lateral-clearance band, returning
                    // NONE otherwise — in which case we keep the within-lane bump. KIRRA
                    // still bounds the pass (head-on RSS) downstream.
                    Some(drivable) if input.request_overtake || within.y_off == 0.0 => {
                        let pass = self.compute_overtake_bump(
                            &guide,
                            drivable.left_boundary(),
                            drivable.right_boundary(),
                            input.objects,
                            input.lane_boundaries,
                            input.no_overtake_ids,
                            s_ego,
                        );
                        if pass.y_off != 0.0 { pass } else { within }
                    }
                    _ => within,
                }
            }
            }
        };

        // Per-object handling, nearest-binding:
        //  - moving LEAD (forward velocity, within the lead lateral band) → MATCH
        //    its speed; if dead-ahead, also hold a following gap (cruise at the
        //    lead's speed, rolling horizon — not a hard stop).
        //  - static / slow in-lane object → STOP SHORT of it.
        // (The checker's RSS still backstops every object independently.)
        let mut s_limit = s_goal;
        let mut limit_kind = LimitKind::Goal;
        let mut lead_match: Option<f64> = None; // slowest lead speed to match
        for obj in input.objects {
            let (s_obj, signed) = project_signed(&guide, obj.pos.x_m, obj.pos.y_m);
            if s_obj <= s_ego {
                continue;
            }
            // Lateral from the ACTUAL (possibly bumped) path.
            let lateral = (signed - bump.at(s_obj - s_ego)).abs();
            // Longitudinal velocity of the object along the guide (forward = lead).
            let h = heading_at(&guide, s_obj);
            let lon_v = obj.vel.x_m * h.cos() + obj.vel.y_m * h.sin();

            if lon_v > self.cfg.lead_speed_threshold_mps && lateral <= self.cfg.lead_lateral_band_m {
                // Follow BEHIND the lead at a gap (match speed; never draw
                // alongside — abreast at small longitudinal gap fails RSS).
                lead_match = Some(lead_match.map_or(lon_v, |s| s.min(lon_v)));
                let gap = s_obj - self.cfg.lead_following_gap_m;
                if gap < s_limit {
                    s_limit = gap;
                    limit_kind = LimitKind::Lead;
                }
            } else if lateral <= self.cfg.object_lane_tolerance_m {
                let stop = s_obj - self.cfg.object_stop_gap_m;
                if stop < s_limit {
                    s_limit = stop;
                    limit_kind = LimitKind::ObjectStop;
                }
            }
        }

        // Predictive yield: a crossing object that will enter the lane ahead within
        // the horizon → yield short of where it crosses (anticipates a cut-in /
        // crossing that the snapshot-only checks miss until it is already close).
        // Junction right-of-way: an agent that must CEDE to the ego is skipped — the
        // ego asserts priority and proceeds instead of waiting for it.
        for obj in input.objects {
            if input.cedes_to_ego_ids.contains(&obj.id) {
                continue;
            }
            if let Some(s_conflict) = self.predict_yield_s(
                obj, input.motion, input.predicted_paths, &guide, s_ego, cur, target,
            ) {
                let yield_s = s_conflict - self.cfg.predictive_yield_gap_m;
                if yield_s < s_limit {
                    s_limit = yield_s;
                    limit_kind = LimitKind::Yield;
                }
            }
        }

        // Behavioral rules layer: traffic signs & signals impose a hard stop/hold
        // line and/or a speed cap. Occy OBEYS the rule; KIRRA stays the *physical*
        // authority (it never enforces traffic law). A nearer mandatory stop line
        // overrides object/lead handling with a clean decel-to-stop.
        let behavioral =
            behavior::evaluate_controls(input.controls, input.ego.pose.x_m, cur, &BehaviorConfig::default());
        if let Some(stop_x) = behavioral.stop_x_m {
            let s_stop = project_arc_length(&guide, stop_x, input.ego.pose.y_m);
            if s_stop > s_ego && s_stop < s_limit {
                s_limit = s_stop;
                limit_kind = LimitKind::Behavioral;
            }
        }

        // Commanded pull-over: once the rightward shift fired, author a controlled stop
        // at the road edge — a little past the ramp. A nearer hazard/behavioral stop
        // still binds first (the `< s_limit` guard), so the ego never drives past a
        // hazard to finish parking. KIRRA bounds the parked pose independently.
        if input.request_pull_over && bump.y_off != 0.0 {
            let s_stop = s_ego + bump.ramp_len + self.cfg.pull_over_stop_margin_m;
            if s_stop < s_limit {
                s_limit = s_stop;
                limit_kind = LimitKind::PullOver;
            }
        }

        // Target speed by the binding limit, then clamped by any behavioral speed
        // cap (regulatory / advisory / yield).
        let mut target = if bump.y_off != 0.0 {
            target.min(self.cfg.lateral_pass_speed_mps)
        } else {
            match limit_kind {
                LimitKind::Lead => target.min(lead_match.unwrap_or(target)),
                LimitKind::ObjectStop => target.min(self.cfg.object_approach_speed_mps),
                LimitKind::Goal | LimitKind::Behavioral | LimitKind::Yield | LimitKind::PullOver => target,
            }
        };
        if let Some(cap) = behavioral.speed_cap_mps {
            target = target.min(cap);
        }
        // A lead follows at a rolling gap (no brake-to-zero); everything else stops.
        let lead_gap_limit = limit_kind == LimitKind::Lead;
        let dist = (s_limit - s_ego).max(0.0);

        // Arrived, blocked too close to advance, or the mode admits no forward
        // speed → HOLD (never re-accelerate, never creep into the gap).
        if dist <= self.cfg.goal_tolerance_m || target <= STOP_EPSILON_MPS {
            return PlanOutput::safe_stop(input.ego.pose);
        }

        // Trapezoidal speed-profiled resample of the guide.
        //
        // Reserve one slot under the checker's `MAX_TRAJECTORY_HORIZON` so the
        // terminal controlled-stop point can always be appended without pushing
        // the proposal over the cap (which the containment gate rejects).
        let budget = self
            .cfg
            .max_points
            .min(MAX_TRAJECTORY_HORIZON.saturating_sub(1))
            .max(2);
        let dt = self.cfg.sample_dt_s.max(1e-3);
        let decel = self.cfg.max_decel_mps2.max(1e-3);
        // Full velocity profile (forward–backward): the decel-feasible speed cap at
        // every station, subsuming the curvature cap and the brake-to-stop trigger.
        let v_profile = self.velocity_profile(&guide, s_ego, dist, target, decel, !lead_gap_limit);
        let mut traj: Vec<TrajectoryPoint> = Vec::with_capacity(budget + 1);
        let mut s = 0.0_f64; // distance travelled from ego along the guide
        let mut v = cur.min(target.max(cur)); // start at current speed
        let mut a = 0.0_f64; // current acceleration (jerk-limited; smooth launch)
        let mut t = 0.0_f64;
        let mut reached = false;
        let jerk = self.cfg.max_jerk_mps3.max(1e-3);

        while traj.len() < budget {
            let along = s_ego + s;
            let (bx, by) = point_at(&guide, along);
            let h = heading_at(&guide, along);
            // Apply the lateral-avoidance bump perpendicular to the guide.
            let lat = bump.at(s);
            traj.push(TrajectoryPoint {
                pose: Pose {
                    x_m: bx - lat * h.sin(),
                    y_m: by + lat * h.cos(),
                    heading_rad: h,
                },
                velocity_mps: v,
                time_from_start_s: t,
            });

            let remaining = dist - s;
            if remaining <= self.cfg.goal_tolerance_m {
                reached = true;
                break;
            }
            // Track the velocity profile (the decel-feasible cap, already accounting
            // for curves and the stop) with a JERK-LIMITED (S-curve) acceleration:
            // accelerate toward the profile speed, ease the speed down where the
            // profile drops (a curve or the approach to the stop), cruise when on it.
            // `a` slews toward the desired accel bounded by `max_jerk` so the
            // commanded acceleration never steps. The forward accel limit here is the
            // forward feasibility pass; the backward pass made the profile reachable.
            let target_eff = sample_profile(&v_profile, s);
            // Speed change still to come while `a` ramps to 0 at max jerk — the
            // ease-off band so v neither overshoots up nor undershoots the target.
            let coast = a * a / (2.0 * jerk);
            let a_des = if v + coast < target_eff {
                self.cfg.max_accel_mps2
            } else if v - coast > target_eff {
                -decel // above the profile (curve / stop approach) → ease the speed down
            } else {
                0.0 // on the profile → cruise
            };
            // Slew `a` toward `a_des`, clamped by the per-step jerk budget.
            a = if (a_des - a).abs() <= jerk * dt {
                a_des
            } else {
                a + (jerk * dt).copysign(a_des - a)
            };
            v = (v + a * dt).clamp(0.0, target.max(0.0));
            s += v * dt;
            t += dt;
        }

        // On reaching the stop limit, pin a clean zero-velocity hold there
        // (controlled stop-and-hold) — UNLESS following a lead, where the tail is
        // left at the matched cruising speed (rolling horizon). On horizon
        // truncation we likewise leave the tail as-is.
        if reached && !lead_gap_limit && traj.last().is_none_or(|p| p.velocity_mps > STOP_EPSILON_MPS) {
            let (gbx, gby) = point_at(&guide, s_limit);
            let gh = heading_at(&guide, s_limit);
            let glat = bump.at(dist);
            traj.push(TrajectoryPoint {
                pose: Pose {
                    x_m: gbx - glat * gh.sin(),
                    y_m: gby + glat * gh.cos(),
                    heading_rad: gh,
                },
                velocity_mps: 0.0,
                time_from_start_s: t + dt,
            });
        }

        // The checker requires ≥ 2 points; if geometry degenerated, HOLD.
        if traj.len() < 2 {
            return PlanOutput::safe_stop(input.ego.pose);
        }

        // When the path was bumped, recompute headings from consecutive poses so
        // the checker's per-pose steering derivation matches the actual curved
        // path (not the straight-guide tangent).
        if bump.y_off != 0.0 {
            for i in 0..traj.len() - 1 {
                let (ax, ay) = (traj[i].pose.x_m, traj[i].pose.y_m);
                let (bx, by) = (traj[i + 1].pose.x_m, traj[i + 1].pose.y_m);
                if (bx - ax).hypot(by - ay) > 1e-6 {
                    traj[i].pose.heading_rad = (by - ay).atan2(bx - ax);
                }
            }
            let n = traj.len();
            traj[n - 1].pose.heading_rad = traj[n - 2].pose.heading_rad;
        }
        PlanOutput { trajectory: traj, kind: ProposalKind::Motion }
    }
}

// --- geometry helpers (private; pure, allocation-bounded by polyline length) ---

fn dist2d(ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt()
}

/// Path curvature (1/m) at arc-length `s` on the guide, via the Menger curvature of
/// three samples a span `d` apart: `κ = 4·area / (|AB|·|BC|·|CA|)`. Robust to the
/// piecewise-linear guide (a wider `d` averages over vertices). Returns 0 on a
/// straight / degenerate triple.
fn curvature_at(guide: &[(f64, f64)], s: f64, d: f64) -> f64 {
    let a = point_at(guide, s - d);
    let b = point_at(guide, s);
    let c = point_at(guide, s + d);
    let ab = dist2d(a.0, a.1, b.0, b.1);
    let bc = dist2d(b.0, b.1, c.0, c.1);
    let ca = dist2d(c.0, c.1, a.0, a.1);
    let denom = ab * bc * ca;
    if denom < 1e-9 {
        return 0.0;
    }
    // Twice the signed triangle area (cross product), magnitude.
    let area2 = ((b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)).abs();
    (2.0 * area2) / denom
}

/// The speed (m/s) at which a curvature TRANSITION of rate `dkappa_ds` (1/m per m of arc length)
/// at curvature `kappa` keeps the bicycle-model steering rate at the comfort limit `max_rate_rads`.
/// Steering angle `δ = atan(L·κ)` ⇒ `dδ/dt = [L/(1+(Lκ)²)]·(dκ/ds)·v`; solving `dδ/dt = max_rate`
/// for `v` gives the cap. Returns `f64::INFINITY` (no cap) on a straight / constant-curvature path
/// (`dκ/ds ≈ 0`) or when disabled (`max_rate ≤ 0`), so it is a no-op exactly there.
fn steering_rate_speed_cap(kappa: f64, dkappa_ds: f64, wheelbase_m: f64, max_rate_rads: f64) -> f64 {
    let dk = dkappa_ds.abs();
    if dk <= 1e-6 || max_rate_rads <= 0.0 {
        return f64::INFINITY;
    }
    let l = wheelbase_m.max(1e-3);
    let ddelta_dkappa = l / (1.0 + (l * kappa).powi(2));
    max_rate_rads / (ddelta_dkappa * dk)
}

/// Candidates per side the joint optimizer samples (plus the centerline) — `2·N+1` paths total,
/// keeping the spatiotemporal search WCET-bounded.
const JOINT_OFFSET_SAMPLES_PER_SIDE: usize = 3;
/// Deviation penalty (s per metre of |offset|) added to a candidate's traversal time, so the
/// optimizer prefers the centerline unless an offset buys real time (a flatter corner) — and is a
/// no-op on a straight road (equal times ⇒ the zero-offset centerline's zero penalty wins).
const JOINT_DEVIATION_WEIGHT_S_PER_M: f64 = 0.04;
/// Arc length (m) over which a candidate offset ramps in from the ego, so the path does not jump
/// laterally at the start (the ego sits on the centerline).
const JOINT_OFFSET_RAMP_M: f64 = 6.0;

/// Total arc length of a polyline.
fn polyline_len(p: &[(f64, f64)]) -> f64 {
    p.windows(2).map(|w| dist2d(w[0].0, w[0].1, w[1].0, w[1].1)).sum()
}

/// **Signed** path curvature (1/m) at arc-length `s`: the unsigned Menger [`curvature_at`] magnitude
/// with the turn's sign — **positive = left turn** (the path bends toward +perpendicular). The joint
/// optimizer's apex offset uses the sign to cut toward the INSIDE of each bend.
fn signed_curvature_at(guide: &[(f64, f64)], s: f64, d: f64) -> f64 {
    let kappa = curvature_at(guide, s, d);
    let a = point_at(guide, s - d);
    let b = point_at(guide, s);
    let c = point_at(guide, s + d);
    // z of (b−a)×(c−b): > 0 ⇒ left turn.
    let cross = (b.0 - a.0) * (c.1 - b.1) - (b.1 - a.1) * (c.0 - b.0);
    if cross >= 0.0 { kappa } else { -kappa }
}

/// A copy of `guide` displaced by an **apex-varying** perpendicular offset: at each station the
/// offset is `delta · signed_κ(s)/κ_max`, ramped in from the ego — so it is ZERO on a straight
/// (κ=0), peaks at the bend's apex (max κ), and is signed toward the INSIDE of the turn. This is a
/// real corner-cut (it shortens the path across the apex), where a constant offset merely
/// parallel-shifts an already-Chaikin-smoothed line and buys nothing. The joint optimizer scores
/// each candidate's curvature + length through the velocity profile.
fn offset_guide(guide: &[(f64, f64)], s_ego: f64, delta: f64, kappa_max: f64) -> Vec<(f64, f64)> {
    if delta == 0.0 || guide.len() < 2 || kappa_max <= 1e-6 {
        return guide.to_vec();
    }
    let mut acc = 0.0;
    let mut out = Vec::with_capacity(guide.len());
    for i in 0..guide.len() {
        if i > 0 {
            acc += dist2d(guide[i - 1].0, guide[i - 1].1, guide[i].0, guide[i].1);
        }
        let ramp = if acc <= s_ego { 0.0 } else { ((acc - s_ego) / JOINT_OFFSET_RAMP_M).min(1.0) };
        let frac = (signed_curvature_at(guide, acc, 3.0) / kappa_max).clamp(-1.0, 1.0);
        let o = delta * frac * ramp;
        let h = heading_at(guide, acc);
        out.push((guide[i].0 - o * h.sin(), guide[i].1 + o * h.cos()));
    }
    out
}

/// The four corners (m) of the vehicle footprint at pose `(x, y, heading)`: a rectangle
/// `±half_len` along heading × `±half_wid` across it. Used by the joint optimizer's oriented
/// containment so a candidate line's ANGLED footprint (its corners reach further laterally on a
/// curve) is checked, not just the path centroid.
fn footprint_corners(x: f64, y: f64, heading: f64, half_len: f64, half_wid: f64) -> [(f64, f64); 4] {
    let (c, s) = (heading.cos(), heading.sin());
    let mut out = [(0.0, 0.0); 4];
    for (i, (sl, sw)) in [(1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)].iter().enumerate() {
        let (dx, dy) = (sl * half_len, sw * half_wid);
        out[i] = (x + dx * c - dy * s, y + dx * s + dy * c);
    }
    out
}

/// The corridor's narrowest half-width (m): the minimum, over centerline samples, of the
/// perpendicular distance to either boundary. A conservative scalar the oriented containment checks
/// each footprint corner's centerline-relative lateral offset against. Uses `project_signed`, so it
/// is correct on a curve (unlike an x-indexed boundary lookup).
fn corridor_half_width(centerline: &[(f64, f64)], left: &[Point], right: &[Point]) -> f64 {
    let left2: Vec<(f64, f64)> = left.iter().map(|p| (p.x_m, p.y_m)).collect();
    let right2: Vec<(f64, f64)> = right.iter().map(|p| (p.x_m, p.y_m)).collect();
    let len = polyline_len(centerline);
    let mut hw = f64::INFINITY;
    let mut s = 0.0;
    while s <= len {
        let (cx, cy) = point_at(centerline, s);
        let dl = project_signed(&left2, cx, cy).1.abs();
        let dr = project_signed(&right2, cx, cy).1.abs();
        hw = hw.min(dl).min(dr);
        s += VELOCITY_PROFILE_DS;
    }
    hw
}

/// Position at arc-length `s` along a `Point` polyline (the predicted-path / lane
/// centerline). `s <= 0` → the first vertex; past the end → clamped to the last
/// (the object holds at the path end rather than extrapolating off it).
fn point_on_path(points: &[Point], s: f64) -> (f64, f64) {
    match points.first() {
        None => return (0.0, 0.0),
        Some(p0) if s <= 0.0 => return (p0.x_m, p0.y_m),
        _ => {}
    }
    let mut acc = 0.0;
    for w in points.windows(2) {
        let seg = dist2d(w[0].x_m, w[0].y_m, w[1].x_m, w[1].y_m);
        if acc + seg >= s {
            let f = if seg > 1e-9 { (s - acc) / seg } else { 0.0 };
            return (w[0].x_m + f * (w[1].x_m - w[0].x_m), w[0].y_m + f * (w[1].y_m - w[0].y_m));
        }
        acc += seg;
    }
    let last = points.last().unwrap();
    (last.x_m, last.y_m)
}

/// Chaikin corner-cutting smoothing of an open polyline, applied `iterations`
/// times. Each interior corner `P` is replaced by the two points ¼ and ¾ of the
/// way along its incident edges, rounding the corner; the FIRST and LAST vertices
/// are kept (the path must still start at the ego projection and end at the goal).
/// The new vertices are convex combinations of adjacent originals — they lie on the
/// original edges — so for a corridor CENTERLINE the result stays within the
/// corridor (containment still backstops). A straight polyline is unchanged.
fn chaikin_smooth(poly: &[(f64, f64)], iterations: usize) -> Vec<(f64, f64)> {
    let mut pts = poly.to_vec();
    for _ in 0..iterations {
        if pts.len() < 3 {
            break; // nothing to round
        }
        let mut next = Vec::with_capacity(pts.len() * 2);
        next.push(pts[0]); // keep the start
        for w in pts.windows(2) {
            let (p, q) = (w[0], w[1]);
            next.push((0.75 * p.0 + 0.25 * q.0, 0.75 * p.1 + 0.25 * q.1)); // ¼
            next.push((0.25 * p.0 + 0.75 * q.0, 0.25 * p.1 + 0.75 * q.1)); // ¾
        }
        next.push(pts[pts.len() - 1]); // keep the end
        pts = next;
    }
    pts
}

/// Corridor centerline = pairwise midpoints of the boundary polylines over their
/// shared prefix. `len < 2` means "unusable" (caller falls back to ego→goal).
fn centerline_from(left: &[Point], right: &[Point]) -> Vec<(f64, f64)> {
    let n = left.len().min(right.len());
    (0..n)
        .map(|i| {
            (
                0.5 * (left[i].x_m + right[i].x_m),
                0.5 * (left[i].y_m + right[i].y_m),
            )
        })
        .collect()
}

/// Prefix-sum arc length up to each vertex.
fn cumulative(poly: &[(f64, f64)]) -> Vec<f64> {
    let mut acc = Vec::with_capacity(poly.len());
    let mut total = 0.0;
    for (i, &(x, y)) in poly.iter().enumerate() {
        if i > 0 {
            total += dist2d(poly[i - 1].0, poly[i - 1].1, x, y);
        }
        acc.push(total);
    }
    acc
}

/// Point on the polyline at arc length `s` (clamped to `[0, total]`).
fn point_at(poly: &[(f64, f64)], s: f64) -> (f64, f64) {
    match poly.len() {
        0 => return (0.0, 0.0),
        1 => return poly[0],
        _ => {}
    }
    let cum = cumulative(poly);
    let total = *cum.last().unwrap();
    let s = s.clamp(0.0, total);
    for i in 1..poly.len() {
        if s <= cum[i] {
            let seg = cum[i] - cum[i - 1];
            let f = if seg > 1e-9 { (s - cum[i - 1]) / seg } else { 0.0 };
            return (
                poly[i - 1].0 + f * (poly[i].0 - poly[i - 1].0),
                poly[i - 1].1 + f * (poly[i].1 - poly[i - 1].1),
            );
        }
    }
    *poly.last().unwrap()
}

/// Tangent heading (rad) of the polyline at arc length `s`.
fn heading_at(poly: &[(f64, f64)], s: f64) -> f64 {
    if poly.len() < 2 {
        return 0.0;
    }
    let cum = cumulative(poly);
    let total = *cum.last().unwrap();
    let s = s.clamp(0.0, total);
    for i in 1..poly.len() {
        if s <= cum[i] + 1e-9 {
            return (poly[i].1 - poly[i - 1].1).atan2(poly[i].0 - poly[i - 1].0);
        }
    }
    let n = poly.len();
    (poly[n - 1].1 - poly[n - 2].1).atan2(poly[n - 1].0 - poly[n - 2].0)
}

/// Nearest point on the polyline to `(qx, qy)`, as `(arc_length, lateral_dist)`.
fn project_point(poly: &[(f64, f64)], qx: f64, qy: f64) -> (f64, f64) {
    if poly.len() < 2 {
        return (0.0, f64::INFINITY);
    }
    let cum = cumulative(poly);
    let mut best = (f64::INFINITY, 0.0_f64); // (lateral_dist, arc_length)
    for i in 1..poly.len() {
        let (ax, ay) = poly[i - 1];
        let (bx, by) = poly[i];
        let (ex, ey) = (bx - ax, by - ay);
        let seg2 = ex * ex + ey * ey;
        let t = if seg2 > 1e-9 {
            (((qx - ax) * ex + (qy - ay) * ey) / seg2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (px, py) = (ax + t * ex, ay + t * ey);
        let d = dist2d(qx, qy, px, py);
        if d < best.0 {
            best = (d, cum[i - 1] + t * (cum[i] - cum[i - 1]));
        }
    }
    (best.1, best.0)
}

/// Arc length of the point on the polyline nearest to `(qx, qy)`.
fn project_arc_length(poly: &[(f64, f64)], qx: f64, qy: f64) -> f64 {
    project_point(poly, qx, qy).0
}

/// Nearest point on the polyline to `(qx, qy)` as `(arc_length, signed_lateral)`,
/// where `signed_lateral > 0` means the query lies to the LEFT of the guide
/// direction. `|signed_lateral|` equals the perpendicular distance.
fn project_signed(poly: &[(f64, f64)], qx: f64, qy: f64) -> (f64, f64) {
    if poly.len() < 2 {
        return (0.0, 0.0);
    }
    let cum = cumulative(poly);
    let mut best = (f64::INFINITY, 0.0_f64, 0.0_f64); // (dist, arc_s, signed)
    for i in 1..poly.len() {
        let (ax, ay) = poly[i - 1];
        let (bx, by) = poly[i];
        let (ex, ey) = (bx - ax, by - ay);
        let seg2 = ex * ex + ey * ey;
        let t = if seg2 > 1e-9 {
            (((qx - ax) * ex + (qy - ay) * ey) / seg2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (px, py) = (ax + t * ex, ay + t * ey);
        let d = dist2d(qx, qy, px, py);
        if d < best.0 {
            let seg_len = seg2.sqrt().max(1e-9);
            // 2D cross product of segment direction × (q - a): left-positive.
            let signed = (ex * (qy - ay) - ey * (qx - ax)) / seg_len;
            best = (d, cum[i - 1] + t * (cum[i] - cum[i - 1]), signed);
        }
    }
    (best.1, best.2)
}

/// Interpolated boundary `y` at longitudinal `x` (boundary vertices are x-ordered).
fn boundary_y_at(boundary: &[Point], x: f64) -> f64 {
    match boundary.first() {
        None => return 0.0,
        Some(p) if x <= p.x_m => return p.y_m,
        _ => {}
    }
    for w in boundary.windows(2) {
        if x <= w[1].x_m {
            let dx = w[1].x_m - w[0].x_m;
            let f = if dx.abs() > 1e-9 { (x - w[0].x_m) / dx } else { 0.0 };
            return w[0].y_m + f * (w[1].y_m - w[0].y_m);
        }
    }
    boundary.last().unwrap().y_m
}

/// A trapezoidal lateral-offset profile along the guide: ramp 0 → `y_off`, hold
/// across the object, ramp back to 0. `at(s)` is the lateral offset at distance
/// `s` from the ego, applied perpendicular to the guide.
#[derive(Debug, Clone, Copy)]
struct LateralBump {
    y_off: f64,
    ramp_len: f64,
    hold_start: f64,
    hold_end: f64,
}

impl LateralBump {
    const NONE: Self = Self { y_off: 0.0, ramp_len: 1.0, hold_start: 0.0, hold_end: 0.0 };

    fn at(&self, s: f64) -> f64 {
        if self.y_off == 0.0 {
            return 0.0;
        }
        // Smoothstep ramps (C1: zero slope at both ends) so the path has no
        // heading corners — a linear ramp's corners spike the steering rate.
        let smooth = |u: f64| {
            let u = u.clamp(0.0, 1.0);
            u * u * (3.0 - 2.0 * u)
        };
        let up0 = self.hold_start - self.ramp_len;
        if s <= up0 {
            0.0
        } else if s < self.hold_start {
            self.y_off * smooth((s - up0) / self.ramp_len)
        } else if s <= self.hold_end {
            self.y_off
        } else {
            let down1 = self.hold_end + self.ramp_len;
            self.y_off * smooth((down1 - s) / self.ramp_len)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_ros2_adapter::config::VehicleConfig;
    use kirra_ros2_adapter::corridor::MockCorridorSource;
    use kirra_ros2_adapter::validate_trajectory_slow;

    fn sample_input<'a>(map: &'a dyn CorridorSource) -> PlanInput<'a> {
        PlanInput {
            ego: EgoState {
                pose: Pose { x_m: 0.0, y_m: 0.0, heading_rad: 0.0 },
                linear_x_mps: 3.0,
                yaw_rate_rads: 0.0,
                stamp_ms: 0,
            },
            goal: Goal { target: Pose { x_m: 50.0, y_m: 0.0, heading_rad: 0.0 } },
            map,
            objects: &[],
            controls: &[],
            lane_boundaries: &[],
            motion: &[],
            predicted_paths: &[],
            cedes_to_ego_ids: &[],
            lane_change_to_m: None,
            no_overtake_ids: &[],
            drivable: None,
            posture: FleetPosture::Nominal,
            target_speed_mps: None,
            request_overtake: false,
            request_pull_over: false,
            lane_graph: None,
            signal_states: &[],        }
    }

    #[test]
    fn safe_stop_is_valid_stop_proposal() {
        let out = PlanOutput::safe_stop(Pose { x_m: 1.0, y_m: 2.0, heading_rad: 0.0 });
        assert_eq!(out.kind, ProposalKind::SafeStop);
        assert!(out.trajectory.len() >= 2, "the checker requires >= 2 points");
        assert!(
            out.trajectory.iter().all(|p| p.velocity_mps == 0.0),
            "a safe-stop proposal is zero velocity"
        );
    }

    #[test]
    fn stop_planner_output_feeds_the_checker() {
        // Construct → feed the EXISTING #131 validation entry → no panic. This is
        // the locked shape proving its job: a planner output is consumable by the
        // real checker at the type level. Verdict content is whatever it is.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let mut planner = StopPlanner;
        let out = planner.plan(&sample_input(&corridor));

        let _verdict: TrajectoryVerdict = validate_trajectory_slow(
            &out.trajectory,
            &corridor,
            &[], // no perceived objects
            &VehicleConfig::default_urban(),
            None, // no odom
            FleetPosture::Nominal,
        );
    }

    #[test]
    fn planner_is_object_safe() {
        let corridor = MockCorridorSource::straight_5m_half_width(10.0);
        let mut boxed: Box<dyn Planner> = Box::new(StopPlanner);
        let out = boxed.plan(&sample_input(&corridor));
        assert_eq!(out.kind, ProposalKind::SafeStop);
    }

    #[test]
    fn planner_mode_maps_every_posture() {
        assert_eq!(planner_mode(FleetPosture::Nominal), PlannerMode::Full);
        assert_eq!(planner_mode(FleetPosture::Degraded), PlannerMode::Conservative);
        assert_eq!(planner_mode(FleetPosture::LockedOut), PlannerMode::MrcOnly);
    }

    // --- GeometricPlanner (Phase-1 reference) -------------------------------

    /// Ego positioned a few metres INTO the corridor (so the vehicle footprint's
    /// rear stays inside the drivable space), with a goal reachable inside the
    /// horizon — the setup a containment-admissible proposal needs.
    fn inside_corridor_input(map: &dyn CorridorSource) -> PlanInput<'_> {
        PlanInput {
            ego: EgoState {
                pose: Pose { x_m: 10.0, y_m: 0.0, heading_rad: 0.0 },
                linear_x_mps: 3.0,
                yaw_rate_rads: 0.0,
                stamp_ms: 0,
            },
            goal: Goal { target: Pose { x_m: 25.0, y_m: 0.0, heading_rad: 0.0 } },
            map,
            objects: &[],
            controls: &[],
            lane_boundaries: &[],
            motion: &[],
            predicted_paths: &[],
            cedes_to_ego_ids: &[],
            lane_change_to_m: None,
            no_overtake_ids: &[],
            drivable: None,
            posture: FleetPosture::Nominal,
            target_speed_mps: None,
            request_overtake: false,
            request_pull_over: false,
            lane_graph: None,
            signal_states: &[],        }
    }

    #[test]
    fn geometric_planner_proposes_motion_toward_goal() {
        // Default sample: goal (x=50) is beyond the rolling horizon, so this is
        // the "drive toward the goal" case (no terminal stop), checked for
        // monotonic in-corridor motion that ramps up.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let mut p = GeometricPlanner::default();
        let out = p.plan(&sample_input(&corridor));

        assert_eq!(out.kind, ProposalKind::Motion);
        assert!(out.trajectory.len() >= 2, "checker requires >= 2 points");
        // Centerline is along +X at y = 0 → poses advance in +X and stay centered.
        let xs: Vec<f64> = out.trajectory.iter().map(|t| t.pose.x_m).collect();
        assert!(
            xs.windows(2).all(|w| w[1] >= w[0] - 1e-6),
            "trajectory is monotonic along the corridor"
        );
        assert!(
            out.trajectory.iter().all(|t| t.pose.y_m.abs() < 5.0),
            "every pose stays inside the 5 m half-width corridor"
        );
        // Ramps up from the 3 m/s current speed toward cruise.
        let vmax = out.trajectory.iter().map(|t| t.velocity_mps).fold(0.0, f64::max);
        assert!(vmax > 3.0, "proposal accelerates toward cruise, got vmax {vmax}");
    }

    #[test]
    fn geometric_planner_reaches_goal_and_stops() {
        // A goal inside the horizon → reaches it with a controlled stop-and-hold.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let mut p = GeometricPlanner::default();
        let out = p.plan(&inside_corridor_input(&corridor));

        assert_eq!(out.kind, ProposalKind::Motion);
        assert!(
            out.trajectory.last().unwrap().velocity_mps <= STOP_EPSILON_MPS,
            "terminal point is a controlled stop at the goal"
        );
    }

    #[test]
    fn geometric_planner_output_is_checker_admissible() {
        // The strong claim: the real #131 checker ADMITS a nominal in-corridor
        // proposal (Accept or Clamp — both "safe to drive"), not just consumes it.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let mut p = GeometricPlanner::default();
        let out = p.plan(&inside_corridor_input(&corridor));
        assert!(out.trajectory.len() <= MAX_TRAJECTORY_HORIZON, "within checker horizon");

        let verdict = validate_trajectory_slow(
            &out.trajectory,
            &corridor,
            &[], // no perceived objects
            &VehicleConfig::default_urban(),
            None, // no odom
            FleetPosture::Nominal,
        );
        assert!(
            matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
            "checker should admit the nominal proposal, got {verdict:?}"
        );
    }

    #[test]
    fn speed_profile_is_jerk_limited() {
        // Launch toward a FAR goal (no obstacle): the profile ramps acceleration as
        // an S-curve rather than the old bang-bang step to full accel. The jerk (the
        // second difference of the speed samples) stays within max_jerk, and the
        // launch acceleration ramps up instead of jumping to the limit on step one.
        let corridor = MockCorridorSource::straight_5m_half_width(300.0);
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_objects(&corridor, 5.0, 250.0, &[]));
        let cfg = GeometricPlannerConfig::default();
        let dt = cfg.sample_dt_s;

        let v: Vec<f64> = out.trajectory.iter().map(|tp| tp.velocity_mps).collect();
        assert!(v.len() >= 4, "need a profile to measure, got {}", v.len());
        // Far goal → horizon-truncated cruise tail (no terminal stop-jump to skew jerk).
        let mut max_jerk_seen = 0.0_f64;
        for w in v.windows(3) {
            let jerk = (w[2] - 2.0 * w[1] + w[0]) / (dt * dt);
            max_jerk_seen = max_jerk_seen.max(jerk.abs());
        }
        assert!(max_jerk_seen <= cfg.max_jerk_mps3 * 1.05 + 1e-9,
            "speed-profile jerk {max_jerk_seen} exceeds max_jerk {}", cfg.max_jerk_mps3);
        // Smooth launch: the first acceleration is well below max (ramped in), where
        // the old bang-bang profile jumped straight to max_accel on step one.
        let a0 = (v[1] - v[0]) / dt;
        assert!(a0 < cfg.max_accel_mps2,
            "launch acceleration {a0} should ramp, not jump to max {}", cfg.max_accel_mps2);
        assert!(a0 > 0.0, "and the ego does accelerate from the low start speed");
    }

    /// A corridor that turns at x=20 (centerline kink (0,0)→(20,0)→(40,2.5)),
    /// half-width 5. The midpoint boundaries reproduce the kink as the guide.
    struct KinkedCorridor {
        left: Vec<Point>,
        right: Vec<Point>,
    }
    impl KinkedCorridor {
        fn new() -> Self {
            Self {
                left: vec![
                    Point { x_m: 0.0, y_m: 5.0 },
                    Point { x_m: 20.0, y_m: 5.0 },
                    Point { x_m: 40.0, y_m: 7.5 },
                ],
                right: vec![
                    Point { x_m: 0.0, y_m: -5.0 },
                    Point { x_m: 20.0, y_m: -5.0 },
                    Point { x_m: 40.0, y_m: -2.5 },
                ],
            }
        }
    }
    impl CorridorSource for KinkedCorridor {
        fn left_boundary(&self) -> &[Point] { &self.left }
        fn right_boundary(&self) -> &[Point] { &self.right }
        fn confidence(&self) -> f32 { 0.95 }
        fn age_ms(&self) -> u64 { 10 }
    }

    fn kinked_input(map: &dyn CorridorSource) -> PlanInput<'_> {
        PlanInput {
            ego: EgoState {
                pose: Pose { x_m: 0.0, y_m: 0.0, heading_rad: 0.0 },
                linear_x_mps: 2.0,
                yaw_rate_rads: 0.0,
                stamp_ms: 0,
            },
            goal: Goal { target: Pose { x_m: 40.0, y_m: 2.5, heading_rad: 0.0 } },
            map,
            objects: &[],
            controls: &[],
            lane_boundaries: &[],
            motion: &[],
            predicted_paths: &[],
            cedes_to_ego_ids: &[],
            lane_change_to_m: None,
            no_overtake_ids: &[],
            drivable: None,
            posture: FleetPosture::Nominal,
            target_speed_mps: None,
            request_overtake: false,
            request_pull_over: false,
            lane_graph: None,
            signal_states: &[],        }
    }

    /// Corridor that turns ~22° at x=20 and continues well past it; ego starts
    /// INSIDE (rear overhang has room), goal beyond the curve.
    fn curve_corridor() -> KinkedCorridor {
        // Centerline (0,0)→(20,0)→(34,20)→(48,40)→(62,60): a sharp ~55° turn at
        // x=20, then straight. Boundaries are ±5 in y about the centerline.
        KinkedCorridor {
            left: vec![
                Point { x_m: 0.0, y_m: 5.0 }, Point { x_m: 20.0, y_m: 5.0 },
                Point { x_m: 34.0, y_m: 25.0 }, Point { x_m: 48.0, y_m: 45.0 },
                Point { x_m: 62.0, y_m: 65.0 },
            ],
            right: vec![
                Point { x_m: 0.0, y_m: -5.0 }, Point { x_m: 20.0, y_m: -5.0 },
                Point { x_m: 34.0, y_m: 15.0 }, Point { x_m: 48.0, y_m: 35.0 },
                Point { x_m: 62.0, y_m: 55.0 },
            ],
        }
    }
    fn curve_input(map: &dyn CorridorSource) -> PlanInput<'_> {
        PlanInput {
            ego: EgoState {
                pose: Pose { x_m: 6.0, y_m: 0.0, heading_rad: 0.0 },
                linear_x_mps: 6.0,
                yaw_rate_rads: 0.0,
                stamp_ms: 0,
            },
            goal: Goal { target: Pose { x_m: 70.0, y_m: 20.0, heading_rad: 0.0 } },
            ..kinked_input(map)
        }
    }

    #[test]
    fn curvature_aware_speed_slows_for_a_curve() {
        let corr = curve_corridor();
        // In-curve region (world x in [18, 44]) min speed: the ego SLOWS for the bend.
        // Isolate the lateral-accel (curvature) cap: disable the steering-rate cap so this test
        // varies only `comfort_lateral_accel_mps2` (the steering-rate cap has its own tests).
        let curve_min_speed = |comfort: f64| -> f64 {
            let cfg = GeometricPlannerConfig { comfort_lateral_accel_mps2: comfort, max_steering_rate_rads: 0.0, ..Default::default() };
            let out = GeometricPlanner::new(cfg).plan(&curve_input(&corr));
            out.trajectory.iter()
                .filter(|p| p.pose.x_m >= 18.0 && p.pose.x_m <= 44.0)
                .map(|p| p.velocity_mps)
                .fold(f64::INFINITY, f64::min)
        };
        let slowed = curve_min_speed(2.0);          // curvature-aware
        let unslowed = curve_min_speed(1.0e9);       // cap effectively disabled
        assert!(slowed.is_finite() && unslowed.is_finite(), "ego reaches the curve");
        assert!(slowed < unslowed - 1.0,
            "curvature-aware speed slows through the bend: slowed={slowed:.2}, unslowed={unslowed:.2}");
        assert!(slowed < 6.0, "and it actually slows below cruise in the bend, got {slowed:.2}");

        // The cap REDUCES the path's actual (Menger) lateral accel v²·κ vs taking
        // the bend at cruise, and keeps it under the checker's ceiling — measured
        // off the guide curvature, not per-pose heading deltas (which spike at the
        // piecewise-linear guide's vertices).
        let guide = chaikin_smooth(
            &centerline_from(corr.left_boundary(), corr.right_boundary()),
            GeometricPlannerConfig::default().path_smoothing_iterations,
        );
        let peak_lat = |comfort: f64| -> f64 {
            let cfg = GeometricPlannerConfig { comfort_lateral_accel_mps2: comfort, max_steering_rate_rads: 0.0, ..Default::default() };
            let out = GeometricPlanner::new(cfg).plan(&curve_input(&corr));
            out.trajectory.iter()
                .map(|p| {
                    let s = project_arc_length(&guide, p.pose.x_m, p.pose.y_m);
                    p.velocity_mps.powi(2) * curvature_at(&guide, s, 3.0)
                })
                .fold(0.0_f64, f64::max)
        };
        let lat_capped = peak_lat(2.0);
        let lat_uncapped = peak_lat(1.0e9);
        assert!(lat_capped < lat_uncapped - 1.0,
            "the cap reduces peak lateral accel: capped={lat_capped:.2}, uncapped={lat_uncapped:.2}");
        assert!(lat_capped < 3.5, "and keeps it under the checker's ceiling, got {lat_capped:.2}");

        let out = GeometricPlanner::default().plan(&curve_input(&corr));
        let verdict = validate_trajectory_slow(
            &out.trajectory, &corr, &[], &VehicleConfig::default_urban(), None, FleetPosture::Nominal,
        );
        assert!(matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
            "curvature-aware turn is admissible, got {verdict:?}");
    }

    #[test]
    fn velocity_profile_handles_a_curve_then_a_stop() {
        // The full forward–backward profile on a guide that is straight, then turns
        // at arc≈30, then straight to a stop at the end. One pass must: reach cruise
        // on the open straight (not needlessly slow), dip for the curve, recover
        // after it, and decelerate to 0 at the stop — a SEQUENCE the incremental
        // look-ahead cannot resolve jointly.
        // Isolate the forward–backward curve+stop sequencing on this RAW (unsmoothed) guide: disable
        // the steering-rate cap, whose curvature-transition bound on a sharp unsmoothed vertex would
        // (correctly) ripple upstream and confound the "cruise on the open straight" check. The
        // steering-rate cap has its own tests (on smoothed guides, as the planner actually uses).
        let p = GeometricPlanner::new(GeometricPlannerConfig { max_steering_rate_rads: 0.0, ..Default::default() });
        let guide: Vec<(f64, f64)> = vec![
            (0.0, 0.0), (30.0, 0.0), (42.0, 14.0), (90.0, 62.0),
        ];
        let dist = 80.0;
        let (target, decel) = (8.0, 2.5);
        let prof = p.velocity_profile(&guide, 0.0, dist, target, decel, true);
        assert!(prof.len() > 10);

        let at = |s: f64| sample_profile(&prof, s);
        // Reaches cruise on the early open straight (arc 10–20, before the bend).
        let early_max = (10..=20).map(|x| at(x as f64)).fold(0.0_f64, f64::max);
        assert!(early_max > target - 0.2, "cruises where unconstrained, got {early_max:.2}");
        // Dips for the curve (around arc 30).
        let curve_min = (26..=38).map(|x| at(x as f64)).fold(f64::INFINITY, f64::min);
        assert!(curve_min < target - 1.0, "slows for the curve, got {curve_min:.2}");
        // Recovers after the curve (arc 50+) — not stuck slow.
        let recover = at(52.0);
        assert!(recover > curve_min + 0.5, "speeds back up after the curve, got {recover:.2}");
        // Decelerates to a stop at the end.
        assert!(*prof.last().unwrap() < 0.1, "ends stopped, got {}", prof.last().unwrap());
        // Every downstep is deceleration-feasible (backward-pass invariant).
        for w in prof.windows(2) {
            let max_drop = w[0] - (w[1] * w[1] + 2.0 * decel * VELOCITY_PROFILE_DS).sqrt();
            assert!(max_drop <= 1e-6, "profile decel-infeasible: {} -> {}", w[0], w[1]);
        }
    }

    #[test]
    fn chaikin_smoothing_reduces_path_curvature_at_a_corridor_kink() {
        // The raw guide's heading JUMPS at the kink (a coarse-corridor vertex) →
        // a curvature spike. Chaikin corner-cutting spreads the turn over several
        // samples → far lower PEAK path curvature (Δheading/Δs), the comfort win and
        // the steering-rate the checker derives. (The path STILL follows the same
        // corridor; admitting a turn at cruise additionally needs curvature-aware
        // SPEED — a separate lever — since steering rate scales with speed.)
        let corr = KinkedCorridor::new();
        let peak_curvature = |iters: usize| -> f64 {
            let cfg = GeometricPlannerConfig {
                path_smoothing_iterations: iters,
                ..Default::default()
            };
            let out = GeometricPlanner::new(cfg).plan(&kinked_input(&corr));
            out.trajectory
                .windows(2)
                .map(|w| {
                    let dh = (w[1].pose.heading_rad - w[0].pose.heading_rad).abs();
                    let ds = dist2d(w[0].pose.x_m, w[0].pose.y_m, w[1].pose.x_m, w[1].pose.y_m)
                        .max(1e-6);
                    dh / ds
                })
                .fold(0.0_f64, f64::max)
        };
        let raw = peak_curvature(0);
        let smooth = peak_curvature(2);
        assert!(smooth < raw * 0.5,
            "smoothing should at least halve peak path curvature: raw={raw}, smooth={smooth}");
        assert!(smooth.is_finite() && smooth > 0.0, "the smoothed path still turns");
        // The smoothed proposal is still a forward Motion that advances past the kink.
        let out = GeometricPlanner::default().plan(&kinked_input(&corr));
        assert_eq!(out.kind, ProposalKind::Motion);
        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(max_x > 20.0, "the smoothed path drives through the kink, got max_x {max_x}");
    }

    #[test]
    fn steering_rate_cap_helper_is_sound() {
        let (l, rate) = (2.7, 0.4);
        // No curvature transition (dκ/ds = 0) or disabled → no cap.
        assert_eq!(steering_rate_speed_cap(0.05, 0.0, l, rate), f64::INFINITY);
        assert_eq!(steering_rate_speed_cap(0.05, 0.02, l, 0.0), f64::INFINITY);
        // A transition → a finite, positive speed cap.
        let cap = steering_rate_speed_cap(0.05, 0.02, l, rate);
        assert!(cap.is_finite() && cap > 0.0, "a transition produces a finite cap, got {cap}");
        // A sharper transition (larger |dκ/ds|) → tighter cap; more rate budget → looser cap.
        assert!(steering_rate_speed_cap(0.05, 0.04, l, rate) < cap, "sharper transition ⇒ slower");
        assert!(steering_rate_speed_cap(0.05, 0.02, l, rate * 2.0) > cap, "more rate budget ⇒ faster");
        // Only the magnitude of dκ/ds matters (entering vs exiting a bend are symmetric).
        assert_eq!(steering_rate_speed_cap(0.05, -0.02, l, rate), cap);
    }

    #[test]
    fn steering_rate_cap_slows_a_sharp_transition_and_kirra_admits() {
        // Isolate the steering-rate cap from the lateral-accel cap (disable the latter): on the
        // kink's sharp curvature TRANSITION the steering-rate cap slows the ego where the κ-only cap
        // would not (κ is still small at the entry, but dκ/ds is large).
        let corr = curve_corridor(); // a sharp ~55° turn at x≈20
        let transition_min_speed = |rate: f64| -> f64 {
            // Disable the κ-cap so the two arms differ ONLY in the steering-rate cap — the variable
            // under test. At the bend ENTRY κ is still ramping (loose κ-cap) but dκ/ds is large.
            let cfg = GeometricPlannerConfig { comfort_lateral_accel_mps2: 1.0e9, max_steering_rate_rads: rate, ..Default::default() };
            GeometricPlanner::new(cfg)
                .plan(&curve_input(&corr))
                .trajectory
                .iter()
                .filter(|p| p.pose.x_m >= 18.0 && p.pose.x_m <= 44.0)
                .map(|p| p.velocity_mps)
                .fold(f64::INFINITY, f64::min)
        };
        let capped = transition_min_speed(0.4);
        let uncapped = transition_min_speed(0.0);
        assert!(capped.is_finite() && uncapped.is_finite(), "the ego reaches the transition");
        assert!(capped < uncapped - 0.5,
            "the steering-rate cap slows the sharp transition: capped={capped:.2}, uncapped={uncapped:.2}");

        // The shipped default (both caps on) is checker-admissible through the bend.
        let out = GeometricPlanner::default().plan(&curve_input(&corr));
        let verdict = validate_trajectory_slow(
            &out.trajectory, &corr, &[], &VehicleConfig::default_urban(), None, FleetPosture::Nominal,
        );
        assert!(matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
            "the steering-rate-capped turn is admissible, got {verdict:?}");
    }

    #[test]
    fn steering_rate_cap_is_a_no_op_on_a_straight_road() {
        // No curvature ⇒ no transition ⇒ the cap never binds: the speed profile is byte-identical
        // whether it is enabled or not (the WCET-critical straight path is unchanged).
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let peak = |rate: f64| -> f64 {
            let cfg = GeometricPlannerConfig { max_steering_rate_rads: rate, ..Default::default() };
            GeometricPlanner::new(cfg)
                .plan(&sample_input(&corr))
                .trajectory
                .iter()
                .map(|p| p.velocity_mps)
                .fold(0.0_f64, f64::max)
        };
        assert!((peak(0.4) - peak(0.0)).abs() < 1e-9, "no curvature transition ⇒ identical profile");
    }

    #[test]
    fn joint_optimizer_finds_a_faster_line_through_a_bend_and_kirra_admits() {
        // A wide (±9 m) corridor with a sharp ~52° bend, taken through the production CHAIKIN-SMOOTHED
        // guide. The curvature-proportional APEX offset cuts the corner (shortens the path across the
        // apex) where a constant parallel shift bought nothing, and the oriented-footprint containment
        // (not a swing-slack heuristic) keeps the line admissible. Centerline
        // (0,0)→(20,0)→(34,18)→(48,36)→(90,36).
        let corr = KinkedCorridor {
            left: vec![
                Point { x_m: 0.0, y_m: 9.0 }, Point { x_m: 20.0, y_m: 9.0 },
                Point { x_m: 34.0, y_m: 27.0 }, Point { x_m: 48.0, y_m: 45.0 }, Point { x_m: 90.0, y_m: 45.0 },
            ],
            right: vec![
                Point { x_m: 0.0, y_m: -9.0 }, Point { x_m: 20.0, y_m: -9.0 },
                Point { x_m: 34.0, y_m: 9.0 }, Point { x_m: 48.0, y_m: 27.0 }, Point { x_m: 90.0, y_m: 27.0 },
            ],
        };
        let goal = (82.0, 36.0);
        let input = PlanInput {
            ego: EgoState { pose: Pose { x_m: 6.0, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: 6.0, yaw_rate_rads: 0.0, stamp_ms: 0 },
            goal: Goal { target: Pose { x_m: goal.0, y_m: goal.1, heading_rad: 0.0 } },
            ..kinked_input(&corr)
        };
        let cfg = GeometricPlannerConfig { joint_path_optimize: true, ..Default::default() };
        let p = GeometricPlanner::new(cfg);
        let base = chaikin_smooth(
            &centerline_from(corr.left_boundary(), corr.right_boundary()),
            GeometricPlannerConfig::default().path_smoothing_iterations,
        );
        let s_ego = project_arc_length(&base, 6.0, 0.0);
        let (target, decel) = (8.0, 2.5);
        let best = p.optimize_guide(&base, &corr, s_ego, goal, target, decel);
        // Time to the goal along each line (the optimizer's objective).
        let time_to_goal = |g: &[(f64, f64)]| -> f64 {
            let gs = project_arc_length(g, goal.0, goal.1);
            let dist = (gs - s_ego).max(VELOCITY_PROFILE_DS);
            p.velocity_profile(g, s_ego, dist, target, decel, true)
                .iter()
                .map(|v| VELOCITY_PROFILE_DS / v.max(0.1))
                .sum::<f64>()
        };
        let (t_best, t_base) = (time_to_goal(&best), time_to_goal(&base));
        assert!(t_best < t_base - 0.05, "the optimizer found a faster line to the goal: best={t_best:.2}s, centerline={t_base:.2}s");

        // Safety: the chosen in-corridor line is checker-admissible, driven end to end.
        let out = GeometricPlanner::new(cfg).plan(&input);
        assert_eq!(out.kind, ProposalKind::Motion, "the optimized plan drives the bend");
        let verdict = validate_trajectory_slow(
            &out.trajectory, &corr, &[], &VehicleConfig::default_urban(), None, FleetPosture::Nominal,
        );
        assert!(matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
            "KIRRA admits the joint-optimized line, got {verdict:?}");
    }

    #[test]
    fn joint_optimizer_is_a_no_op_on_a_straight_road() {
        // No curvature ⇒ every offset candidate has the same traversal time ⇒ the zero-offset
        // centerline (zero deviation penalty) wins ⇒ the plan is byte-identical to the optimizer off.
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let plan_with = |joint: bool| {
            let cfg = GeometricPlannerConfig { joint_path_optimize: joint, ..Default::default() };
            GeometricPlanner::new(cfg).plan(&sample_input(&corr))
        };
        let on = plan_with(true);
        let off = plan_with(false);
        assert_eq!(on.trajectory.len(), off.trajectory.len(), "same trajectory length");
        for (a, b) in on.trajectory.iter().zip(&off.trajectory) {
            assert!((a.pose.y_m - b.pose.y_m).abs() < 1e-9 && (a.velocity_mps - b.velocity_mps).abs() < 1e-9,
                "straight road: the optimizer keeps the centerline ⇒ identical plan");
        }
    }

    #[test]
    fn signed_curvature_has_the_turn_sign() {
        // A left bend (turning toward +y) is positive; a right bend negative; a straight ~0. Evaluate
        // at the bend VERTEX (arc 15) so the ±d samples straddle the turn, not a straight segment.
        let left = vec![(0.0, 0.0), (15.0, 0.0), (28.0, 10.0)];
        let right = vec![(0.0, 0.0), (15.0, 0.0), (28.0, -10.0)];
        let straight = vec![(0.0, 0.0), (10.0, 0.0), (20.0, 0.0), (30.0, 0.0)];
        assert!(signed_curvature_at(&left, 15.0, 3.0) > 0.0, "left turn ⇒ +κ");
        assert!(signed_curvature_at(&right, 15.0, 3.0) < 0.0, "right turn ⇒ −κ");
        assert!(signed_curvature_at(&straight, 15.0, 3.0).abs() < 1e-6, "straight ⇒ 0");
    }

    #[test]
    fn footprint_corners_rotate_with_heading() {
        // At heading 0 the corners are axis-aligned at (±half_len, ±half_wid) about the pose.
        let c = footprint_corners(0.0, 0.0, 0.0, 2.0, 1.0);
        assert!(c.iter().any(|p| (p.0 - 2.0).abs() < 1e-9 && (p.1 - 1.0).abs() < 1e-9));
        assert!(c.iter().any(|p| (p.0 + 2.0).abs() < 1e-9 && (p.1 + 1.0).abs() < 1e-9));
        // Rotated +90°: the +length axis now points +y, so a front corner sits near (∓1, +2).
        let r = footprint_corners(0.0, 0.0, std::f64::consts::FRAC_PI_2, 2.0, 1.0);
        assert!(r.iter().any(|p| p.1 > 1.9), "the length axis swung to +y");
        assert!(r.iter().all(|p| p.0.abs() <= 1.0 + 1e-9), "the width axis is now along x");
    }

    #[test]
    fn joint_optimizer_keeps_the_centerline_when_the_corridor_is_too_narrow() {
        // A narrow corridor that barely fits the footprint leaves no lateral room: oriented
        // containment rejects every offset, so the optimizer returns the centerline unchanged.
        let corr = KinkedCorridor {
            left: vec![
                Point { x_m: 0.0, y_m: 1.3 }, Point { x_m: 20.0, y_m: 1.3 },
                Point { x_m: 34.0, y_m: 19.0 }, Point { x_m: 48.0, y_m: 37.0 },
            ],
            right: vec![
                Point { x_m: 0.0, y_m: -1.3 }, Point { x_m: 20.0, y_m: -1.3 },
                Point { x_m: 34.0, y_m: 16.4 }, Point { x_m: 48.0, y_m: 34.4 },
            ],
        };
        let base = chaikin_smooth(
            &centerline_from(corr.left_boundary(), corr.right_boundary()),
            GeometricPlannerConfig::default().path_smoothing_iterations,
        );
        let p = GeometricPlanner::new(GeometricPlannerConfig { joint_path_optimize: true, ..Default::default() });
        let s_ego = project_arc_length(&base, 6.0, 0.0);
        let best = p.optimize_guide(&base, &corr, s_ego, (40.0, 30.0), 8.0, 2.5);
        assert_eq!(best, base, "no lateral room ⇒ the centerline is kept (oriented containment rejects offsets)");
    }

    #[test]
    fn geometric_planner_locked_out_only_stops() {
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let mut input = sample_input(&corridor);
        input.posture = FleetPosture::LockedOut;
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input);

        assert_eq!(out.kind, ProposalKind::SafeStop);
        assert!(out.trajectory.iter().all(|t| t.velocity_mps == 0.0));
    }

    #[test]
    fn geometric_planner_degraded_is_non_increasing() {
        // Ego moving at 2 m/s, cruise 8 m/s: Degraded must never propose a speed
        // above the current speed (decel-only; no re-acceleration).
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let mut input = sample_input(&corridor);
        input.posture = FleetPosture::Degraded;
        input.ego.linear_x_mps = 2.0;
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input);

        let vmax = out
            .trajectory
            .iter()
            .map(|t| t.velocity_mps)
            .fold(0.0_f64, f64::max);
        assert!(
            vmax <= 2.0 + 1e-9,
            "Degraded proposal must be non-increasing vs current speed, got {vmax}"
        );
    }

    #[test]
    fn geometric_planner_at_goal_holds() {
        // Goal coincident with ego → arrived → HOLD (safe-stop), never re-accelerate.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let mut input = sample_input(&corridor);
        input.goal.target = input.ego.pose;
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input);

        assert_eq!(out.kind, ProposalKind::SafeStop);
    }

    #[test]
    fn geometric_planner_respects_horizon_cap() {
        // A far goal must not exceed the bounded horizon.
        let corridor = MockCorridorSource::straight_5m_half_width(10_000.0);
        let mut input = sample_input(&corridor);
        input.goal.target = Pose { x_m: 9_000.0, y_m: 0.0, heading_rad: 0.0 };
        let cfg = GeometricPlannerConfig { max_points: 20, ..Default::default() };
        let mut p = GeometricPlanner::new(cfg);
        let out = p.plan(&input);

        // max_points proposal points (+ at most one terminal stop point if reached).
        assert!(out.trajectory.len() <= 21, "horizon cap respected");
        assert!(out.trajectory.len() >= 2);
    }

    // --- Independence: KIRRA judges Occy, it does not rubber-stamp it -------
    //
    // The `geometric_planner_output_is_checker_admissible` test proves the
    // checker ADMITS a good proposal. These prove the converse — that the same
    // test *can fail* — by feeding the REAL checker hand-built trajectories
    // standing in for a MISBEHAVING planner. They show the checker exercises
    // judgment on Occy's output and backstops it independently of Occy's own
    // good behavior (the safety argument rests on the checker, not on Occy).

    #[test]
    fn checker_rejects_out_of_corridor_trajectory() {
        // A trajectory leaving the 5 m corridor (y = 10) → hard reject. Proves
        // the admissibility check is not a rubber stamp.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let traj = vec![
            TrajectoryPoint {
                pose: Pose { x_m: 10.0, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 2.0,
                time_from_start_s: 0.0,
            },
            TrajectoryPoint {
                pose: Pose { x_m: 12.0, y_m: 10.0, heading_rad: 1.3 },
                velocity_mps: 2.0,
                time_from_start_s: 1.0,
            },
        ];
        let verdict = validate_trajectory_slow(
            &traj,
            &corridor,
            &[],
            &VehicleConfig::default_urban(),
            None,
            FleetPosture::Nominal,
        );
        assert_eq!(
            verdict,
            TrajectoryVerdict::MRCFallback,
            "checker must reject a departure from the drivable corridor"
        );
    }

    #[test]
    fn checker_does_not_clean_accept_overspeed_trajectory() {
        // In-corridor but at 50 m/s (> 35 max): the checker derates (Clamp) or
        // refuses — it never clean-Accepts. Proves the checker judges speed.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let traj = vec![
            TrajectoryPoint {
                pose: Pose { x_m: 10.0, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 50.0,
                time_from_start_s: 0.0,
            },
            TrajectoryPoint {
                pose: Pose { x_m: 15.0, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 50.0,
                time_from_start_s: 0.1,
            },
        ];
        let verdict = validate_trajectory_slow(
            &traj,
            &corridor,
            &[],
            &VehicleConfig::default_urban(),
            None,
            FleetPosture::Nominal,
        );
        assert_ne!(
            verdict,
            TrajectoryVerdict::Accept,
            "checker must not clean-Accept an overspeed trajectory, got {verdict:?}"
        );
    }

    #[test]
    fn checker_catches_reacceleration_in_degraded() {
        // INDEPENDENCE, tested at the MARGIN: if Occy (wrongly) re-accelerated in
        // Degraded, the checker's #70 non-increasing gate catches it. We inject
        // the SUBTLEST realistic drift — a 5% re-acceleration (2.0 → 2.1 m/s),
        // not an obvious jump — because that is where independence is actually
        // tested: a checker that only catches gross violations is not a real
        // backstop. The gate denies on `proposed > current + 1e-9`, so this
        // marginal increase must still hard-reject (→ MRCFallback). In-corridor
        // and otherwise well-formed, so the ONLY denial reason is the increase.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let traj = vec![
            TrajectoryPoint {
                pose: Pose { x_m: 10.0, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 2.0,
                time_from_start_s: 0.0,
            },
            TrajectoryPoint {
                pose: Pose { x_m: 10.205, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 2.1, // a mere +0.1 m/s — subtle, but a re-acceleration
                time_from_start_s: 0.1,
            },
        ];
        let verdict = validate_trajectory_slow(
            &traj,
            &corridor,
            &[],
            &VehicleConfig::default_urban(),
            None,
            FleetPosture::Degraded,
        );
        assert_eq!(
            verdict,
            TrajectoryVerdict::MRCFallback,
            "checker must reject even a marginal re-acceleration in Degraded, got {verdict:?}"
        );
    }

    // --- Obstacle-aware planning (#90 Occy 1.B) ----------------------------

    fn obj_at(x: f64, y: f64) -> PerceivedObject {
        PerceivedObject {
            id: 1,
            pos: Point { x_m: x, y_m: y },
            velocity_mps: 0.0,
            heading_rad: 0.0,
            vel: Point { x_m: 0.0, y_m: 0.0 },
        }
    }

    /// PlanInput in a wide corridor with an explicit object list. Starts at a low
    /// speed (2 m/s) so a slow obstacle approach reaches its stop within the
    /// bounded horizon.
    fn input_with_objects<'a>(
        map: &'a dyn CorridorSource,
        ego_x: f64,
        goal_x: f64,
        objects: &'a [PerceivedObject],
    ) -> PlanInput<'a> {
        PlanInput {
            ego: EgoState {
                pose: Pose { x_m: ego_x, y_m: 0.0, heading_rad: 0.0 },
                linear_x_mps: 2.0,
                yaw_rate_rads: 0.0,
                stamp_ms: 0,
            },
            goal: Goal { target: Pose { x_m: goal_x, y_m: 0.0, heading_rad: 0.0 } },
            map,
            objects,
            controls: &[],
            lane_boundaries: &[],
            motion: &[],
            predicted_paths: &[],
            cedes_to_ego_ids: &[],
            lane_change_to_m: None,
            no_overtake_ids: &[],
            drivable: None,
            posture: FleetPosture::Nominal,
            target_speed_mps: None,
            request_overtake: false,
            request_pull_over: false,
            lane_graph: None,
            signal_states: &[],        }
    }

    #[test]
    fn geometric_planner_stops_short_of_in_path_object() {
        // Object dead ahead at x=30; goal beyond it. Occy must propose a controlled
        // stop SHORT of the object (by ~object_stop_gap_m), not drive up to it.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [obj_at(30.0, 0.0)];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_objects(&corridor, 18.0, 60.0, &objs));

        assert_eq!(out.kind, ProposalKind::Motion);
        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(
            max_x < 30.0 - 3.0,
            "must stop short of the object at x=30 (with a gap), got max_x {max_x}"
        );
        assert!(
            out.trajectory.last().unwrap().velocity_mps <= STOP_EPSILON_MPS,
            "stop short is a controlled stop"
        );
    }

    #[test]
    fn geometric_planner_caps_approach_speed() {
        // Approaching an in-path object, Occy does NOT ramp to cruise (8 m/s) — it
        // holds the slow approach speed so the proposal stays sane near the hazard.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [obj_at(30.0, 0.0)];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_objects(&corridor, 18.0, 60.0, &objs));

        let vmax = out.trajectory.iter().map(|t| t.velocity_mps).fold(0.0, f64::max);
        assert!(vmax <= 2.5, "approach speed capped well below cruise, got vmax {vmax}");
    }

    #[test]
    fn checker_admits_a_safe_stop_behind_but_mrcs_driving_into_a_stopped_object() {
        // A dead-center stopped object. The §4 RSS-conjunction fix: a controlled stop a safe
        // distance BEHIND it (a stopped queue) is now ADMITTED — the lateral side-RSS no longer
        // spuriously MRCs a longitudinally-safe, laterally-stationary lead. But KIRRA remains the
        // authority: a trajectory that drives INTO the object at speed (longitudinally unsafe) is
        // still MRC'd. The planner proposes safety; the checker bounds genuine danger.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [obj_at(30.0, 0.0)];
        let cfg = VehicleConfig::default_urban();

        // Occy's controlled stop short of the object → now admitted (safe same-lane stop).
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_objects(&corridor, 18.0, 60.0, &objs));
        let safe = validate_trajectory_slow(&out.trajectory, &corridor, &objs, &cfg, None, FleetPosture::Nominal);
        assert!(
            matches!(safe, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
            "a controlled stop a safe distance behind a stopped object is admitted, got {safe:?}"
        );

        // A trajectory barreling INTO the object at speed (way inside the longitudinal RSS
        // distance) is still MRC'd — independence preserved.
        let into: Vec<TrajectoryPoint> = (0..5)
            .map(|i| TrajectoryPoint {
                pose: Pose { x_m: 26.0 + i as f64, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 8.0,
                time_from_start_s: i as f64 * 0.1,
            })
            .collect();
        let danger = validate_trajectory_slow(&into, &corridor, &objs, &cfg, None, FleetPosture::Nominal);
        assert_eq!(danger, TrajectoryVerdict::MRCFallback, "driving into the object at speed is MRC'd, got {danger:?}");
    }

    #[test]
    fn geometric_planner_ignores_off_path_object_and_checker_admits() {
        // Object well off the path (y=10, beyond the RSS lateral-alignment band):
        // Occy ignores it and drives to the goal, AND the checker admits the
        // proposal (the object is filtered as a different-lane object). This is the
        // obstacle-aware payoff: a genuinely passable object → normal progress.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [obj_at(15.0, 10.0)];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_objects(&corridor, 10.0, 25.0, &objs));

        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(max_x > 24.0, "off-path object ignored → reaches the goal, got max_x {max_x}");

        let verdict = validate_trajectory_slow(
            &out.trajectory,
            &corridor,
            &objs,
            &VehicleConfig::default_urban(),
            None,
            FleetPosture::Nominal,
        );
        assert!(
            matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
            "checker admits driving past an off-path object, got {verdict:?}"
        );
    }

    #[test]
    fn geometric_planner_holds_for_close_object() {
        // Object so close ahead that the stop-gap leaves no room to advance → HOLD
        // (never creep into the gap).
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [obj_at(12.0, 0.0)];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_objects(&corridor, 10.0, 60.0, &objs));

        assert_eq!(out.kind, ProposalKind::SafeStop, "blocked too close → HOLD");
    }

    // --- Lateral avoidance / route-around (#451) ---------------------------

    #[test]
    fn geometric_planner_routes_around_offcenter_object() {
        // Off-center object at (20, 3) in a wide corridor: Occy bends the path
        // laterally away from it instead of stopping — a Motion proposal whose
        // path offsets to the far side.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [obj_at(20.0, 3.0)];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_objects(&corridor, 8.0, 35.0, &objs));

        assert_eq!(out.kind, ProposalKind::Motion, "routes around, does not stop");
        let min_y = out.trajectory.iter().map(|t| t.pose.y_m).fold(0.0, f64::min);
        assert!(min_y <= -1.0, "path offsets away from the object, got min_y {min_y}");
    }

    #[test]
    fn geometric_planner_route_around_is_checker_admissible() {
        // The #451 payoff: a route-around proposal is ADMITTED by the real checker
        // — the object ends up beyond the RSS lateral band, so it is filtered and
        // the offset path passes (the verdict the corridor refinement alone could
        // not deliver).
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [obj_at(20.0, 3.0)];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_objects(&corridor, 8.0, 35.0, &objs));

        let verdict = validate_trajectory_slow(
            &out.trajectory,
            &corridor,
            &objs,
            &VehicleConfig::default_urban(),
            None,
            FleetPosture::Nominal,
        );
        assert!(
            matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
            "checker admits the route-around proposal, got {verdict:?}"
        );
    }

    #[test]
    fn geometric_planner_stops_when_offset_infeasible() {
        // Object too close to the centerline (y=0.5) to clear within
        // `lateral_offset_max_m` → Occy must NOT squeeze past; it falls back to the
        // obstacle-aware stop-short (no big lateral offset).
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [obj_at(20.0, 0.5)];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_objects(&corridor, 8.0, 35.0, &objs));

        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        let min_y = out.trajectory.iter().map(|t| t.pose.y_m).fold(0.0, f64::min);
        assert!(max_x < 17.0, "stops short of the object at x=20, got max_x {max_x}");
        assert!(min_y > -0.5, "no route-around squeeze, got min_y {min_y}");
    }

    // --- Lead-following (moving objects) -----------------------------------

    fn moving_obj_at(x: f64, y: f64, vx: f64) -> PerceivedObject {
        PerceivedObject {
            id: 1,
            pos: Point { x_m: x, y_m: y },
            velocity_mps: vx.abs(),
            heading_rad: 0.0,
            vel: Point { x_m: vx, y_m: 0.0 },
        }
    }

    #[test]
    fn lead_following_matches_speed_not_stop() {
        // A dead-ahead LEAD moving forward at 4 m/s: Occy matches its speed and
        // follows (cruising at ~4, never stops), instead of stop-shorting it as if
        // it were a wall.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [moving_obj_at(20.0, 0.0, 4.0)];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_objects(&corridor, 8.0, 35.0, &objs));

        assert_eq!(out.kind, ProposalKind::Motion, "follows the lead, not stop");
        let vmax = out.trajectory.iter().map(|t| t.velocity_mps).fold(0.0, f64::max);
        assert!((3.5..=4.5).contains(&vmax), "matches the lead's ~4 m/s, got {vmax}");
        assert!(
            out.trajectory.last().unwrap().velocity_mps > 3.0,
            "keeps following at speed (no stop-to-zero)"
        );
    }

    #[test]
    fn lead_following_offcenter_is_checker_admissible() {
        // An off-center slower lead (y=3, 4 m/s): barreling past at cruise (8) gets
        // RSS-rejected, but matching its speed is ADMITTED. Occy matches → the
        // checker accepts the encounter.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [moving_obj_at(20.0, 3.0, 4.0)];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_objects(&corridor, 8.0, 35.0, &objs));

        let vmax = out.trajectory.iter().map(|t| t.velocity_mps).fold(0.0, f64::max);
        assert!(vmax <= 4.5, "matches the lead, not cruise, got {vmax}");

        let verdict = validate_trajectory_slow(
            &out.trajectory,
            &corridor,
            &objs,
            &VehicleConfig::default_urban(),
            None,
            FleetPosture::Nominal,
        );
        assert!(
            matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
            "checker admits the speed-matched encounter, got {verdict:?}"
        );
    }

    // --- Behavioral rules: traffic signs & signals (#90 / Occy 1.C) ---------

    fn input_with_controls<'a>(
        map: &'a dyn CorridorSource,
        ego_x: f64,
        goal_x: f64,
        controls: &'a [TrafficControl],
    ) -> PlanInput<'a> {
        PlanInput {
            ego: EgoState {
                pose: Pose { x_m: ego_x, y_m: 0.0, heading_rad: 0.0 },
                linear_x_mps: 2.0,
                yaw_rate_rads: 0.0,
                stamp_ms: 0,
            },
            goal: Goal { target: Pose { x_m: goal_x, y_m: 0.0, heading_rad: 0.0 } },
            map,
            objects: &[],
            controls,
            lane_boundaries: &[],
            motion: &[],
            predicted_paths: &[],
            cedes_to_ego_ids: &[],
            lane_change_to_m: None,
            no_overtake_ids: &[],
            drivable: None,
            posture: FleetPosture::Nominal,
            target_speed_mps: None,
            request_overtake: false,
            request_pull_over: false,
            lane_graph: None,
            signal_states: &[],        }
    }

    #[test]
    fn red_light_makes_occy_stop_at_the_line() {
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let controls = [TrafficControl::TrafficLight { stop_line_x_m: 20.0, state: SignalState::Red }];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_controls(&corridor, 8.0, 60.0, &controls));

        assert_eq!(out.kind, ProposalKind::Motion);
        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!((18.0..=20.5).contains(&max_x), "stops at the red-light line ~20, got {max_x}");
        assert!(
            out.trajectory.last().unwrap().velocity_mps <= STOP_EPSILON_MPS,
            "controlled stop at the line"
        );
    }

    #[test]
    fn green_light_does_not_stop() {
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let controls = [TrafficControl::TrafficLight { stop_line_x_m: 20.0, state: SignalState::Green }];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_controls(&corridor, 8.0, 35.0, &controls));

        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(max_x > 22.0, "green → drives through the light, got max_x {max_x}");
    }

    #[test]
    fn speed_limit_caps_the_planner() {
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let controls = [TrafficControl::SpeedLimit { from_x_m: 0.0, limit_mps: 5.0 }];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_controls(&corridor, 8.0, 60.0, &controls));

        let vmax = out.trajectory.iter().map(|t| t.velocity_mps).fold(0.0, f64::max);
        assert!(vmax <= 5.2, "obeys the 5 m/s limit, not cruise 8, got {vmax}");
        assert!(vmax > 4.0, "still progresses near the limit, got {vmax}");
    }

    #[test]
    fn stop_sign_then_satisfied_proceeds() {
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let unsat = [TrafficControl::StopSign { stop_line_x_m: 20.0, satisfied: false }];
        let sat = [TrafficControl::StopSign { stop_line_x_m: 20.0, satisfied: true }];
        let mut p = GeometricPlanner::default();

        let stops = p.plan(&input_with_controls(&corridor, 8.0, 60.0, &unsat));
        let stop_max = stops.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(stop_max <= 20.5, "unsatisfied stop sign → stop at the line, got {stop_max}");

        let goes = p.plan(&input_with_controls(&corridor, 8.0, 35.0, &sat));
        let go_max = goes.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(go_max > 22.0, "satisfied stop sign → proceed, got {go_max}");
    }

    #[test]
    fn behavioral_stop_is_checker_admissible() {
        // Obeying a red light produces a clean in-corridor decel-to-stop the
        // physical checker admits (KIRRA admits the stop; it would still MRC
        // cross-traffic regardless of the signal).
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let controls = [TrafficControl::TrafficLight { stop_line_x_m: 20.0, state: SignalState::Red }];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_controls(&corridor, 8.0, 60.0, &controls));

        let verdict = validate_trajectory_slow(
            &out.trajectory,
            &corridor,
            &[],
            &VehicleConfig::default_urban(),
            None,
            FleetPosture::Nominal,
        );
        assert!(
            matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
            "checker admits the controlled stop at the line, got {verdict:?}"
        );
    }

    // --- Lane-line lateral rules: gating route-around ----------------------

    fn input_objs_lanes<'a>(
        map: &'a dyn CorridorSource,
        ego_x: f64,
        goal_x: f64,
        objects: &'a [PerceivedObject],
        lanes: &'a [LaneBoundary],
    ) -> PlanInput<'a> {
        PlanInput {
            ego: EgoState {
                pose: Pose { x_m: ego_x, y_m: 0.0, heading_rad: 0.0 },
                linear_x_mps: 2.0,
                yaw_rate_rads: 0.0,
                stamp_ms: 0,
            },
            goal: Goal { target: Pose { x_m: goal_x, y_m: 0.0, heading_rad: 0.0 } },
            map,
            objects,
            controls: &[],
            lane_boundaries: lanes,
            motion: &[],
            predicted_paths: &[],
            cedes_to_ego_ids: &[],
            lane_change_to_m: None,
            no_overtake_ids: &[],
            drivable: None,
            posture: FleetPosture::Nominal,
            target_speed_mps: None,
            request_overtake: false,
            request_pull_over: false,
            lane_graph: None,
            signal_states: &[],        }
    }

    #[test]
    fn broken_line_permits_route_around() {
        // Off-center object + a BROKEN line on the offset side → Occy may cross →
        // routes around (offsets) and the checker admits it. The object sits in the
        // ego's footprint-overlap band (within RSS_LONGITUDINAL_OVERLAP_M) but
        // outside the stop-short band, so a STRAIGHT path would be a near-miss the
        // checker rejects (see the SOLID-line pair) — routing around is what clears it.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [obj_at(20.0, 2.3)];
        let broken = [LaneBoundary { y_m: -0.5, line: LineType::Broken }];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_objs_lanes(&corridor, 8.0, 35.0, &objs, &broken));

        let min_y = out.trajectory.iter().map(|t| t.pose.y_m).fold(0.0, f64::min);
        assert!(min_y <= -1.0, "broken line → routes around, got min_y {min_y}");
    }

    #[test]
    fn solid_line_forbids_route_around_and_kirra_backstops() {
        // Same object, but a SOLID line on the offset side: Occy must NOT cross it
        // → no route-around (drives straight). The object is in the ego's
        // footprint-overlap band (y=2.3 < RSS_LONGITUDINAL_OVERLAP_M) yet outside
        // the stop-short band (> object_lane_tolerance), so Occy drives straight
        // PAST it at cruise — a near-miss the straight path can't avoid — and KIRRA
        // (physical authority) rejects it. Occy obeys the line; KIRRA enforces the
        // collision safety.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [obj_at(20.0, 2.3)];
        let solid = [LaneBoundary { y_m: -0.5, line: LineType::Solid }];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_objs_lanes(&corridor, 8.0, 35.0, &objs, &solid));

        let min_y = out.trajectory.iter().map(|t| t.pose.y_m).fold(0.0, f64::min);
        assert!(min_y > -0.5, "solid line → no route-around (no offset), got min_y {min_y}");

        let verdict = validate_trajectory_slow(
            &out.trajectory,
            &corridor,
            &objs,
            &VehicleConfig::default_urban(),
            None,
            FleetPosture::Nominal,
        );
        assert_eq!(
            verdict,
            TrajectoryVerdict::MRCFallback,
            "can't legally pass → KIRRA backstops the straight path, got {verdict:?}"
        );
    }

    // --- Predictive yielding (object motion prediction) --------------------

    fn crossing_obj_at(x: f64, y: f64, vx: f64, vy: f64) -> PerceivedObject {
        PerceivedObject {
            id: 1,
            pos: Point { x_m: x, y_m: y },
            velocity_mps: vx.hypot(vy),
            heading_rad: vy.atan2(vx),
            vel: Point { x_m: vx, y_m: vy },
        }
    }

    #[test]
    fn yields_to_crossing_object() {
        // An object off to the side at (20, 5) crossing INTO the lane at 3 m/s:
        // not in-path now (so stop-short ignores it), but prediction sees it will
        // enter the lane ahead → Occy yields short of the crossing point.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [crossing_obj_at(20.0, 5.0, 0.0, -3.0)];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_objects(&corridor, 8.0, 35.0, &objs));

        assert_eq!(out.kind, ProposalKind::Motion);
        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(max_x < 17.0, "yields short of the predicted crossing at x=20, got {max_x}");
        assert!(
            out.trajectory.last().unwrap().velocity_mps <= STOP_EPSILON_MPS,
            "controlled stop to yield"
        );
    }

    #[test]
    fn crossing_yield_is_checker_admissible() {
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [crossing_obj_at(20.0, 5.0, 0.0, -3.0)];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_objects(&corridor, 8.0, 35.0, &objs));

        let verdict = validate_trajectory_slow(
            &out.trajectory,
            &corridor,
            &objs,
            &VehicleConfig::default_urban(),
            None,
            FleetPosture::Nominal,
        );
        assert!(
            matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
            "checker admits the predictive yield, got {verdict:?}"
        );
    }

    #[test]
    fn object_crossing_away_does_not_yield() {
        // Same object but moving AWAY from the lane (+y) → no predicted conflict.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [crossing_obj_at(20.0, 5.0, 0.0, 3.0)];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_objects(&corridor, 8.0, 35.0, &objs));

        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(max_x > 22.0, "object moving away → no yield, drives on, got {max_x}");
    }

    #[test]
    fn slow_drifting_object_does_not_yield() {
        // Drifting toward the lane too slowly to reach it within the horizon → no yield.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [crossing_obj_at(20.0, 5.0, 0.0, -0.2)];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_objects(&corridor, 8.0, 35.0, &objs));

        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(max_x > 22.0, "won't enter the lane in time → no yield, got {max_x}");
    }

    #[test]
    fn ctrv_yields_to_turning_in_object_where_cv_misses() {
        // An object moving PARALLEL to the lane (+x) but turning INTO it. The
        // constant-velocity predictor sees it stay out (no yield); the CTRV
        // predictor (fed the yaw rate via the motion channel) sees the curve and
        // yields. The win the Taj CTRV tracker enables on the planning side.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [crossing_obj_at(20.0, 4.0, 3.0, 0.0)];
        let mut p = GeometricPlanner::default();

        // CV (no motion state): object stays parallel → no predicted entry → drives on.
        let cv = p.plan(&input_with_objects(&corridor, 8.0, 35.0, &objs));
        let cv_max = cv.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(cv_max > 22.0, "CV: parallel object → no yield, got {cv_max}");

        // CTRV (yaw rate turning it into the lane): predicts the entry → yields.
        let motion = [MotionState { id: 1, yaw_rate_rad_s: -0.4 }];
        let inp = PlanInput { motion: &motion, ..input_with_objects(&corridor, 8.0, 35.0, &objs) };
        let ctrv = p.plan(&inp);
        let ctrv_max = ctrv.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(ctrv_max < 21.0, "CTRV: predicts the turn-in → yields short, got {ctrv_max}");
        assert!(ctrv_max < cv_max - 5.0, "CTRV yields meaningfully shorter than CV");
    }

    // --- Temporal-overlap (space-time) yielding ----------------------------

    #[test]
    fn fast_crosser_that_clears_in_time_is_not_yielded_to() {
        // SAME geometry as `yields_to_crossing_object` (crosser at x=20, ego from
        // x=8) but the crosser is FAST (vy=-8): it enters AND clears the lane band
        // (~0.4–0.9 s) long before the ego could reach x=20 (~2.6 s). Space-time:
        // the object is already gone → no conflict → no yield. The pre-temporal
        // spatial-only predictor stopped the ego short of a crosser that has passed.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [crossing_obj_at(20.0, 5.0, 0.0, -8.0)];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_objects(&corridor, 8.0, 35.0, &objs));

        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(max_x > 22.0,
            "fast crosser clears before the ego arrives → drives on (no yield), got {max_x}");
        // And it remains checker-admissible (the crosser's snapshot is 5 m aside).
        let verdict = validate_trajectory_slow(
            &out.trajectory, &corridor, &objs, &VehicleConfig::default_urban(), None,
            FleetPosture::Nominal,
        );
        assert!(matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
            "no-yield trajectory is admissible, got {verdict:?}");
    }

    #[test]
    fn slow_crosser_still_in_the_lane_when_ego_arrives_is_yielded_to() {
        // The contrast: a SLOW crosser (vy=-1.5) that enters the lane band and is
        // STILL there through the prediction horizon (never confirmed to clear) →
        // the temporal relaxation does NOT apply → yield (conservative). Proves the
        // gate only drops provably-cleared conflicts, never a persisting one.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [crossing_obj_at(25.0, 4.0, 0.0, -1.5)];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_with_objects(&corridor, 8.0, 40.0, &objs));

        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(max_x < 23.0, "persisting in-lane crosser → yields short of it, got {max_x}");
        assert!(out.trajectory.last().unwrap().velocity_mps <= STOP_EPSILON_MPS,
            "controlled stop to yield");
    }

    // --- Intention priors (lane-following predicted path) ------------------

    #[test]
    fn intention_prior_suppresses_a_spurious_yield_to_a_lane_keeping_vehicle() {
        // An object in the adjacent lane (y=4) moving forward at 5 m/s with an
        // apparent lateral drift toward the ego lane (vy=-2). KINEMATIC prediction
        // (CV/CTRV) extrapolates the drift INTO the ego lane → yields. But the
        // tracker's INTENTION-aware path says the vehicle keeps its own lane
        // (straight along y=4) → it never enters the ego lane → no yield. The drift
        // was transient; intent beats kinematics, and KIRRA still backstops.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [crossing_obj_at(20.0, 4.0, 5.0, -2.0)];

        // Kinematic (no predicted path) → extrapolated drift-in → yields short.
        let mut p = GeometricPlanner::default();
        let kin = p.plan(&input_with_objects(&corridor, 8.0, 40.0, &objs));
        let kin_max = kin.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(kin_max < 22.0, "kinematic: extrapolated drift-in → yields, got {kin_max}");

        // Intention prior: the lane-following path keeps it at y=4 → no entry → drives on.
        let lane_path = [Point { x_m: 20.0, y_m: 4.0 }, Point { x_m: 90.0, y_m: 4.0 }];
        let paths = [PredictedPath { id: 1, points: &lane_path }];
        let inp = PlanInput {
            predicted_paths: &paths,
            cedes_to_ego_ids: &[],
            ..input_with_objects(&corridor, 8.0, 40.0, &objs)
        };
        let intent = p.plan(&inp);
        let intent_max = intent.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(intent_max > 30.0,
            "intent: lane-keeping vehicle → no yield, drives on, got {intent_max}");
        assert!(intent_max > kin_max + 8.0, "intent drives meaningfully further than kinematic");
    }

    #[test]
    fn intention_prior_still_yields_when_the_path_does_enter_the_lane() {
        // Control: when the predicted path genuinely turns INTO the ego lane, the
        // intent rollout finds the entry and yields — the prior is not a blanket
        // "never yield", it follows the supplied path.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [crossing_obj_at(20.0, 4.0, 5.0, 0.0)]; // moving parallel
        let turn_in = [Point { x_m: 20.0, y_m: 4.0 }, Point { x_m: 28.0, y_m: 0.0 }];
        let paths = [PredictedPath { id: 1, points: &turn_in }];
        let mut p = GeometricPlanner::default();
        let inp = PlanInput {
            predicted_paths: &paths,
            cedes_to_ego_ids: &[],
            ..input_with_objects(&corridor, 8.0, 40.0, &objs)
        };
        let out = p.plan(&inp);
        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(max_x < 26.0, "path turns into the lane → yields, got {max_x}");
    }

    #[test]
    fn multi_modal_yields_against_the_worst_case_mode() {
        // ONE object, TWO predicted modes (same id): a benign lane-keep (stays at y=4,
        // never enters) AND a cut-in that turns into the ego lane. Either mode alone:
        // the lane-keep would NOT yield, the cut-in WOULD. Multi-modal must yield
        // against the worst case (the cut-in) — a single dangerous hypothesis is enough.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [crossing_obj_at(20.0, 4.0, 5.0, 0.0)];
        let lane_keep = [Point { x_m: 20.0, y_m: 4.0 }, Point { x_m: 90.0, y_m: 4.0 }];
        let cut_in = [Point { x_m: 20.0, y_m: 4.0 }, Point { x_m: 28.0, y_m: 0.0 }];
        let mut p = GeometricPlanner::default();

        // Benign mode alone → drives on (control).
        let benign = [PredictedPath { id: 1, points: &lane_keep }];
        let benign_out = p.plan(&PlanInput {
            predicted_paths: &benign,
            cedes_to_ego_ids: &[],
            ..input_with_objects(&corridor, 8.0, 40.0, &objs)
        });
        let benign_max = benign_out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(benign_max > 30.0, "benign mode alone → no yield, got {benign_max}");

        // Both modes present → must yield against the cut-in, regardless of order.
        for order in [[lane_keep, cut_in], [cut_in, lane_keep]] {
            let modes = [
                PredictedPath { id: 1, points: &order[0] },
                PredictedPath { id: 1, points: &order[1] },
            ];
            let out = p.plan(&PlanInput {
                predicted_paths: &modes,
                cedes_to_ego_ids: &[],
                ..input_with_objects(&corridor, 8.0, 40.0, &objs)
            });
            let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
            assert!(max_x < 26.0, "worst-case (cut-in) mode forces a yield, got {max_x}");
        }
    }

    // --- Junction right-of-way negotiation ---------------------------------

    #[test]
    fn junction_right_of_way_lets_the_ego_proceed_against_a_ceding_agent() {
        // A crossing agent at a junction (same geometry as `yields_to_crossing_object`).
        // By default the ego YIELDS (waits before the conflict). When the agent must
        // CEDE to the ego (right-of-way), the ego asserts priority and PROCEEDS —
        // same geometry, the cede list is the only change. KIRRA still backstops.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [crossing_obj_at(20.0, 5.0, 0.0, -3.0)]; // id 1, crossing in
        let mut p = GeometricPlanner::default();

        let yielded = p.plan(&input_with_objects(&corridor, 8.0, 35.0, &objs));
        let y_max = yielded.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(y_max < 17.0, "default → yields to the crossing agent, got {y_max}");

        let cede = [1u64];
        let inp = PlanInput {
            cedes_to_ego_ids: &cede,
            ..input_with_objects(&corridor, 8.0, 35.0, &objs)
        };
        let proceeded = p.plan(&inp);
        let p_max = proceeded.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(p_max > 22.0, "ceding agent → ego asserts priority and proceeds, got {p_max}");
    }

    #[test]
    fn junction_context_integration_derives_the_cede_list_occy_consumes() {
        // THE INTEGRATION LAYER, end-to-end: a Lanelet2-style map (ego lane 100 has
        // right-of-way over the crossing lane 200) → `junction_context(ego_pose, objs)`
        // → the cede list Occy consumes → the ego asserts priority and proceeds. The
        // MAP derives what `junction_right_of_way_lets_the_ego_proceed` supplied by hand.
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [crossing_obj_at(20.0, 5.0, 0.0, -3.0)]; // id 1, in the crossing lane

        let map = LaneGraph::new()
            .with_lane(Lane::straight(100, 0.0, 0.0, 100.0, 2.5, LineType::Solid, LineType::Solid))
            .with_lane(Lane::straight(200, 5.0, 0.0, 40.0, 2.5, LineType::Solid, LineType::Solid))
            .with_right_of_way(100, 200);

        // The integration call: ego POSE + objects → both junction sets.
        let ctx = map.junction_context(Point { x_m: 8.0, y_m: 0.0 }, &objs);
        assert_eq!(ctx.ego_lane, Some(100));
        assert_eq!(ctx.cedes_to_ego, vec![1], "map derives the cede list");
        assert!(ctx.must_yield_to.is_empty());

        // Feed the DERIVED cede list straight into Occy → it proceeds against the agent.
        let mut p = GeometricPlanner::default();
        let inp = PlanInput {
            cedes_to_ego_ids: &ctx.cedes_to_ego,
            ..input_with_objects(&corridor, 8.0, 35.0, &objs)
        };
        let out = p.plan(&inp);
        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(max_x > 22.0, "map-derived cede list → ego proceeds, got {max_x}");
    }

    #[test]
    fn junction_still_yields_to_a_non_ceding_agent_when_another_cedes() {
        // Two crossing conflicts: a NEAR agent (id 1, x=20) cedes to the ego; a FAR
        // one (id 2, x=32) does not (higher priority). The ego proceeds past the
        // ceding agent's conflict but STILL yields to the non-ceding one — priority
        // is per-agent, not a blanket "ignore all".
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let mut a1 = crossing_obj_at(20.0, 5.0, 0.0, -3.0); // cedes (fast crosser)
        a1.id = 1;
        let mut a2 = crossing_obj_at(32.0, 5.0, 0.0, -1.5); // does not — still crossing on arrival
        a2.id = 2;
        let objs = [a1, a2];
        let cede = [1u64];
        let mut p = GeometricPlanner::default();
        let inp = PlanInput {
            cedes_to_ego_ids: &cede,
            ..input_with_objects(&corridor, 8.0, 45.0, &objs)
        };
        let out = p.plan(&inp);
        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(max_x > 18.0, "proceeds past the ceding agent's conflict, got {max_x}");
        assert!(max_x < 30.0, "but still yields to the non-ceding agent, got {max_x}");
    }

    // --- Commanded lane change ---------------------------------------------

    fn input_lane_change<'a>(
        map: &'a dyn CorridorSource,
        ego_x: f64,
        goal_x: f64,
        target: f64,
        lanes: &'a [LaneBoundary],
    ) -> PlanInput<'a> {
        PlanInput {
            ego: EgoState {
                pose: Pose { x_m: ego_x, y_m: 0.0, heading_rad: 0.0 },
                linear_x_mps: 2.0,
                yaw_rate_rads: 0.0,
                stamp_ms: 0,
            },
            goal: Goal { target: Pose { x_m: goal_x, y_m: 0.0, heading_rad: 0.0 } },
            map,
            objects: &[],
            controls: &[],
            lane_boundaries: lanes,
            motion: &[],
            predicted_paths: &[],
            cedes_to_ego_ids: &[],
            lane_change_to_m: Some(target),
            no_overtake_ids: &[],
            drivable: None,
            posture: FleetPosture::Nominal,
            target_speed_mps: None,
            request_overtake: false,
            request_pull_over: false,
            lane_graph: None,
            signal_states: &[],        }
    }

    #[test]
    fn commanded_lane_change_across_broken_line_shifts_and_holds() {
        // Commanded shift to the right lane (-3 m) across a BROKEN line → permitted.
        // The path ramps over and HOLDS the offset (does not return like a bump).
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let broken = [LaneBoundary { y_m: -0.5, line: LineType::Broken }];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_lane_change(&corridor, 8.0, 40.0, -3.0, &broken));

        let min_y = out.trajectory.iter().map(|t| t.pose.y_m).fold(0.0, f64::min);
        assert!(min_y <= -2.5, "shifts toward the target lane, got min_y {min_y}");
        // Held, not returned: the last point stays in the new lane.
        assert!(
            out.trajectory.last().unwrap().pose.y_m <= -2.0,
            "lane change is sustained (held), got {}",
            out.trajectory.last().unwrap().pose.y_m
        );
    }

    #[test]
    fn lane_change_blocked_by_solid_line_stays_in_lane() {
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let solid = [LaneBoundary { y_m: -0.5, line: LineType::Solid }];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_lane_change(&corridor, 8.0, 40.0, -3.0, &solid));

        let min_y = out.trajectory.iter().map(|t| t.pose.y_m).fold(0.0, f64::min);
        assert!(min_y > -0.5, "solid line → no lane change, stays in lane, got {min_y}");
    }

    #[test]
    fn lane_change_is_checker_admissible() {
        let corridor = MockCorridorSource::straight_5m_half_width(100.0);
        let broken = [LaneBoundary { y_m: -0.5, line: LineType::Broken }];
        let mut p = GeometricPlanner::default();
        let out = p.plan(&input_lane_change(&corridor, 8.0, 40.0, -3.0, &broken));

        let verdict = validate_trajectory_slow(
            &out.trajectory,
            &corridor,
            &[],
            &VehicleConfig::default_urban(),
            None,
            FleetPosture::Nominal,
        );
        assert!(
            matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
            "checker admits the lane-change trajectory, got {verdict:?}"
        );
    }
}



