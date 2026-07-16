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
//! - The checker entry is [`kirra_trajectory::validate_trajectory_slow`] (the
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

pub mod behavior;
pub use behavior::{
    BehaviorConfig, Behavioral, LaneBoundary, LineType, SignalState, TrafficControl,
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
pub use mick_llm::{
    build_courier_prompt, build_courier_prompt_with_request, build_prompt,
    build_prompt_with_request, courier_intent_schema, intent_schema, sanitize_request, LlmBrain,
    MockModel, ModelClient, ModelError, Persona, MICK_MAX_REQUEST_CHARS,
};

pub mod learned;
pub use learned::{
    CalibrationMethod, LearnedManeuverPlanner, LearnedPlanner, QuantizedLearnedPlanner,
    QuantizedScorerWeights, ScoredPlanner, ScorerWeights, Teacher,
};

// M-1 (parko/DOER_MODEL_SCALEUP.md): the real-sized doer scorer — offset×speed
// vocabulary grid, ~32-dim scene encoding, N-layer MLP trained by seeded
// in-Rust SGD backprop (gradient-checked). Same ScoredPlanner seam as v1.
pub mod learned_v2;
pub use learned_v2::{
    teacher_candidate_score, teacher_choice, train_planner_v2, LayerWeightsV2, LearnedPlannerV2,
    QuantLayerWeightsV2, QuantizedLearnedPlannerV2, QuantizedScorerWeightsV2, ScorerConfigV2,
    ScorerWeightsV2, TrainConfigV2, WeightsError, FEATURE_DIM_V2,
};

/// Fast-loop trajectory tracker — the System-1 conformance side of the dual-rate loop, which
/// follows one admitted trajectory by elapsed time across many fast ticks.
pub mod fast_loop;
pub use fast_loop::{FastLoopTracker, TrackedCommand};

/// The `GeometricPlanner` geometric doer (config, kinematics, corridor
/// helpers). Extracted from the crate root (de-monolith) so `lib.rs` stays
/// the planner's type vocabulary + module hub; every other planner
/// (`learned`, `learned_v2`, `mick`) is likewise its own module.
mod geometric;
pub use geometric::{GeometricPlanner, GeometricPlannerConfig};

/// Ego world-state the planner consumes.
///
/// `// PHASE-0 LOCKED` — derived from `kirra_trajectory::state::EgoOdom`
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
/// [`kirra_trajectory::validate_trajectory_slow`]. No curvature / accel / metadata
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
            TrajectoryPoint {
                pose: at,
                velocity_mps: 0.0,
                time_from_start_s: 0.0,
            },
            TrajectoryPoint {
                pose: at,
                velocity_mps: 0.0,
                time_from_start_s: 0.1,
            },
        ];
        PlanOutput {
            trajectory,
            kind: ProposalKind::SafeStop,
        }
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
