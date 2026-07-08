//! WP-22 (MGA G-3, software half) — Extreme-Value-Theory / MBPTA tail fitting.
//!
//! Measurement-based probabilistic timing analysis (MBPTA) fits the TAIL of an
//! execution-time sample to a Generalized Pareto Distribution (GPD) via the
//! peaks-over-threshold (POT) method, then extrapolates a **pWCET** — an execution
//! time exceeded with only a target (tiny) probability `p` — instead of trusting a
//! high-water mark that a longer campaign might exceed.
//!
//! This module provides the ANALYSIS primitives:
//! - [`fit_gpd_pot`] — GPD parameters `(ξ, σ)` from threshold exceedances
//!   (method-of-moments, valid for `ξ < 0.5`; fail-closed on degenerate input);
//! - [`pwcet_return_level`] — the POT return-level quantile `x_p`;
//! - [`estimate_pwcet`] — threshold selection + fit + return level + the i.i.d. /
//!   stationarity diagnostics MBPTA requires before a fit is trustworthy;
//! - [`lag1_autocorrelation`] / [`stationarity_split_mean_ratio`] — the
//!   representativity checks (an unmet check makes the pWCET INDICATIVE, not WCET);
//! - [`pwcet_converged`] — the convergence criterion over a growing campaign.
//!
//! **Normative (WCET_MEASUREMENT_METHODOLOGY.md §4):** a pWCET fitted from HOST
//! samples is INDICATIVE, never a WCET claim — the evidence class is fixed by the
//! measurement ENVIRONMENT (`MeasurementEnv::is_certified_wcet`), not by the
//! statistics. This module computes the curve; it never certifies it.
//!
//! Feature-gated (`evt`, which pulls `std` for the f64 math) so the certifiable
//! `no_std`, zero-alloc, integer-only core is compiled out of the production target
//! entirely. Also compiled under `test` so the workspace suite exercises it.

// The crate is `#![no_std]`; the `evt` feature links `std` (via the crate-root
// `extern crate std`), but the std prelude is not auto-imported, so name the heap
// types explicitly. Under `test` they come from the prelude, so the import is
// non-test-only to avoid a redundant-import warning.
#[cfg(not(test))]
use std::vec::Vec;

/// Why an EVT/MBPTA computation could not proceed — all fail-closed (a bad fit is
/// refused, never fabricated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvtError {
    /// Fewer exceedances than the minimum needed for a stable tail fit.
    TooFewExceedances { got: usize, need: usize },
    /// A non-finite (NaN/∞) sample or parameter was encountered.
    NonFinite,
    /// The exceedance variance is ~0 (all exceedances equal) — no tail to fit.
    DegenerateVariance,
    /// The threshold quantile is not in `(0, 1)`, or the target probability is not
    /// a small positive value below the threshold exceedance rate.
    InvalidParameter,
    /// The fit implied `ξ ≥ 0.5` (infinite variance) — the method-of-moments
    /// estimator is not valid; a heavier-tailed estimator / more data is required.
    TailTooHeavy,
}

impl core::fmt::Display for EvtError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EvtError::TooFewExceedances { got, need } => {
                write!(f, "too few exceedances for a stable GPD fit: {got} < {need}")
            }
            EvtError::NonFinite => write!(f, "non-finite sample or parameter"),
            EvtError::DegenerateVariance => write!(f, "exceedance variance ~0 — no tail to fit"),
            EvtError::InvalidParameter => write!(f, "invalid threshold quantile or target probability"),
            EvtError::TailTooHeavy => write!(f, "fit implied ξ ≥ 0.5 (method-of-moments invalid)"),
        }
    }
}
impl std::error::Error for EvtError {}

/// The minimum number of threshold exceedances for a trustworthy GPD fit. MBPTA
/// practice puts this in the tens–hundreds; 30 is a conservative floor below which
/// the fit is refused rather than reported.
pub const MIN_EXCEEDANCES: usize = 30;

/// Fitted Generalized Pareto Distribution tail parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpdParams {
    /// Shape ξ. `>0` heavy (bounded-above only asymptotically), `=0` exponential,
    /// `<0` short (finite upper endpoint at `u − σ/ξ`).
    pub xi: f64,
    /// Scale σ (`> 0`).
    pub sigma: f64,
}

/// Fit a GPD to threshold `exceedances` (each `= sample − threshold ≥ 0`) via the
/// method of moments: `ξ = ½(1 − m²/s²)`, `σ = ½·m·(1 + m²/s²)`, where `m`, `s²`
/// are the exceedance mean and variance. Valid for `ξ < 0.5`; fail-closed on too
/// few exceedances, non-finite input, degenerate variance, or a too-heavy tail.
pub fn fit_gpd_pot(exceedances: &[f64]) -> Result<GpdParams, EvtError> {
    if exceedances.len() < MIN_EXCEEDANCES {
        return Err(EvtError::TooFewExceedances { got: exceedances.len(), need: MIN_EXCEEDANCES });
    }
    if exceedances.iter().any(|x| !x.is_finite()) {
        return Err(EvtError::NonFinite);
    }
    let n = exceedances.len() as f64;
    let mean = exceedances.iter().sum::<f64>() / n;
    // Sample variance (unbiased, n−1). Exceedances are ≥ 0; mean > 0 for a real tail.
    let var = exceedances.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / (n - 1.0);
    if !mean.is_finite() || !var.is_finite() {
        return Err(EvtError::NonFinite);
    }
    if var <= 0.0 || mean <= 0.0 {
        return Err(EvtError::DegenerateVariance);
    }
    let r = (mean * mean) / var; // = 1 − 2ξ for a true GPD
    let xi = 0.5 * (1.0 - r);
    let sigma = 0.5 * mean * (1.0 + r);
    if xi >= 0.5 {
        return Err(EvtError::TailTooHeavy);
    }
    if sigma <= 0.0 || !sigma.is_finite() || !xi.is_finite() {
        return Err(EvtError::NonFinite);
    }
    Ok(GpdParams { xi, sigma })
}

/// The POT return level: the value `x_p` exceeded with probability `target_prob`,
/// given the fitted GPD, the `threshold` `u`, and the exceedance rate
/// `n_exceed / n_total` (`ζ`). Fail-closed unless `0 < target_prob < ζ` (the target
/// must be rarer than the threshold — we are extrapolating INTO the tail).
///
/// `x_p = u + (σ/ξ)·[(p/ζ)^(−ξ) − 1]` for `ξ ≠ 0`; the `ξ → 0` limit is
/// `u + σ·ln(ζ/p)` (an exponential tail).
pub fn pwcet_return_level(
    params: &GpdParams,
    threshold: f64,
    n_total: usize,
    n_exceed: usize,
    target_prob: f64,
) -> Result<f64, EvtError> {
    if !threshold.is_finite() || !params.xi.is_finite() || !params.sigma.is_finite() {
        return Err(EvtError::NonFinite);
    }
    if n_total == 0 || n_exceed == 0 || n_exceed > n_total {
        return Err(EvtError::InvalidParameter);
    }
    let zeta = n_exceed as f64 / n_total as f64;
    if !(target_prob > 0.0 && target_prob < zeta) {
        return Err(EvtError::InvalidParameter);
    }
    let x = if params.xi.abs() < 1e-9 {
        threshold + params.sigma * (zeta / target_prob).ln()
    } else {
        threshold + (params.sigma / params.xi) * ((target_prob / zeta).powf(-params.xi) - 1.0)
    };
    if !x.is_finite() {
        return Err(EvtError::NonFinite);
    }
    Ok(x)
}

/// A complete pWCET estimate + the MBPTA representativity diagnostics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PwcetEstimate {
    /// The estimated pWCET (execution time exceeded with prob. `target_prob`).
    pub pwcet: f64,
    /// The POT threshold used (`u`).
    pub threshold: f64,
    pub n_total: usize,
    pub n_exceed: usize,
    /// Fitted GPD tail.
    pub gpd: GpdParams,
    /// Lag-1 autocorrelation of the full sample (≈0 supports i.i.d.).
    pub lag1_autocorr: f64,
    /// First-half / second-half mean ratio (≈1 supports stationarity).
    pub stationarity_ratio: f64,
    pub target_prob: f64,
}

/// End-to-end pWCET from a raw execution-time `samples` set: select the POT
/// threshold at `threshold_quantile` (e.g. 0.95), fit the GPD to the exceedances,
/// compute the return level at `target_prob`, and attach the i.i.d. / stationarity
/// diagnostics. Fail-closed throughout. `samples` need not be sorted.
pub fn estimate_pwcet(
    samples: &[f64],
    threshold_quantile: f64,
    target_prob: f64,
) -> Result<PwcetEstimate, EvtError> {
    if !(threshold_quantile > 0.0 && threshold_quantile < 1.0) {
        return Err(EvtError::InvalidParameter);
    }
    if samples.iter().any(|x| !x.is_finite()) {
        return Err(EvtError::NonFinite);
    }
    if samples.len() < MIN_EXCEEDANCES {
        return Err(EvtError::TooFewExceedances { got: samples.len(), need: MIN_EXCEEDANCES });
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap()); // finite-checked above
    let threshold = empirical_quantile(&sorted, threshold_quantile);
    let exceedances: Vec<f64> =
        samples.iter().filter(|&&x| x > threshold).map(|&x| x - threshold).collect();
    let gpd = fit_gpd_pot(&exceedances)?;
    let n_exceed = exceedances.len();
    let pwcet = pwcet_return_level(&gpd, threshold, samples.len(), n_exceed, target_prob)?;
    Ok(PwcetEstimate {
        pwcet,
        threshold,
        n_total: samples.len(),
        n_exceed,
        gpd,
        lag1_autocorr: lag1_autocorrelation(samples),
        stationarity_ratio: stationarity_split_mean_ratio(samples),
        target_prob,
    })
}

/// The empirical quantile of `sorted` (ascending) at `q ∈ (0,1)` — nearest-rank.
fn empirical_quantile(sorted: &[f64], q: f64) -> f64 {
    debug_assert!(!sorted.is_empty());
    let rank = (q * sorted.len() as f64).ceil() as usize;
    let idx = rank.clamp(1, sorted.len()) - 1;
    sorted[idx]
}

/// Lag-1 autocorrelation — an i.i.d. indicator. Near 0 supports independence; a
/// large magnitude signals serial correlation (a monotone trend or periodicity),
/// which makes a POT fit unrepresentative. Returns 0 for a degenerate (zero
/// variance) or too-short series.
#[must_use]
pub fn lag1_autocorrelation(samples: &[f64]) -> f64 {
    let n = samples.len();
    if n < 2 {
        return 0.0;
    }
    let mean = samples.iter().sum::<f64>() / n as f64;
    let denom: f64 = samples.iter().map(|x| (x - mean) * (x - mean)).sum();
    if denom <= 0.0 {
        return 0.0;
    }
    let numer: f64 =
        (0..n - 1).map(|i| (samples[i] - mean) * (samples[i + 1] - mean)).sum();
    numer / denom
}

/// First-half / second-half mean ratio — a coarse stationarity indicator. Near 1
/// supports a stable mean; far from 1 signals drift (warm-up, thermal throttling).
/// Returns 1.0 for a degenerate series.
#[must_use]
pub fn stationarity_split_mean_ratio(samples: &[f64]) -> f64 {
    let n = samples.len();
    if n < 2 {
        return 1.0;
    }
    let mid = n / 2;
    let first = samples[..mid].iter().sum::<f64>() / mid as f64;
    let second = samples[mid..].iter().sum::<f64>() / (n - mid) as f64;
    if first == 0.0 {
        return if second == 0.0 { 1.0 } else { f64::INFINITY };
    }
    second / first
}

/// Convergence criterion: a growing campaign's pWCET estimates have CONVERGED when
/// the last estimate's relative change from the previous is within `rel_tol`. Fewer
/// than two estimates is "not yet converged".
#[must_use]
pub fn pwcet_converged(estimates: &[f64], rel_tol: f64) -> bool {
    let n = estimates.len();
    if n < 2 {
        return false;
    }
    let (prev, last) = (estimates[n - 2], estimates[n - 1]);
    if !prev.is_finite() || !last.is_finite() || prev == 0.0 {
        return false;
    }
    ((last - prev) / prev).abs() <= rel_tol
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic GPD quantile-grid sample: `y_i` at evenly spaced probabilities,
    /// whose empirical moments converge to the GPD's — so method-of-moments recovers
    /// `(ξ, σ)`. No RNG (the crate is zero-dependency).
    fn gpd_quantile_grid(xi: f64, sigma: f64, n: usize) -> Vec<f64> {
        (1..=n)
            .map(|i| {
                let p = i as f64 / (n as f64 + 1.0);
                if xi.abs() < 1e-12 {
                    -sigma * (1.0 - p).ln()
                } else {
                    (sigma / xi) * ((1.0 - p).powf(-xi) - 1.0)
                }
            })
            .collect()
    }

    /// Exponential(λ) quantile grid (a GPD with ξ=0): `x_i = −ln(1−p_i)/λ`.
    fn exponential_grid(lambda: f64, n: usize) -> Vec<f64> {
        (1..=n).map(|i| {
            let p = i as f64 / (n as f64 + 1.0);
            -(1.0 - p).ln() / lambda
        }).collect()
    }

    #[test]
    fn fit_recovers_known_gpd_params() {
        let sample = gpd_quantile_grid(0.2, 1.0, 2000);
        let g = fit_gpd_pot(&sample).unwrap();
        // A finite quantile grid truncates the extreme upper tail, so the empirical
        // variance is biased slightly low → ξ recovered a touch under 0.2 (0.178
        // here). A ~15% tolerance is the honest recovery bound (more data tightens
        // it — which is itself the convergence property `pwcet_converged` checks).
        assert!((g.xi - 0.2).abs() < 0.03, "xi ~ 0.2, got {}", g.xi);
        assert!((g.sigma - 1.0).abs() < 0.03, "sigma ~ 1.0, got {}", g.sigma);
    }

    #[test]
    fn fit_recovers_exponential_tail_near_zero_xi() {
        let sample = gpd_quantile_grid(0.0, 2.0, 2000);
        let g = fit_gpd_pot(&sample).unwrap();
        assert!(g.xi.abs() < 0.03, "exponential tail → xi ~ 0, got {}", g.xi);
        assert!((g.sigma - 2.0).abs() < 0.05, "sigma ~ 2.0, got {}", g.sigma);
    }

    #[test]
    fn pwcet_return_level_matches_the_analytic_exponential_quantile() {
        // Full sample ~ Exp(λ=1). Threshold at the 90th percentile; exceedances are
        // Exp(1) again (memoryless), so the POT return level at p must match the true
        // Exp quantile x_p = −ln(p)/λ.
        // Exact FORMULA check (no fit): for a true exponential tail (ξ=0, σ=1) with
        // threshold u at the 90th percentile of Exp(1) and ζ=0.1, the POT return
        // level must equal the analytic Exp quantile to floating precision.
        let u = -(0.10_f64).ln(); // 90th pct of Exp(1) = 2.302585
        let p = 1e-5;
        let exact = pwcet_return_level(&GpdParams { xi: 0.0, sigma: 1.0 }, u, 1000, 100, p).unwrap();
        let analytic = -(p).ln(); // 11.512925
        assert!((exact - analytic).abs() < 1e-6, "formula {exact} == analytic {analytic}");

        // End-to-end FIT + extrapolation. Target 1e-5 is rarer than any observed
        // sample (the grid's rarest is ~2e-4), so the pWCET genuinely extrapolates
        // BEYOND the high-water mark — the point of MBPTA over trusting the HWM. The
        // finite quantile grid biases the fit, so a ~8% band is the honest end-to-end
        // bound (the exact formula above pins the math; this pins the pipeline).
        let samples = exponential_grid(1.0, 5000);
        let est = estimate_pwcet(&samples, 0.90, p).unwrap();
        assert!(
            (est.pwcet - analytic).abs() / analytic < 0.08,
            "end-to-end pWCET {} within 8% of analytic {analytic}",
            est.pwcet
        );
        let hwm = samples.iter().cloned().fold(f64::MIN, f64::max);
        assert!(est.pwcet > hwm, "pWCET {} must exceed the high-water mark {hwm}", est.pwcet);
    }

    #[test]
    fn fit_is_fail_closed() {
        assert!(matches!(fit_gpd_pot(&[1.0; 5]), Err(EvtError::TooFewExceedances { .. })));
        let mut bad = gpd_quantile_grid(0.1, 1.0, 100);
        bad[0] = f64::NAN;
        assert_eq!(fit_gpd_pot(&bad), Err(EvtError::NonFinite));
        assert_eq!(fit_gpd_pot(&[5.0; 50]), Err(EvtError::DegenerateVariance));
    }

    #[test]
    fn return_level_requires_target_rarer_than_the_threshold() {
        let g = GpdParams { xi: 0.1, sigma: 1.0 };
        // ζ = 100/1000 = 0.1; a target ≥ ζ is not an extrapolation → refused.
        assert!(pwcet_return_level(&g, 10.0, 1000, 100, 0.2).is_err());
        assert!(pwcet_return_level(&g, 10.0, 1000, 100, 0.0).is_err());
        assert!(pwcet_return_level(&g, 10.0, 1000, 100, 0.01).is_ok());
    }

    #[test]
    fn iid_and_stationarity_diagnostics() {
        // A strictly increasing ramp: strong positive lag-1 autocorrelation + a
        // second-half mean well above the first (non-stationary).
        let ramp: Vec<f64> = (0..1000).map(|i| i as f64).collect();
        assert!(lag1_autocorrelation(&ramp) > 0.9, "a ramp is highly autocorrelated");
        assert!(stationarity_split_mean_ratio(&ramp) > 2.0, "a ramp's mean drifts up");

        // An alternating sequence: strong NEGATIVE lag-1 autocorrelation, stationary mean.
        let alt: Vec<f64> = (0..1000).map(|i| if i % 2 == 0 { 1.0 } else { -1.0 }).collect();
        assert!(lag1_autocorrelation(&alt) < -0.9, "alternating → strong negative autocorr");
        assert!((stationarity_split_mean_ratio(&alt)).abs() < 5.0, "alternating mean is ~stationary");
    }

    #[test]
    fn convergence_detects_a_stabilized_series() {
        assert!(!pwcet_converged(&[10.0], 0.01), "one estimate is never converged");
        assert!(!pwcet_converged(&[10.0, 12.0], 0.01), "20% change is not converged");
        assert!(pwcet_converged(&[10.0, 10.05], 0.01), "0.5% change is within 1% tol");
        assert!(!pwcet_converged(&[0.0, 1.0], 0.01), "a zero baseline is not converged");
    }
}
