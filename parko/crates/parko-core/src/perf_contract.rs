//! Doer performance contract + eval harness — Q-0 (see `parko/QUANTIZATION_DESIGN.md`).
//!
//! The **measuring stick** for per-silicon doer tuning. It turns "did we close the
//! gap on chip X?" into a CI pass/fail over three axes (§4 of the design note):
//!
//! 1. **Latency** — p99 of one inference tick ≤ the loop budget.
//! 2. **Plan quality** — a higher-is-better scalar (e.g. progress ratio vs. the
//!    `SafetyAware` teacher, or vocabulary-argmax agreement) ≥ a floor.
//! 3. **Admissibility** — the fraction of proposals the checker accepts without an
//!    MRC must not regress beyond a small budget below the FP32 reference.
//!
//! A `(chip, backend, precision)` [`EvalRow`] **passes** iff all three hold.
//!
//! **Safety framing.** This module measures the *untrusted doer*; it has no safety
//! authority. Quantization/precision is a QUALITY knob — a worse row yields a
//! slower or lower-quality (still checker-bounded) plan, never unsafe actuation.
//! The KIRRA checker remains the sole fail-closed authority; this contract only
//! decides which artifact ships for *performance/quality*, not for safety.
//!
//! Q-0 scope: the contract, the latency measurement (real, via a monotonic clock),
//! and the FP32-reference comparison. The `quality` / `admissibility` scalars are
//! *inputs* here — producing them from real models + the checker is Q-1+.

use std::time::Instant;

use crate::backend::{
    BackendDescriptor, BackendError, InferenceBackend, ModelHandle, PrecisionMode, TensorBatch,
};

/// Latency percentiles over a set of per-call samples (nanoseconds).
///
/// Percentiles use the **nearest-rank** method on the sorted samples, so `p99` is
/// an actually-observed sample, never an interpolation — a conservative choice for
/// a latency budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatencyStats {
    pub count: usize,
    pub min_ns: u64,
    pub p50_ns: u64,
    pub p99_ns: u64,
    pub max_ns: u64,
}

impl LatencyStats {
    /// Compute stats from raw per-call durations (ns). Returns `None` for an empty
    /// input (no samples ⇒ no statistic).
    #[must_use]
    pub fn from_samples(samples: &[u64]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        Some(Self {
            count: sorted.len(),
            min_ns: sorted[0],
            p50_ns: nearest_rank(&sorted, 50),
            p99_ns: nearest_rank(&sorted, 99),
            max_ns: sorted[sorted.len() - 1],
        })
    }
}

/// Nearest-rank percentile of a **sorted** slice. `pct` is 1..=100. Index is
/// `ceil(pct/100 * n) - 1`, clamped into range.
fn nearest_rank(sorted: &[u64], pct: u32) -> u64 {
    debug_assert!(!sorted.is_empty());
    debug_assert!((1..=100).contains(&pct), "pct must be 1..=100");
    let n = sorted.len() as u64;
    // ceil(pct * n / 100) without floats.
    let rank = (u64::from(pct) * n).div_ceil(100).max(1);
    let idx = (rank - 1).min(n - 1) as usize;
    sorted[idx]
}

/// A finite fraction in `[0, 1]` — the valid shape for an admissibility value.
/// Rejects NaN/±inf and out-of-range values so the gate fails closed on garbage.
fn is_fraction(x: f64) -> bool {
    x.is_finite() && (0.0..=1.0).contains(&x)
}

/// The per-deployment acceptance thresholds. The `quality_floor` and the budgets
/// are policy inputs — the concrete per-class numbers (courier / delivery-av /
/// robotaxi, keyed by `KIRRA_VEHICLE_CLASS`) are still TBD (design note §11), so
/// callers pass explicit values. [`PerfContract::illustrative`] is a *documented
/// placeholder* for tests/examples, not a shipped threshold.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PerfContract {
    /// Max allowed p99 latency for one inference tick (ns).
    pub p99_latency_budget_ns: u64,
    /// Minimum acceptable plan-quality scalar (higher is better; e.g. 0..=1).
    pub quality_floor: f64,
    /// Max allowed admissibility drop below the FP32 reference (e.g. 0.02 = 2 pts).
    pub admissibility_regression_budget: f64,
}

impl PerfContract {
    /// A clearly-labelled **placeholder** contract for tests/docs — NOT a shipped
    /// threshold. Real per-class numbers are tracked in design-note §11.
    #[must_use]
    pub fn illustrative() -> Self {
        Self {
            p99_latency_budget_ns: 10_000_000, // 10 ms — placeholder slow-loop tick
            quality_floor: 0.90,
            admissibility_regression_budget: 0.02,
        }
    }
}

/// One measured `(chip, backend, precision, model)` row of the contract table.
#[derive(Debug, Clone, PartialEq)]
pub struct EvalRow {
    pub descriptor: BackendDescriptor,
    pub precision: PrecisionMode,
    pub model_id: String,
    pub latency: LatencyStats,
    /// Higher-is-better plan-quality scalar (produced upstream; Q-1+).
    pub quality: f64,
    /// Fraction of proposals the checker admits without MRC (0..=1).
    pub admissibility: f64,
}

/// Why a row failed the contract (empty set ⇒ pass).
#[derive(Debug, Clone, PartialEq)]
pub enum ContractFailure {
    LatencyExceeded {
        p99_ns: u64,
        budget_ns: u64,
    },
    QualityBelowFloor {
        quality: f64,
        floor: f64,
    },
    AdmissibilityRegressed {
        row: f64,
        reference: f64,
        budget: f64,
    },
    /// A malformed contract threshold or an out-of-range row/reference metric.
    /// The gate fails closed rather than silently passing on garbage input
    /// (e.g. a NaN `quality_floor`, a negative budget, or `admissibility > 1`).
    InvalidInput {
        reason: &'static str,
    },
}

/// The verdict for one row against the contract + FP32 reference.
#[derive(Debug, Clone, PartialEq)]
pub struct ContractVerdict {
    pub failures: Vec<ContractFailure>,
}

impl ContractVerdict {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Evaluate `row` against the `contract` and the FP32 `reference` row (the quality/
/// admissibility oracle). All three axes are checked and every failure is
/// collected (not short-circuited) so the table shows the full picture.
///
/// The `reference` supplies the admissibility baseline; a row is penalised only for
/// dropping *below* it by more than the budget — a row that is *more* admissible
/// than FP32 never fails that axis. (Evaluating the reference against itself yields
/// a zero admissibility regression, so FP32 only ever fails on latency/quality.)
#[must_use]
pub fn evaluate(row: &EvalRow, reference: &EvalRow, contract: &PerfContract) -> ContractVerdict {
    let mut failures = Vec::new();

    // Fail-closed on a malformed contract or row BEFORE the axis checks. A NaN
    // threshold would make the corresponding `<` / `>` compare false and silently
    // pass; an admissibility outside [0,1] is not a valid fraction and yields a
    // meaningless regression. Since this function gates CI, garbage input must
    // read as a failure, never a pass. (Non-finite row.quality / regression are
    // additionally caught inline below — this is the input-shape guard.)
    if !contract.quality_floor.is_finite() {
        failures.push(ContractFailure::InvalidInput {
            reason: "contract.quality_floor is non-finite",
        });
    }
    if !contract.admissibility_regression_budget.is_finite()
        || contract.admissibility_regression_budget < 0.0
    {
        failures.push(ContractFailure::InvalidInput {
            reason: "contract.admissibility_regression_budget is non-finite or negative",
        });
    }
    if !is_fraction(row.admissibility) {
        failures.push(ContractFailure::InvalidInput {
            reason: "row.admissibility is not a finite fraction in [0,1]",
        });
    }
    if !is_fraction(reference.admissibility) {
        failures.push(ContractFailure::InvalidInput {
            reason: "reference.admissibility is not a finite fraction in [0,1]",
        });
    }

    if row.latency.p99_ns > contract.p99_latency_budget_ns {
        failures.push(ContractFailure::LatencyExceeded {
            p99_ns: row.latency.p99_ns,
            budget_ns: contract.p99_latency_budget_ns,
        });
    }
    // Fail-closed on a non-finite metric: NaN/±inf means a bad upstream
    // measurement, which must never pass the gate (a plain `<` would let NaN
    // through, since every comparison with NaN is false). `is_finite()` rejects
    // NaN AND ±inf while staying clippy-clean (no negated partial-ord compare).
    if !row.quality.is_finite() || row.quality < contract.quality_floor {
        failures.push(ContractFailure::QualityBelowFloor {
            quality: row.quality,
            floor: contract.quality_floor,
        });
    }
    let regression = reference.admissibility - row.admissibility;
    if !regression.is_finite() || regression > contract.admissibility_regression_budget {
        failures.push(ContractFailure::AdmissibilityRegressed {
            row: row.admissibility,
            reference: reference.admissibility,
            budget: contract.admissibility_regression_budget,
        });
    }

    ContractVerdict { failures }
}

/// Measure the p50/p99/max latency of `backend.run(model, inputs)` over `iters`
/// timed calls after `warmup` discarded ones. Uses a monotonic [`Instant`] (ns) —
/// the parko `Clock` is ms-resolution (control-loop scheduling), too coarse for a
/// per-inference tick. `iters == 0` returns a [`BackendError::ExecutionFailure`].
///
/// NOTE: host timing here is INDICATIVE. A row that feeds an on-vehicle latency
/// claim must be measured on the target silicon, pinned and warmed (design note
/// §4) — this runner is the harness, not the certified measurement.
pub fn run_latency<B: InferenceBackend + ?Sized>(
    backend: &B,
    model: &ModelHandle,
    inputs: &TensorBatch,
    iters: usize,
    warmup: usize,
) -> Result<LatencyStats, BackendError> {
    // A public harness API shouldn't crash the process on a bad parameter —
    // return a typed error rather than panicking.
    if iters == 0 {
        return Err(BackendError::ExecutionFailure(
            "run_latency needs at least one measured iteration (iters == 0)".to_string(),
        ));
    }
    for _ in 0..warmup {
        backend.run(model, inputs)?;
    }
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        backend.run(model, inputs)?;
        // Saturate rather than truncate (`as u64`): a pathologically long sample
        // stays huge (and fails the latency budget) instead of wrapping to a
        // small, falsely-passing value.
        samples.push(u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX));
    }
    // `iters >= 1` (guarded above) guarantees at least one sample.
    Ok(LatencyStats::from_samples(&samples).expect("iters >= 1 ⇒ non-empty samples"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::mock::MockBackend;
    use std::collections::HashMap;

    fn stats(p99: u64) -> LatencyStats {
        LatencyStats {
            count: 1,
            min_ns: 1,
            p50_ns: 1,
            p99_ns: p99,
            max_ns: p99,
        }
    }

    fn row(precision: PrecisionMode, p99: u64, quality: f64, admissibility: f64) -> EvalRow {
        EvalRow {
            descriptor: BackendDescriptor::Cpu,
            precision,
            model_id: "planner".into(),
            latency: stats(p99),
            quality,
            admissibility,
        }
    }

    #[test]
    fn nearest_rank_percentiles() {
        // 1..=100 → p50 = 50, p99 = 99, max = 100, min = 1.
        let samples: Vec<u64> = (1..=100).collect();
        let s = LatencyStats::from_samples(&samples).unwrap();
        assert_eq!(s.count, 100);
        assert_eq!(s.min_ns, 1);
        assert_eq!(s.p50_ns, 50);
        assert_eq!(s.p99_ns, 99);
        assert_eq!(s.max_ns, 100);
    }

    #[test]
    fn empty_samples_have_no_stats() {
        assert_eq!(LatencyStats::from_samples(&[]), None);
    }

    #[test]
    fn single_sample_stats() {
        let s = LatencyStats::from_samples(&[42]).unwrap();
        assert_eq!((s.min_ns, s.p50_ns, s.p99_ns, s.max_ns), (42, 42, 42, 42));
    }

    #[test]
    fn fp32_reference_passes_quality_and_admissibility_against_itself() {
        let c = PerfContract::illustrative();
        let reference = row(PrecisionMode::FP32, 5_000_000, 0.99, 0.98);
        let v = evaluate(&reference, &reference, &c);
        // Within latency budget, above quality floor, zero admissibility regression.
        assert!(v.passed(), "FP32 reference should pass: {:?}", v.failures);
    }

    #[test]
    fn a_within_budget_int8_row_passes() {
        let c = PerfContract::illustrative();
        let reference = row(PrecisionMode::FP32, 8_000_000, 0.99, 0.98);
        let int8 = row(PrecisionMode::INT8, 3_000_000, 0.95, 0.975); // faster, slight drops
        assert!(evaluate(&int8, &reference, &c).passed());
    }

    #[test]
    fn latency_over_budget_fails() {
        let c = PerfContract::illustrative();
        let reference = row(PrecisionMode::FP32, 5_000_000, 0.99, 0.98);
        let slow = row(PrecisionMode::FP16, 20_000_000, 0.99, 0.98); // p99 > 10 ms
        let v = evaluate(&slow, &reference, &c);
        assert!(!v.passed());
        assert!(matches!(
            v.failures[0],
            ContractFailure::LatencyExceeded { .. }
        ));
    }

    #[test]
    fn quality_below_floor_fails() {
        let c = PerfContract::illustrative();
        let reference = row(PrecisionMode::FP32, 5_000_000, 0.99, 0.98);
        let bad = row(PrecisionMode::INT8, 2_000_000, 0.80, 0.98); // 0.80 < 0.90 floor
        let v = evaluate(&bad, &reference, &c);
        assert!(v.failures.contains(&ContractFailure::QualityBelowFloor {
            quality: 0.80,
            floor: 0.90
        }));
    }

    #[test]
    fn admissibility_regression_beyond_budget_fails() {
        let c = PerfContract::illustrative();
        let reference = row(PrecisionMode::FP32, 5_000_000, 0.99, 0.98);
        // 0.98 - 0.90 = 0.08 > 0.02 budget.
        let regressed = row(PrecisionMode::INT8, 2_000_000, 0.95, 0.90);
        let v = evaluate(&regressed, &reference, &c);
        assert!(matches!(
            v.failures.last().unwrap(),
            ContractFailure::AdmissibilityRegressed { .. }
        ));
    }

    #[test]
    fn more_admissible_than_reference_never_fails_that_axis() {
        let c = PerfContract::illustrative();
        let reference = row(PrecisionMode::FP32, 5_000_000, 0.99, 0.95);
        let better = row(PrecisionMode::INT8, 2_000_000, 0.95, 0.99); // higher admissibility
        let v = evaluate(&better, &reference, &c);
        assert!(v.passed());
    }

    #[test]
    fn non_finite_quality_fails_closed() {
        let c = PerfContract::illustrative();
        let reference = row(PrecisionMode::FP32, 5_000_000, 0.99, 0.98);
        for bad_q in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let bad = row(PrecisionMode::INT8, 2_000_000, bad_q, 0.98);
            let v = evaluate(&bad, &reference, &c);
            assert!(!v.passed(), "non-finite quality {bad_q} must fail closed");
            assert!(v
                .failures
                .iter()
                .any(|f| matches!(f, ContractFailure::QualityBelowFloor { .. })));
        }
    }

    #[test]
    fn non_finite_admissibility_fails_closed() {
        let c = PerfContract::illustrative();
        let reference = row(PrecisionMode::FP32, 5_000_000, 0.99, 0.98);
        for bad_a in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let bad = row(PrecisionMode::INT8, 2_000_000, 0.95, bad_a);
            let v = evaluate(&bad, &reference, &c);
            assert!(
                !v.passed(),
                "non-finite admissibility {bad_a} must fail closed"
            );
            assert!(v
                .failures
                .iter()
                .any(|f| matches!(f, ContractFailure::AdmissibilityRegressed { .. })));
        }
    }

    #[test]
    fn non_finite_contract_thresholds_fail_closed() {
        let reference = row(PrecisionMode::FP32, 5_000_000, 0.99, 0.98);
        let good = row(PrecisionMode::INT8, 2_000_000, 0.95, 0.975);
        // A NaN quality_floor would make `quality < floor` false and silently pass.
        let mut c = PerfContract::illustrative();
        c.quality_floor = f64::NAN;
        let v = evaluate(&good, &reference, &c);
        assert!(!v.passed(), "NaN quality_floor must fail closed");
        assert!(v.failures.contains(&ContractFailure::InvalidInput {
            reason: "contract.quality_floor is non-finite",
        }));
        // A negative or non-finite regression budget is equally malformed.
        for bad_budget in [f64::NAN, f64::INFINITY, -0.01] {
            let mut c = PerfContract::illustrative();
            c.admissibility_regression_budget = bad_budget;
            let v = evaluate(&good, &reference, &c);
            assert!(!v.passed(), "bad budget {bad_budget} must fail closed");
            assert!(v.failures.contains(&ContractFailure::InvalidInput {
                reason: "contract.admissibility_regression_budget is non-finite or negative",
            }));
        }
    }

    #[test]
    fn out_of_range_admissibility_fails_closed() {
        let c = PerfContract::illustrative();
        let reference = row(PrecisionMode::FP32, 5_000_000, 0.99, 0.98);
        // Admissibility is a fraction; 1.5 is not a valid measurement.
        let bad_row = row(PrecisionMode::INT8, 2_000_000, 0.95, 1.5);
        let v = evaluate(&bad_row, &reference, &c);
        assert!(!v.passed(), "admissibility > 1 must fail closed");
        assert!(v.failures.contains(&ContractFailure::InvalidInput {
            reason: "row.admissibility is not a finite fraction in [0,1]",
        }));
        // Same for a negative reference admissibility.
        let bad_ref = row(PrecisionMode::FP32, 5_000_000, 0.99, -0.1);
        let good = row(PrecisionMode::INT8, 2_000_000, 0.95, 0.97);
        let v = evaluate(&good, &bad_ref, &c);
        assert!(!v.passed(), "reference admissibility < 0 must fail closed");
        assert!(v.failures.contains(&ContractFailure::InvalidInput {
            reason: "reference.admissibility is not a finite fraction in [0,1]",
        }));
    }

    #[test]
    fn zero_iters_is_a_typed_error_not_a_panic() {
        let backend = MockBackend::new(HashMap::new(), BackendDescriptor::Cpu);
        let model = backend.load_model("planner").unwrap();
        let inputs = TensorBatch {
            named_tensors: HashMap::new(),
            metadata: HashMap::new(),
        };
        let err = run_latency(&backend, &model, &inputs, 0, 5).unwrap_err();
        assert!(matches!(err, BackendError::ExecutionFailure(_)));
        assert_eq!(backend.call_count(), 0, "no calls when iters == 0");
    }

    #[test]
    fn run_latency_measures_against_a_backend() {
        let mut out = HashMap::new();
        out.insert("scores".to_string(), vec![1.0_f32, 2.0, 3.0]);
        let backend = MockBackend::new(out, BackendDescriptor::Cpu);
        let model = backend.load_model("planner").unwrap();
        let inputs = TensorBatch {
            named_tensors: HashMap::new(),
            metadata: HashMap::new(),
        };

        let s = run_latency(&backend, &model, &inputs, 200, 20).unwrap();
        assert_eq!(s.count, 200);
        assert!(s.p99_ns >= s.p50_ns && s.max_ns >= s.p99_ns);
        // warmup(20) + measured(200) calls hit the backend.
        assert_eq!(backend.call_count(), 220);
    }
}
