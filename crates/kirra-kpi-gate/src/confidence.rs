//! Confidence intervals for the scenario-KPI rates (WP-23 / G-16).
//!
//! A Monte-Carlo corpus makes each KPI a SAMPLE proportion, so a bare point
//! estimate ("`unsafe_miss_rate` = 0.000") hides its sampling uncertainty: at
//! n = 200 a true rate of 0.5 % still reads 0 about a third of the time. This
//! module turns a `(successes, trials)` count into a two-sided confidence
//! interval so the gate can be re-expressed as a bound on the INTERVAL — the
//! statistically honest form of a proportion pass/fail:
//!
//! - a "must be small" rate (`unsafe_miss_rate`) passes iff its UPPER bound is
//!   under the threshold (we are confident the true rate is small);
//! - a "must be large" rate (`hazard_recall`, admissibility) passes iff its
//!   LOWER bound is over the threshold.
//!
//! Bigger n ⇒ tighter interval ⇒ the same policy floor is a stronger claim (the
//! per-PR sampled subset vs the nightly full corpus, WP-23).
//!
//! Two estimators, both pure `f64`, no dependencies:
//! - [`wilson_interval`] — the Wilson score interval (closed-form, well-behaved
//!   near 0/1 and at small n; the recommended default for proportions);
//! - [`clopper_pearson_interval`] — the exact (Beta-quantile) interval, more
//!   conservative; the one to cite when guaranteed coverage is required.
//!
//! Both fail closed on an empty sample (`trials == 0` → the widest interval
//! `[0, 1]` with a `0.0` point), so a lower-bound gate never passes on no
//! evidence.

/// Two-sided z for 95 % coverage (Φ⁻¹(0.975)).
pub const Z_95: f64 = 1.959_963_984_540_054;

/// Default significance for the exact interval (95 % coverage).
pub const ALPHA_95: f64 = 0.05;

/// A closed proportion confidence interval plus its point estimate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConfidenceInterval {
    /// The sample proportion `successes / trials` (`0.0` on an empty sample).
    pub point: f64,
    /// Lower confidence bound, clamped to `[0, 1]`.
    pub lo: f64,
    /// Upper confidence bound, clamped to `[0, 1]`.
    pub hi: f64,
}

impl ConfidenceInterval {
    /// The fail-closed maximal-uncertainty interval for an empty sample.
    const EMPTY: Self = Self { point: 0.0, lo: 0.0, hi: 1.0 };
}

/// Wilson score interval for `successes`/`trials` at the two-sided `z`
/// (e.g. [`Z_95`]). Fail-closed on `trials == 0` → `[0, 1]`.
#[must_use]
pub fn wilson_interval(successes: u64, trials: u64, z: f64) -> ConfidenceInterval {
    if trials == 0 {
        return ConfidenceInterval::EMPTY;
    }
    let n = trials as f64;
    let p = (successes.min(trials)) as f64 / n;
    let z2 = z * z;
    let denom = 1.0 + z2 / n;
    let center = (p + z2 / (2.0 * n)) / denom;
    let margin = (z / denom) * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
    ConfidenceInterval {
        point: p,
        lo: (center - margin).max(0.0),
        hi: (center + margin).min(1.0),
    }
}

/// Clopper–Pearson (exact) interval at two-sided significance `alpha`
/// (e.g. [`ALPHA_95`] for 95 % coverage). Uses the Beta-quantile identities
/// `lo = BetaInv(α/2; k, n−k+1)`, `hi = BetaInv(1−α/2; k+1, n−k)`, with the
/// degenerate ends closed in form (`k = 0 ⇒ lo = 0`, `k = n ⇒ hi = 1`).
/// Fail-closed on `trials == 0` → `[0, 1]`.
#[must_use]
pub fn clopper_pearson_interval(successes: u64, trials: u64, alpha: f64) -> ConfidenceInterval {
    if trials == 0 {
        return ConfidenceInterval::EMPTY;
    }
    let n = trials;
    let k = successes.min(n);
    let point = k as f64 / n as f64;
    let lo = if k == 0 {
        0.0
    } else {
        beta_quantile(alpha / 2.0, k as f64, (n - k + 1) as f64)
    };
    let hi = if k == n {
        1.0
    } else {
        beta_quantile(1.0 - alpha / 2.0, (k + 1) as f64, (n - k) as f64)
    };
    ConfidenceInterval { point, lo, hi }
}

/// The `p`-quantile of Beta(a, b): the `x` with `I_x(a, b) = p`. Bisection on the
/// monotone regularized incomplete beta — 100 iterations bracket `x` to well
/// under `f64` precision.
fn beta_quantile(p: f64, a: f64, b: f64) -> f64 {
    let mut lo = 0.0f64;
    let mut hi = 1.0f64;
    for _ in 0..100 {
        let mid = 0.5 * (lo + hi);
        if regularized_incomplete_beta(mid, a, b) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// Regularized incomplete beta `I_x(a, b)` — the CDF of Beta(a, b) — via the
/// Numerical-Recipes continued-fraction (`betai`/`betacf`).
fn regularized_incomplete_beta(x: f64, a: f64, b: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let ln_beta = ln_gamma(a) + ln_gamma(b) - ln_gamma(a + b);
    let front = (a * x.ln() + b * (1.0 - x).ln() - ln_beta).exp();
    // Use the continued fraction for whichever tail converges faster.
    if x < (a + 1.0) / (a + b + 2.0) {
        front * betacf(x, a, b) / a
    } else {
        1.0 - front * betacf(1.0 - x, b, a) / b
    }
}

/// The Lentz-iteration continued fraction for [`regularized_incomplete_beta`].
fn betacf(x: f64, a: f64, b: f64) -> f64 {
    const TINY: f64 = 1e-30;
    const EPS: f64 = 1e-15;
    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..300 {
        let m_f = f64::from(m);
        let m2 = 2.0 * m_f;
        // Even step.
        let aa = m_f * (b - m_f) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        h *= d * c;
        // Odd step.
        let aa = -(a + m_f) * (qab + m_f) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < EPS {
            break;
        }
    }
    h
}

/// Natural log of the gamma function (Lanczos approximation, g = 7, accurate to
/// ~1e-15 for the positive arguments this module uses).
fn ln_gamma(x: f64) -> f64 {
    const G: f64 = 7.0;
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        // Reflection: ln Γ(x) = ln(π) − ln(sin πx) − ln Γ(1−x). Kept so the
        // function is total; the CP path only ever passes x ≥ 1.
        core::f64::consts::PI.ln() - (core::f64::consts::PI * x).sin().ln() - ln_gamma(1.0 - x)
    } else {
        let x = x - 1.0;
        let mut a = C[0];
        let t = x + G + 0.5;
        for (i, &c) in C.iter().enumerate().skip(1) {
            a += c / (x + i as f64);
        }
        0.5 * (2.0 * core::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn wilson_stays_in_unit_interval_and_brackets_interior_points() {
        for &(k, n) in &[(0u64, 10u64), (1, 10), (5, 10), (8, 10), (10, 10), (250, 300)] {
            let ci = wilson_interval(k, n, Z_95);
            assert!(ci.lo >= 0.0 && ci.hi <= 1.0 && ci.lo <= ci.hi, "clamped/ordered: {ci:?}");
            // The Wilson interval brackets the point estimate for INTERIOR k; at the
            // 0/n extremes the score interval legitimately pulls toward ½ and need
            // not contain the MLE (a known, correct property).
            if k > 0 && k < n {
                assert!(ci.lo <= ci.point && ci.point <= ci.hi, "brackets interior point: {ci:?}");
            }
        }
    }

    #[test]
    fn empty_sample_fails_closed_to_full_width() {
        let w = wilson_interval(0, 0, Z_95);
        let cp = clopper_pearson_interval(0, 0, ALPHA_95);
        assert_eq!((w.point, w.lo, w.hi), (0.0, 0.0, 1.0));
        assert_eq!((cp.point, cp.lo, cp.hi), (0.0, 0.0, 1.0));
    }

    #[test]
    fn interval_narrows_as_n_grows() {
        // Same 80 % proportion at n = 10 vs n = 1000 — the wider n is tighter.
        let small = wilson_interval(8, 10, Z_95);
        let large = wilson_interval(800, 1000, Z_95);
        assert!(large.hi - large.lo < small.hi - small.lo);
    }

    #[test]
    fn clopper_pearson_degenerate_ends_match_closed_form() {
        // k = 0, n = 10, 95 %: lo = 0, hi = 1 − (α/2)^(1/n).
        let cp0 = clopper_pearson_interval(0, 10, ALPHA_95);
        assert_eq!(cp0.lo, 0.0);
        let expect_hi = 1.0 - 0.025_f64.powf(0.1);
        assert!(approx(cp0.hi, expect_hi, 1e-6), "k=0 hi {} vs {expect_hi}", cp0.hi);
        // k = n = 10: lo = (α/2)^(1/n), hi = 1.
        let cpn = clopper_pearson_interval(10, 10, ALPHA_95);
        assert_eq!(cpn.hi, 1.0);
        let expect_lo = 0.025_f64.powf(0.1);
        assert!(approx(cpn.lo, expect_lo, 1e-6), "k=n lo {} vs {expect_lo}", cpn.lo);
    }

    #[test]
    fn clopper_pearson_interior_matches_reference() {
        // k = 8, n = 10 at 95 % — the textbook Clopper–Pearson value.
        let cp = clopper_pearson_interval(8, 10, ALPHA_95);
        assert!(approx(cp.lo, 0.443_9, 2e-3), "lo {}", cp.lo);
        assert!(approx(cp.hi, 0.974_8, 2e-3), "hi {}", cp.hi);
    }

    #[test]
    fn clopper_pearson_is_more_conservative_than_wilson() {
        // Exact coverage is wider than the score interval for an interior point.
        let w = wilson_interval(8, 10, Z_95);
        let cp = clopper_pearson_interval(8, 10, ALPHA_95);
        assert!(cp.lo <= w.lo, "CP lo {} should be ≤ Wilson lo {}", cp.lo, w.lo);
        assert!(cp.hi >= w.hi, "CP hi {} should be ≥ Wilson hi {}", cp.hi, w.hi);
    }

    #[test]
    fn regularized_incomplete_beta_is_a_cdf() {
        // Monotone in x, pinned at the ends.
        assert_eq!(regularized_incomplete_beta(0.0, 2.0, 3.0), 0.0);
        assert_eq!(regularized_incomplete_beta(1.0, 2.0, 3.0), 1.0);
        let a = regularized_incomplete_beta(0.3, 2.0, 3.0);
        let b = regularized_incomplete_beta(0.6, 2.0, 3.0);
        assert!(a < b, "monotone: {a} !< {b}");
        // I_x(1,1) is the uniform CDF, i.e. x.
        assert!(approx(regularized_incomplete_beta(0.42, 1.0, 1.0), 0.42, 1e-9));
    }
}
