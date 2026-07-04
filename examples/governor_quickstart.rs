//! Kirra SDK quickstart (Rust) — the CHECKER bounding a DOER's proposals.
//!
//! Kirra's load-bearing thesis: a planner (the DOER) PROPOSES a scalar command;
//! the safety governor (the CHECKER) BOUNDS it, fail-closed, against a hard
//! kinematic envelope. The doer is never trusted for safety — the checker is the
//! invariant. This example feeds a governor a sequence of proposed velocities
//! (some safe, some out-of-envelope, one corrupt `NaN`) and prints what the
//! checker actually emits to the actuator.
//!
//! Run it:
//! ```text
//! cargo run --example governor_quickstart
//! ```
//!
//! The C-ABI equivalent of this path is in `examples/c/` (linking `libkirra_verifier`).

use kirra_verifier::kinematics_contract::KinematicContract;
use kirra_verifier::kirra_core::KirraKernelGovernor;
use kirra_verifier::SafetyGovernor;

fn main() {
    // The hard envelope the checker enforces. A proposal outside this is clamped;
    // a corrupt (non-finite) proposal fails closed to `fallback_linear_speed`.
    let contract = KinematicContract {
        max_linear_velocity: 2.0,      // m/s — the absolute speed ceiling
        max_angular_velocity: 1.0,     // rad/s
        max_linear_acceleration: 10.0, // m/s² — rate-of-change bound
        fallback_linear_speed: 0.0,    // the fail-closed safe-stop scalar
    };

    // KirraKernelGovernor<C>::new(contract, initial_scalar, cap_min, cap_max).
    let mut governor = KirraKernelGovernor::new(contract, 0.0, -2.0, 2.0);

    // A doer's proposed linear velocities over successive 50 ms ticks. The last is
    // deliberately corrupt to show the fail-closed path.
    let dt = 0.05;
    let proposals = [1.0_f64, 1.8, 5.0 /* over-envelope */, f64::NAN /* corrupt */];

    println!("proposed -> emitted   (why)");
    println!("---------------------------");
    for demand in proposals {
        let verdict = governor.evaluate(demand, dt);
        // `sanitized_scalar` is what reaches the actuator — NEVER non-finite, NEVER
        // outside the envelope. `mitigation` is the structured, `Copy` reason code
        // for the verdict (shown here via `Debug`).
        println!(
            "{:>8.2} -> {:>7.2}   ({:?})",
            demand, verdict.sanitized_scalar, verdict.mitigation
        );
        assert!(
            verdict.sanitized_scalar.is_finite(),
            "the checker must never emit a non-finite command"
        );
        assert!(
            verdict.sanitized_scalar.abs() <= 2.0,
            "the checker must never emit outside the hard envelope"
        );
    }

    // The governor also exposes its trust posture and last emitted output.
    println!("\ntrust mode after the corrupt tick: {:?}", governor.trust_mode());
    println!("last emitted output: {:.2} m/s", governor.last_output());
}
