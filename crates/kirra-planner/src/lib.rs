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
//! - Posture is [`kirra_runtime_sdk::verifier::FleetPosture`].
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
pub use kirra_ros2_adapter::state::{Pose, TrajectoryPoint, TrajectoryVerdict};
pub use kirra_runtime_sdk::verifier::FleetPosture;

use kirra_ros2_adapter::corridor::{CorridorSource, Point};
// Derive (never guess) the checker's hard trajectory-length cap: the #131
// containment gate rejects `len > MAX_TRAJECTORY_HORIZON`, so a proposal must
// stay within it (including the terminal stop point) to be admissible.
use kirra_runtime_sdk::gateway::containment::MAX_TRAJECTORY_HORIZON;

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

/// World-state input to [`Planner::plan`].
///
/// `// PHASE-0 LOCKED` — derived from the checker's own consumed inputs: ego
/// state, the drivable-space handle (the **same** [`CorridorSource`] trait
/// `validate_trajectory_slow` consumes), and the fleet posture. Borrowed `map`
/// keeps it allocation-free and lets the planner and the checker read one corridor.
pub struct PlanInput<'a> {
    pub ego: EgoState,
    pub goal: Goal,
    /// Drivable-space handle — the same `CorridorSource` the checker re-reads.
    pub map: &'a dyn CorridorSource,
    /// Fleet posture → planner mode (see [`planner_mode`]).
    pub posture: FleetPosture,
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

// PHASE-0 LOCKED — derived from kirra_runtime_sdk::verifier::FleetPosture.
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

        // Guide path: corridor centerline if usable, else a straight ego→goal line.
        let center = centerline_from(input.map.left_boundary(), input.map.right_boundary());
        let guide: Vec<(f64, f64)> = if center.len() >= 2 {
            center
        } else {
            vec![
                (input.ego.pose.x_m, input.ego.pose.y_m),
                (input.goal.target.x_m, input.goal.target.y_m),
            ]
        };

        // Travel window: ego projection → goal projection along the guide.
        let s_ego = project_arc_length(&guide, input.ego.pose.x_m, input.ego.pose.y_m);
        let s_goal = project_arc_length(&guide, input.goal.target.x_m, input.goal.target.y_m);
        let dist = (s_goal - s_ego).max(0.0);

        // Arrived, or the mode admits no forward speed → HOLD (never re-accelerate).
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
        let mut traj: Vec<TrajectoryPoint> = Vec::with_capacity(budget + 1);
        let mut s = 0.0_f64; // distance travelled from ego along the guide
        let mut v = cur.min(target.max(cur)); // start at current speed
        let mut t = 0.0_f64;
        let mut reached = false;

        while traj.len() < budget {
            let along = s_ego + s;
            let (x, y) = point_at(&guide, along);
            let heading = heading_at(&guide, along);
            traj.push(TrajectoryPoint {
                pose: Pose { x_m: x, y_m: y, heading_rad: heading },
                velocity_mps: v,
                time_from_start_s: t,
            });

            let remaining = dist - s;
            if remaining <= self.cfg.goal_tolerance_m {
                reached = true;
                break;
            }
            // Brake when within stopping distance, else accelerate toward target.
            let brake_dist = (v * v) / (2.0 * decel);
            if remaining <= brake_dist {
                v = (v - decel * dt).max(0.0);
            } else {
                v = (v + self.cfg.max_accel_mps2 * dt).min(target);
            }
            s += v * dt;
            t += dt;
        }

        // On reaching the goal, pin a clean zero-velocity hold at the goal
        // projection (controlled stop-and-hold). On horizon truncation we leave
        // the rolling-horizon tail as-is — the next plan cycle continues it.
        if reached && traj.last().is_none_or(|p| p.velocity_mps > STOP_EPSILON_MPS) {
            let (gx, gy) = point_at(&guide, s_goal);
            let gh = heading_at(&guide, s_goal);
            traj.push(TrajectoryPoint {
                pose: Pose { x_m: gx, y_m: gy, heading_rad: gh },
                velocity_mps: 0.0,
                time_from_start_s: t + dt,
            });
        }

        // The checker requires ≥ 2 points; if geometry degenerated, HOLD.
        if traj.len() < 2 {
            return PlanOutput::safe_stop(input.ego.pose);
        }
        PlanOutput { trajectory: traj, kind: ProposalKind::Motion }
    }
}

// --- geometry helpers (private; pure, allocation-bounded by polyline length) ---

fn dist2d(ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt()
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

/// Arc length of the point on the polyline nearest to `(qx, qy)`.
fn project_arc_length(poly: &[(f64, f64)], qx: f64, qy: f64) -> f64 {
    if poly.len() < 2 {
        return 0.0;
    }
    let cum = cumulative(poly);
    let mut best = (f64::INFINITY, 0.0_f64);
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
    best.1
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
            posture: FleetPosture::Nominal,
        }
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
            posture: FleetPosture::Nominal,
        }
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
}
