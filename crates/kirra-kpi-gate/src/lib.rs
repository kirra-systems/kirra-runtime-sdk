//! # Scenario-KPI CI gate (WS-3.1)
//!
//! Thresholds the three fleet-safety KPIs the execution plan names —
//! **`unsafe_miss_rate`**, **admissibility**, **`hazard_recall`** — over a
//! parameterized, deterministic scenario corpus in the low hundreds, so that
//! *no safety-relevant PR merges without the KPI gate* (WS-3 DoD).
//!
//! Three deliberate properties:
//!
//! - **The gate consumes the EXISTING metric harnesses** — the doer-eval
//!   admissibility producer (`kirra_doer_eval::AdmissibilityTally` over the
//!   real checker `validate_trajectory_slow`) and the taj safety-weighted
//!   perception scorecard (`kirra_taj::SemanticEvalSummary`). It adds corpus
//!   + thresholds + an exit code; it never re-implements a metric.
//! - **Deterministic.** Generators are closed-form parameter sweeps (no RNG,
//!   no time); the learned planner is seeded. A red gate is a real change in
//!   doer/checker/perception behavior, never flake.
//! - **The bar predates the model** (the taj crate's own discipline): today's
//!   perception rows are produced through the scripted `MockSemanticDetector`
//!   seam and pin perfection (`unsafe_miss_rate = 0`, `hazard_recall = 1`);
//!   when the real RGB→TensorRT detector lands behind the same seam, it
//!   inherits this gate on day one and the thresholds become a negotiation
//!   with evidence, not a wish. These two rows are labelled `*_seam_pinned` in
//!   the scorecard: they are a harness SMOKE TEST (the identity function scores
//!   perfectly), **not** a measurement — a reader must not mistake their `0` for
//!   a fielded miss rate.
//! - **The oracle is proven to DISCRIMINATE** (#777 F1): because the seam-pinned
//!   rows are tautological, the gate also carries NEGATIVE-CONTROL rows —
//!   dropout / far-range-bias / class-confusion / lateral-shrink / phantom
//!   detector faults injected over generated ground truth — each asserted to
//!   BREACH the safety metric (an unsafe fault must drive `unsafe_miss_rate` high;
//!   a phantom must stay over-conservative, never unsafe). This is mutation
//!   testing OF the metric, in the gate itself: a future fusion change that
//!   blinds the oracle turns a negative-control row red.
//!
//! Thresholds live in `ci/scenario_kpi_thresholds.json` (repo root) — the
//! reviewed, versioned policy. The binary exits non-zero on any breach and
//! prints the scorecard either way.

pub mod closedloop;
pub mod confidence;
pub mod differential;
pub mod montecarlo;
pub mod sotif_coverage;

use kirra_core::corridor::{MockCorridorSource, Point};
use kirra_core::frame_integrity::FrameTrust;
use kirra_core::trajectory::{PerceivedObject, TrajectoryVerdict};
use kirra_doer_eval::{AdmissibilityTally, EvalScenario};
use kirra_planner::{GeometricPlanner, GeometricPlannerConfig, LearnedPlanner, Planner, Teacher};
use kirra_taj::{
    LaserScan, MockSemanticDetector, SemanticClass, SemanticDetection, SemanticDetector,
    SemanticEvalFrame, SemanticEvalSummary, TajConfig, TajCorridor, TajPhaseA,
};
use kirra_trajectory::validation::validate_trajectory_slow_capped;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Doer corpus — parameterized sweep (WS-3.1: "corpus to low hundreds")
// ---------------------------------------------------------------------------

/// Hazard kind axis for the doer sweep. All are checker-visible
/// `PerceivedObject`s; the kinds differ in longitudinal motion so the RSS
/// terms (stationary lead / slower lead / oncoming closer) are all exercised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HazardKind {
    /// Stationary object on the centerline (the stopped-queue case).
    Stopped,
    /// Lead vehicle moving away slowly (+1.0 m/s along +x).
    LeadMoving,
    /// Oncoming vehicle closing at 2.0 m/s (−x velocity).
    Oncoming,
}

pub(crate) fn hazard(kind: HazardKind, id: u64, x_m: f64) -> PerceivedObject {
    let (v, heading) = match kind {
        HazardKind::Stopped => (0.0, 0.0),
        HazardKind::LeadMoving => (1.0, 0.0),
        HazardKind::Oncoming => (2.0, std::f64::consts::PI),
    };
    let vx = match kind {
        HazardKind::Stopped => 0.0,
        HazardKind::LeadMoving => 1.0,
        HazardKind::Oncoming => -2.0,
    };
    PerceivedObject {
        id,
        pos: Point { x_m, y_m: 0.0 },
        velocity_mps: v,
        heading_rad: heading,
        vel: Point { x_m: vx, y_m: 0.0 },
    }
}

/// The WS-3.1 doer corpus: the BASE family — a deterministic closed-form sweep
/// over hazard kind × hazard distance × ego speed × goal distance, plus a
/// clear-road row per (speed, goal) — UNION the #796 F3 families below
/// (cut-in, lateral-offset, curved-corridor, multi-object, Degraded,
/// occlusion). No RNG — the corpus is identical on every run and every
/// machine.
///
/// Base size: 3 kinds × 10 distances × 4 speeds × 2 goals + 4×2 clear-road
/// = **248 scenarios**; family sizes are pinned individually by test
/// (total **398**).
#[must_use]
pub fn generated_doer_corpus() -> Vec<EvalScenario> {
    let road = || MockCorridorSource::straight_5m_half_width(100.0);
    let speeds = [1.0, 2.0, 4.0, 6.0];
    let goals = [40.0, 60.0];
    let distances = [12.0, 16.0, 20.0, 24.0, 28.0, 32.0, 36.0, 40.0, 44.0, 48.0];
    let kinds = [
        HazardKind::Stopped,
        HazardKind::LeadMoving,
        HazardKind::Oncoming,
    ];

    let mut corpus = Vec::new();
    for &speed in &speeds {
        for &goal in &goals {
            corpus.push(EvalScenario::new(
                format!("clear_v{speed}_g{goal}"),
                road(),
                vec![],
                5.0,
                speed,
                goal,
            ));
            for &kind in &kinds {
                for &dist in &distances {
                    corpus.push(EvalScenario::new(
                        format!("{kind:?}_x{dist}_v{speed}_g{goal}"),
                        road(),
                        vec![hazard(kind, 1, dist)],
                        5.0,
                        speed,
                        goal,
                    ));
                }
            }
        }
    }
    corpus.extend(cutin_family());
    corpus.extend(lateral_offset_family());
    corpus.extend(curved_family());
    corpus.extend(multi_object_family());
    corpus.extend(degraded_family());
    corpus.extend(occlusion_family());
    corpus
}

// ---------------------------------------------------------------------------
// #796 F3 — corpus families beyond the base longitudinal sweep
//
// The base family above is ONE logical scenario: a straight road, a
// centerline hazard with purely longitudinal velocity, Nominal posture. The
// families below add the axes the review named missing — each a closed-form
// deterministic sweep (no RNG, no transcendentals beyond IEEE-exact `sqrt`,
// so the corpus cannot drift across toolchains), each carrying a name PREFIX
// that assigns per-family gate rows (aggregate-only gating masks a family
// regression — Simpson's paradox once families mix).
// ---------------------------------------------------------------------------

/// Cut-in: an object laterally OFFSET from the ego path, CLOSING laterally
/// (pure lateral velocity toward the centerline — the RSS §4 conjunction's
/// `lateral_cut_in` term, which the base family never fires). Heading is a
/// constant ±π/2 (no `atan2`, determinism).
fn cutin_family() -> Vec<EvalScenario> {
    let mut v = Vec::new();
    for &x in &[10.0_f64, 15.0, 20.0, 25.0, 30.0] {
        for &side in &[3.0_f64, -3.0] {
            for &speed in &[2.0_f64, 4.0, 6.0] {
                for &closing in &[0.5_f64, 1.5] {
                    let toward_center = -side.signum() * closing;
                    let obj = PerceivedObject {
                        id: 1,
                        pos: Point { x_m: x, y_m: side },
                        velocity_mps: closing,
                        heading_rad: toward_center.signum() * std::f64::consts::FRAC_PI_2,
                        vel: Point {
                            x_m: 0.0,
                            y_m: toward_center,
                        },
                    };
                    let tag = if side > 0.0 { "l" } else { "r" };
                    v.push(EvalScenario::new(
                        format!("cutin_x{x}_{tag}_v{speed}_c{closing}"),
                        MockCorridorSource::straight_5m_half_width(100.0),
                        vec![obj],
                        5.0,
                        speed,
                        60.0,
                    ));
                }
            }
        }
    }
    v
}

/// Lateral-offset hazards: STATIONARY objects abreast of the path but off the
/// centerline. Guards the RSS §4 conjunction from regressing to
/// lateral-on-proximity-alone (which over-rejects a safe parked queue — the
/// admissible direction is the load-bearing one here).
fn lateral_offset_family() -> Vec<EvalScenario> {
    let mut v = Vec::new();
    for &x in &[8.0_f64, 16.0, 24.0, 32.0] {
        for &y in &[2.5_f64, -2.5, 4.0, -4.0] {
            for &speed in &[2.0_f64, 4.0] {
                let obj = PerceivedObject {
                    id: 1,
                    pos: Point { x_m: x, y_m: y },
                    velocity_mps: 0.0,
                    heading_rad: 0.0,
                    vel: Point { x_m: 0.0, y_m: 0.0 },
                };
                v.push(EvalScenario::new(
                    format!("latoff_x{x}_y{y}_v{speed}"),
                    MockCorridorSource::straight_5m_half_width(100.0),
                    vec![obj],
                    5.0,
                    speed,
                    60.0,
                ));
            }
        }
    }
    v
}

/// A 45°-bend corridor built from exact line intersections (`sqrt` only —
/// IEEE-exact, deterministic): straight to `x = bend`, then a diagonal leg of
/// arc length 40 m. Returns (corridor, centerline end, mid-diagonal point).
fn bend_corridor(bend: f64) -> (MockCorridorSource, Point, Point) {
    let c = 0.5_f64.sqrt(); // cos 45° = sin 45°
    let half = 5.0;
    let leg = 40.0;
    // Offset-line ∩ straight-boundary corner points (closed form, see #796 F3).
    let left = vec![
        Point {
            x_m: 0.0,
            y_m: half,
        },
        Point {
            x_m: bend + half - 2.0 * half * c,
            y_m: half,
        },
        Point {
            x_m: bend + (leg - half) * c,
            y_m: (leg + half) * c,
        },
    ];
    let right = vec![
        Point {
            x_m: 0.0,
            y_m: -half,
        },
        Point {
            x_m: bend - half + 2.0 * half * c,
            y_m: -half,
        },
        Point {
            x_m: bend + (leg + half) * c,
            y_m: (leg - half) * c,
        },
    ];
    let end = Point {
        x_m: bend + leg * c,
        y_m: leg * c,
    };
    let mid = Point {
        x_m: bend + (leg / 2.0) * c,
        y_m: (leg / 2.0) * c,
    };
    (MockCorridorSource::from_boundaries(left, right), end, mid)
}

/// Curved corridor: the planner must FOLLOW the bend (the geometric planner's
/// centerline guide; the straight-vocabulary learned planner is measured
/// honestly against it), with the goal at the arc end and an optional stopped
/// hazard mid-bend.
fn curved_family() -> Vec<EvalScenario> {
    let mut v = Vec::new();
    for &bend in &[20.0_f64, 30.0] {
        for &speed in &[1.0_f64, 2.0, 4.0] {
            for &with_hazard in &[false, true] {
                let (road, end, mid) = bend_corridor(bend);
                let objects = if with_hazard {
                    vec![PerceivedObject {
                        id: 1,
                        pos: mid,
                        velocity_mps: 0.0,
                        heading_rad: 0.0,
                        vel: Point { x_m: 0.0, y_m: 0.0 },
                    }]
                } else {
                    vec![]
                };
                let tag = if with_hazard { "stopped" } else { "clear" };
                v.push(
                    EvalScenario::new(
                        format!("curve_b{bend}_{tag}_v{speed}"),
                        road,
                        objects,
                        5.0,
                        speed,
                        end.x_m,
                    )
                    .with_goal_point(end.x_m, end.y_m),
                );
            }
        }
    }
    v
}

/// Multi-object: a stopped QUEUE of three, and a stopped lead with an
/// oncoming behind it — one object's clearance must never mask another's
/// bound (the worst object binds).
fn multi_object_family() -> Vec<EvalScenario> {
    let mut v = Vec::new();
    for &x0 in &[12.0_f64, 20.0, 28.0, 36.0] {
        for &speed in &[2.0_f64, 4.0, 6.0] {
            v.push(EvalScenario::new(
                format!("multi_queue3_x{x0}_v{speed}"),
                MockCorridorSource::straight_5m_half_width(100.0),
                vec![
                    hazard(HazardKind::Stopped, 1, x0),
                    hazard(HazardKind::Stopped, 2, x0 + 6.0),
                    hazard(HazardKind::Stopped, 3, x0 + 12.0),
                ],
                5.0,
                speed,
                60.0,
            ));
        }
        for &speed in &[2.0_f64, 4.0] {
            v.push(EvalScenario::new(
                format!("multi_mixed_x{x0}_v{speed}"),
                MockCorridorSource::straight_5m_half_width(100.0),
                vec![
                    hazard(HazardKind::Stopped, 1, x0),
                    hazard(HazardKind::Oncoming, 2, x0 + 20.0),
                ],
                5.0,
                speed,
                60.0,
            ));
        }
    }
    v
}

/// Degraded posture: the checker's decel-to-stop envelope (#70) over clear
/// road and a stopped lead — the corpus' first non-Nominal rows.
fn degraded_family() -> Vec<EvalScenario> {
    let road = || MockCorridorSource::straight_5m_half_width(100.0);
    let mut v = Vec::new();
    for &speed in &[1.0_f64, 2.0, 4.0, 6.0] {
        v.push(
            EvalScenario::new(
                format!("degraded_clear_v{speed}"),
                road(),
                vec![],
                5.0,
                speed,
                60.0,
            )
            .with_posture(kirra_core::FleetPosture::Degraded),
        );
    }
    for &x in &[16.0_f64, 32.0] {
        for &speed in &[2.0_f64, 4.0, 6.0] {
            v.push(
                EvalScenario::new(
                    format!("degraded_stopped_x{x}_v{speed}"),
                    road(),
                    vec![hazard(HazardKind::Stopped, 1, x)],
                    5.0,
                    speed,
                    60.0,
                )
                .with_posture(kirra_core::FleetPosture::Degraded),
            );
        }
    }
    v
}

/// Occlusion (RSS Rule 4): clear road with an ARMED assured-clear distance —
/// the checker refuses a trajectory that outruns what the ego has observed.
/// The planners are occlusion-blind, so the family measures the checker's
/// sight-vs-speed discrimination across the sweep.
fn occlusion_family() -> Vec<EvalScenario> {
    let mut v = Vec::new();
    for &sight in &[5.0_f64, 10.0, 20.0, 40.0] {
        for &speed in &[1.0_f64, 2.0, 4.0, 6.0] {
            v.push(
                EvalScenario::new(
                    format!("occl_s{sight}_v{speed}"),
                    MockCorridorSource::straight_5m_half_width(100.0),
                    vec![],
                    5.0,
                    speed,
                    60.0,
                )
                .with_sight_distance(sight),
            );
        }
    }
    v
}

/// One corpus family: its stable key, the scenario-name prefix that assigns
/// membership, and the static gate-row names its two planner rates report
/// under. `""` prefix = the base family (everything no other prefix claims).
pub struct CorpusFamily {
    pub key: &'static str,
    pub prefix: &'static str,
    pub geometric_row: &'static str,
    pub learned_row: &'static str,
}

/// The reviewed family universe (#796 F3). Order is display order; membership
/// is first-prefix-match with base as the fallback.
pub const CORPUS_FAMILIES: &[CorpusFamily] = &[
    CorpusFamily {
        key: "base",
        prefix: "",
        geometric_row: "geometric_admissibility_fam_base",
        learned_row: "learned_admissibility_fam_base",
    },
    CorpusFamily {
        key: "cutin",
        prefix: "cutin_",
        geometric_row: "geometric_admissibility_fam_cutin",
        learned_row: "learned_admissibility_fam_cutin",
    },
    CorpusFamily {
        key: "lateral_offset",
        prefix: "latoff_",
        geometric_row: "geometric_admissibility_fam_lateral_offset",
        learned_row: "learned_admissibility_fam_lateral_offset",
    },
    CorpusFamily {
        key: "curved",
        prefix: "curve_",
        geometric_row: "geometric_admissibility_fam_curved",
        learned_row: "learned_admissibility_fam_curved",
    },
    CorpusFamily {
        key: "multi_object",
        prefix: "multi_",
        geometric_row: "geometric_admissibility_fam_multi_object",
        learned_row: "learned_admissibility_fam_multi_object",
    },
    CorpusFamily {
        key: "degraded",
        prefix: "degraded_",
        geometric_row: "geometric_admissibility_fam_degraded",
        learned_row: "learned_admissibility_fam_degraded",
    },
    CorpusFamily {
        key: "occlusion",
        prefix: "occl_",
        geometric_row: "geometric_admissibility_fam_occlusion",
        learned_row: "learned_admissibility_fam_occlusion",
    },
];

/// The family a scenario name belongs to: first non-empty prefix that
/// matches, else `"base"`.
#[must_use]
pub fn family_of(scenario_name: &str) -> &'static str {
    CORPUS_FAMILIES
        .iter()
        .filter(|f| !f.prefix.is_empty())
        .find(|f| scenario_name.starts_with(f.prefix))
        .map_or("base", |f| f.key)
}

/// One scenario's checker verdict for a proposal: the REAL slow-loop checker,
/// with the scenario's armed occlusion sight distance (if any) routed to the
/// RSS Rule 4 bound. A `None` sight distance is byte-identical to
/// `kirra_doer_eval::verdict_of` (the capped call collapses to the plain
/// wrapper's argument set).
pub(crate) fn scenario_verdict(
    sc: &EvalScenario,
    out: &kirra_planner::PlanOutput,
) -> TrajectoryVerdict {
    validate_trajectory_slow_capped(
        &out.trajectory,
        sc.corridor(),
        sc.objects(),
        sc.config(),
        None,
        sc.posture(),
        None,
        sc.sight_distance_m(),
        None,
        None,
        FrameTrust::Trusted,
    )
}

/// Admissibility of a planner over the corpus: every proposal is run through
/// the REAL checker ([`scenario_verdict`]); the rate is
/// `kirra_doer_eval::AdmissibilityTally::admissibility_rate` (fail-closed:
/// an empty corpus scores 0.0, not 1.0).
pub(crate) fn admissibility_over(
    corpus: &[EvalScenario],
    mut plan: impl FnMut(&EvalScenario) -> kirra_planner::PlanOutput,
) -> (f64, AdmissibilityTally) {
    let mut tally = AdmissibilityTally::default();
    for sc in corpus {
        let out = plan(sc);
        tally.record(scenario_verdict(sc, &out));
    }
    (tally.admissibility_rate(), tally)
}

// ---------------------------------------------------------------------------
// Perception corpus — parameterized frames through the fusion oracle
// ---------------------------------------------------------------------------

/// One owned perception case (the borrowed `SemanticEvalFrame` shape needs an
/// owner for corridor + detection sets).
pub struct PerceptionCase {
    pub name: String,
    pub corridor: TajCorridor,
    pub truth: Vec<SemanticDetection>,
    pub detected: Vec<SemanticDetection>,
}

/// A wide-open ~20 m corridor built through the real Phase-A geometric
/// pipeline (the same substrate the taj fusion tests use), so the binding
/// hazard in each frame is the semantic one, never geometry.
pub(crate) fn open_corridor() -> TajCorridor {
    let taj = TajPhaseA::new(TajConfig {
        forward_extent_m: 20.0,
        ..Default::default()
    });
    let n = 180usize;
    let mut ranges = vec![f32::INFINITY; n];
    ranges[10] = 30.0;
    ranges[170] = 30.0;
    let scan = LaserScan {
        angle_min_rad: -std::f64::consts::FRAC_PI_2,
        angle_increment_rad: std::f64::consts::PI / (n as f64 - 1.0),
        range_min_m: 0.1,
        range_max_m: 40.0,
        ranges,
        stamp_ms: 0,
    };
    taj.process(&scan, 0).corridor
}

/// The WS-3.1 perception corpus: hazard class × near distance × lateral
/// span, plus hazard-free frames. The detector under test is the shipped
/// seam — today the scripted [`MockSemanticDetector`] fed the same scene, so
/// the corpus pins perfection; the real detector inherits the corpus (and
/// the thresholds) unchanged behind the same trait.
///
/// Size: 2 classes × 11 distances × 3 spans + 5 clear = **71 frames**
/// (pinned by test).
#[must_use]
pub fn generated_perception_corpus() -> Vec<PerceptionCase> {
    let classes = [SemanticClass::Water, SemanticClass::StaticObstacle];
    let spans: [(f64, f64); 3] = [(-5.0, 5.0), (-2.0, 1.0), (0.5, 4.0)];
    let mut cases = Vec::new();

    for i in 0..5u32 {
        cases.push(PerceptionCase {
            name: format!("clear_{i}"),
            corridor: open_corridor(),
            truth: vec![],
            detected: MockSemanticDetector::default().detect(),
        });
    }
    for &class in &classes {
        for step in 0..11u32 {
            let near_x = 3.0 + 1.5 * f64::from(step);
            for (si, &(lo, hi)) in spans.iter().enumerate() {
                let det = SemanticDetection {
                    class,
                    near_x_m: near_x,
                    lateral_min_m: lo,
                    lateral_max_m: hi,
                };
                // The shipped detector seam: a scripted mock carrying the
                // scene's hazards — `detect()` is the trait the real model
                // will implement.
                let detector = MockSemanticDetector {
                    detections: vec![det],
                };
                cases.push(PerceptionCase {
                    name: format!("{class:?}_x{near_x}_span{si}"),
                    corridor: open_corridor(),
                    truth: vec![det],
                    detected: detector.detect(),
                });
            }
        }
    }
    cases
}

/// Score the perception corpus through the taj safety-weighted harness.
#[must_use]
pub fn score_perception(cases: &[PerceptionCase]) -> SemanticEvalSummary {
    SemanticEvalSummary::from_frames(cases.iter().map(|c| SemanticEvalFrame {
        corridor: &c.corridor,
        truth: &c.truth,
        detected: &c.detected,
    }))
}

// ---------------------------------------------------------------------------
// #777 F1 — negative-control fault families (mutation testing OF the metric)
//
// The seam-pinned corpus above feeds the detector its own ground truth, so its
// `unsafe_miss_rate = 0` / `hazard_recall = 1` rows are TAUTOLOGICAL — they
// score the identity function and cannot fail under any code change. That pins
// perfection, not discrimination. These negative controls instead DERIVE the
// detector output from truth by applying a parameterized detector FAULT, and the
// gate asserts the metric BREACHES — proving the oracle actually catches every
// fault family it will be trusted to catch. If a future fusion change blinds the
// oracle (e.g. a tolerance bump in `score_frame`), a negative-control row that no
// longer breaches turns the gate red.
// ---------------------------------------------------------------------------

/// A parameterized detector fault, applied to ground truth to synthesize a
/// faulty detector output for a negative-control corpus (#777 F1). Each maps to
/// a real perception failure mode the safety oracle must catch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetectorFault {
    /// The detector sees nothing — the true hazard is dropped entirely. The
    /// corridor reads as free → the ego drives into the hazard (`UnsafeMiss`).
    Dropout,
    /// The detector reports the hazard ~6 m FARTHER out than it is, so the
    /// drivable extent runs past the true hazard (`UnsafeMiss`). Mirrors the
    /// `detector_seeing_the_hazard_too_far_out` unit case.
    RangeBiasFar,
    /// The detector mis-classifies the hazard as `Road` (drivable), so the
    /// fusion filters it out and the corridor never clips (`UnsafeMiss`).
    ClassConfusion,
    /// The detector's lateral extent shrinks off the corridor, so the hazard no
    /// longer overlaps and never binds (`UnsafeMiss`).
    LateralShrink,
}

/// The far-range bias offset (m) applied by [`DetectorFault::RangeBiasFar`].
/// Comfortably above `DEFAULT_CLIP_TOL_M` so the shift is a real miss, not jitter.
const RANGE_BIAS_FAR_M: f64 = 6.0;

/// Synthesize the faulty detector output for one frame's ground truth.
#[must_use]
fn faulted_detected(truth: &[SemanticDetection], fault: DetectorFault) -> Vec<SemanticDetection> {
    match fault {
        DetectorFault::Dropout => Vec::new(),
        DetectorFault::RangeBiasFar => truth
            .iter()
            .map(|d| SemanticDetection {
                near_x_m: d.near_x_m + RANGE_BIAS_FAR_M,
                ..*d
            })
            .collect(),
        DetectorFault::ClassConfusion => truth
            .iter()
            .map(|d| SemanticDetection {
                class: SemanticClass::Road,
                ..*d
            })
            .collect(),
        DetectorFault::LateralShrink => truth
            .iter()
            // Push the lateral span far off the corridor centerline (the corridor
            // is lidar-bounded to ~±30 m near the ego) so it no longer overlaps.
            .map(|d| SemanticDetection {
                lateral_min_m: 500.0,
                lateral_max_m: 501.0,
                ..*d
            })
            .collect(),
    }
}

/// Build a negative-control corpus: the HAZARD frames of `base` (truth
/// non-empty), with `detected` re-derived from `truth` under `fault`. Scoring
/// this must breach `unsafe_miss_rate` (the oracle caught the fault).
#[must_use]
pub(crate) fn negative_control_corpus(
    base: &[PerceptionCase],
    fault: DetectorFault,
) -> Vec<PerceptionCase> {
    base.iter()
        .filter(|c| !c.truth.is_empty())
        .map(|c| PerceptionCase {
            name: format!("{}_{fault:?}", c.name),
            corridor: c.corridor.clone(),
            truth: c.truth.clone(),
            detected: faulted_detected(&c.truth, fault),
        })
        .collect()
}

/// Build the PHANTOM negative-control corpus: the world is CLEAR (`truth`
/// emptied) but the detector HALLUCINATES the hazard. This must drive
/// `over_conservative_rate` high WHILE keeping `unsafe_miss_rate == 0` — a
/// phantom is an availability cost, never a safety breach, and the oracle must
/// classify it on the safe side.
#[must_use]
pub(crate) fn phantom_control_corpus(base: &[PerceptionCase]) -> Vec<PerceptionCase> {
    base.iter()
        .filter(|c| !c.truth.is_empty())
        .map(|c| PerceptionCase {
            name: format!("{}_Phantom", c.name),
            corridor: c.corridor.clone(),
            truth: Vec::new(),
            detected: c.truth.clone(),
        })
        .collect()
}

/// The minimum `unsafe_miss_rate` a genuinely-unsafe fault family must produce
/// for its negative-control row to pass — i.e. the oracle catches ≥ 90 % of the
/// injected faults. Below this the oracle is considered blinded (gate red).
const NEG_CONTROL_BREACH_MIN: f64 = 0.9;

// ---------------------------------------------------------------------------
// Thresholds + gate verdict
// ---------------------------------------------------------------------------

/// The reviewed KPI policy (`ci/scenario_kpi_thresholds.json`). Every field
/// is required — a threshold that silently defaults is not a policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KpiThresholds {
    /// Free-text rationale — travels with the numbers.
    #[serde(rename = "_comment")]
    pub comment: String,
    /// #796 F2 — which detector this profile bounds: `"mock"` (the scripted
    /// seam, perfection-pinned) or `"real"` (the pre-committed AoU-target
    /// profile in `ci/scenario_kpi_thresholds_real_detector.json`). Defaults
    /// to `"mock"` so the original profile is unchanged on disk; stamped into
    /// the F11 evidence artifact so a report names the profile it gated.
    #[serde(default = "default_detector")]
    pub detector: String,
    /// Min fraction of GEOMETRIC-planner proposals the checker admits
    /// without an MRC (`Accept | Clamp`) over the doer corpus.
    pub geometric_admissibility_min: f64,
    /// Min admissibility for the seeded SafetyAware learned planner.
    pub learned_admissibility_min: f64,
    /// #796 F3 — per-family admissibility floors (one entry per
    /// [`CORPUS_FAMILIES`] key, every field required): the aggregate rows
    /// above cannot mask a family regression once families mix.
    pub families: FamilyThresholds,
    /// Max fraction of perception frames where the detector's drivable
    /// extent runs PAST ground truth (the catastrophic direction).
    pub unsafe_miss_rate_max: f64,
    /// Min fraction of true binding hazards the detector catches.
    pub hazard_recall_min: f64,
    /// EP-20 differential: max fraction of truth-hazard frames where Phase-B
    /// FAILED to diverge from hazard-blind Phase-A (the unsafe direction).
    pub differential_missed_tighten_max: f64,
    /// EP-20 differential: max fraction of all frames with an UNJUSTIFIED
    /// Phase-B tighten (availability cost, over-conservative direction).
    pub differential_phantom_tighten_max: f64,
}

fn default_detector() -> String {
    "mock".to_string()
}

/// One family's admissibility floors (both planners). Required fields —
/// a family without a reviewed bound must fail to PARSE, never silently pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FamilyBounds {
    pub geometric_min: f64,
    pub learned_min: f64,
}

/// #796 F3 — the per-family floor table. One NAMED field per
/// [`CORPUS_FAMILIES`] key (`deny_unknown_fields` + all-required keeps the
/// policy file and the family universe lock-stepped: adding a family without
/// a bound, or a bound for a removed family, reds at load).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FamilyThresholds {
    pub base: FamilyBounds,
    pub cutin: FamilyBounds,
    pub lateral_offset: FamilyBounds,
    pub curved: FamilyBounds,
    pub multi_object: FamilyBounds,
    pub degraded: FamilyBounds,
    pub occlusion: FamilyBounds,
}

impl FamilyThresholds {
    /// The bounds for a [`CORPUS_FAMILIES`] key. Panics on an unknown key —
    /// the family table and this struct are lock-stepped by construction (and
    /// by the `family_universe_is_lock_stepped` test), so an unknown key is a
    /// programming error, not a runtime condition.
    #[must_use]
    pub fn bounds_for(&self, key: &str) -> &FamilyBounds {
        match key {
            "base" => &self.base,
            "cutin" => &self.cutin,
            "lateral_offset" => &self.lateral_offset,
            "curved" => &self.curved,
            "multi_object" => &self.multi_object,
            "degraded" => &self.degraded,
            "occlusion" => &self.occlusion,
            other => panic!("unknown corpus family key: {other}"),
        }
    }
}

/// One evaluated KPI row: measured value vs its bound.
#[derive(Debug, Clone, Serialize)]
pub struct KpiRow {
    pub name: &'static str,
    pub measured: f64,
    /// `">="` or `"<="` — which side of `bound` passes.
    pub direction: &'static str,
    pub bound: f64,
    pub pass: bool,
}

impl KpiRow {
    fn at_least(name: &'static str, measured: f64, bound: f64) -> Self {
        Self {
            name,
            measured,
            direction: ">=",
            bound,
            pass: measured >= bound,
        }
    }
    fn at_most(name: &'static str, measured: f64, bound: f64) -> Self {
        Self {
            name,
            measured,
            direction: "<=",
            bound,
            pass: measured <= bound,
        }
    }
}

/// The full gate outcome: every row, plus corpus sizes for the report.
#[derive(Debug, Clone, Serialize)]
pub struct GateReport {
    pub doer_scenarios: usize,
    pub perception_frames: usize,
    pub rows: Vec<KpiRow>,
}

impl GateReport {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.rows.iter().all(|r| r.pass)
    }
}

/// Run the whole gate: generate both corpora, produce the three KPIs through
/// the existing harnesses, and threshold them.
#[must_use]
pub fn run_gate(t: &KpiThresholds) -> GateReport {
    // One per-(planner × scenario) verdict pass feeds BOTH the aggregate rate
    // rows and the #796 F3 per-family rows (identical construction to the
    // F4/F5 manifest gate — the rates and the named sets can never disagree).
    let verdicts = per_scenario_verdicts();
    let doer_scenarios = verdicts.len() / 2;

    // Fail-closed rate: an empty selection scores 0.0, never 1.0.
    let rate_where = |planner: &str, family: Option<&str>| -> f64 {
        let mut total = 0usize;
        let mut admitted = 0usize;
        for r in verdicts.iter().filter(|r| r.planner == planner) {
            if family.is_some_and(|f| family_of(&r.scenario) != f) {
                continue;
            }
            total += 1;
            admitted += usize::from(r.admissible);
        }
        if total == 0 {
            0.0
        } else {
            admitted as f64 / total as f64
        }
    };

    let geo_rate = rate_where("geometric", None);
    let learned_rate = rate_where("learned_safetyaware_seed7", None);

    let cases = generated_perception_corpus();
    let perception = score_perception(&cases);

    let mut rows = vec![
        KpiRow::at_least(
            "geometric_admissibility",
            geo_rate,
            t.geometric_admissibility_min,
        ),
        KpiRow::at_least(
            "learned_admissibility",
            learned_rate,
            t.learned_admissibility_min,
        ),
    ];

    // #796 F3 — per-family admissibility rows: a regression inside one family
    // cannot hide behind the aggregate (Simpson's paradox once families mix).
    for fam in CORPUS_FAMILIES {
        let bounds = t.families.bounds_for(fam.key);
        rows.push(KpiRow::at_least(
            fam.geometric_row,
            rate_where("geometric", Some(fam.key)),
            bounds.geometric_min,
        ));
        rows.push(KpiRow::at_least(
            fam.learned_row,
            rate_where("learned_safetyaware_seed7", Some(fam.key)),
            bounds.learned_min,
        ));
    }

    rows.extend([
        // #777 F1: these two rows are SEAM-PINNED — the mock detector is fed its
        // own ground truth, so they score the identity function and cannot fail.
        // Kept as a harness smoke test (labelled so CI output can't be mistaken
        // for a real measurement); the DISCRIMINANCE evidence is the
        // negative-control rows below.
        KpiRow::at_most(
            "unsafe_miss_rate_seam_pinned",
            perception.unsafe_miss_rate(),
            t.unsafe_miss_rate_max,
        ),
        KpiRow::at_least(
            "hazard_recall_seam_pinned",
            perception.hazard_recall(),
            t.hazard_recall_min,
        ),
    ]);

    // #777 F1 — negative-control fault families: each MUST breach the safety
    // metric, proving the oracle discriminates the fault (mutation testing of the
    // metric, in the gate itself rather than one unit test off to the side).
    for fault in [
        DetectorFault::Dropout,
        DetectorFault::RangeBiasFar,
        DetectorFault::ClassConfusion,
        DetectorFault::LateralShrink,
    ] {
        let faulted = negative_control_corpus(&cases, fault);
        let s = score_perception(&faulted);
        // Static name so `KpiRow.name: &'static str` holds; one arm per fault.
        let name: &'static str = match fault {
            DetectorFault::Dropout => "negctl_dropout_unsafe_miss",
            DetectorFault::RangeBiasFar => "negctl_range_bias_far_unsafe_miss",
            DetectorFault::ClassConfusion => "negctl_class_confusion_unsafe_miss",
            DetectorFault::LateralShrink => "negctl_lateral_shrink_unsafe_miss",
        };
        rows.push(KpiRow::at_least(
            name,
            s.unsafe_miss_rate(),
            NEG_CONTROL_BREACH_MIN,
        ));
    }

    // #777 F1 — phantom control: a hallucinated hazard over a CLEAR world is an
    // availability cost, not a safety breach. The oracle must drive
    // over_conservative_rate high (it caught the phantom) while keeping
    // unsafe_miss_rate exactly 0 (it did NOT mislabel a phantom as unsafe).
    let phantom = phantom_control_corpus(&cases);
    let ps = score_perception(&phantom);
    rows.push(KpiRow::at_least(
        "negctl_phantom_over_conservative",
        ps.over_conservative_rate(),
        NEG_CONTROL_BREACH_MIN,
    ));
    rows.push(KpiRow::at_most(
        "negctl_phantom_no_unsafe_miss",
        ps.unsafe_miss_rate(),
        0.0,
    ));

    // EP-20 — differential perception rows: Phase-A (geometric, hazard-blind)
    // vs Phase-B (semantic fusion) over the SHARED ground-truth corpus. The
    // seam-pinned rows are the harness smoke test (mock detector = truth ⇒
    // exact 0s); the DISCRIMINANCE evidence is the differential negative
    // controls below, mirroring the #777 pattern.
    let diff = differential::differential_summary(
        cases
            .iter()
            .map(|c| (&c.corridor, c.truth.as_slice(), c.detected.as_slice())),
    );
    rows.push(KpiRow::at_most(
        "differential_forbidden_loosen",
        diff.forbidden_loosen as f64,
        0.0, // HARD invariant: semantic fusion is derate-only, never looser.
    ));
    rows.push(KpiRow::at_most(
        "differential_missed_tighten_seam_pinned",
        diff.missed_tighten_rate(),
        t.differential_missed_tighten_max,
    ));
    rows.push(KpiRow::at_most(
        "differential_phantom_tighten_seam_pinned",
        diff.phantom_tighten_rate(),
        t.differential_phantom_tighten_max,
    ));

    // EP-20 negative controls: every detector fault family must surface as a
    // MISSED Phase-A/Phase-B divergence (the fusion went hazard-blind), and a
    // hallucinated hazard must surface as a PHANTOM divergence — proving the
    // differential classifier discriminates both directions.
    for fault in [
        DetectorFault::Dropout,
        DetectorFault::RangeBiasFar,
        DetectorFault::ClassConfusion,
        DetectorFault::LateralShrink,
    ] {
        let faulted = negative_control_corpus(&cases, fault);
        let d = differential::differential_summary(
            faulted
                .iter()
                .map(|c| (&c.corridor, c.truth.as_slice(), c.detected.as_slice())),
        );
        let name: &'static str = match fault {
            DetectorFault::Dropout => "negctl_dropout_differential_missed",
            DetectorFault::RangeBiasFar => "negctl_range_bias_far_differential_missed",
            DetectorFault::ClassConfusion => "negctl_class_confusion_differential_missed",
            DetectorFault::LateralShrink => "negctl_lateral_shrink_differential_missed",
        };
        rows.push(KpiRow::at_least(
            name,
            d.missed_tighten_rate(),
            NEG_CONTROL_BREACH_MIN,
        ));
        rows.push(KpiRow::at_most(
            match fault {
                DetectorFault::Dropout => "negctl_dropout_differential_no_loosen",
                DetectorFault::RangeBiasFar => "negctl_range_bias_far_differential_no_loosen",
                DetectorFault::ClassConfusion => "negctl_class_confusion_differential_no_loosen",
                DetectorFault::LateralShrink => "negctl_lateral_shrink_differential_no_loosen",
            },
            d.forbidden_loosen as f64,
            0.0,
        ));
    }
    {
        let phantom = phantom_control_corpus(&cases);
        let d = differential::differential_summary(
            phantom
                .iter()
                .map(|c| (&c.corridor, c.truth.as_slice(), c.detected.as_slice())),
        );
        // Every phantom-corpus frame is clear-truth with a hallucinated
        // detection, so the differential must classify (nearly) all of them
        // PhantomTighten — the over-conservative direction, discriminated.
        rows.push(KpiRow::at_least(
            "negctl_phantom_differential_phantom_tighten",
            d.phantom_tighten_rate(),
            NEG_CONTROL_BREACH_MIN,
        ));
    }

    GateReport {
        doer_scenarios,
        perception_frames: cases.len(),
        rows,
    }
}

// ---------------------------------------------------------------------------
// WP-23 (G-16 software half) — the Monte-Carlo campaign gate
// ---------------------------------------------------------------------------

use confidence::{clopper_pearson_interval, wilson_interval, ALPHA_95, Z_95};
use montecarlo::{
    sample_doer_corpus, sample_perception_corpus, Bound, McGateReport, McKpiRow, MonteCarloPolicy,
    Profile,
};

/// A Wilson (gated) + Clopper–Pearson (reported) interval pair from a count.
fn intervals(
    successes: u64,
    trials: u64,
) -> (
    confidence::ConfidenceInterval,
    confidence::ConfidenceInterval,
) {
    (
        wilson_interval(successes, trials, Z_95),
        clopper_pearson_interval(successes, trials, ALPHA_95),
    )
}

/// Run the WP-23 Monte-Carlo campaign for a `profile`: sample the corpora at the
/// policy's seed + size, score each KPI through the existing harnesses, and gate
/// each on a [`confidence`] interval (a "must be small" rate on its UPPER bound,
/// a "must be large" rate on its LOWER bound). The #777 F1 negative controls are
/// re-run over the SAMPLED hazard frames so the oracle-discrimination evidence
/// survives resampling. Deterministic: fixed `(seed, sizes)` ⇒ fixed verdict.
#[must_use]
pub fn run_montecarlo_gate(policy: &MonteCarloPolicy, profile: Profile) -> McGateReport {
    let sizes = policy.sizes(profile);
    let seed = policy.seed;

    // --- Doer admissibility (geometric + seeded learned) over the sampled corpus.
    let corpus = sample_doer_corpus(seed, sizes.doer_samples);
    let (_, geo_tally) = admissibility_over(&corpus, |sc| {
        GeometricPlanner::new(GeometricPlannerConfig::default()).plan(&sc.plan_input())
    });
    let learned = LearnedPlanner::trained(7, Teacher::SafetyAware);
    let (_, learned_tally) = admissibility_over(&corpus, |sc| {
        learned.plan_with_chosen_index(&sc.plan_input()).1
    });

    let geo_ok = (geo_tally.accept + geo_tally.clamp) as u64;
    let (geo_w, geo_cp) = intervals(geo_ok, geo_tally.total() as u64);
    let learned_ok = (learned_tally.accept + learned_tally.clamp) as u64;
    let (learned_w, learned_cp) = intervals(learned_ok, learned_tally.total() as u64);

    // --- Perception KPIs over the sampled frames.
    let cases = sample_perception_corpus(seed, sizes.perception_samples);
    let perc = score_perception(&cases);
    let (miss_w, miss_cp) = intervals(perc.unsafe_miss as u64, perc.frames as u64);
    let (recall_w, recall_cp) = intervals(
        perc.true_hazards_caught as u64,
        perc.frames_with_true_hazard as u64,
    );

    let mut rows = vec![
        McKpiRow::new(
            "geometric_admissibility",
            geo_w,
            geo_cp,
            Bound::CiLowerAtLeast(policy.geometric_admissibility_lo_min),
        ),
        McKpiRow::new(
            "learned_admissibility",
            learned_w,
            learned_cp,
            Bound::CiLowerAtLeast(policy.learned_admissibility_lo_min),
        ),
        McKpiRow::new(
            "unsafe_miss_rate_seam_pinned",
            miss_w,
            miss_cp,
            Bound::CiUpperAtMost(policy.unsafe_miss_rate_hi_max),
        ),
        McKpiRow::new(
            "hazard_recall_seam_pinned",
            recall_w,
            recall_cp,
            Bound::CiLowerAtLeast(policy.hazard_recall_lo_min),
        ),
    ];

    // --- #777 F1 negative controls, re-run over the sampled hazard frames: each
    // fault family must still breach unsafe_miss (CI lower bound over the floor).
    for fault in [
        DetectorFault::Dropout,
        DetectorFault::RangeBiasFar,
        DetectorFault::ClassConfusion,
        DetectorFault::LateralShrink,
    ] {
        let s = score_perception(&negative_control_corpus(&cases, fault));
        let (w, cp) = intervals(s.unsafe_miss as u64, s.frames as u64);
        let name: &'static str = match fault {
            DetectorFault::Dropout => "negctl_dropout_unsafe_miss",
            DetectorFault::RangeBiasFar => "negctl_range_bias_far_unsafe_miss",
            DetectorFault::ClassConfusion => "negctl_class_confusion_unsafe_miss",
            DetectorFault::LateralShrink => "negctl_lateral_shrink_unsafe_miss",
        };
        rows.push(McKpiRow::new(
            name,
            w,
            cp,
            Bound::CiLowerAtLeast(policy.negctl_breach_lo_min),
        ));
    }

    // --- Phantom control: over-conservative breach (statistical), and the HARD
    // invariant that a phantom is NEVER scored unsafe (point == 0, no CI slack).
    let ps = score_perception(&phantom_control_corpus(&cases));
    let (over_w, over_cp) = intervals(ps.over_conservative as u64, ps.frames as u64);
    rows.push(McKpiRow::new(
        "negctl_phantom_over_conservative",
        over_w,
        over_cp,
        Bound::CiLowerAtLeast(policy.negctl_breach_lo_min),
    ));
    let (pmiss_w, pmiss_cp) = intervals(ps.unsafe_miss as u64, ps.frames as u64);
    rows.push(McKpiRow::new(
        "negctl_phantom_no_unsafe_miss",
        pmiss_w,
        pmiss_cp,
        Bound::PointAtMost(0.0),
    ));

    McGateReport {
        seed,
        doer_samples: corpus.len(),
        perception_samples: cases.len(),
        rows,
    }
}

// ---------------------------------------------------------------------------
// #796 F4/F5 — the NAMED known-failure manifest (set-equality ratchet)
//
// `run_gate`'s rate rows discard the tallies' identities: a PR that fixes 3
// scenarios and breaks 3 different ones is byte-identical to no-change, and
// the honor-system float ratchet is exposed to libm/toolchain FP flips (F7).
// The manifest mechanizes both: the failing-scenario SET is committed BY NAME
// per planner (`ci/scenario_kpi_known_failures.json`) and the gate demands
// SET EQUALITY —
//   * a measured failure NOT in the manifest = a REGRESSION (unsafe
//     direction, always red);
//   * a manifest entry that no longer fails = an IMPROVEMENT (also red, until
//     the manifest is tightened to lock it in — a one-line reviewed change,
//     never silent).
// Any FP flip is therefore attributable to a scenario NAME, and the bounds
// are integer/set-valued, not truncated float rates.
// ---------------------------------------------------------------------------

/// The committed known-failure manifest: per planner, the exact scenario
/// names the checker currently refuses (MRC/Pending). Keys are stable planner
/// labels; scenario names come from `generated_doer_corpus` (deterministic).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureManifest {
    /// Free-text rationale — travels with the names.
    #[serde(rename = "_comment")]
    pub comment: String,
    /// Geometric planner (`GeometricPlannerConfig::default()`).
    pub geometric: Vec<String>,
    /// Seeded SafetyAware learned planner (`trained(7, Teacher::SafetyAware)`).
    pub learned_safetyaware_seed7: Vec<String>,
}

/// One per-scenario, per-planner checker verdict — the F11 CSV artifact row
/// and the manifest gate's raw material.
#[derive(Debug, Clone, Serialize)]
pub struct PlannerVerdictRow {
    pub planner: &'static str,
    pub scenario: String,
    pub verdict: &'static str,
    /// `Accept | Clamp` (the `admissibility_rate` predicate).
    pub admissible: bool,
}

fn verdict_label(v: kirra_core::trajectory::TrajectoryVerdict) -> (&'static str, bool) {
    use kirra_core::trajectory::TrajectoryVerdict as V;
    match v {
        V::Accept => ("Accept", true),
        V::Clamp => ("Clamp", true),
        V::MRCFallback => ("MRCFallback", false),
        V::Pending => ("Pending", false),
    }
}

/// Every (planner × scenario) verdict over the deterministic corpus — the
/// same construction as `run_gate`'s rate rows, with identities kept.
#[must_use]
pub fn per_scenario_verdicts() -> Vec<PlannerVerdictRow> {
    let corpus = generated_doer_corpus();
    let learned = LearnedPlanner::trained(7, Teacher::SafetyAware);
    let mut rows = Vec::with_capacity(corpus.len() * 2);
    for sc in &corpus {
        let out = GeometricPlanner::new(GeometricPlannerConfig::default()).plan(&sc.plan_input());
        let (verdict, admissible) = verdict_label(scenario_verdict(sc, &out));
        rows.push(PlannerVerdictRow {
            planner: "geometric",
            scenario: sc.name.clone(),
            verdict,
            admissible,
        });
        let out = learned.plan_with_chosen_index(&sc.plan_input()).1;
        let (verdict, admissible) = verdict_label(scenario_verdict(sc, &out));
        rows.push(PlannerVerdictRow {
            planner: "learned_safetyaware_seed7",
            scenario: sc.name.clone(),
            verdict,
            admissible,
        });
    }
    rows
}

/// One planner's set-equality outcome against the manifest.
#[derive(Debug, Clone, Serialize)]
pub struct ManifestDiff {
    pub planner: &'static str,
    /// Failing now, NOT in the manifest — a regression (unsafe direction).
    pub new_failures: Vec<String>,
    /// In the manifest, no longer failing — an improvement to lock in.
    pub fixed: Vec<String>,
    /// Manifest names that don't exist in the corpus at all (a rename/typo —
    /// a stale manifest must red, not silently gate nothing).
    pub unknown_names: Vec<String>,
}

impl ManifestDiff {
    #[must_use]
    pub fn pass(&self) -> bool {
        self.new_failures.is_empty() && self.fixed.is_empty() && self.unknown_names.is_empty()
    }
}

/// The F4/F5 gate: measured failing sets vs the committed manifest, per
/// planner, SET EQUALITY required.
#[must_use]
pub fn run_manifest_gate(manifest: &FailureManifest) -> Vec<ManifestDiff> {
    use std::collections::BTreeSet;
    let rows = per_scenario_verdicts();
    let corpus_names: BTreeSet<&str> = rows.iter().map(|r| r.scenario.as_str()).collect();
    let diff_for = |planner: &'static str, committed: &[String]| -> ManifestDiff {
        let measured: BTreeSet<&str> = rows
            .iter()
            .filter(|r| r.planner == planner && !r.admissible)
            .map(|r| r.scenario.as_str())
            .collect();
        let committed_set: BTreeSet<&str> = committed.iter().map(String::as_str).collect();
        ManifestDiff {
            planner,
            new_failures: measured
                .difference(&committed_set)
                .map(|s| (*s).to_string())
                .collect(),
            fixed: committed_set
                .difference(&measured)
                .filter(|s| corpus_names.contains(**s))
                .map(|s| (*s).to_string())
                .collect(),
            unknown_names: committed_set
                .iter()
                .filter(|s| !corpus_names.contains(**s))
                .map(|s| (*s).to_string())
                .collect(),
        }
    };
    vec![
        diff_for("geometric", &manifest.geometric),
        diff_for(
            "learned_safetyaware_seed7",
            &manifest.learned_safetyaware_seed7,
        ),
    ]
}

/// #796 F11 — the corpus fingerprint stamped into the evidence artifact: a
/// SHA-256 over every doer scenario name + every perception case name, in
/// corpus order. Any generator change (added axis, renamed cell, resized
/// sweep) changes the stamp, so a report is attributable to an exact corpus.
#[must_use]
pub fn corpus_fingerprint_sha256() -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for sc in generated_doer_corpus() {
        h.update(sc.name.as_bytes());
        h.update([0u8]);
    }
    for case in generated_perception_corpus() {
        h.update(case.name.as_bytes());
        h.update([0u8]);
    }
    hex_lower(&h.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed manifest path (repo root), used by the equality pin below.
    const MANIFEST_PATH: &str = "../../ci/scenario_kpi_known_failures.json";

    fn committed_manifest() -> FailureManifest {
        let raw = std::fs::read_to_string(MANIFEST_PATH).expect("read known-failures manifest");
        serde_json::from_str(&raw).expect("parse known-failures manifest")
    }

    /// #796 F4/F5 — the committed manifest matches the measured failing sets
    /// EXACTLY (set equality). A new failure OR an unrecorded fix reds this.
    #[test]
    fn known_failure_manifest_is_set_equal_to_measured() {
        let diffs = run_manifest_gate(&committed_manifest());
        for d in &diffs {
            assert!(
                d.pass(),
                "manifest drift for {}: new_failures={:?} fixed={:?} unknown={:?} — a new \
                 failure is a regression; a fix must be locked in by tightening \
                 ci/scenario_kpi_known_failures.json (regenerate via the ignored \
                 dump_failing_scenario_names test)",
                d.planner,
                d.new_failures,
                d.fixed,
                d.unknown_names
            );
        }
    }

    /// The manifest counts must reconcile with the rate rows (347/398 and
    /// 329/398 today): the names ARE the integer bounds (#796 F5).
    #[test]
    fn manifest_counts_reconcile_with_the_rate_floors() {
        let m = committed_manifest();
        assert_eq!(m.geometric.len(), 51, "398 - 347 admitted");
        assert_eq!(m.learned_safetyaware_seed7.len(), 69, "398 - 329 admitted");
    }

    /// The gate DISCRIMINATES: perturbing the committed sets in either
    /// direction (drop a name → "fixed"; add a bogus-but-real name → covered
    /// by new_failures on the other side; add a nonexistent name → unknown)
    /// is caught by name.
    #[test]
    fn manifest_gate_flags_drift_in_both_directions() {
        let mut m = committed_manifest();
        let dropped = m.geometric.pop().expect("manifest has entries");
        m.learned_safetyaware_seed7
            .push("no_such_scenario".to_string());
        let diffs = run_manifest_gate(&m);
        let geo = &diffs[0];
        assert_eq!(
            geo.new_failures,
            vec![dropped],
            "a dropped entry surfaces as a NEW failure by name"
        );
        let learned = &diffs[1];
        assert_eq!(
            learned.unknown_names,
            vec!["no_such_scenario".to_string()],
            "a stale/typo name is flagged, never silently ignored"
        );
    }

    /// Regeneration utility (documented in the manifest _comment): dumps the
    /// measured failing names as JSON arrays. `--ignored --nocapture`.
    #[test]
    #[ignore = "manifest regeneration utility, not a gate"]
    fn dump_failing_scenario_names() {
        for planner in ["geometric", "learned_safetyaware_seed7"] {
            let names: Vec<String> = per_scenario_verdicts()
                .into_iter()
                .filter(|r| r.planner == planner && !r.admissible)
                .map(|r| r.scenario)
                .collect();
            println!(
                "{planner}: {}",
                serde_json::to_string_pretty(&names).unwrap()
            );
        }
    }

    /// #796 F2 — the pre-committed real-detector profile parses, is labeled,
    /// and differs from the mock profile ONLY in `hazard_recall_min` (the one
    /// bound with a plan-sourced number: the AoU 0.9 target). Every other
    /// bound — including the catastrophic-direction `unsafe_miss_rate_max` —
    /// is pinned identical: a relaxation there must arrive as a reviewed
    /// change to the profile file, never ride in silently.
    #[test]
    fn real_detector_profile_relaxes_only_the_plan_sourced_bound() {
        let mock: KpiThresholds = serde_json::from_str(
            &std::fs::read_to_string("../../ci/scenario_kpi_thresholds.json").unwrap(),
        )
        .unwrap();
        let real: KpiThresholds = serde_json::from_str(
            &std::fs::read_to_string("../../ci/scenario_kpi_thresholds_real_detector.json")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            mock.detector, "mock",
            "the original profile defaults to mock"
        );
        assert_eq!(real.detector, "real", "the new profile is labeled");
        assert_eq!(real.hazard_recall_min, 0.9, "the plan-sourced AoU target");
        assert!(real.hazard_recall_min < mock.hazard_recall_min);
        // Everything else identical — especially the catastrophic direction.
        assert_eq!(real.unsafe_miss_rate_max, mock.unsafe_miss_rate_max);
        assert_eq!(
            real.geometric_admissibility_min,
            mock.geometric_admissibility_min
        );
        assert_eq!(
            real.learned_admissibility_min,
            mock.learned_admissibility_min
        );
        assert_eq!(
            real.differential_missed_tighten_max,
            mock.differential_missed_tighten_max
        );
        assert_eq!(
            real.differential_phantom_tighten_max,
            mock.differential_phantom_tighten_max
        );
        // #796 F3: the per-family floors are doer-side (detector-independent)
        // — pinned identical across profiles like every other non-plan-sourced
        // bound.
        assert_eq!(real.families, mock.families);
    }

    /// #796 F11 — the corpus fingerprint is stable across runs (determinism)
    /// and 64 hex chars.
    #[test]
    fn corpus_fingerprint_is_deterministic() {
        let a = corpus_fingerprint_sha256();
        assert_eq!(a, corpus_fingerprint_sha256());
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Corpus sizes are pinned: a silent shrink would weaken the gate while
    /// it kept reporting green. #796 F3: PER-FAMILY counts are pinned too —
    /// an aggregate pin alone would let one family shrink while another grew.
    #[test]
    fn corpus_sizes_are_pinned_at_low_hundreds() {
        let corpus = generated_doer_corpus();
        assert_eq!(corpus.len(), 398);
        assert_eq!(generated_perception_corpus().len(), 71);

        let count = |fam: &str| corpus.iter().filter(|s| family_of(&s.name) == fam).count();
        assert_eq!(count("base"), 248);
        assert_eq!(count("cutin"), 60);
        assert_eq!(count("lateral_offset"), 32);
        assert_eq!(count("curved"), 12);
        assert_eq!(count("multi_object"), 20);
        assert_eq!(count("degraded"), 10);
        assert_eq!(count("occlusion"), 16);
    }

    /// #796 F3 — the family universe is lock-stepped end to end: every
    /// [`CORPUS_FAMILIES`] key resolves committed bounds (a missing JSON
    /// entry cannot parse; a missing `bounds_for` arm panics here, not in
    /// CI's report path), every family is non-empty in the live corpus, and
    /// scenario names never collide across prefixes.
    #[test]
    fn family_universe_is_lock_stepped() {
        let t = committed_thresholds();
        let corpus = generated_doer_corpus();
        for fam in CORPUS_FAMILIES {
            let b = t.families.bounds_for(fam.key);
            assert!(
                b.geometric_min.is_finite() && b.learned_min.is_finite(),
                "family {} must carry finite committed bounds",
                fam.key
            );
            assert!(
                corpus.iter().any(|s| family_of(&s.name) == fam.key),
                "family {} must be populated in the live corpus",
                fam.key
            );
        }
        // Base-family names must not accidentally carry a family prefix (a
        // rename that silently re-homes scenarios would corrupt every
        // per-family floor).
        let mut seen = std::collections::BTreeSet::new();
        for s in &corpus {
            assert!(seen.insert(s.name.clone()), "duplicate name {}", s.name);
        }
    }

    /// #796 F3 — a FAMILY regression reds the gate even when the aggregate
    /// still clears its floor: the per-family row is attributed by name.
    #[test]
    fn family_breach_reds_the_gate_independently_of_the_aggregate() {
        let mut t = committed_thresholds();
        t.families.curved.geometric_min = 1.01; // unreachable: rate ≤ 1.0
        let report = run_gate(&t);
        assert!(!report.passed());
        assert!(
            report
                .rows
                .iter()
                .any(|r| r.name == "geometric_admissibility_fam_curved" && !r.pass),
            "the breach must attribute to the curved family row: {report:#?}"
        );
        // The aggregate row itself still passes — the family row is what
        // caught it (the anti-Simpson property).
        assert!(report
            .rows
            .iter()
            .any(|r| r.name == "geometric_admissibility" && r.pass));
    }

    /// The generators are deterministic: two invocations produce identical
    /// scenario names in identical order (no RNG, no time).
    #[test]
    fn generators_are_deterministic() {
        let a: Vec<String> = generated_doer_corpus()
            .into_iter()
            .map(|s| s.name)
            .collect();
        let b: Vec<String> = generated_doer_corpus()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(a, b);
        let pa: Vec<String> = generated_perception_corpus()
            .into_iter()
            .map(|c| c.name)
            .collect();
        let pb: Vec<String> = generated_perception_corpus()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(pa, pb);
    }

    fn committed_thresholds() -> KpiThresholds {
        // The gate tests run from the crate dir; the binary defaults to the
        // repo-root path. Resolve relative to the manifest.
        let p = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../ci/scenario_kpi_thresholds.json"
        );
        serde_json::from_str(&std::fs::read_to_string(p).expect("committed thresholds exist"))
            .expect("thresholds parse")
    }

    /// THE WS-3.1 DoD (green half): the gate PASSES against the committed
    /// thresholds — the numbers in ci/ are honest for the current tree.
    #[test]
    fn gate_passes_against_committed_thresholds() {
        let report = run_gate(&committed_thresholds());
        assert!(
            report.passed(),
            "the committed thresholds must hold on the current tree: {report:#?}"
        );
    }

    /// THE WS-3.1 DoD (red half): a KPI regression turns the gate red. An
    /// impossible bound stands in for the regression — the wiring from
    /// measured value to verdict is what is under test.
    #[test]
    fn gate_goes_red_on_a_kpi_breach() {
        let mut t = committed_thresholds();
        t.geometric_admissibility_min = 1.01; // unreachable: rate is ≤ 1.0
        let report = run_gate(&t);
        assert!(!report.passed(), "an unreachable bound must red the gate");
        assert!(
            report
                .rows
                .iter()
                .any(|r| r.name == "geometric_admissibility" && !r.pass),
            "the breach must be attributed to the right row: {report:#?}"
        );
    }

    /// The perception axis detects a real unsafe miss: a detector that drops
    /// a true hazard breaches unsafe_miss_rate/hazard_recall — the metric is
    /// live, not vacuously green.
    #[test]
    fn blind_detector_breaches_the_perception_kpis() {
        let mut cases = generated_perception_corpus();
        for c in &mut cases {
            c.detected.clear(); // a detector that sees nothing
        }
        let s = score_perception(&cases);
        assert!(
            s.unsafe_miss_rate() > 0.5,
            "a blind detector must unsafe-miss most hazard frames"
        );
        assert_eq!(s.hazard_recall(), 0.0, "a blind detector catches nothing");
    }

    /// #777 F1 — every UNSAFE fault family breaches the safety metric (the oracle
    /// discriminates it), while the PHANTOM family is caught on the SAFE side
    /// (over-conservative, never unsafe). This is mutation testing OF the metric:
    /// if a future fusion change blinded the oracle, one of these would drop below
    /// the breach floor.
    #[test]
    fn negative_control_families_breach_the_safety_metric() {
        let base = generated_perception_corpus();

        // Sanity: the SEAM-PINNED (identity) corpus does NOT breach — so the
        // breaches below come from the injected fault, not the corpus.
        assert_eq!(
            score_perception(&base).unsafe_miss_rate(),
            0.0,
            "the identity corpus must score 0 unsafe-miss (else the negctls prove nothing)"
        );

        for fault in [
            DetectorFault::Dropout,
            DetectorFault::RangeBiasFar,
            DetectorFault::ClassConfusion,
            DetectorFault::LateralShrink,
        ] {
            let s = score_perception(&negative_control_corpus(&base, fault));
            assert!(
                s.unsafe_miss_rate() >= NEG_CONTROL_BREACH_MIN,
                "{fault:?} must breach unsafe_miss_rate (>= {NEG_CONTROL_BREACH_MIN}); got {}",
                s.unsafe_miss_rate()
            );
        }

        let ps = score_perception(&phantom_control_corpus(&base));
        assert!(
            ps.over_conservative_rate() >= NEG_CONTROL_BREACH_MIN,
            "a phantom hazard must drive over_conservative_rate high; got {}",
            ps.over_conservative_rate()
        );
        assert_eq!(
            ps.unsafe_miss_rate(),
            0.0,
            "a phantom must NEVER be scored as an unsafe miss (availability cost, not a breach)"
        );
    }

    /// #777 F1 — the negative-control rows are PART OF THE GATE (not an off-to-the-
    /// side unit test): the gate report carries a breach-asserting row per fault
    /// family, and they pass on the current tree.
    #[test]
    fn gate_carries_passing_negative_control_rows() {
        let report = run_gate(&committed_thresholds());
        for name in [
            "negctl_dropout_unsafe_miss",
            "negctl_range_bias_far_unsafe_miss",
            "negctl_class_confusion_unsafe_miss",
            "negctl_lateral_shrink_unsafe_miss",
            "negctl_phantom_over_conservative",
            "negctl_phantom_no_unsafe_miss",
        ] {
            let row = report
                .rows
                .iter()
                .find(|r| r.name == name)
                .unwrap_or_else(|| panic!("gate must carry the {name} row: {report:#?}"));
            assert!(
                row.pass,
                "negative-control row {name} must pass (oracle discriminates the fault): {row:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // WP-23 — Monte-Carlo campaign gate
    // -----------------------------------------------------------------------

    fn committed_mc_policy() -> montecarlo::MonteCarloPolicy {
        let p = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../ci/scenario_kpi_montecarlo.json"
        );
        serde_json::from_str(&std::fs::read_to_string(p).expect("committed MC policy exists"))
            .expect("MC policy parses")
    }

    /// THE WP-23 DoD (green half): the Monte-Carlo campaign PASSES against the
    /// committed CI-bound policy at the per-PR size — the reviewed floors are
    /// honest for the current tree.
    #[test]
    fn montecarlo_gate_passes_against_committed_policy() {
        let report = run_montecarlo_gate(&committed_mc_policy(), montecarlo::Profile::PerPr);
        assert!(
            report.passed(),
            "committed MC floors must hold at the per-PR sample size: {:#?}",
            report.rows.iter().filter(|r| !r.pass).collect::<Vec<_>>()
        );
        // Every KPI + every negative-control row is present (no silent shrink).
        assert_eq!(
            report.rows.len(),
            4 + 4 + 2,
            "all KPI + negctl rows present"
        );
    }

    /// THE WP-23 DoD (red half): a floor set past the measured interval reds the
    /// campaign, and the breach is attributed to the right row. The wiring from
    /// a confidence bound to a verdict is what is under test.
    #[test]
    fn montecarlo_gate_goes_red_on_an_unreachable_floor() {
        let mut policy = committed_mc_policy();
        policy.unsafe_miss_rate_hi_max = -0.001; // unreachable: an upper bound < 0
        let report = run_montecarlo_gate(&policy, montecarlo::Profile::PerPr);
        assert!(
            !report.passed(),
            "an impossible CI bound must red the campaign"
        );
        assert!(
            report
                .rows
                .iter()
                .any(|r| r.name == "unsafe_miss_rate_seam_pinned" && !r.pass),
            "the breach must attribute to the unsafe_miss row"
        );
    }

    /// The campaign is deterministic by seed: the same policy yields byte-identical
    /// verdicts (a red run is a real regression, never flake).
    #[test]
    fn montecarlo_gate_is_deterministic() {
        let policy = committed_mc_policy();
        let a = run_montecarlo_gate(&policy, montecarlo::Profile::PerPr);
        let b = run_montecarlo_gate(&policy, montecarlo::Profile::PerPr);
        let fingerprint = |r: &montecarlo::McGateReport| -> Vec<(&'static str, bool, f64, f64)> {
            r.rows
                .iter()
                .map(|x| (x.name, x.pass, x.wilson.lo, x.wilson.hi))
                .collect()
        };
        assert_eq!(fingerprint(&a), fingerprint(&b));
    }

    /// The negative-control rows survive resampling: an injected detector fault
    /// still breaches the safety metric's CI lower bound over the SAMPLED hazard
    /// frames (the #777 F1 oracle-discrimination evidence is not a fixed-corpus
    /// artifact).
    #[test]
    fn montecarlo_negative_controls_still_breach_under_sampling() {
        let report = run_montecarlo_gate(&committed_mc_policy(), montecarlo::Profile::PerPr);
        for name in [
            "negctl_dropout_unsafe_miss",
            "negctl_range_bias_far_unsafe_miss",
            "negctl_class_confusion_unsafe_miss",
            "negctl_lateral_shrink_unsafe_miss",
            "negctl_phantom_over_conservative",
            "negctl_phantom_no_unsafe_miss",
        ] {
            let row = report
                .rows
                .iter()
                .find(|r| r.name == name)
                .unwrap_or_else(|| panic!("campaign must carry {name}"));
            assert!(
                row.pass,
                "negative-control row {name} must pass under sampling: {row:?}"
            );
        }
    }

    /// An empty corpus fails closed through the underlying tally (0.0 ≠ 1.0).
    #[test]
    fn empty_corpus_scores_zero_admissibility() {
        let (rate, tally) = admissibility_over(&[], |_| unreachable!("no scenarios"));
        assert_eq!(rate, 0.0);
        assert_eq!(tally.total(), 0);
    }
}
