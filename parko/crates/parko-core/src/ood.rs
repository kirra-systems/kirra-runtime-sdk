//! **Out-of-distribution / input-shift monitor (WP-24 / G-15 software half, part b).**
//!
//! The gap: the ML doer (a detector / policy net) is calibrated on one input
//! distribution, but at run time the world drifts — fog, glare, a sensor
//! degrading, a scene the training set never covered. A silent distribution
//! shift is exactly when a learned model's outputs are least trustworthy, yet
//! nothing in the stack notices. This module is the noticing: it compares the
//! live distribution of a model feature (e.g. the per-frame detection-confidence
//! distribution, or an activation summary) against a frozen **calibration
//! baseline** and, on drift, RECOMMENDS a more restrictive [`SafetyPosture`] that
//! the runtime folds in via `posture.escalate(recommended)` — the same seam a
//! redundancy comparator uses.
//!
//! **Drift metric — Population Stability Index (PSI).** Both the baseline and the
//! live window are binned over a fixed range; `PSI = Σ (live_i − base_i)·ln(live_i /
//! base_i)`. PSI measures a change in distribution SHAPE, not merely the mean, and
//! carries well-known bands: `< 0.10` stable, `0.10–0.25` a moderate shift,
//! `≥ 0.25` a significant shift. Those map directly to Nominal / Degraded /
//! LockedOut.
//!
//! **Doer-checker invariant (never violated).** The monitor is *derate-only* and
//! *fail-closed*:
//! - it only ever recommends a posture at least as restrictive as `Nominal`
//!   (it can tighten the envelope, never relax it);
//! - a corrupt feature stream (a non-finite value — a `NaN` activation is a
//!   broken model, not merely a shifted one) fails closed to `LockedOut`;
//! - an under-filled live window (not enough evidence THIS tick) is a no-op
//!   (`Nominal`) — "absent input → no-op", so a quiet tick never flaps the
//!   posture. Persistent absence of a REQUIRED monitor is a watchdog concern,
//!   not this pure per-tick assessment.
//!
//! The whole module is pure (no I/O, no clock, no global state), so the
//! escalation logic and the false-positive budget are fully unit-testable; the
//! backend/tick wiring that feeds it live confidences is the recorded follow-up.

use crate::safety::SafetyPosture;

/// PSI at/above which the distribution is a MODERATE shift → recommend `Degraded`.
pub const DEFAULT_WARN_PSI: f64 = 0.10;
/// PSI at/above which the distribution is a SIGNIFICANT shift → recommend `LockedOut`.
pub const DEFAULT_FAULT_PSI: f64 = 0.25;
/// Minimum live-window size below which a tick is treated as "no evidence" (no-op).
pub const DEFAULT_MIN_WINDOW: usize = 30;
/// Minimum calibration corpus for a trustworthy baseline.
pub const MIN_CALIBRATION_SAMPLES: usize = 100;
/// Proportion floor, so an empty bin never yields `ln(0)` / division by zero.
const PSI_EPS: f64 = 1e-6;

/// Why the monitor reached its recommendation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OodReason {
    /// PSI below the warn band — in-distribution.
    Stable,
    /// PSI in the warn band — a moderate shift.
    ModerateShift,
    /// PSI at/above the fault band — a significant shift.
    SevereShift,
    /// A non-finite feature value — a corrupt model output (hard fault).
    NonFiniteInput,
    /// Too few samples this tick to assess — no evidence (no-op).
    InsufficientWindow,
}

/// A monitor could not be constructed from the given calibration corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OodError {
    /// Fewer calibration samples than [`MIN_CALIBRATION_SAMPLES`].
    TooFewCalibrationSamples { got: usize, need: usize },
    /// `bins == 0`, or the range is empty/inverted (`lo >= hi`).
    InvalidBinning,
    /// A non-finite calibration sample.
    NonFiniteCalibration,
}

impl core::fmt::Display for OodError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            OodError::TooFewCalibrationSamples { got, need } => {
                write!(f, "too few calibration samples: {got} < {need}")
            }
            OodError::InvalidBinning => write!(f, "invalid bins/range for the baseline"),
            OodError::NonFiniteCalibration => write!(f, "non-finite calibration sample"),
        }
    }
}
impl std::error::Error for OodError {}

/// A frozen reference distribution: the binned proportions of the calibration
/// corpus over a fixed `[lo, hi)` range. Compared against a live window by PSI.
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationBaseline {
    lo: f64,
    hi: f64,
    /// Per-bin proportions (sum to 1), floored at [`PSI_EPS`].
    proportions: Vec<f64>,
}

impl CalibrationBaseline {
    /// Build a baseline from a calibration corpus. Fail-closed: too few samples,
    /// invalid binning, or a non-finite sample is refused (a baseline that cannot
    /// be trusted must not silently license a monitor).
    pub fn from_samples(samples: &[f64], bins: usize, lo: f64, hi: f64) -> Result<Self, OodError> {
        if bins == 0 || !lo.is_finite() || !hi.is_finite() || lo >= hi {
            return Err(OodError::InvalidBinning);
        }
        if samples.len() < MIN_CALIBRATION_SAMPLES {
            return Err(OodError::TooFewCalibrationSamples {
                got: samples.len(),
                need: MIN_CALIBRATION_SAMPLES,
            });
        }
        if samples.iter().any(|x| !x.is_finite()) {
            return Err(OodError::NonFiniteCalibration);
        }
        Ok(Self { lo, hi, proportions: binned_proportions(samples, bins, lo, hi) })
    }

    /// The number of bins.
    #[must_use]
    pub fn bins(&self) -> usize {
        self.proportions.len()
    }
}

/// A per-tick assessment: the drift statistic, the recommended posture, and why.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OodAssessment {
    /// The Population Stability Index vs the baseline (`NaN` on a hard fault).
    pub psi: f64,
    /// The posture the monitor recommends (escalation-only; `Nominal` = no-op).
    pub recommended: SafetyPosture,
    pub reason: OodReason,
}

/// The input-shift monitor: a frozen baseline plus the PSI→posture thresholds.
#[derive(Debug, Clone)]
pub struct OodMonitor {
    baseline: CalibrationBaseline,
    warn_psi: f64,
    fault_psi: f64,
    min_window: usize,
}

impl OodMonitor {
    /// A monitor with the default PSI bands and window minimum.
    #[must_use]
    pub fn new(baseline: CalibrationBaseline) -> Self {
        Self {
            baseline,
            warn_psi: DEFAULT_WARN_PSI,
            fault_psi: DEFAULT_FAULT_PSI,
            min_window: DEFAULT_MIN_WINDOW,
        }
    }

    /// Override the thresholds (the warn band must not exceed the fault band).
    #[must_use]
    pub fn with_thresholds(mut self, warn_psi: f64, fault_psi: f64, min_window: usize) -> Self {
        // Keep the bands ordered; a caller inversion would otherwise make the
        // Degraded band unreachable. Clamp defensively rather than panic.
        self.warn_psi = warn_psi.min(fault_psi);
        self.fault_psi = warn_psi.max(fault_psi);
        self.min_window = min_window;
        self
    }

    /// Assess a live window of the monitored feature. Derate-only and fail-closed
    /// (see the module docs): a non-finite value → `LockedOut`; an under-filled
    /// window → `Nominal` no-op; otherwise PSI mapped through the bands.
    #[must_use]
    pub fn assess(&self, window: &[f64]) -> OodAssessment {
        if window.iter().any(|x| !x.is_finite()) {
            return OodAssessment {
                psi: f64::NAN,
                recommended: SafetyPosture::LockedOut,
                reason: OodReason::NonFiniteInput,
            };
        }
        if window.len() < self.min_window {
            return OodAssessment {
                psi: 0.0,
                recommended: SafetyPosture::Nominal,
                reason: OodReason::InsufficientWindow,
            };
        }
        let live = binned_proportions(window, self.baseline.bins(), self.baseline.lo, self.baseline.hi);
        let psi = population_stability_index(&self.baseline.proportions, &live);
        let (recommended, reason) = if psi >= self.fault_psi {
            (SafetyPosture::LockedOut, OodReason::SevereShift)
        } else if psi >= self.warn_psi {
            (SafetyPosture::Degraded, OodReason::ModerateShift)
        } else {
            (SafetyPosture::Nominal, OodReason::Stable)
        };
        OodAssessment { psi, recommended, reason }
    }
}

/// Bin `samples` over `[lo, hi)` into `bins` buckets and return the proportions,
/// each floored at [`PSI_EPS`] (so PSI never hits `ln(0)`). Values outside the
/// range clamp into the edge bins. `samples` is assumed non-empty and finite
/// (the constructors check this).
fn binned_proportions(samples: &[f64], bins: usize, lo: f64, hi: f64) -> Vec<f64> {
    let mut counts = vec![0u64; bins];
    let span = hi - lo;
    for &x in samples {
        let t = ((x - lo) / span * bins as f64).floor();
        let idx = (t as isize).clamp(0, bins as isize - 1) as usize;
        counts[idx] += 1;
    }
    let n = samples.len() as f64;
    counts.iter().map(|&c| (c as f64 / n).max(PSI_EPS)).collect()
}

/// Population Stability Index between two same-length proportion vectors:
/// `Σ (live_i − base_i)·ln(live_i / base_i)`. Both are expected floored (no zero
/// bins). Symmetric-ish and always `≥ 0`.
fn population_stability_index(base: &[f64], live: &[f64]) -> f64 {
    debug_assert_eq!(base.len(), live.len());
    base.iter()
        .zip(live)
        .map(|(&b, &l)| (l - b) * (l / b).ln())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny SplitMix64 so the false-positive-budget test can draw many nominal
    /// windows deterministically (no `rand` dep, no clock).
    struct Rng(u64);
    impl Rng {
        fn f64(&mut self) -> f64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    // A calibration corpus concentrated around 0.8 confidence (a well-calibrated
    // detector on in-distribution frames), 400 samples over [0,1].
    fn nominal_baseline() -> CalibrationBaseline {
        let mut rng = Rng(1);
        let samples: Vec<f64> = (0..400).map(|_| (0.6 + 0.3 * rng.f64()).clamp(0.0, 1.0)).collect();
        CalibrationBaseline::from_samples(&samples, 10, 0.0, 1.0).unwrap()
    }

    #[test]
    fn baseline_construction_fails_closed() {
        assert!(matches!(
            CalibrationBaseline::from_samples(&[0.5; 10], 10, 0.0, 1.0),
            Err(OodError::TooFewCalibrationSamples { .. })
        ));
        assert!(matches!(
            CalibrationBaseline::from_samples(&[0.5; 200], 0, 0.0, 1.0),
            Err(OodError::InvalidBinning)
        ));
        assert!(matches!(
            CalibrationBaseline::from_samples(&[0.5; 200], 10, 1.0, 0.0),
            Err(OodError::InvalidBinning)
        ));
        let mut bad = vec![0.5; 200];
        bad[3] = f64::NAN;
        assert!(matches!(
            CalibrationBaseline::from_samples(&bad, 10, 0.0, 1.0),
            Err(OodError::NonFiniteCalibration)
        ));
    }

    #[test]
    fn in_distribution_window_stays_nominal() {
        let m = OodMonitor::new(nominal_baseline());
        let mut rng = Rng(7);
        // A live window from the SAME distribution as calibration.
        let window: Vec<f64> = (0..100).map(|_| (0.6 + 0.3 * rng.f64()).clamp(0.0, 1.0)).collect();
        let a = m.assess(&window);
        assert_eq!(a.recommended, SafetyPosture::Nominal, "psi={}", a.psi);
        assert_eq!(a.reason, OodReason::Stable);
        assert!(a.psi < DEFAULT_WARN_PSI);
    }

    #[test]
    fn a_moderate_shift_escalates_to_degraded() {
        let m = OodMonitor::new(nominal_baseline());
        let mut rng = Rng(11);
        // Confidence collapses toward the low-mid range (a detector losing its
        // grip on a shifted scene) — a moderate but not extreme move.
        let window: Vec<f64> = (0..100).map(|_| (0.35 + 0.3 * rng.f64()).clamp(0.0, 1.0)).collect();
        let a = m.assess(&window);
        assert!(
            a.recommended.severity() >= SafetyPosture::Degraded.severity(),
            "a shifted window must escalate; psi={} rec={:?}",
            a.psi,
            a.recommended
        );
    }

    #[test]
    fn a_severe_shift_locks_out() {
        let m = OodMonitor::new(nominal_baseline());
        // The whole window piles into the lowest-confidence bin — a collapsed
        // detector; PSI is large.
        let window = vec![0.02_f64; 100];
        let a = m.assess(&window);
        assert_eq!(a.recommended, SafetyPosture::LockedOut, "psi={}", a.psi);
        assert_eq!(a.reason, OodReason::SevereShift);
    }

    #[test]
    fn non_finite_feature_is_a_hard_fault() {
        let m = OodMonitor::new(nominal_baseline());
        let mut window = vec![0.7_f64; 100];
        window[10] = f64::INFINITY;
        let a = m.assess(&window);
        assert_eq!(a.recommended, SafetyPosture::LockedOut);
        assert_eq!(a.reason, OodReason::NonFiniteInput);
    }

    #[test]
    fn under_filled_window_is_a_noop() {
        let m = OodMonitor::new(nominal_baseline());
        let a = m.assess(&[0.1_f64; 5]); // < DEFAULT_MIN_WINDOW, and low-conf — but no evidence
        assert_eq!(a.recommended, SafetyPosture::Nominal);
        assert_eq!(a.reason, OodReason::InsufficientWindow);
    }

    #[test]
    fn recommendation_is_derate_only() {
        // Over a sweep of windows, the monitor NEVER returns something below Nominal
        // (there is nothing below Nominal, but this pins the escalation-only contract
        // against a future variant addition).
        let m = OodMonitor::new(nominal_baseline());
        let mut rng = Rng(3);
        for _ in 0..200 {
            let window: Vec<f64> = (0..60).map(|_| rng.f64()).collect();
            let a = m.assess(&window);
            assert!(a.recommended.severity() >= SafetyPosture::Nominal.severity());
        }
    }

    /// The false-positive budget: over many independent in-distribution windows,
    /// the monitor must NOT escalate more than a small fraction of the time — an
    /// OOD monitor that cried wolf on nominal input would derate a healthy fleet.
    #[test]
    fn false_positive_budget_on_nominal_input() {
        let m = OodMonitor::new(nominal_baseline());
        let mut rng = Rng(0xBEEF);
        let trials = 500;
        let mut false_escalations = 0;
        for _ in 0..trials {
            let window: Vec<f64> = (0..100).map(|_| (0.6 + 0.3 * rng.f64()).clamp(0.0, 1.0)).collect();
            if m.assess(&window).recommended != SafetyPosture::Nominal {
                false_escalations += 1;
            }
        }
        // Budget: ≤ 2% false escalations on same-distribution windows.
        assert!(
            false_escalations * 50 <= trials,
            "false-positive rate too high: {false_escalations}/{trials}"
        );
    }

    #[test]
    fn psi_of_identical_distributions_is_zero() {
        let p = vec![0.25, 0.25, 0.25, 0.25];
        assert!(population_stability_index(&p, &p).abs() < 1e-12);
    }
}
