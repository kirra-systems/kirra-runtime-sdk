//! # Doer performance eval harness (Q-1a)
//!
//! The **metric producers** for the doer quantization plan
//! (`parko/QUANTIZATION_Q1_SCOPE.md`). Q-0 landed the parko-side measuring stick
//! (`parko_core::perf_contract`) with `quality` and `admissibility` as *inputs*;
//! this crate turns those two inputs into **real producers**, computed the only
//! place they can be — where a doer's proposal meets the checker.
//!
//! Two scalars, over a scenario corpus:
//!
//! - **Admissibility** — the fraction of a planner's proposals the KIRRA checker
//!   admits *without an MRC*. A proposal is run through
//!   [`kirra_trajectory::validate_trajectory_slow`]; a verdict of `Accept` or
//!   `Clamp` is "admitted" (a `Clamp` is a per-pose speed derate, not a
//!   maximal-risk-condition maneuver — the proposal was still admitted), while
//!   `MRCFallback` / `Pending` are refusals. The harness also reports the stricter
//!   `Accept`-only rate.
//! - **Plan quality** — **argmax-agreement rate** vs. a reference planner: does the
//!   candidate pick the same trajectory-vocabulary entry the reference does? The
//!   doer output is a *ranking*, so the safety-relevant question is whether a
//!   perturbation (a different teacher today; a quantized net in Q-1a step 3)
//!   changes the argmax. **Mean progress ratio** is the secondary signal — how far
//!   toward the goal the chosen plan reaches — to catch an argmax that shifts to a
//!   still-admissible but lower-progress entry.
//!
//! ## Safety framing
//!
//! This crate measures the *untrusted doer*; it has **no safety authority**. It
//! calls the checker read-only to score proposals — it never relaxes it. A worse
//! score means a slower or more-conservative (still checker-bounded) plan, never
//! unsafe actuation. The checker remains the sole fail-closed authority.
//!
//! ## Q-1a scope
//!
//! The producers + the scenario harness are real and tested here. Step 2 (this
//! crate) exercises them with the two existing learned planners as the
//! reference/candidate pair (`SafetyAware` vs `ProgressOnly`) — a genuine,
//! deterministic disagreement that proves the metrics *detect* misalignment. Step 3
//! feeds a real in-Rust-quantized planner as the candidate into the same
//! [`evaluate_corpus`] to produce the FP32-vs-int8 deltas, and emits the
//! cross-workspace scorecard.

// Q-1b: minimal ONNX export of the learned scorer (FP32 + int8-QDQ artifacts).
pub mod onnx;

use kirra_core::corridor::{CorridorSource, MockCorridorSource, Point};
use kirra_core::trajectory::{PerceivedObject, TrajectoryVerdict};
use kirra_core::FleetPosture;
use kirra_planner::{
    EgoState, Goal, LearnedPlanner, LearnedPlannerV2, PlanInput, PlanOutput, Pose,
    QuantizedLearnedPlanner, QuantizedLearnedPlannerV2, ScoredPlanner,
};
use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Verdict reductions — the admissibility boolean(s)
// ---------------------------------------------------------------------------

/// "Admitted without an MRC" = `Accept | Clamp`. A `Clamp` is a per-pose speed
/// derate, not an MRC — the proposal was still admitted (matches the existing
/// `learned_doer_bounded_by_kirra.rs` seam and `MickEvalSummary`).
#[must_use]
pub fn admitted(v: TrajectoryVerdict) -> bool {
    matches!(v, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp)
}

/// The stricter "admitted with NO derate" = `Accept` only.
#[must_use]
pub fn accepted_clean(v: TrajectoryVerdict) -> bool {
    matches!(v, TrajectoryVerdict::Accept)
}

/// Run a proposal through the KIRRA checker and return its verdict. A thin,
/// reusable generalization of the test-only `kirra_verdict` seam. `latest_odom` is
/// `None` (the harness scores a standalone proposal, not a fast-loop conformance
/// check).
#[must_use]
pub fn verdict_of(
    plan: &PlanOutput,
    corridor: &dyn CorridorSource,
    objects: &[PerceivedObject],
    config: &VehicleConfig,
    posture: FleetPosture,
) -> TrajectoryVerdict {
    validate_trajectory_slow(&plan.trajectory, corridor, objects, config, None, posture)
}

// ---------------------------------------------------------------------------
// Admissibility tally (planner-agnostic — you feed it verdicts)
// ---------------------------------------------------------------------------

/// Counts of each verdict over a run — the admissibility producer's accumulator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdmissibilityTally {
    pub accept: usize,
    pub clamp: usize,
    pub mrc: usize,
    pub pending: usize,
}

impl AdmissibilityTally {
    /// Fold one verdict in.
    pub fn record(&mut self, v: TrajectoryVerdict) {
        match v {
            TrajectoryVerdict::Accept => self.accept += 1,
            TrajectoryVerdict::Clamp => self.clamp += 1,
            TrajectoryVerdict::MRCFallback => self.mrc += 1,
            TrajectoryVerdict::Pending => self.pending += 1,
        }
    }

    #[must_use]
    pub fn total(&self) -> usize {
        self.accept + self.clamp + self.mrc + self.pending
    }

    /// Fraction admitted **without an MRC** (`Accept | Clamp`). An empty tally is
    /// `0.0` — fail-closed: no evidence is not the same as "all admissible".
    #[must_use]
    pub fn admissibility_rate(&self) -> f64 {
        rate(self.accept + self.clamp, self.total())
    }

    /// Fraction admitted with **no derate** (`Accept` only). Empty ⇒ `0.0`.
    #[must_use]
    pub fn strict_accept_rate(&self) -> f64 {
        rate(self.accept, self.total())
    }
}

// ---------------------------------------------------------------------------
// Quality tally (argmax-agreement + progress)
// ---------------------------------------------------------------------------

/// The plan-quality producer's accumulator: argmax agreements vs. the reference,
/// plus a running progress sum.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct QualityTally {
    pub agreements: usize,
    pub scenarios: usize,
    pub progress_sum: f64,
}

impl QualityTally {
    /// Fold one scenario in: whether the candidate's argmax matched the reference's,
    /// and the candidate plan's progress ratio (`0..=1`).
    pub fn record(&mut self, argmax_agrees: bool, progress_ratio: f64) {
        self.scenarios += 1;
        if argmax_agrees {
            self.agreements += 1;
        }
        self.progress_sum += progress_ratio;
    }

    /// Headline quality: fraction of scenarios where the candidate chose the same
    /// vocabulary entry as the reference. Empty ⇒ `0.0`.
    #[must_use]
    pub fn argmax_agreement_rate(&self) -> f64 {
        rate(self.agreements, self.scenarios)
    }

    /// Secondary quality: mean progress ratio of the candidate's plans. Empty ⇒ `0.0`.
    #[must_use]
    pub fn mean_progress(&self) -> f64 {
        if self.scenarios == 0 {
            0.0
        } else {
            self.progress_sum / self.scenarios as f64
        }
    }
}

/// The result of scoring a candidate planner over a corpus, against a reference.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DoerEvalSummary {
    /// Admissibility of the **candidate**'s proposals.
    pub admissibility: AdmissibilityTally,
    /// Plan quality of the **candidate** vs. the **reference**.
    pub quality: QualityTally,
}

// ---------------------------------------------------------------------------
// The scenario + the corpus runner
// ---------------------------------------------------------------------------

/// One evaluation scenario: it OWNS the world the doer plans in and the checker
/// checks against (the corridor + objects), plus the ego/goal and the checker
/// config/posture. Owning them lets [`Self::input`] hand out a borrowed
/// [`PlanInput`] and the same corridor/objects to the checker within one call —
/// the self-referential-borrow shape the planner tests build by hand.
pub struct EvalScenario {
    /// Scenario label. `String` so parameterized generators (WS-3.1) can mint
    /// names like `hazard_x24_v4.0_g60`; the hand-written corpus uses literals.
    pub name: String,
    corridor: MockCorridorSource,
    objects: Vec<PerceivedObject>,
    ego: EgoState,
    goal: Goal,
    config: VehicleConfig,
    posture: FleetPosture,
}

impl EvalScenario {
    /// A `Nominal`-posture, urban-config scenario. `ego_x` / `goal_x` are along the
    /// demo road frame (+x); `objects` are the perceived hazards the doer plans
    /// around and the checker runs RSS against.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        corridor: MockCorridorSource,
        objects: Vec<PerceivedObject>,
        ego_x: f64,
        ego_speed_mps: f64,
        goal_x: f64,
    ) -> Self {
        Self {
            name: name.into(),
            corridor,
            objects,
            ego: EgoState {
                pose: Pose { x_m: ego_x, y_m: 0.0, heading_rad: 0.0 },
                linear_x_mps: ego_speed_mps,
                yaw_rate_rads: 0.0,
                stamp_ms: 0,
            },
            goal: Goal { target: Pose { x_m: goal_x, y_m: 0.0, heading_rad: 0.0 } },
            config: VehicleConfig::default_urban(),
            posture: FleetPosture::Nominal,
        }
    }

    /// Override the fleet posture (default `Nominal`).
    #[must_use]
    pub fn with_posture(mut self, posture: FleetPosture) -> Self {
        self.posture = posture;
        self
    }

    // Read-only world accessors (WS-3.1): the KPI gate scores planners other
    // than the `ScoredPlanner` pair `evaluate_corpus` takes, so it needs the
    // same world handles `verdict_of` wants. Borrowed, never mutable — a
    // scenario stays immutable once built.
    #[must_use]
    pub fn corridor(&self) -> &MockCorridorSource {
        &self.corridor
    }
    #[must_use]
    pub fn objects(&self) -> &[PerceivedObject] {
        &self.objects
    }
    #[must_use]
    pub fn config(&self) -> &VehicleConfig {
        &self.config
    }
    #[must_use]
    pub fn posture(&self) -> FleetPosture {
        self.posture
    }

    /// Build the borrowed [`PlanInput`] the planner sees. Every optional/behavioral
    /// field is empty/`None` (a bare motion-planning world) — the harness measures
    /// the base doer↔checker loop, not the behavioral layer. Public so a caller can
    /// build a calibration corpus (`&[PlanInput]`) for [`quantize_over_corpus`].
    #[must_use]
    pub fn plan_input(&self) -> PlanInput<'_> {
        PlanInput {
            ego: self.ego,
            goal: self.goal,
            map: &self.corridor,
            objects: &self.objects,
            controls: &[],
            lane_boundaries: &[],
            motion: &[],
            predicted_paths: &[],
            cedes_to_ego_ids: &[],
            lane_change_to_m: None,
            no_overtake_ids: &[],
            drivable: None,
            posture: self.posture,
            target_speed_mps: None,
            request_overtake: false,
            request_pull_over: false,
            lane_graph: None,
            signal_states: &[],
        }
    }
}

/// Score `candidate` over `corpus`, using `reference` as the argmax-agreement
/// oracle. For each scenario the candidate's proposal is run through the checker
/// (admissibility) and its chosen vocabulary index is compared to the reference's
/// (quality). Both are `&dyn ScoredPlanner` so an FP32 [`LearnedPlanner`] and its
/// int8 [`QuantizedLearnedPlanner`] compare through one interface — the FP32-vs-int8
/// row Q-1a step 3 produces. Exactly two scorer passes per scenario (candidate +
/// reference); the per-scenario cost is dominated by the checker pass regardless.
///
/// VOCABULARY CAVEAT: argmax agreement compares vocabulary INDICES, so candidate
/// and reference must share a vocabulary (fp32-vs-quantized of the SAME model,
/// v2-vs-v2, …). Comparing planners with different vocabularies (v1's 4 entries
/// vs v2's grid) makes the quality axis meaningless; admissibility is still valid.
#[must_use]
pub fn evaluate_corpus(
    corpus: &[EvalScenario],
    candidate: &dyn ScoredPlanner,
    reference: &dyn ScoredPlanner,
) -> DoerEvalSummary {
    evaluate_scenarios(corpus.iter(), candidate, reference)
}

/// The ref-taking core of [`evaluate_corpus`]: score `candidate` (argmax-compared
/// to `reference`) over any iterator of scenarios. Reused by the held-out
/// generalization producer, where the calibration and held-out partitions are
/// NON-contiguous subsets (`&EvalScenario` refs, not a slice), so a slice-taking
/// signature would not fit.
fn evaluate_scenarios<'a>(
    scenarios: impl IntoIterator<Item = &'a EvalScenario>,
    candidate: &dyn ScoredPlanner,
    reference: &dyn ScoredPlanner,
) -> DoerEvalSummary {
    let mut admissibility = AdmissibilityTally::default();
    let mut quality = QualityTally::default();

    for sc in scenarios {
        let input = sc.plan_input();
        // One scorer pass gives BOTH the candidate's plan and its argmax.
        let (cand_choice, plan) = candidate.plan_with_chosen_index(&input);
        let ref_choice = reference.chosen_index(&input);

        let v = verdict_of(&plan, &sc.corridor, &sc.objects, &sc.config, sc.posture);
        admissibility.record(v);

        let pr = progress_ratio(&plan, sc.ego.pose.x_m, sc.goal.target.x_m);
        quality.record(cand_choice == ref_choice, pr);
    }

    DoerEvalSummary { admissibility, quality }
}

/// Int8-quantize `planner` (PTQ), calibrating over `corpus` — a convenience wrapper
/// that builds the `&[PlanInput]` calibration set from the scenarios' worlds and
/// calls [`kirra_planner::LearnedPlanner::quantize_int8`].
///
/// This calibrates over the WHOLE corpus by design: the *shipped* artifact should
/// use every scenario available (more calibration data is strictly better for the
/// deployed model). The train==test hazard that used to live here — reporting a
/// quality number computed on the same scenarios the calibration saw — is now
/// guarded separately by [`generalization_report`], which measures argmax-agreement
/// on a HELD-OUT partition the calibration never touched (Q1 scope §5).
#[must_use]
pub fn quantize_over_corpus(
    planner: &LearnedPlanner,
    corpus: &[EvalScenario],
) -> QuantizedLearnedPlanner {
    let scenarios: Vec<&EvalScenario> = corpus.iter().collect();
    quantize_over_refs(planner, &scenarios)
}

/// Fit the int8 PTQ scales over an arbitrary set of scenarios (by reference, so a
/// NON-contiguous subset works — e.g. the calibration partition of a
/// [`split_corpus`]). The shared calibration core: [`quantize_over_corpus`] passes
/// the whole corpus, [`generalization_report`] passes only the calibration partition.
#[must_use]
fn quantize_over_refs(
    planner: &LearnedPlanner,
    scenarios: &[&EvalScenario],
) -> QuantizedLearnedPlanner {
    let inputs: Vec<PlanInput<'_>> = scenarios.iter().map(|s| s.plan_input()).collect();
    planner.quantize_int8(&inputs)
}

/// The v2 sibling of [`quantize_over_corpus`] (M-2): int8-quantize the N-layer
/// v2 planner, calibrating over the whole corpus (same shipped-artifact rationale;
/// the held-out overfit guard is [`generalization_report`], v1 today — Q1 scope §5).
#[must_use]
pub fn quantize_v2_over_corpus(
    planner: &LearnedPlannerV2,
    corpus: &[EvalScenario],
) -> QuantizedLearnedPlannerV2 {
    let inputs: Vec<PlanInput<'_>> = corpus.iter().map(EvalScenario::plan_input).collect();
    planner.quantize_int8(&inputs)
}

// ---------------------------------------------------------------------------
// Held-out calibration/eval split (Q1 scope §5 — no train==test masquerade)
// ---------------------------------------------------------------------------

/// A deterministic partition of a corpus into a CALIBRATION set (where the PTQ
/// scales are fit) and a disjoint HELD-OUT set (where quality is measured). Closes
/// the Q1 scope §5 caveat: a quantization quality number computed on the same
/// scenarios the calibration saw is a train==test number and must never be read as
/// a release gate. Holds `&EvalScenario` (the scenario owns a non-`Clone` world),
/// so the split is by reference, never a copy.
pub struct CorpusSplit<'a> {
    /// The scenarios the PTQ calibration is fit over.
    pub calibration: Vec<&'a EvalScenario>,
    /// The disjoint scenarios quality is measured over — never seen by calibration.
    pub holdout: Vec<&'a EvalScenario>,
}

/// Partition `corpus` so every `holdout_every_n`-th scenario (the last of each
/// block) lands in the held-out set and the rest form the calibration set. By
/// INDEX, so the split is reproducible and order-preserving — no RNG, matching the
/// crate's determinism ethos. Disjoint and covering by construction.
///
/// # Panics
/// Panics if `holdout_every_n < 2`: `n == 1` would hold out every scenario, leaving
/// no calibration data (a fail-loud misuse, not a silent empty calibration).
#[must_use]
pub fn split_corpus(corpus: &[EvalScenario], holdout_every_n: usize) -> CorpusSplit<'_> {
    assert!(
        holdout_every_n >= 2,
        "holdout_every_n must be >= 2 (n=1 holds out everything, leaving no calibration data)"
    );
    let mut calibration = Vec::new();
    let mut holdout = Vec::new();
    for (i, sc) in corpus.iter().enumerate() {
        if i % holdout_every_n == holdout_every_n - 1 {
            holdout.push(sc);
        } else {
            calibration.push(sc);
        }
    }
    CorpusSplit { calibration, holdout }
}

/// The held-out generalization measurement (Q1 scope §5). Calibrates the int8 PTQ
/// on the `split.calibration` partition ONLY, then scores argmax-agreement +
/// admissibility (vs the FP32 reference) on the calibration partition AND the
/// disjoint held-out partition. The held-out numbers are the ones a release gate
/// may trust; the calibration-vs-held-out gap surfaces overfit to the calibration
/// distribution (the SOTIF corpus-variance risk made measurable).
#[derive(Debug, Clone, PartialEq)]
pub struct GeneralizationReport {
    pub calibration_scenarios: usize,
    pub holdout_scenarios: usize,
    /// Int8 argmax-agreement with FP32, measured ON the calibration partition.
    pub calib_argmax_agreement: f64,
    /// Int8 argmax-agreement with FP32, measured ON the disjoint held-out partition.
    pub holdout_argmax_agreement: f64,
    pub calib_admissibility: f64,
    pub holdout_admissibility: f64,
}

impl GeneralizationReport {
    /// Calibration-minus-held-out argmax agreement. Positive ⇒ the int8 model
    /// agrees with FP32 *less* on unseen scenarios than on calibrated ones — the
    /// overfit signal. Near-zero ⇒ the quantization generalizes.
    #[must_use]
    pub fn agreement_gap(&self) -> f64 {
        self.calib_argmax_agreement - self.holdout_argmax_agreement
    }
}

/// Produce the [`GeneralizationReport`] for `fp32` under `split`: quantize on the
/// calibration partition, evaluate the resulting int8 planner on both partitions.
#[must_use]
pub fn generalization_report(fp32: &LearnedPlanner, split: &CorpusSplit<'_>) -> GeneralizationReport {
    let int8 = quantize_over_refs(fp32, &split.calibration);
    let calib = evaluate_scenarios(split.calibration.iter().copied(), &int8, fp32);
    let holdout = evaluate_scenarios(split.holdout.iter().copied(), &int8, fp32);
    GeneralizationReport {
        calibration_scenarios: split.calibration.len(),
        holdout_scenarios: split.holdout.len(),
        calib_argmax_agreement: calib.quality.argmax_agreement_rate(),
        holdout_argmax_agreement: holdout.quality.argmax_agreement_rate(),
        calib_admissibility: calib.admissibility.admissibility_rate(),
        holdout_admissibility: holdout.admissibility.admissibility_rate(),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Progress ratio: fraction of the ego→goal forward distance the plan's furthest
/// pose reaches, clamped to `0..=1`. A goal at or behind the ego (no forward
/// distance) yields `0.0` (degenerate — no progress to measure).
fn progress_ratio(plan: &PlanOutput, ego_x: f64, goal_x: f64) -> f64 {
    let span = goal_x - ego_x;
    if span <= f64::EPSILON {
        return 0.0;
    }
    let reach = plan.trajectory.iter().map(|t| t.pose.x_m).fold(f64::MIN, f64::max);
    ((reach - ego_x) / span).clamp(0.0, 1.0)
}

fn rate(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        n as f64 / d as f64
    }
}

/// A stopped car on the centerline at `x_m` — the canonical demo hazard.
#[must_use]
pub fn stopped_car(id: u64, x_m: f64) -> PerceivedObject {
    PerceivedObject {
        id,
        pos: Point { x_m, y_m: 0.0 },
        velocity_mps: 0.0,
        heading_rad: 0.0,
        vel: Point { x_m: 0.0, y_m: 0.0 },
    }
}

/// A small deterministic corpus reusing the planner tests' world shape: a clear
/// road plus stopped-car hazards at a spread of distances. Enough to exercise both
/// the agree-and-admit case (clear road / distant hazard) and the
/// misalignment-detecting case (near hazard the progress-only net barrels into).
#[must_use]
pub fn demo_corpus() -> Vec<EvalScenario> {
    let road = || MockCorridorSource::straight_5m_half_width(100.0);
    vec![
        EvalScenario::new("clear_road", road(), vec![], 5.0, 2.0, 40.0),
        EvalScenario::new("hazard_far", road(), vec![stopped_car(1, 45.0)], 5.0, 2.0, 40.0),
        EvalScenario::new("hazard_mid", road(), vec![stopped_car(1, 25.0)], 5.0, 2.0, 40.0),
        EvalScenario::new("hazard_near", road(), vec![stopped_car(1, 18.0)], 5.0, 2.0, 40.0),
    ]
}

// ---------------------------------------------------------------------------
// Cross-workspace scorecard (Q1 scope §4, seam A)
// ---------------------------------------------------------------------------

/// One precision's row: the doer-eval scalars for a labelled `(model, precision)`
/// run, ready for the parko-side Q-1b runner to join with the on-target latency row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScorecardRow {
    /// e.g. `"fp32"` (reference) or `"int8-ptq"`.
    pub label: String,
    pub admissibility_rate: f64,
    pub strict_accept_rate: f64,
    pub argmax_agreement_rate: f64,
    pub mean_progress: f64,
    pub scenarios: usize,
    pub mrc: usize,
}

impl ScorecardRow {
    /// Build a row from a summary.
    #[must_use]
    pub fn from_summary(label: impl Into<String>, s: &DoerEvalSummary) -> Self {
        Self {
            label: label.into(),
            admissibility_rate: s.admissibility.admissibility_rate(),
            strict_accept_rate: s.admissibility.strict_accept_rate(),
            argmax_agreement_rate: s.quality.argmax_agreement_rate(),
            mean_progress: s.quality.mean_progress(),
            scenarios: s.quality.scenarios,
            mrc: s.admissibility.mrc,
        }
    }
}

/// The offline scorecard the root workspace emits and the parko-side Q-1b runner
/// reads (Q1 scope §4, option A: a file seam keeps the two workspaces decoupled).
/// Versioned so the wire format can evolve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scorecard {
    pub schema_version: u32,
    pub rows: Vec<ScorecardRow>,
}

impl Scorecard {
    /// Current scorecard wire-format version.
    pub const SCHEMA_VERSION: u32 = 1;

    /// A scorecard stamped with the current [`Self::SCHEMA_VERSION`].
    #[must_use]
    pub fn new(rows: Vec<ScorecardRow>) -> Self {
        Self { schema_version: Self::SCHEMA_VERSION, rows }
    }

    /// Serialize to pretty JSON — the file the cross-workspace seam carries.
    #[must_use]
    pub fn to_json(&self) -> String {
        // Infallible for this type: `to_string_pretty` writes into an in-memory
        // buffer (no I/O to fail), and the derived `Serialize` over `String` /
        // number / `Vec` fields is total — serde_json renders a non-finite `f64`
        // as JSON `null` rather than erroring (and the scorecard's rates are finite
        // by construction anyway). `expect` documents that invariant.
        serde_json::to_string_pretty(self).expect("scorecard serialization is infallible")
    }

    /// Parse a scorecard from JSON.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_planner::Teacher;

    const SEED: u64 = 0xC0FFEE;

    #[test]
    fn admissibility_tally_arithmetic() {
        let mut t = AdmissibilityTally::default();
        // empty ⇒ fail-closed 0.0, not a divide-by-zero.
        assert_eq!(t.total(), 0);
        assert_eq!(t.admissibility_rate(), 0.0);
        assert_eq!(t.strict_accept_rate(), 0.0);

        t.record(TrajectoryVerdict::Accept);
        t.record(TrajectoryVerdict::Clamp);
        t.record(TrajectoryVerdict::MRCFallback);
        t.record(TrajectoryVerdict::Pending);
        assert_eq!(t.total(), 4);
        // Accept|Clamp = 2/4 admitted-without-MRC; Accept-only = 1/4.
        assert_eq!(t.admissibility_rate(), 0.5);
        assert_eq!(t.strict_accept_rate(), 0.25);
    }

    #[test]
    fn clamp_is_admitted_but_not_clean() {
        assert!(admitted(TrajectoryVerdict::Accept));
        assert!(admitted(TrajectoryVerdict::Clamp));
        assert!(!admitted(TrajectoryVerdict::MRCFallback));
        assert!(!admitted(TrajectoryVerdict::Pending));

        assert!(accepted_clean(TrajectoryVerdict::Accept));
        assert!(!accepted_clean(TrajectoryVerdict::Clamp)); // a derate is not clean
        assert!(!accepted_clean(TrajectoryVerdict::MRCFallback));
    }

    #[test]
    fn quality_tally_arithmetic() {
        let mut q = QualityTally::default();
        assert_eq!(q.argmax_agreement_rate(), 0.0); // empty ⇒ 0.0
        assert_eq!(q.mean_progress(), 0.0);

        q.record(true, 1.0);
        q.record(false, 0.0);
        q.record(true, 0.5);
        assert_eq!(q.scenarios, 3);
        assert_eq!(q.argmax_agreement_rate(), 2.0 / 3.0);
        assert!((q.mean_progress() - 0.5).abs() < 1e-9);
    }

    /// A planner scored against ITSELF: it always picks the same argmax (agreement
    /// = 1.0), and a safety-aware net is admitted on every demo scenario.
    #[test]
    fn same_planner_agrees_and_is_admitted() {
        let corpus = demo_corpus();
        let candidate = LearnedPlanner::trained(SEED, Teacher::SafetyAware);
        let reference = LearnedPlanner::trained(SEED, Teacher::SafetyAware);

        let s = evaluate_corpus(&corpus, &candidate, &reference);

        assert_eq!(
            s.quality.argmax_agreement_rate(),
            1.0,
            "a planner agrees with itself on every scenario"
        );
        assert_eq!(
            s.admissibility.admissibility_rate(),
            1.0,
            "the safety-aware net is admitted on every demo scenario: {:?}",
            s.admissibility
        );
    }

    /// The load-bearing test: the producers DETECT a misaligned candidate. A
    /// progress-only net (candidate) disagrees with the safety-aware reference on
    /// the hazard scenarios AND gets MRC'd by the checker there — so BOTH scalars
    /// drop below 1.0. This is what makes them a real quality gate rather than a
    /// number that always passes.
    #[test]
    fn misalignment_is_caught_by_both_metrics() {
        let corpus = demo_corpus();
        let candidate = LearnedPlanner::trained(SEED, Teacher::ProgressOnly);
        let reference = LearnedPlanner::trained(SEED, Teacher::SafetyAware);

        let s = evaluate_corpus(&corpus, &candidate, &reference);

        assert!(
            s.admissibility.admissibility_rate() < 1.0,
            "the progress-only net is MRC'd on at least one hazard: {:?}",
            s.admissibility
        );
        assert!(
            s.admissibility.mrc > 0,
            "at least one proposal is refused with an MRC: {:?}",
            s.admissibility
        );
        assert!(
            s.quality.argmax_agreement_rate() < 1.0,
            "the misaligned net picks a different vocabulary entry on ≥1 scenario"
        );
    }

    /// Progress ratio is bounded and directionally sane: a stopping plan makes less
    /// progress than a barreling one.
    #[test]
    fn progress_ratio_orders_stop_below_go() {
        let corpus = vec![EvalScenario::new(
            "hazard_mid",
            MockCorridorSource::straight_5m_half_width(100.0),
            vec![stopped_car(1, 25.0)],
            5.0,
            2.0,
            40.0,
        )];

        let safe = LearnedPlanner::trained(SEED, Teacher::SafetyAware);
        let prog = LearnedPlanner::trained(SEED, Teacher::ProgressOnly);
        let ref_safe = LearnedPlanner::trained(SEED, Teacher::SafetyAware);
        let ref_prog = LearnedPlanner::trained(SEED, Teacher::ProgressOnly);

        let safe_progress = evaluate_corpus(&corpus, &safe, &ref_safe).quality.mean_progress();
        let prog_progress = evaluate_corpus(&corpus, &prog, &ref_prog).quality.mean_progress();

        assert!((0.0..=1.0).contains(&safe_progress));
        assert!((0.0..=1.0).contains(&prog_progress));
        assert!(
            prog_progress > safe_progress,
            "the progress-only net reaches further (prog {prog_progress} > safe {safe_progress})"
        );
    }

    // --- Q-1a step 3: the in-Rust PTQ -------------------------------------

    /// The core step-3 claim: an int8-PTQ'd planner stays checker-admissible within
    /// a small budget of FP32 AND its argmax barely moves — quantization on a
    /// ranking MLP is a quality perturbation the checker already tolerates.
    #[test]
    fn int8_ptq_stays_admissible_and_agrees_with_fp32() {
        let corpus = demo_corpus();
        let fp32 = LearnedPlanner::trained(SEED, Teacher::SafetyAware);
        let int8 = quantize_over_corpus(&fp32, &corpus);

        let fp32_summary = evaluate_corpus(&corpus, &fp32, &fp32);
        let int8_summary = evaluate_corpus(&corpus, &int8, &fp32);

        // Admissibility must not regress beyond a small budget vs. FP32.
        const BUDGET: f64 = 0.05;
        let fp32_adm = fp32_summary.admissibility.admissibility_rate();
        let int8_adm = int8_summary.admissibility.admissibility_rate();
        assert!(
            int8_adm + BUDGET >= fp32_adm,
            "int8 admissibility {int8_adm} regressed >{BUDGET} below fp32 {fp32_adm}"
        );

        // A ranking MLP is quantization-robust: the int8 argmax matches FP32 on
        // (almost) every scenario.
        assert!(
            int8_summary.quality.argmax_agreement_rate() >= 0.75,
            "int8 argmax agreement too low: {}",
            int8_summary.quality.argmax_agreement_rate()
        );

        // Calibration produced real, finite, positive per-tensor scales.
        let (s_w1, s_w2, s_x, s_h) = int8.scales();
        for s in [s_w1, s_w2, s_x, s_h] {
            assert!(s.is_finite() && s > 0.0, "degenerate calibration scale {s}");
        }
    }

    /// Quantization is a pure function of `(planner, calibration set)` — same inputs,
    /// byte-identical artifact and eval. (Reproducibility is a scorecard-provenance
    /// requirement; see Q1 scope §5.)
    #[test]
    fn quantization_is_deterministic() {
        let corpus = demo_corpus();
        let fp32 = LearnedPlanner::trained(SEED, Teacher::SafetyAware);
        let a = quantize_over_corpus(&fp32, &corpus);
        let b = quantize_over_corpus(&fp32, &corpus);
        assert_eq!(a.scales(), b.scales());
        assert_eq!(
            evaluate_corpus(&corpus, &a, &fp32),
            evaluate_corpus(&corpus, &b, &fp32),
            "same planner + same calibration ⇒ identical eval"
        );
    }

    // ---- Held-out calibration/eval split (Q1 scope §5) ----

    /// A larger deterministic corpus (16 scenarios) spanning clear roads at a range
    /// of ego speeds/goals and hazards across a spread of distances — big enough to
    /// split into a calibration and a disjoint held-out partition meaningfully.
    fn varied_corpus() -> Vec<EvalScenario> {
        let road = || MockCorridorSource::straight_5m_half_width(100.0);
        let mut v = Vec::new();
        for (i, (spd, goal)) in [(2.0, 40.0), (3.0, 50.0), (4.0, 60.0), (1.5, 35.0)]
            .into_iter()
            .enumerate()
        {
            v.push(EvalScenario::new(format!("clear_{i}"), road(), vec![], 5.0, spd, goal));
        }
        for (i, dist) in
            [15.0, 18.0, 22.0, 25.0, 30.0, 35.0, 40.0, 45.0, 50.0, 55.0, 60.0, 28.0]
                .into_iter()
                .enumerate()
        {
            v.push(EvalScenario::new(
                format!("hazard_{i}"),
                road(),
                vec![stopped_car(1, dist)],
                5.0,
                2.0,
                65.0,
            ));
        }
        v
    }

    /// The split is disjoint, covering, and deterministic — the structural
    /// guarantees a held-out measurement rests on.
    #[test]
    fn split_is_disjoint_covering_and_deterministic() {
        let corpus = varied_corpus();
        let split = split_corpus(&corpus, 4);

        // Covering: every scenario lands in exactly one partition.
        assert_eq!(split.calibration.len() + split.holdout.len(), corpus.len());
        // Every 4th scenario is held out → 16/4 = 4.
        assert_eq!(split.holdout.len(), 4);
        assert_eq!(split.calibration.len(), 12);

        // Disjoint by name: no held-out scenario appears in calibration.
        let calib_names: std::collections::HashSet<&str> =
            split.calibration.iter().map(|s| s.name.as_str()).collect();
        for h in &split.holdout {
            assert!(
                !calib_names.contains(h.name.as_str()),
                "held-out scenario {} leaked into calibration",
                h.name
            );
        }

        // Deterministic: same partition on a re-split.
        let again = split_corpus(&corpus, 4);
        let names = |s: &CorpusSplit| -> Vec<String> {
            s.holdout.iter().map(|x| x.name.clone()).collect()
        };
        assert_eq!(names(&split), names(&again));
    }

    /// `split_corpus(_, 1)` would leave no calibration data — fail loud, not silent.
    #[test]
    #[should_panic(expected = "holdout_every_n must be >= 2")]
    fn split_rejects_holding_out_everything() {
        let _ = split_corpus(&varied_corpus(), 1);
    }

    /// The report measures the DISJOINT held-out partition, not the calibration set.
    /// Proven with a split whose two partitions have genuinely different
    /// admissibility: calibration = all clear road (admissibility 1.0), held-out =
    /// a near hazard the progress-only distillation is derated on. The report's
    /// held-out admissibility must equal an INDEPENDENT eval of the held-out set —
    /// which would fail if the producer accidentally re-scored the calibration set.
    #[test]
    fn generalization_report_scores_the_holdout_not_the_calibration_set() {
        let road = || MockCorridorSource::straight_5m_half_width(100.0);
        // 3 clear-road calibration scenarios + 1 near-hazard held-out (index 3 with
        // stride 4 → the 4th is held out).
        let corpus = vec![
            EvalScenario::new("clear_a", road(), vec![], 5.0, 2.0, 40.0),
            EvalScenario::new("clear_b", road(), vec![], 5.0, 3.0, 50.0),
            EvalScenario::new("clear_c", road(), vec![], 5.0, 2.5, 45.0),
            EvalScenario::new("hazard_near", road(), vec![stopped_car(1, 16.0)], 5.0, 2.0, 40.0),
        ];
        let split = split_corpus(&corpus, 4);
        assert_eq!(split.calibration.len(), 3);
        assert_eq!(split.holdout.len(), 1);
        assert_eq!(split.holdout[0].name, "hazard_near");

        let fp32 = LearnedPlanner::trained(SEED, Teacher::SafetyAware);
        let report = generalization_report(&fp32, &split);

        // Independently evaluate the same int8 (calibrated on the calibration
        // partition) over ONLY the held-out scenario, and confirm the report used it.
        let int8 = quantize_over_refs(&fp32, &split.calibration);
        let independent =
            evaluate_scenarios(split.holdout.iter().copied(), &int8, &fp32);
        assert_eq!(
            report.holdout_admissibility,
            independent.admissibility.admissibility_rate(),
            "the report must score the held-out partition, not re-score calibration"
        );
        assert_eq!(report.holdout_scenarios, 1);
        assert_eq!(report.calibration_scenarios, 3);
    }

    /// The GATE (Q1 scope §5): the int8 planner calibrated on the calibration
    /// partition keeps its argmax-agreement with FP32 at/above a documented floor
    /// ON THE HELD-OUT partition — a held-out number, never a train==test one. The
    /// generalization gap stays within budget (the quantization does not overfit the
    /// calibration distribution at this scale).
    #[test]
    fn heldout_argmax_agreement_meets_the_release_floor() {
        let corpus = varied_corpus();
        let split = split_corpus(&corpus, 4);
        let fp32 = LearnedPlanner::trained(SEED, Teacher::SafetyAware);

        let report = generalization_report(&fp32, &split);

        assert!(report.holdout_scenarios >= 1, "the held-out partition is non-empty");
        // Held-out floor: a genuinely unseen argmax-agreement, the release-trustable
        // number. Matches the whole-corpus int8 floor used elsewhere (0.75).
        assert!(
            report.holdout_argmax_agreement >= 0.75,
            "held-out int8 argmax agreement below floor: {} (calib {})",
            report.holdout_argmax_agreement,
            report.calib_argmax_agreement
        );
        // Overfit budget: calibration should not agree *much* more than held-out.
        assert!(
            report.agreement_gap() <= 0.25,
            "calibration-vs-held-out overfit gap too large: {} (calib {}, holdout {})",
            report.agreement_gap(),
            report.calib_argmax_agreement,
            report.holdout_argmax_agreement
        );
    }

    /// The cross-workspace seam: a scorecard serializes to versioned JSON and parses
    /// back byte-for-byte — the file the parko-side Q-1b runner consumes.
    #[test]
    fn scorecard_round_trips() {
        let corpus = demo_corpus();
        let fp32 = LearnedPlanner::trained(SEED, Teacher::SafetyAware);
        let int8 = quantize_over_corpus(&fp32, &corpus);

        let fp32_s = evaluate_corpus(&corpus, &fp32, &fp32);
        let int8_s = evaluate_corpus(&corpus, &int8, &fp32);

        let card = Scorecard::new(vec![
            ScorecardRow::from_summary("fp32", &fp32_s),
            ScorecardRow::from_summary("int8-ptq", &int8_s),
        ]);
        let json = card.to_json();
        assert!(json.contains("\"schema_version\""), "scorecard carries a version");
        let parsed = Scorecard::from_json(&json).expect("valid scorecard JSON");
        assert_eq!(card, parsed);
        assert_eq!(parsed.schema_version, Scorecard::SCHEMA_VERSION);
        assert_eq!(parsed.rows.len(), 2);
    }
}
