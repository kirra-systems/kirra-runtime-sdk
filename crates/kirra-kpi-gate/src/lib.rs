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

use kirra_core::corridor::{MockCorridorSource, Point};
use kirra_core::trajectory::PerceivedObject;
use kirra_doer_eval::{verdict_of, AdmissibilityTally, EvalScenario};
use kirra_planner::{GeometricPlanner, GeometricPlannerConfig, LearnedPlanner, Planner, Teacher};
use kirra_taj::{
    LaserScan, SemanticClass, SemanticDetection, SemanticDetector, MockSemanticDetector,
    SemanticEvalFrame, SemanticEvalSummary, TajConfig, TajCorridor, TajPhaseA,
};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Doer corpus — parameterized sweep (WS-3.1: "corpus to low hundreds")
// ---------------------------------------------------------------------------

/// Hazard kind axis for the doer sweep. All are checker-visible
/// `PerceivedObject`s; the kinds differ in longitudinal motion so the RSS
/// terms (stationary lead / slower lead / oncoming closer) are all exercised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HazardKind {
    /// Stationary object on the centerline (the stopped-queue case).
    Stopped,
    /// Lead vehicle moving away slowly (+1.0 m/s along +x).
    LeadMoving,
    /// Oncoming vehicle closing at 2.0 m/s (−x velocity).
    Oncoming,
}

fn hazard(kind: HazardKind, id: u64, x_m: f64) -> PerceivedObject {
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

/// The WS-3.1 doer corpus: a deterministic closed-form sweep over
/// hazard kind × hazard distance × ego speed × goal distance, plus a
/// clear-road row per (speed, goal). No RNG — the corpus is identical on
/// every run and every machine.
///
/// Size: 3 kinds × 10 distances × 4 speeds × 2 goals + 4×2 clear-road
/// = **248 scenarios** (pinned by test).
#[must_use]
pub fn generated_doer_corpus() -> Vec<EvalScenario> {
    let road = || MockCorridorSource::straight_5m_half_width(100.0);
    let speeds = [1.0, 2.0, 4.0, 6.0];
    let goals = [40.0, 60.0];
    let distances = [12.0, 16.0, 20.0, 24.0, 28.0, 32.0, 36.0, 40.0, 44.0, 48.0];
    let kinds = [HazardKind::Stopped, HazardKind::LeadMoving, HazardKind::Oncoming];

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
    corpus
}

/// Admissibility of a planner over the corpus: every proposal is run through
/// the REAL checker (`validate_trajectory_slow` via `verdict_of`); the rate
/// is `kirra_doer_eval::AdmissibilityTally::admissibility_rate` (fail-closed:
/// an empty corpus scores 0.0, not 1.0).
fn admissibility_over(corpus: &[EvalScenario], mut plan: impl FnMut(&EvalScenario) -> kirra_planner::PlanOutput) -> (f64, AdmissibilityTally) {
    let mut tally = AdmissibilityTally::default();
    for sc in corpus {
        let out = plan(sc);
        tally.record(verdict_of(&out, sc.corridor(), sc.objects(), sc.config(), sc.posture()));
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
fn open_corridor() -> TajCorridor {
    let taj = TajPhaseA::new(TajConfig { forward_extent_m: 20.0, ..Default::default() });
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
                let detector = MockSemanticDetector { detections: vec![det] };
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
            .map(|d| SemanticDetection { near_x_m: d.near_x_m + RANGE_BIAS_FAR_M, ..*d })
            .collect(),
        DetectorFault::ClassConfusion => truth
            .iter()
            .map(|d| SemanticDetection { class: SemanticClass::Road, ..*d })
            .collect(),
        DetectorFault::LateralShrink => truth
            .iter()
            // Push the lateral span far off the corridor centerline (the corridor
            // is lidar-bounded to ~±30 m near the ego) so it no longer overlaps.
            .map(|d| SemanticDetection { lateral_min_m: 500.0, lateral_max_m: 501.0, ..*d })
            .collect(),
    }
}

/// Build a negative-control corpus: the HAZARD frames of `base` (truth
/// non-empty), with `detected` re-derived from `truth` under `fault`. Scoring
/// this must breach `unsafe_miss_rate` (the oracle caught the fault).
#[must_use]
pub(crate) fn negative_control_corpus(base: &[PerceptionCase], fault: DetectorFault) -> Vec<PerceptionCase> {
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
    /// Min fraction of GEOMETRIC-planner proposals the checker admits
    /// without an MRC (`Accept | Clamp`) over the doer corpus.
    pub geometric_admissibility_min: f64,
    /// Min admissibility for the seeded SafetyAware learned planner.
    pub learned_admissibility_min: f64,
    /// Max fraction of perception frames where the detector's drivable
    /// extent runs PAST ground truth (the catastrophic direction).
    pub unsafe_miss_rate_max: f64,
    /// Min fraction of true binding hazards the detector catches.
    pub hazard_recall_min: f64,
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
        Self { name, measured, direction: ">=", bound, pass: measured >= bound }
    }
    fn at_most(name: &'static str, measured: f64, bound: f64) -> Self {
        Self { name, measured, direction: "<=", bound, pass: measured <= bound }
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
    let corpus = generated_doer_corpus();

    // Geometric doer (the shipped default proposer). `Planner::plan` takes
    // &mut self; a fresh planner per scenario keeps scenarios independent.
    let (geo_rate, _) = admissibility_over(&corpus, |sc| {
        GeometricPlanner::new(GeometricPlannerConfig::default()).plan(&sc.plan_input())
    });

    // Seeded SafetyAware learned doer (the ScoredPlanner path — same seam the
    // quantization scorecard uses).
    let learned = LearnedPlanner::trained(7, Teacher::SafetyAware);
    let (learned_rate, _) = admissibility_over(&corpus, |sc| {
        learned.plan_with_chosen_index(&sc.plan_input()).1
    });

    let cases = generated_perception_corpus();
    let perception = score_perception(&cases);

    let mut rows = vec![
        KpiRow::at_least("geometric_admissibility", geo_rate, t.geometric_admissibility_min),
        KpiRow::at_least("learned_admissibility", learned_rate, t.learned_admissibility_min),
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
    ];

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
        rows.push(KpiRow::at_least(name, s.unsafe_miss_rate(), NEG_CONTROL_BREACH_MIN));
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
    rows.push(KpiRow::at_most("negctl_phantom_no_unsafe_miss", ps.unsafe_miss_rate(), 0.0));

    GateReport {
        doer_scenarios: corpus.len(),
        perception_frames: cases.len(),
        rows,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Corpus sizes are pinned: a silent shrink would weaken the gate while
    /// it kept reporting green.
    #[test]
    fn corpus_sizes_are_pinned_at_low_hundreds() {
        assert_eq!(generated_doer_corpus().len(), 248);
        assert_eq!(generated_perception_corpus().len(), 71);
    }

    /// The generators are deterministic: two invocations produce identical
    /// scenario names in identical order (no RNG, no time).
    #[test]
    fn generators_are_deterministic() {
        let a: Vec<String> = generated_doer_corpus().into_iter().map(|s| s.name).collect();
        let b: Vec<String> = generated_doer_corpus().into_iter().map(|s| s.name).collect();
        assert_eq!(a, b);
        let pa: Vec<String> = generated_perception_corpus().into_iter().map(|c| c.name).collect();
        let pb: Vec<String> = generated_perception_corpus().into_iter().map(|c| c.name).collect();
        assert_eq!(pa, pb);
    }

    fn committed_thresholds() -> KpiThresholds {
        // The gate tests run from the crate dir; the binary defaults to the
        // repo-root path. Resolve relative to the manifest.
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../ci/scenario_kpi_thresholds.json");
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
            report.rows.iter().any(|r| r.name == "geometric_admissibility" && !r.pass),
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
        assert!(s.unsafe_miss_rate() > 0.5, "a blind detector must unsafe-miss most hazard frames");
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
            let row = report.rows.iter().find(|r| r.name == name)
                .unwrap_or_else(|| panic!("gate must carry the {name} row: {report:#?}"));
            assert!(row.pass, "negative-control row {name} must pass (oracle discriminates the fault): {row:?}");
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
