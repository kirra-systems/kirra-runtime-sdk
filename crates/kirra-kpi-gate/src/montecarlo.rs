//! Seeded Monte-Carlo scenario campaign (WP-23 / G-16 software half).
//!
//! The WS-3.1 gate ([`crate::run_gate`]) runs a deterministic closed-form sweep
//! in the low hundreds — exact rationals, ideal for a per-PR ratchet, but too
//! coarse to make a *statistical* statement about the tail ("the true
//! `unsafe_miss_rate` is under 1 % with 95 % confidence"). This module adds the
//! campaign half: a **seeded procedural generator** that samples the SAME
//! scenario schema continuously, scaling the corpus to 10⁴–10⁵, and a gate that
//! re-expresses each threshold as a bound on a [`crate::confidence`] interval.
//!
//! Design rules (unchanged from the deterministic gate):
//! - **Deterministic by seed.** The generator is a pure [`SplitMix64`] stream —
//!   no OS RNG, no clock. The same `(seed, n)` yields byte-identical corpora on
//!   every machine, so a red campaign is a real regression, never flake.
//! - **Reuses the existing harnesses.** Sampling produces the same
//!   `EvalScenario` / [`crate::PerceptionCase`] types the deterministic corpus
//!   uses; scoring goes through the real checker and the taj oracle. It adds
//!   corpus scale + confidence intervals, never a new metric.
//! - **Negative controls preserved.** The campaign re-runs the #777 F1
//!   fault-injection families over the sampled hazard frames and asserts each
//!   still breaches — the oracle-discrimination evidence survives the resampling.
//!
//! The per-PR profile samples a fast subset; the nightly profile runs the full
//! 10⁴–10⁵ corpus (tighter intervals ⇒ the same policy floor is a stronger
//! claim). Both read one reviewed policy, `ci/scenario_kpi_montecarlo.json`.

use serde::{Deserialize, Serialize};

use crate::confidence::ConfidenceInterval;

// ---------------------------------------------------------------------------
// Deterministic PRNG — SplitMix64 (dependency-free, reproducible)
// ---------------------------------------------------------------------------

/// A minimal SplitMix64 generator: a single `u64` state advanced by the golden-
/// ratio increment, finalized with the standard avalanche. Chosen for the
/// campaign because it is (a) trivially reproducible across platforms, (b)
/// dependency-free (no `rand`), and (c) well-distributed enough for scenario
/// sampling. Not cryptographic — it never needs to be.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// Seed the stream. Every distinct seed gives an independent, reproducible
    /// scenario sequence.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Next raw 64-bit draw.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform `f64` in `[0, 1)` (53-bit mantissa, so strictly `< 1.0`).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A uniform `f64` in `[lo, hi)`.
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_f64()
    }

    /// A uniform index in `[0, n)` (`n` must be `> 0`).
    pub fn below(&mut self, n: usize) -> usize {
        debug_assert!(n > 0);
        ((self.next_f64() * n as f64) as usize).min(n - 1)
    }

    /// Bernoulli draw: `true` with probability `p`.
    pub fn chance(&mut self, p: f64) -> bool {
        self.next_f64() < p
    }
}

// Stream-separation constants so the doer and perception corpora drawn from the
// same policy seed are independent (not the identical `f64` sequence).
const DOER_STREAM: u64 = 0xD0E7_5747_0000_0001;
const PERCEPTION_STREAM: u64 = 0x9E12_C3F7_0000_0002;

/// Fraction of sampled scenarios with NO hazard (a clear-road control row).
const CLEAR_FRACTION: f64 = 0.15;

// ---------------------------------------------------------------------------
// Procedural corpora — continuous sampling over the existing scenario schema
// ---------------------------------------------------------------------------

/// Sample `n` doer scenarios from `seed`: ego speed × goal distance × (hazard
/// kind, longitudinal distance, small lateral offset), with a
/// [`CLEAR_FRACTION`] of clear-road rows. Continuous analogue of
/// [`crate::generated_doer_corpus`] — same axes, same `EvalScenario` type, but
/// densely sampled instead of gridded. Deterministic: identical `(seed, n)` ⇒
/// identical corpus.
#[must_use]
pub fn sample_doer_corpus(seed: u64, n: usize) -> Vec<crate::EvalScenario> {
    use crate::{hazard, HazardKind};
    use kirra_core::corridor::MockCorridorSource;

    let kinds = [HazardKind::Stopped, HazardKind::LeadMoving, HazardKind::Oncoming];
    let mut rng = SplitMix64::new(seed ^ DOER_STREAM);
    let mut corpus = Vec::with_capacity(n);
    for i in 0..n {
        let speed = rng.range(1.0, 6.0);
        let goal = rng.range(40.0, 60.0);
        let objects = if rng.chance(CLEAR_FRACTION) {
            Vec::new()
        } else {
            let kind = kinds[rng.below(kinds.len())];
            let dist = rng.range(12.0, 48.0);
            let lateral = rng.range(-1.0, 1.0);
            let mut h = hazard(kind, 1, dist);
            h.pos.y_m = lateral;
            vec![h]
        };
        corpus.push(crate::EvalScenario::new(
            format!("mc_doer_{i}"),
            MockCorridorSource::straight_5m_half_width(100.0),
            objects,
            5.0,
            speed,
            goal,
        ));
    }
    corpus
}

/// Sample `n` perception frames from `seed`: hazard class × near distance ×
/// lateral span, with a [`CLEAR_FRACTION`] of hazard-free frames. Continuous
/// analogue of [`crate::generated_perception_corpus`], fed through the same
/// scripted detector seam (so today it pins the seam behaviour; the real
/// detector inherits the campaign behind the same trait).
#[must_use]
pub fn sample_perception_corpus(seed: u64, n: usize) -> Vec<crate::PerceptionCase> {
    use kirra_taj::{MockSemanticDetector, SemanticClass, SemanticDetection, SemanticDetector};

    let classes = [SemanticClass::Water, SemanticClass::StaticObstacle];
    let mut rng = SplitMix64::new(seed ^ PERCEPTION_STREAM);
    let mut cases = Vec::with_capacity(n);
    for i in 0..n {
        if rng.chance(CLEAR_FRACTION) {
            cases.push(crate::PerceptionCase {
                name: format!("mc_perc_{i}_clear"),
                corridor: crate::open_corridor(),
                truth: Vec::new(),
                detected: MockSemanticDetector::default().detect(),
            });
        } else {
            let class = classes[rng.below(classes.len())];
            let near_x = rng.range(3.0, 18.0);
            let lat_lo = rng.range(-5.0, 0.5);
            let width = rng.range(0.5, 5.0);
            let det = SemanticDetection {
                class,
                near_x_m: near_x,
                lateral_min_m: lat_lo,
                lateral_max_m: lat_lo + width,
            };
            let detector = MockSemanticDetector { detections: vec![det] };
            cases.push(crate::PerceptionCase {
                name: format!("mc_perc_{i}"),
                corridor: crate::open_corridor(),
                truth: vec![det],
                detected: detector.detect(),
            });
        }
    }
    cases
}

// ---------------------------------------------------------------------------
// Reviewed policy — thresholds re-expressed as confidence-interval bounds
// ---------------------------------------------------------------------------

/// Per-profile corpus sizes (a fast per-PR subset vs the full nightly campaign).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleSizes {
    pub doer_samples: usize,
    pub perception_samples: usize,
}

/// Which campaign profile to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// The fast sampled subset that gates every PR.
    PerPr,
    /// The full 10⁴–10⁵ corpus that runs nightly (tighter intervals).
    Nightly,
}

/// The reviewed Monte-Carlo campaign policy (`ci/scenario_kpi_montecarlo.json`).
/// Floors are **confidence-interval bounds**, not point thresholds: they account
/// for sampling uncertainty, so a passing campaign is a statistical statement,
/// not a single lucky draw. Every field is required.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonteCarloPolicy {
    /// Free-text rationale — travels with the numbers.
    #[serde(rename = "_comment")]
    pub comment: String,
    /// The campaign seed (reproducibility anchor; bumping it is a policy change).
    pub seed: u64,
    /// Per-PR sampled-subset sizes.
    pub per_pr: SampleSizes,
    /// Nightly full-corpus sizes.
    pub nightly: SampleSizes,
    /// Min CI LOWER bound on geometric-planner admissibility.
    pub geometric_admissibility_lo_min: f64,
    /// Min CI LOWER bound on the seeded SafetyAware learned planner's admissibility.
    pub learned_admissibility_lo_min: f64,
    /// Max CI UPPER bound on `unsafe_miss_rate` (the safety bar).
    pub unsafe_miss_rate_hi_max: f64,
    /// Min CI LOWER bound on `hazard_recall`.
    pub hazard_recall_lo_min: f64,
    /// Min CI LOWER bound each negative-control fault family's breach must clear
    /// (the oracle-discrimination floor under resampling).
    pub negctl_breach_lo_min: f64,
}

impl MonteCarloPolicy {
    /// The corpus sizes for a profile.
    #[must_use]
    pub fn sizes(&self, profile: Profile) -> SampleSizes {
        match profile {
            Profile::PerPr => self.per_pr,
            Profile::Nightly => self.nightly,
        }
    }
}

// ---------------------------------------------------------------------------
// Campaign gate rows + report
// ---------------------------------------------------------------------------

/// How a KPI's confidence interval is gated. A statistical bound gates on the
/// interval; a hard invariant (a phantom must NEVER be scored unsafe) gates on
/// the point estimate — it is an absolute, not a coverage claim.
#[derive(Debug, Clone, Copy)]
pub enum Bound {
    /// Confident the true rate is ≥ `min`: gate on the CI LOWER bound.
    CiLowerAtLeast(f64),
    /// Confident the true rate is ≤ `max`: gate on the CI UPPER bound.
    CiUpperAtMost(f64),
    /// Hard invariant: the POINT estimate must be ≤ `max` (no statistical slack).
    PointAtMost(f64),
}

impl Bound {
    fn value(self) -> f64 {
        match self {
            Bound::CiLowerAtLeast(v) | Bound::CiUpperAtMost(v) | Bound::PointAtMost(v) => v,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Bound::CiLowerAtLeast(_) => "ci.lo >=",
            Bound::CiUpperAtMost(_) => "ci.hi <=",
            Bound::PointAtMost(_) => "point <=",
        }
    }
}

/// One campaign KPI row: the Wilson interval that gates it, the exact
/// (Clopper–Pearson) interval reported alongside, and the verdict.
#[derive(Debug, Clone, Copy)]
pub struct McKpiRow {
    pub name: &'static str,
    /// Wilson score interval (the gated one).
    pub wilson: ConfidenceInterval,
    /// Clopper–Pearson exact interval (reported for context; more conservative).
    pub exact: ConfidenceInterval,
    pub bound: Bound,
    pub pass: bool,
}

impl McKpiRow {
    #[must_use]
    pub fn new(
        name: &'static str,
        wilson: ConfidenceInterval,
        exact: ConfidenceInterval,
        bound: Bound,
    ) -> Self {
        let pass = match bound {
            Bound::CiLowerAtLeast(m) => wilson.lo >= m,
            Bound::CiUpperAtMost(m) => wilson.hi <= m,
            Bound::PointAtMost(m) => wilson.point <= m,
        };
        Self { name, wilson, exact, bound, pass }
    }

    /// The bound direction/value for display (e.g. `ci.hi <= 0.0500`).
    #[must_use]
    pub fn bound_display(&self) -> (&'static str, f64) {
        (self.bound.label(), self.bound.value())
    }
}

/// The full campaign outcome: every row plus the corpus provenance.
#[derive(Debug, Clone)]
pub struct McGateReport {
    pub seed: u64,
    pub doer_samples: usize,
    pub perception_samples: usize,
    pub rows: Vec<McKpiRow>,
}

impl McGateReport {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.rows.iter().all(|r| r.pass)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator_is_deterministic_by_seed() {
        let a: Vec<String> = sample_doer_corpus(42, 64).into_iter().map(|s| s.name).collect();
        let b: Vec<String> = sample_doer_corpus(42, 64).into_iter().map(|s| s.name).collect();
        assert_eq!(a, b, "same seed ⇒ identical corpus");

        // The world params (not just the names) must be identical too.
        let wa = sample_doer_corpus(42, 32);
        let wb = sample_doer_corpus(42, 32);
        for (x, y) in wa.iter().zip(&wb) {
            assert_eq!(x.objects().len(), y.objects().len());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        // The object-count fingerprint differs across seeds (the clear/hazard
        // Bernoulli draws land differently).
        let counts = |seed| -> Vec<usize> {
            sample_doer_corpus(seed, 128).iter().map(|s| s.objects().len()).collect()
        };
        assert_ne!(counts(1), counts(2), "distinct seeds ⇒ distinct corpora");
    }

    #[test]
    fn splitmix_below_is_in_range() {
        let mut rng = SplitMix64::new(7);
        for _ in 0..10_000 {
            assert!(rng.below(3) < 3);
        }
    }

    #[test]
    fn splitmix_next_f64_is_half_open_unit() {
        let mut rng = SplitMix64::new(99);
        for _ in 0..10_000 {
            let x = rng.next_f64();
            assert!((0.0..1.0).contains(&x));
        }
    }

    #[test]
    fn mc_row_gates_on_the_right_side_of_the_interval() {
        let ci = ConfidenceInterval { point: 0.5, lo: 0.4, hi: 0.6 };
        assert!(McKpiRow::new("t", ci, ci, Bound::CiLowerAtLeast(0.35)).pass);
        assert!(!McKpiRow::new("t", ci, ci, Bound::CiLowerAtLeast(0.45)).pass);
        assert!(McKpiRow::new("t", ci, ci, Bound::CiUpperAtMost(0.65)).pass);
        assert!(!McKpiRow::new("t", ci, ci, Bound::CiUpperAtMost(0.55)).pass);
        let zero = ConfidenceInterval { point: 0.0, lo: 0.0, hi: 0.02 };
        assert!(McKpiRow::new("t", zero, zero, Bound::PointAtMost(0.0)).pass, "point 0 passes");
        assert!(!McKpiRow::new("t", ci, ci, Bound::PointAtMost(0.0)).pass, "point 0.5 fails");
    }
}
