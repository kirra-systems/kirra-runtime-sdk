//! # #796 F8 — the closed-loop episode gate
//!
//! Every row in the deterministic corpus ([`crate::generated_doer_corpus`]) is
//! a SINGLE-TICK, OPEN-LOOP snapshot: one plan, one verdict. That proves the
//! checker's per-proposal discrimination, but it cannot answer the episode
//! question — *does the ego actually stop short of the queue over time, with
//! the planner replanning against the world it changed?* This module closes
//! that gap: a virtual-clock tick loop that runs the REAL planner and the REAL
//! checker every tick, advances the ego along the ADMITTED trajectory (or
//! brakes on a refusal), advances the scripted objects, and gates on
//! **episode-level KPIs** — minimum gap achieved, stopped-by-end, forward
//! progress.
//!
//! Two deliberate properties, matching the crate's discipline:
//!
//! - **Deterministic virtual time.** The clock is a tick counter × a fixed
//!   `DT_S` — no OS clock, no RNG. (The repo's `ScenarioRunner`/`VirtualClock`
//!   named by #796 F8 live in the root verifier crate and drive fleet-POSTURE
//!   events, not ego kinematics; pulling the verifier into this lean gate
//!   crate for a tick counter would invert the dependency layering, so the
//!   loop keeps the same virtual-time semantics locally.)
//! - **The loop is proven to DISCRIMINATE.** A closed loop whose episodes all
//!   pass proves nothing if it would also pass an unsafe system. The gate
//!   therefore carries a control pair driven by a RECKLESS proposer (full
//!   cruise speed, object-blind): with the checker ENFORCED the episode still
//!   holds its gap floor (the checker alone saves it — the doer-checker
//!   thesis, closed-loop); with enforcement DISABLED the same episode
//!   breaches (the KPIs are live, not vacuously green).

use kirra_core::corridor::{CorridorSource, MockCorridorSource, Point};
use kirra_core::trajectory::{PerceivedObject, Pose, TrajectoryPoint, TrajectoryVerdict};
use kirra_core::FleetPosture;
use kirra_planner::{
    EgoState, GeometricPlanner, GeometricPlannerConfig, Goal, PlanInput, PlanOutput, Planner,
};
use kirra_trajectory::VehicleConfig;
use serde::Serialize;

use crate::{hazard, HazardKind};

/// Virtual-clock tick, seconds. Matches the planner's native sample step.
pub const DT_S: f64 = 0.1;
/// Episode length in ticks (15 s of virtual time).
pub const EPISODE_TICKS: usize = 150;
/// Ego is "stopped" below this speed (the #70 stop epsilon).
const STOP_EPSILON_MPS: f64 = 0.05;

/// A stationary centerline hazard (the base family's constructor, reused).
fn stopped_at(id: u64, x_m: f64) -> PerceivedObject {
    hazard(HazardKind::Stopped, id, x_m)
}

/// Episode-level pass requirements. All bounds are center-to-center (the gap
/// floor deliberately exceeds the two half-lengths it abstracts).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct EpisodeSpec {
    /// The minimum center-to-center gap to ANY scripted object that must hold
    /// at every tick. `0.0` disables (clear-road episodes).
    pub min_gap_floor_m: f64,
    /// The ego must be fully stopped (≤ stop epsilon) by episode end.
    pub must_stop: bool,
    /// Minimum forward progress (m along +x from start) by episode end.
    pub min_progress_m: f64,
}

/// One closed-loop episode: the world, the ego start, and the spec.
pub struct Episode {
    pub name: String,
    corridor: MockCorridorSource,
    objects: Vec<PerceivedObject>,
    ego_speed_mps: f64,
    goal_x_m: f64,
    pub spec: EpisodeSpec,
}

/// One episode's measured outcome vs its spec.
#[derive(Debug, Clone, Serialize)]
pub struct EpisodeOutcome {
    pub name: String,
    /// Smallest center-to-center object gap seen over the episode (infinite
    /// serialized as a large sentinel is avoided — clear episodes report
    /// `None`).
    pub min_gap_m: Option<f64>,
    pub final_speed_mps: f64,
    pub progress_m: f64,
    /// Ticks on which the checker REFUSED the proposal (the loop braked).
    pub refusals: usize,
    pub pass: bool,
    /// Why the episode failed (absent when it passed).
    pub reason: Option<String>,
}

/// The full closed-loop gate outcome.
#[derive(Debug, Clone, Serialize)]
pub struct ClosedLoopReport {
    pub episodes: Vec<EpisodeOutcome>,
}

impl ClosedLoopReport {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.episodes.iter().all(|e| e.pass)
    }
}

/// The deterministic episode set (#796 F8): stopped lead, three-deep queue,
/// receding lead, lateral cut-in, and clear-road progress — the families
/// where "what happens over time" is the KPI. Oncoming objects are excluded
/// BY DESIGN: a scripted (non-yielding) oncoming vehicle drives through the
/// ego's stopped position, so no ego behavior can hold a gap floor — the
/// snapshot corpus covers that geometry instead.
#[must_use]
pub fn generated_episodes() -> Vec<Episode> {
    let road = || MockCorridorSource::straight_5m_half_width(200.0);
    let mut v = Vec::new();

    // Clear road: the loop must make progress (a gate that only rewards
    // stopping would pass a bricked vehicle).
    for &speed in &[2.0_f64, 4.0, 6.0] {
        v.push(Episode {
            name: format!("closed_clear_v{speed}"),
            corridor: road(),
            objects: vec![],
            ego_speed_mps: speed,
            goal_x_m: 100.0,
            spec: EpisodeSpec {
                min_gap_floor_m: 0.0,
                must_stop: false,
                min_progress_m: 40.0,
            },
        });
    }

    // Stopped lead: the ego must come to rest short of it, never inside the
    // gap floor.
    for &x in &[20.0_f64, 30.0, 40.0] {
        for &speed in &[2.0_f64, 4.0, 6.0] {
            v.push(Episode {
                name: format!("closed_stop_x{x}_v{speed}"),
                corridor: road(),
                objects: vec![stopped_at(1, x)],
                ego_speed_mps: speed,
                goal_x_m: 100.0,
                spec: EpisodeSpec {
                    min_gap_floor_m: 3.0,
                    must_stop: true,
                    min_progress_m: 0.0,
                },
            });
        }
    }

    // Three-deep stopped queue: the NEAREST object binds; the tail of the
    // queue must not mask it.
    for &speed in &[2.0_f64, 4.0] {
        v.push(Episode {
            name: format!("closed_queue3_x20_v{speed}"),
            corridor: road(),
            objects: vec![
                stopped_at(1, 20.0),
                stopped_at(2, 26.0),
                stopped_at(3, 32.0),
            ],
            ego_speed_mps: speed,
            goal_x_m: 100.0,
            spec: EpisodeSpec {
                min_gap_floor_m: 3.0,
                must_stop: true,
                min_progress_m: 0.0,
            },
        });
    }

    // Receding lead (+1 m/s): the ego follows without ever closing inside the
    // floor; no stop requirement (following is the correct behavior).
    for &speed in &[2.0_f64, 4.0, 6.0] {
        v.push(Episode {
            name: format!("closed_lead_x16_v{speed}"),
            corridor: road(),
            objects: vec![PerceivedObject {
                id: 1,
                pos: Point {
                    x_m: 16.0,
                    y_m: 0.0,
                },
                velocity_mps: 1.0,
                heading_rad: 0.0,
                vel: Point { x_m: 1.0, y_m: 0.0 },
            }],
            ego_speed_mps: speed,
            goal_x_m: 100.0,
            spec: EpisodeSpec {
                min_gap_floor_m: 3.0,
                must_stop: false,
                min_progress_m: 5.0,
            },
        });
    }

    // Lateral cut-in: an object ahead crosses the lane laterally (pure
    // lateral velocity, constant heading — the same determinism discipline as
    // the snapshot cutin_ family). The ego must never close inside the floor
    // while the object transits.
    for &x in &[25.0_f64, 35.0] {
        for &speed in &[2.0_f64, 4.0] {
            v.push(Episode {
                name: format!("closed_cutin_x{x}_v{speed}"),
                corridor: road(),
                objects: vec![PerceivedObject {
                    id: 1,
                    pos: Point { x_m: x, y_m: 3.0 },
                    velocity_mps: 1.0,
                    heading_rad: -std::f64::consts::FRAC_PI_2,
                    vel: Point {
                        x_m: 0.0,
                        y_m: -1.0,
                    },
                }],
                ego_speed_mps: speed,
                goal_x_m: 100.0,
                spec: EpisodeSpec {
                    min_gap_floor_m: 2.5,
                    must_stop: false,
                    min_progress_m: 0.0,
                },
            });
        }
    }

    v
}

/// The proposer driving an episode.
pub enum Proposer {
    /// The shipped geometric doer (fresh per tick — scenario independence).
    Geometric,
    /// The DISCRIMINANCE control: full-cruise, object-blind straight-line
    /// proposals. With the checker enforced this must still hold the gap
    /// floor (the checker brakes it); unenforced it must breach.
    Reckless,
}

/// Whether the checker's verdict is honored by the executor. `false` exists
/// ONLY for the discriminance control (an unsafe executor the loop must
/// catch); the gate itself always enforces.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Enforcement {
    CheckerEnforced,
    UncheckedExecutor,
}

/// Linear interpolation of the trajectory state at time `t_s` from start.
/// Before the first point → the first point; past the end → the last point
/// (a trajectory that ran out holds its final state).
fn sample_trajectory(traj: &[TrajectoryPoint], t_s: f64) -> Option<(Pose, f64)> {
    let first = traj.first()?;
    if t_s <= first.time_from_start_s {
        return Some((first.pose, first.velocity_mps));
    }
    for w in traj.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if t_s <= b.time_from_start_s {
            let span = b.time_from_start_s - a.time_from_start_s;
            if span <= 0.0 {
                return Some((b.pose, b.velocity_mps));
            }
            let f = (t_s - a.time_from_start_s) / span;
            return Some((
                Pose {
                    x_m: a.pose.x_m + f * (b.pose.x_m - a.pose.x_m),
                    y_m: a.pose.y_m + f * (b.pose.y_m - a.pose.y_m),
                    heading_rad: a.pose.heading_rad + f * (b.pose.heading_rad - a.pose.heading_rad),
                },
                a.velocity_mps + f * (b.velocity_mps - a.velocity_mps),
            ));
        }
    }
    let last = traj.last()?;
    Some((last.pose, last.velocity_mps))
}

/// Run one episode: the tick loop. Returns the measured outcome vs the spec.
#[must_use]
pub fn run_episode(ep: &Episode, proposer: &Proposer, enforcement: Enforcement) -> EpisodeOutcome {
    let config = VehicleConfig::default_urban();
    let start_x = 5.0;
    let mut ego = EgoState {
        pose: Pose {
            x_m: start_x,
            y_m: 0.0,
            heading_rad: 0.0,
        },
        linear_x_mps: ep.ego_speed_mps,
        yaw_rate_rads: 0.0,
        stamp_ms: 0,
    };
    let mut objects = ep.objects.clone();
    let mut min_gap: Option<f64> = None;
    let mut refusals = 0usize;

    for tick in 0..EPISODE_TICKS {
        let input = PlanInput {
            ego,
            goal: Goal {
                target: Pose {
                    x_m: ep.goal_x_m,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
            },
            map: &ep.corridor,
            objects: &objects,
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
            signal_states: &[],
        };

        let out = match proposer {
            Proposer::Geometric => {
                GeometricPlanner::new(GeometricPlannerConfig::default()).plan(&input)
            }
            Proposer::Reckless => reckless_plan(&input),
        };

        // The REAL checker, every tick.
        let verdict = kirra_trajectory::validation::validate_trajectory_slow(
            &out.trajectory,
            &ep.corridor as &dyn CorridorSource,
            &objects,
            &config,
            None,
            FleetPosture::Nominal,
        );
        let admitted = matches!(
            verdict,
            TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp
        );

        if admitted || enforcement == Enforcement::UncheckedExecutor {
            if !admitted {
                refusals += 1; // counted even when the sabotaged executor ignores it
            }
            if let Some((pose, speed)) = sample_trajectory(&out.trajectory, DT_S) {
                ego.pose = pose;
                ego.linear_x_mps = speed;
            }
        } else {
            // Refusal → controlled braking along the current heading (the
            // consumer-side safe-stop floor, #405).
            refusals += 1;
            let v0 = ego.linear_x_mps.max(0.0);
            let v1 = (v0 - config.max_decel_mps2 * DT_S).max(0.0);
            let dist = (v0 + v1) * 0.5 * DT_S;
            ego.pose.x_m += dist * ego.pose.heading_rad.cos();
            ego.pose.y_m += dist * ego.pose.heading_rad.sin();
            ego.linear_x_mps = v1;
        }
        ego.stamp_ms = (tick as u64 + 1) * 100;

        // Scripted world advance + gap tracking.
        for o in &mut objects {
            o.pos.x_m += o.vel.x_m * DT_S;
            o.pos.y_m += o.vel.y_m * DT_S;
        }
        for o in &objects {
            let dx = o.pos.x_m - ego.pose.x_m;
            let dy = o.pos.y_m - ego.pose.y_m;
            let gap = (dx * dx + dy * dy).sqrt();
            min_gap = Some(min_gap.map_or(gap, |g: f64| g.min(gap)));
        }
    }

    let progress = ego.pose.x_m - start_x;
    let stopped = ego.linear_x_mps.abs() <= STOP_EPSILON_MPS;

    let mut reason = None;
    if let Some(g) = min_gap {
        if ep.spec.min_gap_floor_m > 0.0 && g < ep.spec.min_gap_floor_m {
            reason = Some(format!(
                "gap floor breached: min {g:.2} m < {:.2} m",
                ep.spec.min_gap_floor_m
            ));
        }
    }
    if reason.is_none() && ep.spec.must_stop && !stopped {
        reason = Some(format!(
            "must_stop unmet: final speed {:.2} m/s",
            ego.linear_x_mps
        ));
    }
    if reason.is_none() && progress < ep.spec.min_progress_m {
        reason = Some(format!(
            "progress {:.1} m < required {:.1} m",
            progress, ep.spec.min_progress_m
        ));
    }

    EpisodeOutcome {
        name: ep.name.clone(),
        min_gap_m: min_gap,
        final_speed_mps: ego.linear_x_mps,
        progress_m: progress,
        refusals,
        pass: reason.is_none(),
        reason,
    }
}

/// The object-blind control proposer: full cruise speed straight down the
/// centerline, no braking — deliberately unsafe. Exists only to prove the
/// loop discriminates (see the module docs).
fn reckless_plan(input: &PlanInput<'_>) -> PlanOutput {
    let cruise = 8.0;
    let mut trajectory = Vec::with_capacity(30);
    for i in 0..30usize {
        let t = i as f64 * DT_S;
        trajectory.push(TrajectoryPoint {
            pose: Pose {
                x_m: input.ego.pose.x_m + cruise * t,
                y_m: 0.0,
                heading_rad: 0.0,
            },
            velocity_mps: cruise,
            time_from_start_s: t,
        });
    }
    PlanOutput {
        trajectory,
        kind: kirra_planner::ProposalKind::Motion,
    }
}

/// The F8 gate: every episode, geometric proposer, checker enforced — plus
/// the discriminance control pair on the nearest stopped-lead episode.
#[must_use]
pub fn run_closedloop_gate() -> ClosedLoopReport {
    let mut episodes: Vec<EpisodeOutcome> = generated_episodes()
        .iter()
        .map(|ep| run_episode(ep, &Proposer::Geometric, Enforcement::CheckerEnforced))
        .collect();

    // Discriminance control pair (the closed-loop analogue of the #777 F1
    // negative controls): a reckless proposer against the nearest stopped
    // lead. Enforced → the checker's refusals brake the ego short of the
    // floor (PASS, and the pass is ATTRIBUTABLE to the checker: refusals must
    // be non-zero). Unenforced → the same episode must BREACH; its row is
    // pass=true when the breach happened (the loop caught the unsafe
    // executor).
    let control = Episode {
        name: "control_reckless".to_string(),
        corridor: MockCorridorSource::straight_5m_half_width(200.0),
        objects: vec![stopped_at(1, 30.0)],
        ego_speed_mps: 6.0,
        goal_x_m: 100.0,
        spec: EpisodeSpec {
            min_gap_floor_m: 3.0,
            must_stop: true,
            min_progress_m: 0.0,
        },
    };
    let enforced = run_episode(&control, &Proposer::Reckless, Enforcement::CheckerEnforced);
    let sabotaged = run_episode(
        &control,
        &Proposer::Reckless,
        Enforcement::UncheckedExecutor,
    );

    episodes.push(EpisodeOutcome {
        name: "negctl_reckless_checker_enforced_holds_floor".to_string(),
        pass: enforced.pass && enforced.refusals > 0,
        reason: if enforced.pass && enforced.refusals > 0 {
            None
        } else {
            Some(format!(
                "checker did not save the reckless proposer: pass={} refusals={} ({:?})",
                enforced.pass, enforced.refusals, enforced.reason
            ))
        },
        ..enforced
    });
    episodes.push(EpisodeOutcome {
        name: "negctl_reckless_unchecked_breaches".to_string(),
        pass: !sabotaged.pass,
        reason: if sabotaged.pass {
            Some("an UNCHECKED reckless executor passed — the episode KPIs are vacuous".to_string())
        } else {
            None
        },
        ..sabotaged
    });

    ClosedLoopReport { episodes }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE F8 DoD (green half): every episode — including the discriminance
    /// control pair — passes on the current tree.
    #[test]
    fn closedloop_gate_passes_on_the_current_tree() {
        let report = run_closedloop_gate();
        for e in &report.episodes {
            assert!(e.pass, "episode failed: {e:#?}");
        }
    }

    /// Episode count is pinned (a silent shrink would weaken the gate).
    #[test]
    fn episode_count_is_pinned() {
        assert_eq!(generated_episodes().len(), 21);
        assert_eq!(run_closedloop_gate().episodes.len(), 23); // + control pair
    }

    /// Determinism: two runs produce identical outcomes bit-for-bit on the
    /// gated fields (virtual clock, no RNG — a red episode is a real change).
    #[test]
    fn closedloop_gate_is_deterministic() {
        let fp = |r: &ClosedLoopReport| -> Vec<(String, bool, Option<u64>, u64)> {
            r.episodes
                .iter()
                .map(|e| {
                    (
                        e.name.clone(),
                        e.pass,
                        e.min_gap_m.map(f64::to_bits),
                        e.progress_m.to_bits(),
                    )
                })
                .collect()
        };
        assert_eq!(fp(&run_closedloop_gate()), fp(&run_closedloop_gate()));
    }

    /// The stopped-lead episodes end with the ego at rest OUTSIDE the gap
    /// floor with the hazard still ahead — stopped short, not stopped past.
    #[test]
    fn stopped_lead_episodes_stop_short_with_margin() {
        for ep in generated_episodes()
            .iter()
            .filter(|e| e.name.starts_with("closed_stop_"))
        {
            let out = run_episode(ep, &Proposer::Geometric, Enforcement::CheckerEnforced);
            assert!(out.pass, "{out:#?}");
            let gap = out.min_gap_m.expect("hazard episode has a gap");
            assert!(gap >= ep.spec.min_gap_floor_m, "{}: gap {gap}", ep.name);
            assert!(
                out.final_speed_mps.abs() <= STOP_EPSILON_MPS,
                "{}: not stopped",
                ep.name
            );
        }
    }

    /// The sampled-trajectory interpolator is total: empty → None, before /
    /// inside / past the span → clamped ends and linear midpoints.
    #[test]
    fn trajectory_sampling_is_total_and_linear() {
        assert!(sample_trajectory(&[], 0.1).is_none());
        let traj = vec![
            TrajectoryPoint {
                pose: Pose {
                    x_m: 0.0,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
                velocity_mps: 2.0,
                time_from_start_s: 0.0,
            },
            TrajectoryPoint {
                pose: Pose {
                    x_m: 1.0,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
                velocity_mps: 4.0,
                time_from_start_s: 0.5,
            },
        ];
        let (p, v) = sample_trajectory(&traj, 0.25).unwrap();
        assert!((p.x_m - 0.5).abs() < 1e-12);
        assert!((v - 3.0).abs() < 1e-12);
        let (p, v) = sample_trajectory(&traj, 9.0).unwrap();
        assert_eq!(p.x_m, 1.0);
        assert_eq!(v, 4.0);
    }
}
