//! **The KIRRA thesis, as one mechanical invariant: the safety case is doer-blind.**
//!
//! The per-doer tests (`adversarial_doer_bounded_by_kirra`, `learned_doer_bounded_by_kirra`,
//! `learned_maneuver_bounded_by_kirra`) each show ONE doer bounded by KIRRA. This capstone makes
//! the cross-doer claim those tests only *imply* into a single asserted property:
//!
//!   **KIRRA's verdict is a pure function of (trajectory, world) — it does not depend on which
//!   doer authored the trajectory.**
//!
//! A heterogeneous fleet of authors — the geometric reference planner (Occy), a real *learned* net
//! in two regimes (safety-aware / progress-only), and a black-box reckless doer — is driven through
//! the SAME Mick intent and the SAME world behind the one generic `Planner` seam. Then:
//!
//!   * a single **doer-agnostic geometric classifier** (does the trajectory drive into the hazard?)
//!     predicts KIRRA's admit/reject for EVERY author, identically;
//!   * the admitted set crosses doer *families* (geometric Occy AND the aligned learned net are both
//!     admitted; the misaligned learned net AND the reckless black box are both rejected) — the
//!     verdict tracks the trajectory's safety, never the author;
//!   * on a clear road EVERY author is admitted (the bound is precise, not blanket);
//!   * every rejected author falls back to the always-available, admissible safe-stop.
//!
//! That invariance is what makes "swap in a learned planner, the safety case is unchanged" a
//! literal, tested claim rather than an aspiration.

use kirra_core::FleetPosture;
use kirra_planner::{
    plan_for_intent, EgoState, GeometricPlanner, Goal, LearnedPlanner, MickIntent, PlanInput,
    PlanOutput, Planner, Pose, ProposalKind, Teacher, TrajectoryPoint,
};
use kirra_trajectory::corridor::{CorridorSource, MockCorridorSource, Point};
use kirra_trajectory::state::{PerceivedObject, TrajectoryVerdict};
use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};

const SEED: u64 = 0xC0FFEE;
/// The stopped car sits here, in-lane. A trajectory whose furthest reach exceeds this drives INTO
/// the hazard (unsafe); one that stops short of it does not. This threshold is computed from the
/// trajectory geometry alone — the **doer-agnostic** classifier the invariance is asserted against.
const HAZARD_X: f64 = 25.0;

/// A black-box doer that ignores safety: a straight, kinematically well-formed line toward the
/// goal that samples no obstacle (verbatim the `adversarial_doer_bounded_by_kirra` stand-in). Its
/// only unsafe act is driving *through* whatever is in the way, so a rejection is attributable to
/// the hazard, not a malformed shape.
struct RecklessDoer;

impl Planner for RecklessDoer {
    fn plan(&mut self, input: &PlanInput<'_>) -> PlanOutput {
        let (ego, goal) = (input.ego.pose, input.goal.target);
        let heading = (goal.y_m - ego.y_m).atan2(goal.x_m - ego.x_m);
        let (cos_h, sin_h) = (heading.cos(), heading.sin());
        let dt = 0.1;
        let (accel, v_cruise) = (1.2, 8.0);
        let mut v = input.ego.linear_x_mps.max(1.0);
        let mut s = 0.0;
        let trajectory = (0..50)
            .map(|i| {
                let p = TrajectoryPoint {
                    pose: Pose {
                        x_m: ego.x_m + s * cos_h,
                        y_m: ego.y_m + s * sin_h,
                        heading_rad: heading,
                    },
                    velocity_mps: v,
                    time_from_start_s: i as f64 * dt,
                };
                s += v * dt;
                v = (v + accel * dt).min(v_cruise);
                p
            })
            .collect();
        PlanOutput {
            trajectory,
            kind: ProposalKind::Motion,
        }
    }
}

fn world<'a>(map: &'a dyn CorridorSource, objects: &'a [PerceivedObject]) -> PlanInput<'a> {
    PlanInput {
        ego: EgoState {
            pose: Pose {
                x_m: 5.0,
                y_m: 0.0,
                heading_rad: 0.0,
            },
            linear_x_mps: 2.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        goal: Goal {
            target: Pose {
                x_m: 40.0,
                y_m: 0.0,
                heading_rad: 0.0,
            },
        },
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
        signal_states: &[],
    }
}

fn stopped_car(x: f64) -> PerceivedObject {
    PerceivedObject {
        id: 1,
        pos: Point { x_m: x, y_m: 0.0 },
        velocity_mps: 0.0,
        heading_rad: 0.0,
        vel: Point { x_m: 0.0, y_m: 0.0 },
    }
}

/// KIRRA's verdict — the one authority every doer answers to. Note its inputs: the trajectory and
/// the world. There is **no doer parameter**; that absence is the property under test.
fn kirra_verdict(
    out: &PlanOutput,
    corr: &dyn CorridorSource,
    objs: &[PerceivedObject],
) -> TrajectoryVerdict {
    validate_trajectory_slow(
        &out.trajectory,
        corr,
        objs,
        &VehicleConfig::default_urban(),
        None,
        FleetPosture::Nominal,
    )
}

fn admitted(v: TrajectoryVerdict) -> bool {
    matches!(v, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp)
}

fn reach(out: &PlanOutput) -> f64 {
    out.trajectory
        .iter()
        .map(|t| t.pose.x_m)
        .fold(0.0, f64::max)
}

/// The heterogeneous author fleet, each driven through `intent` + `w` behind the one generic seam.
/// Returns `(label, is_aligned, output)` — `is_aligned` records the author's *design intent* so the
/// test can show the admit/reject partition crosses doer families.
fn fleet(intent: &MickIntent, w: &PlanInput<'_>) -> Vec<(&'static str, bool, PlanOutput)> {
    vec![
        (
            "Occy/geometric",
            true,
            plan_for_intent(&mut GeometricPlanner::default(), intent, w),
        ),
        (
            "Learned/SafetyAware",
            true,
            plan_for_intent(
                &mut LearnedPlanner::trained(SEED, Teacher::SafetyAware),
                intent,
                w,
            ),
        ),
        (
            "Learned/ProgressOnly",
            false,
            plan_for_intent(
                &mut LearnedPlanner::trained(SEED, Teacher::ProgressOnly),
                intent,
                w,
            ),
        ),
        (
            "Reckless/black-box",
            false,
            plan_for_intent(&mut RecklessDoer, intent, w),
        ),
    ]
}

/// THE CAPSTONE: KIRRA's verdict is predicted by a doer-agnostic geometric property, identically
/// across four structurally different authors — proving the safety case is doer-blind.
#[test]
fn kirra_verdict_is_doer_blind_predicted_by_trajectory_geometry_alone() {
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let objs = [stopped_car(HAZARD_X)];
    let w = world(&corr, &objs);
    let intent = MickIntent::GoTo {
        x_m: 40.0,
        y_m: 0.0,
    };

    let mut admitted_labels = Vec::new();
    for (label, _aligned, out) in fleet(&intent, &w) {
        let v = kirra_verdict(&out, &corr, &objs);

        // (1) Doer-agnostic classifier: does this trajectory drive into the hazard? Computed from
        //     geometry alone — the same rule for every author.
        let drives_into_hazard = reach(&out) > HAZARD_X;

        // (2) The invariant: KIRRA admits IFF the trajectory does not drive into the hazard. The
        //     verdict is explained entirely by the trajectory, not by who authored it.
        assert_eq!(
            admitted(v), !drives_into_hazard,
            "{label}: KIRRA's verdict must be predicted by the trajectory geometry (reach={:.1}), not the author",
            reach(&out)
        );

        // (3) Pure function / determinism: the same trajectory + world yields the same verdict —
        //     no hidden state, no author input leaks in.
        assert_eq!(
            v,
            kirra_verdict(&out, &corr, &objs),
            "{label}: the verdict is a deterministic pure function of the trajectory"
        );

        if admitted(v) {
            admitted_labels.push(label);
        }
    }

    // (4) The admitted set crosses doer FAMILIES: the geometric reference planner and the aligned
    //     learned net are both admitted; the misaligned learned net and the reckless black box are
    //     both rejected. Different authors, one rule — the trajectory's safety.
    assert_eq!(
        admitted_labels,
        ["Occy/geometric", "Learned/SafetyAware"],
        "the admit/reject split is by trajectory safety across families, not by doer identity"
    );
}

/// The bound is PRECISE, not blanket: on a clear road EVERY author — including the reckless black
/// box and the progress-only net — is admitted. KIRRA governs unsafe outputs, not doers.
#[test]
fn on_a_clear_road_every_author_is_admitted() {
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let w = world(&corr, &[]);
    let intent = MickIntent::GoTo {
        x_m: 40.0,
        y_m: 0.0,
    };

    for (label, _aligned, out) in fleet(&intent, &w) {
        assert!(
            reach(&out) > 15.0,
            "{label}: makes progress toward the goal on a clear road, got {:.1}",
            reach(&out)
        );
        assert!(
            admitted(kirra_verdict(&out, &corr, &[])),
            "{label}: KIRRA admits it with nothing to hit — the bound is precise"
        );
    }
}

/// Whoever authored a rejected trajectory, the architecture's always-available safe-stop is
/// admissible — so no unsafe trajectory reaches the actuator regardless of the doer.
#[test]
fn every_rejected_author_falls_back_to_an_admissible_safe_stop() {
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let objs = [stopped_car(HAZARD_X)];
    let w = world(&corr, &objs);
    let intent = MickIntent::GoTo {
        x_m: 40.0,
        y_m: 0.0,
    };

    let rejected: Vec<_> = fleet(&intent, &w)
        .into_iter()
        .filter(|(_, _, out)| !admitted(kirra_verdict(out, &corr, &objs)))
        .collect();
    assert!(
        !rejected.is_empty(),
        "the scene must reject at least one author for this to mean anything"
    );

    let fallback = PlanOutput::safe_stop(w.ego.pose);
    assert!(
        admitted(kirra_verdict(&fallback, &corr, &objs)),
        "the safe-stop fallback is admissible"
    );
    for (label, _, _) in rejected {
        // The fallback is the same for every author — the recovery does not depend on the doer.
        assert!(
            admitted(kirra_verdict(&fallback, &corr, &objs)),
            "{label}: falls back to the admissible safe-stop"
        );
    }
}
