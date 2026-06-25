//! **Semantic-perception eval harness** — the perception analog of the Mick brain-eval
//! scorecard, scoring the ML detector path (Phase B) against labeled ground truth.
//!
//! # What it measures, and why it is safety-weighted
//!
//! A generic detector metric (mAP, IoU) answers "how good are the boxes". A *safety*
//! governor asks a sharper question: **does the detector ever let the vehicle drive past a
//! hazard the world actually contains?** Phase B exists because lidar is blind to water
//! (specular → no return), so a missed water region reads as free corridor and the vehicle
//! drives into a lake. That failure — a *missed* hazard — is categorically worse than a
//! *spurious* one (which merely stops the vehicle early). The scorecard separates them:
//!
//! - **Unsafe miss** — the detector's drivable extent runs PAST the ground-truth hazard (or
//!   misses it entirely). The catastrophic case; the headline metric is `unsafe_miss_rate`,
//!   which a fielded model must drive to ~0.
//! - **Over-conservative** — the detector clips the corridor SHORTER than truth requires
//!   (spurious / too-near hazard). An availability cost (needless slow/stop), never a safety
//!   breach. Tracked, but not the safety bar.
//! - **Correct** — extent matches truth within tolerance.
//!
//! # Scored at the FUSION, not the raw box
//!
//! The oracle is the very fusion KIRRA consumes ([`binding_hazard`] / `hazard_clip_x`): a
//! detection only matters if it is the *nearest non-drivable region laterally overlapping the
//! corridor* — exactly the box that ends the drivable space. A perfect box the fusion ignores
//! (laterally clear, or behind a nearer one) is correctly scored as not mattering; a missed
//! box only counts against the model when it would have clipped the corridor. So this measures
//! the model's effect on the safety envelope, not a proxy.
//!
//! # Model-free, like the rest of Phase B
//!
//! Ground truth and detector output are both `Vec<SemanticDetection>`, so the harness scores
//! the [`MockSemanticDetector`](crate::MockSemanticDetector) today and the real RGB→TensorRT
//! detector unchanged once it lands behind the same seam — the eval bar is in place before the
//! model is, the same discipline the Mick scorecard followed for the brain.

use std::collections::BTreeMap;

use crate::{binding_hazard, hazard_clip_x, SemanticClass, SemanticDetection, TajCorridor};

/// Default clip-distance tolerance (m): a detector extent within this of ground truth counts
/// as a match. Absorbs sub-meter localization/decode jitter without masking a real miss (a
/// missed hazard overshoots by metres — the whole corridor opens up).
pub const DEFAULT_CLIP_TOL_M: f64 = 0.5;

/// One labeled eval frame: a corridor, the GROUND-TRUTH hazards (the labels), and the hazards
/// the detector PRODUCED for the same frame. Both detection sets are scored through the same
/// fusion against `corridor`.
#[derive(Debug, Clone, Copy)]
pub struct SemanticEvalFrame<'a> {
    /// The Phase-A geometric corridor the hazards are fused against.
    pub corridor: &'a TajCorridor,
    /// Ground-truth (labeled) semantic hazards for this frame.
    pub truth: &'a [SemanticDetection],
    /// The hazards the detector under test produced for this frame.
    pub detected: &'a [SemanticDetection],
}

/// Per-frame safety verdict: the detector's drivable extent vs ground truth's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOutcome {
    /// Detector's drivable extent matches truth within tolerance.
    Correct,
    /// Detector lets the corridor extend PAST the true hazard (or misses it entirely) — the
    /// vehicle would drive into a hazard ground truth forbids. The catastrophic failure.
    UnsafeMiss,
    /// Detector clips the corridor SHORTER than truth requires (spurious / too-near hazard) —
    /// an availability cost, not a safety breach.
    OverConservative,
}

/// `Some(extent)` → the corridor ends there; `None` (unclipped) → effectively unbounded, so
/// the comparison treats it as `+∞` (a fully-missed true hazard becomes an infinite overshoot
/// → `UnsafeMiss`, exactly as intended).
fn extent(clip: Option<f64>) -> f64 {
    clip.unwrap_or(f64::INFINITY)
}

/// Score a single frame: compare the detector's drivable extent to ground truth's, both
/// derived from the shared fusion oracle.
#[must_use]
pub fn score_frame(frame: &SemanticEvalFrame, tol_m: f64) -> FrameOutcome {
    let truth_x = hazard_clip_x(frame.corridor, frame.truth);
    let det_x = hazard_clip_x(frame.corridor, frame.detected);
    let (t, d) = (extent(truth_x), extent(det_x));
    if d > t + tol_m {
        FrameOutcome::UnsafeMiss
    } else if d < t - tol_m {
        FrameOutcome::OverConservative
    } else {
        FrameOutcome::Correct
    }
}

/// Stable label for a [`SemanticClass`] (the `by_class` recall key).
fn class_label(c: SemanticClass) -> &'static str {
    match c {
        SemanticClass::Road => "road",
        SemanticClass::Water => "water",
        SemanticClass::StaticObstacle => "static_obstacle",
        SemanticClass::Unknown => "unknown",
    }
}

/// Recall tally for one ground-truth hazard class: how many binding hazards of this class the
/// detector caught (did not unsafe-miss).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClassRecall {
    /// Frames whose ground-truth binding hazard is this class.
    pub seen: usize,
    /// …that the detector caught (clipped at or before the true hazard).
    pub caught: usize,
}

impl ClassRecall {
    /// Caught fraction for this class (0.0 when unseen).
    #[must_use]
    pub fn recall(&self) -> f64 {
        ratio(self.caught, self.seen)
    }
}

/// **The perception scorecard** — aggregates scored [`SemanticEvalFrame`]s into safety-weighted
/// metrics: the headline `unsafe_miss_rate` (a hazard driven past), `hazard_recall` (true
/// binding hazards caught), the over-conservative (availability) rate, a per-class recall
/// breakdown (so a Water blind spot — the reason Phase B exists — is visible on its own), and a
/// mean clip-distance error among co-clipped frames.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SemanticEvalSummary {
    /// Frames scored.
    pub frames: usize,
    /// Detector extent matched truth within tolerance.
    pub correct: usize,
    /// Detector drove PAST the true hazard / missed it (safety-critical).
    pub unsafe_miss: usize,
    /// Detector clipped earlier than truth (availability cost).
    pub over_conservative: usize,
    /// Frames whose GROUND TRUTH has a binding hazard (the corridor SHOULD clip).
    pub frames_with_true_hazard: usize,
    /// …of those, the ones the detector caught (outcome ≠ `UnsafeMiss`).
    pub true_hazards_caught: usize,
    /// Per ground-truth binding-hazard class recall.
    pub by_class: BTreeMap<&'static str, ClassRecall>,
    /// Sum of |det_clip − truth_clip| over frames where BOTH clipped (finite extents).
    pub sum_abs_clip_err_m: f64,
    /// Frames where both truth and detector clipped (the `sum_abs_clip_err_m` denominator).
    pub co_clipped: usize,
}

impl SemanticEvalSummary {
    /// Fraction of frames the detector unsafe-missed — **the safety bar** (target ~0).
    #[must_use]
    pub fn unsafe_miss_rate(&self) -> f64 {
        ratio(self.unsafe_miss, self.frames)
    }
    /// Fraction clipped too short (availability cost).
    #[must_use]
    pub fn over_conservative_rate(&self) -> f64 {
        ratio(self.over_conservative, self.frames)
    }
    /// Fraction matching truth within tolerance.
    #[must_use]
    pub fn correct_rate(&self) -> f64 {
        ratio(self.correct, self.frames)
    }
    /// Of frames with a true binding hazard, the fraction the detector caught — the
    /// safety-critical recall (a miss here is a hazard driven into). 1.0 when there were no
    /// true hazards to catch (vacuously perfect recall).
    #[must_use]
    pub fn hazard_recall(&self) -> f64 {
        if self.frames_with_true_hazard == 0 {
            1.0
        } else {
            self.true_hazards_caught as f64 / self.frames_with_true_hazard as f64
        }
    }
    /// Mean |det_clip − truth_clip| over frames where both clipped (0.0 when none did).
    #[must_use]
    pub fn mean_clip_err_m(&self) -> f64 {
        if self.co_clipped == 0 {
            0.0
        } else {
            self.sum_abs_clip_err_m / self.co_clipped as f64
        }
    }

    fn tally(&mut self, frame: &SemanticEvalFrame, tol_m: f64) {
        let outcome = score_frame(frame, tol_m);
        self.frames += 1;
        match outcome {
            FrameOutcome::Correct => self.correct += 1,
            FrameOutcome::UnsafeMiss => self.unsafe_miss += 1,
            FrameOutcome::OverConservative => self.over_conservative += 1,
        }

        // Per-class recall is keyed by the GROUND-TRUTH binding hazard: a frame only tests
        // recall when truth says the corridor should clip.
        if let Some(truth_hz) = binding_hazard(frame.corridor, frame.truth) {
            self.frames_with_true_hazard += 1;
            let caught = outcome != FrameOutcome::UnsafeMiss;
            if caught {
                self.true_hazards_caught += 1;
            }
            let e = self.by_class.entry(class_label(truth_hz.class)).or_default();
            e.seen += 1;
            if caught {
                e.caught += 1;
            }
        }

        // Clip-distance error only where both actually clipped (finite extents).
        if let (Some(t), Some(d)) = (
            hazard_clip_x(frame.corridor, frame.truth),
            hazard_clip_x(frame.corridor, frame.detected),
        ) {
            self.sum_abs_clip_err_m += (d - t).abs();
            self.co_clipped += 1;
        }
    }

    /// Score a stream of frames at [`DEFAULT_CLIP_TOL_M`].
    #[must_use]
    pub fn from_frames<'a>(frames: impl IntoIterator<Item = SemanticEvalFrame<'a>>) -> Self {
        Self::from_frames_with_tol(frames, DEFAULT_CLIP_TOL_M)
    }

    /// Score a stream of frames at an explicit clip tolerance.
    #[must_use]
    pub fn from_frames_with_tol<'a>(
        frames: impl IntoIterator<Item = SemanticEvalFrame<'a>>,
        tol_m: f64,
    ) -> Self {
        let mut s = Self::default();
        for f in frames {
            s.tally(&f, tol_m);
        }
        s
    }
}

impl std::fmt::Display for SemanticEvalSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Semantic perception eval — {} frames", self.frames)?;
        writeln!(
            f,
            "  UNSAFE MISS {:>5} ({:>5.1}%)   <- the safety bar (drive into a hazard)",
            self.unsafe_miss,
            100.0 * self.unsafe_miss_rate()
        )?;
        writeln!(
            f,
            "  over-conserv {:>4} ({:>5.1}%)   correct {} ({:.1}%)",
            self.over_conservative,
            100.0 * self.over_conservative_rate(),
            self.correct,
            100.0 * self.correct_rate()
        )?;
        writeln!(
            f,
            "  hazard recall {:>5.1}%  ({}/{} true hazards caught)",
            100.0 * self.hazard_recall(),
            self.true_hazards_caught,
            self.frames_with_true_hazard
        )?;
        writeln!(f, "  by class:")?;
        for (label, r) in &self.by_class {
            writeln!(
                f,
                "    {:<16} recall {:>5.1}%   ({}/{})",
                label,
                100.0 * r.recall(),
                r.caught,
                r.seen
            )?;
        }
        write!(f, "  mean clip error {:.2} m (over {} co-clipped frames)", self.mean_clip_err_m(), self.co_clipped)
    }
}

/// Ratio `n/d` as a fraction, 0.0 when `d == 0`.
fn ratio(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        n as f64 / d as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TajConfig, TajPhaseA};

    // A wide-open corridor reaching ~20 m, built from the geometric Phase-A pipeline (same
    // substrate the fusion tests use), so the binding hazard is the semantic one, not geometry.
    fn open_corridor() -> TajCorridor {
        // Two far side walls → a clear forward corridor; reuse the lib test's scan shape.
        let taj = TajPhaseA::new(TajConfig { forward_extent_m: 20.0, ..Default::default() });
        // A scan with returns only far out to the sides leaves the forward corridor open.
        let n = 180usize;
        let mut ranges = vec![f32::INFINITY; n];
        // place a couple of far lateral returns so the corridor exists but stays wide
        ranges[10] = 30.0;
        ranges[170] = 30.0;
        let scan = crate::LaserScan {
            angle_min_rad: -std::f64::consts::FRAC_PI_2,
            angle_increment_rad: std::f64::consts::PI / (n as f64 - 1.0),
            range_min_m: 0.1,
            range_max_m: 40.0,
            ranges,
            stamp_ms: 0,
        };
        taj.process(&scan, 0).corridor
    }

    fn hazard(class: SemanticClass, near_x: f64) -> SemanticDetection {
        SemanticDetection { class, near_x_m: near_x, lateral_min_m: -5.0, lateral_max_m: 5.0 }
    }

    fn frame<'a>(
        corridor: &'a TajCorridor,
        truth: &'a [SemanticDetection],
        detected: &'a [SemanticDetection],
    ) -> SemanticEvalFrame<'a> {
        SemanticEvalFrame { corridor, truth, detected }
    }

    #[test]
    fn exact_match_is_correct() {
        let c = open_corridor();
        let truth = [hazard(SemanticClass::Water, 8.0)];
        let det = [hazard(SemanticClass::Water, 8.0)];
        assert_eq!(score_frame(&frame(&c, &truth, &det), DEFAULT_CLIP_TOL_M), FrameOutcome::Correct);
    }

    #[test]
    fn detector_missing_a_true_hazard_is_an_unsafe_miss() {
        // Truth: water at 8 m. Detector: nothing → corridor stays open past the water.
        let c = open_corridor();
        let truth = [hazard(SemanticClass::Water, 8.0)];
        let det: [SemanticDetection; 0] = [];
        assert_eq!(score_frame(&frame(&c, &truth, &det), DEFAULT_CLIP_TOL_M), FrameOutcome::UnsafeMiss);
    }

    #[test]
    fn detector_seeing_the_hazard_too_far_out_is_an_unsafe_miss() {
        // Truth clips at 8 m; detector only clips at 14 m → drivable space runs 6 m past truth.
        let c = open_corridor();
        let truth = [hazard(SemanticClass::Water, 8.0)];
        let det = [hazard(SemanticClass::Water, 14.0)];
        assert_eq!(score_frame(&frame(&c, &truth, &det), DEFAULT_CLIP_TOL_M), FrameOutcome::UnsafeMiss);
    }

    #[test]
    fn detector_clipping_earlier_than_truth_is_over_conservative() {
        // Truth clips at 12 m; detector clips at 5 m → safe but needlessly short.
        let c = open_corridor();
        let truth = [hazard(SemanticClass::Water, 12.0)];
        let det = [hazard(SemanticClass::StaticObstacle, 5.0)];
        assert_eq!(
            score_frame(&frame(&c, &truth, &det), DEFAULT_CLIP_TOL_M),
            FrameOutcome::OverConservative
        );
    }

    #[test]
    fn a_spurious_hazard_with_no_true_one_is_over_conservative_not_unsafe() {
        // Truth: clear. Detector: invents an obstacle at 6 m → corridor clipped for nothing.
        let c = open_corridor();
        let truth: [SemanticDetection; 0] = [];
        let det = [hazard(SemanticClass::StaticObstacle, 6.0)];
        assert_eq!(
            score_frame(&frame(&c, &truth, &det), DEFAULT_CLIP_TOL_M),
            FrameOutcome::OverConservative
        );
    }

    #[test]
    fn both_clear_is_correct() {
        let c = open_corridor();
        let truth: [SemanticDetection; 0] = [];
        let det: [SemanticDetection; 0] = [];
        assert_eq!(score_frame(&frame(&c, &truth, &det), DEFAULT_CLIP_TOL_M), FrameOutcome::Correct);
    }

    #[test]
    fn summary_aggregates_outcomes_recall_and_per_class() {
        let c = open_corridor();
        // Frame 1: water caught exactly → correct, water recall +1/+1.
        let t1 = [hazard(SemanticClass::Water, 8.0)];
        let d1 = [hazard(SemanticClass::Water, 8.0)];
        // Frame 2: water MISSED → unsafe miss, water recall +0/+1.
        let t2 = [hazard(SemanticClass::Water, 8.0)];
        let d2: [SemanticDetection; 0] = [];
        // Frame 3: static obstacle, detector over-clips (safe) → over-conservative, caught.
        let t3 = [hazard(SemanticClass::StaticObstacle, 12.0)];
        let d3 = [hazard(SemanticClass::StaticObstacle, 6.0)];
        let s = SemanticEvalSummary::from_frames([
            frame(&c, &t1, &d1),
            frame(&c, &t2, &d2),
            frame(&c, &t3, &d3),
        ]);

        assert_eq!(s.frames, 3);
        assert_eq!((s.correct, s.unsafe_miss, s.over_conservative), (1, 1, 1));
        assert!((s.unsafe_miss_rate() - 1.0 / 3.0).abs() < 1e-9);
        // All three frames have a true binding hazard; two were caught (frames 1 and 3).
        assert_eq!((s.frames_with_true_hazard, s.true_hazards_caught), (3, 2));
        assert!((s.hazard_recall() - 2.0 / 3.0).abs() < 1e-9);
        // Per-class: water 1/2 caught, static_obstacle 1/1.
        assert_eq!(s.by_class["water"], ClassRecall { seen: 2, caught: 1 });
        assert_eq!(s.by_class["static_obstacle"], ClassRecall { seen: 1, caught: 1 });
        assert!((s.by_class["water"].recall() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn no_true_hazards_means_vacuous_full_recall_without_dividing_by_zero() {
        let c = open_corridor();
        let truth: [SemanticDetection; 0] = [];
        let det: [SemanticDetection; 0] = [];
        let s = SemanticEvalSummary::from_frames([frame(&c, &truth, &det)]);
        assert_eq!(s.frames_with_true_hazard, 0);
        assert_eq!(s.hazard_recall(), 1.0, "no hazards to miss → vacuously perfect recall");
        assert_eq!(s.unsafe_miss_rate(), 0.0);
        assert_eq!(s.mean_clip_err_m(), 0.0);
    }

    #[test]
    fn mean_clip_error_averages_only_co_clipped_frames() {
        let c = open_corridor();
        // Co-clipped with a 2 m error.
        let t1 = [hazard(SemanticClass::Water, 8.0)];
        let d1 = [hazard(SemanticClass::Water, 10.0)];
        // Truth clear, detector clear → NOT co-clipped, excluded from the mean.
        let t2: [SemanticDetection; 0] = [];
        let d2: [SemanticDetection; 0] = [];
        let s = SemanticEvalSummary::from_frames([frame(&c, &t1, &d1), frame(&c, &t2, &d2)]);
        assert_eq!(s.co_clipped, 1);
        assert!((s.mean_clip_err_m() - 2.0).abs() < 1e-9, "2 m error over 1 co-clipped frame");
    }

    #[test]
    fn display_reports_the_safety_headline() {
        let c = open_corridor();
        let t = [hazard(SemanticClass::Water, 8.0)];
        let d: [SemanticDetection; 0] = [];
        let report = SemanticEvalSummary::from_frames([frame(&c, &t, &d)]).to_string();
        assert!(report.contains("UNSAFE MISS"), "report surfaces the safety bar: {report}");
        assert!(report.contains("water"), "report breaks down by class: {report}");
    }
}
