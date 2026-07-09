//! EP-20 — differential perception CI: Phase-A (geometric) vs Phase-B
//! (semantic fusion) over SHARED synthetic ground truth.
//!
//! Phase-A's corridor is hazard-BLIND by construction (geometry cannot see
//! that flat water is undrivable); Phase-B is Phase-A plus
//! `clip_corridor_to_hazards`. The differential question is not "is Phase-B
//! right?" (the safety-weighted oracle `SemanticEvalSummary` already scores
//! that) but "does Phase-B diverge from Phase-A EXACTLY where the shared
//! ground truth says it must, and nowhere else?" — the negative-control
//! philosophy extended to the perception pipeline pair:
//!
//! * where truth binds a hazard, the phases MUST diverge (Phase-B tightens) —
//!   a non-divergence there is a `MissedTighten`, the differential analogue
//!   of `UnsafeMiss`;
//! * where truth is clear, the phases MUST agree — a divergence there is a
//!   `PhantomTighten` (availability cost, over-conservative direction);
//! * Phase-B may only ever TIGHTEN Phase-A (fusion is derate-only): a
//!   Phase-B corridor extending PAST Phase-A's is `ForbiddenLoosen`, gated
//!   hard at zero. Structurally unreachable through
//!   `clip_corridor_to_hazards` (truncation only) — the row exists so any
//!   future fusion rewrite that CAN loosen turns the gate red by definition
//!   rather than by luck.

use kirra_core::corridor::CorridorSource;
use kirra_taj::{
    clip_corridor_to_hazards, hazard_clip_x, SemanticDetection, TajCorridor, DEFAULT_CLIP_TOL_M,
};

/// One frame's Phase-A vs Phase-B divergence, classified against truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DivergenceClass {
    /// Truth is clear and the phases agree (no semantic clip). The expected
    /// state on hazard-free frames.
    Identical,
    /// Truth binds a hazard and Phase-B tightened to it (within tolerance).
    /// The expected state on hazard frames — semantic fusion earning its keep.
    JustifiedTighten,
    /// Phase-B tightened where truth binds nothing (or far short of the true
    /// hazard). Over-conservative: an availability cost, bounded but not a
    /// safety breach.
    PhantomTighten,
    /// Truth binds a hazard but Phase-B stayed at (or beyond) Phase-A's
    /// hazard-blind extent — the REQUIRED divergence did not happen. The
    /// differential analogue of `UnsafeMiss`.
    MissedTighten,
    /// Phase-B's corridor extends PAST Phase-A's. Fusion is derate-only;
    /// this class must be structurally impossible. Gated hard at zero.
    ForbiddenLoosen,
}

/// Forward drivable extent of a corridor — the far end of its boundary
/// polylines (the scalar Phase-A/Phase-B differ in when a clip fires).
fn extent_x(c: &TajCorridor) -> f64 {
    let far = |pts: &[kirra_core::corridor::Point]| {
        pts.iter().map(|p| p.x_m).fold(f64::NEG_INFINITY, f64::max)
    };
    far(c.left_boundary()).max(far(c.right_boundary()))
}

/// Classify ONE frame: Phase-A corridor + shared ground truth + the detector
/// output Phase-B fuses. `tol_m` is the clip-agreement tolerance (the same
/// tolerance family the semantic oracle scores with).
#[must_use]
pub fn classify_frame(
    phase_a: &TajCorridor,
    truth: &[SemanticDetection],
    detected: &[SemanticDetection],
    tol_m: f64,
) -> DivergenceClass {
    let phase_b = clip_corridor_to_hazards(phase_a, detected);

    // The derate-only invariant, checked on the ACTUAL corridors (not just
    // the clip scalars): Phase-B may never reach past Phase-A.
    if extent_x(&phase_b) > extent_x(phase_a) + 1e-9 {
        return DivergenceClass::ForbiddenLoosen;
    }

    let truth_clip = hazard_clip_x(phase_a, truth);
    let b_clip = hazard_clip_x(phase_a, detected);

    match (truth_clip, b_clip) {
        (None, None) => DivergenceClass::Identical,
        (None, Some(_)) => DivergenceClass::PhantomTighten,
        (Some(_), None) => DivergenceClass::MissedTighten,
        (Some(t), Some(b)) => {
            if (b - t).abs() <= tol_m {
                DivergenceClass::JustifiedTighten
            } else if b > t {
                // Phase-B clipped, but FARTHER out than the true hazard: the
                // corridor still runs past truth — the unsafe direction.
                DivergenceClass::MissedTighten
            } else {
                // Clipped short of the true hazard: over-conservative.
                DivergenceClass::PhantomTighten
            }
        }
    }
}

/// Whole-corpus differential tallies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DifferentialSummary {
    pub total: usize,
    pub identical: usize,
    pub justified_tighten: usize,
    pub phantom_tighten: usize,
    pub missed_tighten: usize,
    pub forbidden_loosen: usize,
    /// Frames where truth binds a hazard (the `MissedTighten` denominator).
    pub truth_hazard_frames: usize,
}

impl DifferentialSummary {
    /// Fraction of truth-hazard frames where the REQUIRED Phase-A/Phase-B
    /// divergence did not happen (unsafe direction). 0 when no hazard frames.
    #[must_use]
    pub fn missed_tighten_rate(&self) -> f64 {
        if self.truth_hazard_frames == 0 {
            0.0
        } else {
            self.missed_tighten as f64 / self.truth_hazard_frames as f64
        }
    }

    /// Fraction of ALL frames with an unjustified tighten (availability cost).
    #[must_use]
    pub fn phantom_tighten_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.phantom_tighten as f64 / self.total as f64
        }
    }

    /// Tally one classified frame.
    pub fn record(&mut self, class: DivergenceClass, truth_binds: bool) {
        self.total += 1;
        if truth_binds {
            self.truth_hazard_frames += 1;
        }
        match class {
            DivergenceClass::Identical => self.identical += 1,
            DivergenceClass::JustifiedTighten => self.justified_tighten += 1,
            DivergenceClass::PhantomTighten => self.phantom_tighten += 1,
            DivergenceClass::MissedTighten => self.missed_tighten += 1,
            DivergenceClass::ForbiddenLoosen => self.forbidden_loosen += 1,
        }
    }
}

/// Run the differential over `(corridor, truth, detected)` frames at the
/// oracle's default clip tolerance.
#[must_use]
pub fn differential_summary<'a>(
    frames: impl IntoIterator<Item = (&'a TajCorridor, &'a [SemanticDetection], &'a [SemanticDetection])>,
) -> DifferentialSummary {
    let mut s = DifferentialSummary::default();
    for (corridor, truth, detected) in frames {
        let class = classify_frame(corridor, truth, detected, DEFAULT_CLIP_TOL_M);
        let truth_binds = hazard_clip_x(corridor, truth).is_some();
        s.record(class, truth_binds);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_taj::SemanticClass;

    fn det(class: SemanticClass, near_x: f64) -> SemanticDetection {
        SemanticDetection { class, near_x_m: near_x, lateral_min_m: -5.0, lateral_max_m: 5.0 }
    }

    fn corridor() -> TajCorridor {
        crate::open_corridor()
    }

    /// The (truth, detected) clip-scalar matrix, including both tolerance
    /// boundaries of the both-clip arm (the cells the gate rows can't reach
    /// through the seam-pinned corpus).
    #[test]
    fn classification_matrix() {
        let c = corridor();
        let water = |x| det(SemanticClass::Water, x);

        // Agreement cells.
        assert_eq!(classify_frame(&c, &[], &[], DEFAULT_CLIP_TOL_M), DivergenceClass::Identical);
        assert_eq!(
            classify_frame(&c, &[water(8.0)], &[water(8.0)], DEFAULT_CLIP_TOL_M),
            DivergenceClass::JustifiedTighten
        );
        // Within tolerance either side → still justified.
        assert_eq!(
            classify_frame(&c, &[water(8.0)], &[water(8.4)], DEFAULT_CLIP_TOL_M),
            DivergenceClass::JustifiedTighten
        );
        assert_eq!(
            classify_frame(&c, &[water(8.0)], &[water(7.6)], DEFAULT_CLIP_TOL_M),
            DivergenceClass::JustifiedTighten
        );

        // Unsafe direction: no clip, or clip beyond truth + tol.
        assert_eq!(
            classify_frame(&c, &[water(8.0)], &[], DEFAULT_CLIP_TOL_M),
            DivergenceClass::MissedTighten
        );
        assert_eq!(
            classify_frame(&c, &[water(8.0)], &[water(9.0)], DEFAULT_CLIP_TOL_M),
            DivergenceClass::MissedTighten
        );

        // Over-conservative direction: clip with clear truth, or far short.
        assert_eq!(
            classify_frame(&c, &[], &[water(8.0)], DEFAULT_CLIP_TOL_M),
            DivergenceClass::PhantomTighten
        );
        assert_eq!(
            classify_frame(&c, &[water(8.0)], &[water(6.0)], DEFAULT_CLIP_TOL_M),
            DivergenceClass::PhantomTighten
        );
    }

    /// A DRIVABLE detection (Road) never binds — the phases agree and the
    /// fusion cannot loosen: ForbiddenLoosen stays structurally unreachable
    /// through clip_corridor_to_hazards (truncation only).
    #[test]
    fn drivable_detection_never_diverges_and_never_loosens() {
        let c = corridor();
        let road = det(SemanticClass::Road, 8.0);
        assert_eq!(
            classify_frame(&c, &[], &[road], DEFAULT_CLIP_TOL_M),
            DivergenceClass::Identical
        );
    }

    /// Rates: denominators are what the docs say they are.
    #[test]
    fn summary_rates() {
        let mut s = DifferentialSummary::default();
        s.record(DivergenceClass::Identical, false);
        s.record(DivergenceClass::JustifiedTighten, true);
        s.record(DivergenceClass::MissedTighten, true);
        s.record(DivergenceClass::PhantomTighten, false);
        assert_eq!(s.total, 4);
        assert_eq!(s.truth_hazard_frames, 2);
        assert!((s.missed_tighten_rate() - 0.5).abs() < 1e-12);
        assert!((s.phantom_tighten_rate() - 0.25).abs() < 1e-12);
        assert_eq!(DifferentialSummary::default().missed_tighten_rate(), 0.0);
    }
}
