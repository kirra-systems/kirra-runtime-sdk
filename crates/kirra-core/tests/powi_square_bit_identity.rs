//! `v.powi(2)` vs `v * v` — bit-exactness evidence for the #1242 proof model.
//!
//! The P6 bicycle model in the frozen kinematics talisman computes
//! `let v2 = v.powi(2);`. `powi` lowers to the `llvm.powi.f64` intrinsic, which
//! CBMC models as the uninterpreted builtin `__builtin_powi`: inside a Kani
//! proof `v2` is therefore an ARBITRARY double, including NaN. That single
//! opaque value is enough to make `v2 > 1e-6` undecidable, which puts the P6
//! `tan` call on every path and fails the harness with
//! `call to foreign "C" function 'tan' is not currently supported` — even for
//! commands whose enforced speed is far below the P6 entry threshold. (Measured:
//! `k3_diag_ceiling_below_p6_entry_threshold`, whose ceiling is 0.0005 m/s so
//! `v2 = 2.5e-7`, failed on exactly that check and passes once `v2` is a plain
//! multiplication.)
//!
//! The proofs therefore stub `f64::powi` with a squaring model. That stub is
//! only sound if the two forms agree BIT-for-bit on the shipped toolchain,
//! which is what this test measures. Rust does not *specify* the agreement
//! (`powi`'s docs reserve the right to a different rounding sequence than
//! `powf`), so it is evidence about this toolchain rather than a guarantee —
//! and a toolchain that broke it would red this test rather than silently
//! weaken a proof.
//!
//! Scope note: the claim is only ever made for exponent 2. Nothing here says
//! anything about `powi(n)` for other `n`.

/// Values chosen to cover the classes where a differing rounding sequence
/// would show up first: subnormals, the exponent extremes, values whose square
/// overflows or flushes to zero, and the ordinary speed range the contract
/// actually operates in.
fn probe_values() -> Vec<f64> {
    let mut v = vec![
        0.0,
        -0.0,
        f64::MIN_POSITIVE,
        -f64::MIN_POSITIVE,
        f64::MIN_POSITIVE / 2.0, // subnormal
        5e-324,                  // smallest subnormal
        1e-200,                  // squares to zero
        1e200,                   // squares to infinity
        f64::MAX,
        f64::MIN,
        f64::EPSILON,
        1.0,
        -1.0,
        core::f64::consts::PI,
        core::f64::consts::SQRT_2,
        1.0 / 3.0,
        // The P6 entry threshold and its neighbourhood: `v2 > 1e-6`.
        1e-3,
        0.0009999999999999998,
        0.0010000000000000002,
        0.0005,
        // The operating range: 0.1 m/s steps to 100 m/s, plus the class ceilings.
        5.0,
        5.225,
        13.4,
        35.0,
    ];
    // A dense sweep of the speed domain the kernel is actually invoked over.
    for step in 0..=1_000u32 {
        let x = f64::from(step) * 0.1;
        v.push(x);
        v.push(-x);
    }
    // A spread of exponents at a fixed mantissa, both signs.
    for e in -320i32..=320 {
        let x = 1.234_567_890_123_456_7_f64 * 2f64.powi(e);
        v.push(x);
        v.push(-x);
    }
    v
}

#[test]
fn powi_two_is_bit_identical_to_self_multiplication() {
    for v in probe_values() {
        let a = v.powi(2);
        let b = v * v;
        assert_eq!(
            a.to_bits(),
            b.to_bits(),
            "v.powi(2) != v * v for v = {v:e}: {a:e} ({:#018x}) vs {b:e} ({:#018x})",
            a.to_bits(),
            b.to_bits()
        );
    }
}

#[test]
fn powi_two_is_bit_identical_on_nan_and_infinities() {
    // NaN payloads are not preserved identically by every operation, so compare
    // the CLASS rather than the bits for NaN, and the bits for the infinities.
    for v in [f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(v.powi(2).to_bits(), (v * v).to_bits(), "v = {v:e}");
    }
    assert!(f64::NAN.powi(2).is_nan() && (f64::NAN * f64::NAN).is_nan());
}
