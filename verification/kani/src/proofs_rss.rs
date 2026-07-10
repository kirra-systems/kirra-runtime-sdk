//! EP-15 proofs — the parko RSS longitudinal primitive
//! (`parko/crates/parko-core/src/rss.rs`).
//!
//! The star property is R2: `longitudinal_safe_distance(·, 0, …)` is
//! MONOTONICALLY NON-DECREASING in the closing speed. The shipped
//! `occlusion_limited_speed` (RSS rule iv) finds its speed cap by BISECTION and
//! its correctness argument explicitly relies on exactly this monotonicity —
//! so this proof underwrites a shipped algorithm, not just a formula.
//!
//! Per the EP-15 plan the quantification is over INTEGER-SCALED grids (speeds
//! in 0.01 m/s steps, parameters in 0.1-unit steps): the theorem covers every
//! grid point in the operational box — a countable but exhaustive domain the
//! bit-precise CBMC float model can decide, scoped honestly around the
//! nonlinear-float limits of full-real-domain monotonicity.
//!
//! LANE SPLIT: R1/R3 run per-PR in the blocking `kani-proofs` lane; R2 (the
//! relational two-evaluation instance) is behind the `deep-proofs` feature and
//! runs in the weekly `kani-deep-weekly` lane — its final UNSAT solve exceeds
//! the per-PR job budget on both CaDiCaL and kissat. Its per-PR gate is the
//! concrete mirror below (full speed-grid walk across all four param axes).
//!
//! Properties (cited from `docs/safety/GOVERNOR_INTEGRITY_EVIDENCE.md` §2):
//!  * R1 fail-closed totality (SG9): for EVERY f64 bit pattern in every
//!    argument — NaN, ±Inf, negatives, zeros, denormals — the result is finite
//!    and ≥ 0 (never NaN, never negative, never Inf). The "misconfigured brake
//!    reads as no-gap-required" failure mode is machine-checked impossible.
//!  * R2 closing-speed monotonicity on the integer-scaled operational grid
//!    (the `occlusion_limited_speed` bisection precondition).
//!  * R3 an invalid (non-positive / non-finite) brake parameter returns
//!    EXACTLY `RSS_FAILSAFE_DISTANCE_M` — the unreachable-gap sentinel, not a
//!    silent 0.

#[allow(unused_imports)]
use crate::rss::{longitudinal_safe_distance, RSS_FAILSAFE_DISTANCE_M};

/// Integer-scaled operational grids (EP-15 "integer-scaled forms").
/// Speeds: 0 ..= 60.00 m/s in 0.01 steps (2× the 22.35 m/s ODD cap, covering
/// closing-speed sums). Params: 0.1 ..= 25.5 in 0.1 steps.
#[cfg(all(kani, feature = "deep-proofs"))]
fn grid_speed(raw: u16) -> f64 {
    f64::from(raw) * 0.01
}
#[cfg(all(kani, feature = "deep-proofs"))]
fn grid_param(raw: u8) -> f64 {
    f64::from(raw.max(1)) * 0.1
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// R1 — SG9 totality over the FULL f64 domain: the result is finite and
    /// non-negative for every possible bit pattern of every argument.
    #[kani::proof]
    fn r1_longitudinal_totality_full_domain() {
        let d = longitudinal_safe_distance(
            kani::any(),
            kani::any(),
            kani::any(),
            kani::any(),
            kani::any(),
            kani::any(),
        );
        assert!(d.is_finite(), "never NaN / never Inf");
        assert!(d >= 0.0, "a required separation is never negative");
    }

    /// R2 — closing-speed monotonicity on the integer-scaled operational grid:
    /// for every pair of grid speeds v1 ≤ v2 and every grid parameter tuple,
    /// the required distance never DECREASES as the closing speed grows. This
    /// is the precondition `occlusion_limited_speed`'s bisection relies on
    /// (`lead_vel = 0`, exactly its call shape).
    // DEEP LANE (weekly), not per-PR. With the squares spelled as exact IEEE
    // multiplications (rss.rs dropped `__builtin_powi`, whose cheap over-
    // approximation both admitted a spurious counterexample AND made this
    // relational two-evaluation proof artificially easy), the instance
    // exceeds the per-PR 45-min job budget on BOTH default CaDiCaL and
    // kissat: kissat's first (SAT, reachability) solve returns in ~20 s, but
    // the final UNSAT solve — the one that actually proves the property — ran
    // 43+ min without returning before the job timeout (CI runs of PR #888).
    // The property itself is believed true and structurally so: with
    // `lead_vel = 0` every step of `longitudinal_safe_distance` on the grid
    // is a composition of IEEE-monotone operations (round-to-nearest
    // preserves weak inequalities), and the BLOCKING per-PR mirror below
    // walks the full speed grid across all four parameter axes. The
    // machine-checked full-lattice proof runs in the `kani-deep-weekly`
    // lane with a multi-hour budget.
    #[cfg(feature = "deep-proofs")]
    #[kani::proof]
    #[kani::solver(kissat)]
    fn r2_longitudinal_monotone_in_closing_speed_on_grid() {
        let v1_raw: u16 = kani::any();
        let v2_raw: u16 = kani::any();
        kani::assume(v1_raw <= 6_000 && v2_raw <= 6_000); // ≤ 60.00 m/s
        kani::assume(v1_raw <= v2_raw);

        let rho = grid_param(kani::any()); // reaction time 0.1 ..= 25.5 s
        let a_max = grid_param(kani::any()); // response accel
        let b_min = grid_param(kani::any()); // ego committed brake
        let b_max = grid_param(kani::any()); // lead worst-case brake

        let d1 = longitudinal_safe_distance(grid_speed(v1_raw), 0.0, rho, a_max, b_min, b_max);
        let d2 = longitudinal_safe_distance(grid_speed(v2_raw), 0.0, rho, a_max, b_min, b_max);
        assert!(
            d1 <= d2,
            "required distance is non-decreasing in closing speed on the grid"
        );
    }

    /// R3 — an invalid ego-brake parameter (zero, negative, or non-finite)
    /// returns EXACTLY the unreachable-gap sentinel, for all other arguments.
    #[kani::proof]
    fn r3_invalid_brake_returns_failsafe_exactly() {
        let brake_min: f64 = kani::any();
        kani::assume(!(brake_min.is_finite() && brake_min > 0.0));

        let d = longitudinal_safe_distance(
            kani::any(),
            kani::any(),
            kani::any(),
            kani::any(),
            brake_min,
            kani::any(),
        );
        assert_eq!(d, RSS_FAILSAFE_DISTANCE_M);
    }
}

// ---------------------------------------------------------------------------
// Concrete mirrors under plain `cargo test`.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod mirrors {
    use super::*;

    #[test]
    fn r1_mirror_totality_probes() {
        let probes = [
            0.0,
            -0.0,
            1.0,
            -1.0,
            f64::MIN_POSITIVE,
            f64::MAX,
            f64::MIN,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ];
        for &a in &probes {
            for &b in &probes {
                let d = longitudinal_safe_distance(a, b, 0.5, 3.0, 4.0, 8.0);
                assert!(d.is_finite() && d >= 0.0, "ego={a} lead={b} -> {d}");
                let d = longitudinal_safe_distance(10.0, 0.0, a, b, 4.0, 8.0);
                assert!(d.is_finite() && d >= 0.0, "rho={a} amax={b} -> {d}");
            }
        }
    }

    /// Walk the whole 0..=60.00 m/s speed grid and assert pairwise (adjacent)
    /// monotonicity — the transitive closure of what the R2 proof checks for
    /// arbitrary pairs — at the given parameter tuple.
    fn assert_monotone_along_speed_grid(rho: f64, a_max: f64, b_min: f64, b_max: f64) {
        let mut prev = longitudinal_safe_distance(0.0, 0.0, rho, a_max, b_min, b_max);
        for raw in 1..=6_000u16 {
            let d =
                longitudinal_safe_distance(f64::from(raw) * 0.01, 0.0, rho, a_max, b_min, b_max);
            assert!(
                prev <= d,
                "non-monotone step at raw={raw} (rho={rho} a_max={a_max} b_min={b_min} b_max={b_max})"
            );
            prev = d;
        }
    }

    #[test]
    fn r2_mirror_monotone_along_grid() {
        assert_monotone_along_speed_grid(0.5, 3.0, 4.0, 8.0);
    }

    /// This crate's per-PR stand-in for the deep-lane R2 proof: sweep each of
    /// the four parameter axes through its ENTIRE 0.1 ..= 25.5 grid (the
    /// others frozen at the shaped tuple) and walk the full speed grid at
    /// every point — ~6.1 M evaluations, exhaustive per-axis, leaving only
    /// cross-axis interaction to the weekly full-lattice Kani run.
    #[test]
    fn r2_mirror_monotone_per_param_axis() {
        let frozen = (0.5, 3.0, 4.0, 8.0);
        for raw in 1..=255u16 {
            let p = f64::from(raw) * 0.1;
            assert_monotone_along_speed_grid(p, frozen.1, frozen.2, frozen.3);
            assert_monotone_along_speed_grid(frozen.0, p, frozen.2, frozen.3);
            assert_monotone_along_speed_grid(frozen.0, frozen.1, p, frozen.3);
            assert_monotone_along_speed_grid(frozen.0, frozen.1, frozen.2, p);
        }
    }

    #[test]
    fn r3_mirror_invalid_brake_is_failsafe() {
        for bad in [0.0, -0.0, -4.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                longitudinal_safe_distance(10.0, 5.0, 0.5, 3.0, bad, 8.0),
                RSS_FAILSAFE_DISTANCE_M
            );
        }
    }
}
