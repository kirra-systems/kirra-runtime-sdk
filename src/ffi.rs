use std::sync::{Mutex, LazyLock};
use crate::kirra_core::{KirraKernelGovernor, RuntimeTrustEngine};
use crate::kinematics_contract::KinematicContract;
use crate::{SafetyGovernor, SafetyContract};

static GLOBAL_GOVERNOR: LazyLock<Mutex<KirraKernelGovernor<KinematicContract>>> = LazyLock::new(|| {
    let contract = KinematicContract {
        max_linear_velocity: 2.0, max_angular_velocity: 1.0,
        max_linear_acceleration: 10.0, fallback_linear_speed: 0.0,
    };
    Mutex::new(KirraKernelGovernor::new(contract, 0.0, -2.0, 2.0))
});

#[no_mangle]
pub extern "C" fn kirra_filter_move_velocity(proposed_velocity: f64, dt: f64) -> f64 {
    GLOBAL_GOVERNOR.lock().map(|mut g| g.evaluate(proposed_velocity, dt).sanitized_scalar).unwrap_or(0.0)
}

#[no_mangle]
pub extern "C" fn kirra_filter_rotate_velocity(proposed_angular: f64, _dt: f64) -> f64 {
    if let Ok(mut g) = GLOBAL_GOVERNOR.lock() {
        let max = g.contract.max_angular_rate();
        // Fail-closed on non-finite input: this shim clamps inline (it does NOT go
        // through `evaluate`), and `NaN.abs() > max` is `false`, so an unguarded
        // `NaN` would be forwarded to the actuator unclamped (#404). Command zero
        // angular rate and decay trust. (`kirra_filter_move_velocity` is fixed
        // transitively — it routes through `evaluate`'s Priority-0 guard.)
        if !proposed_angular.is_finite() {
            g.trust_engine.decay_trust(30);
            return 0.0;
        }
        if proposed_angular.abs() > max {
            g.trust_engine.decay_trust(30);
            proposed_angular.clamp(-max, max)
        } else {
            g.trust_engine.register_safe_tick();
            proposed_angular
        }
    } else { 0.0 }
}

#[no_mangle]
pub extern "C" fn kirra_get_trust_score() -> u32 {
    GLOBAL_GOVERNOR.lock().map(|g| g.trust_engine.current_score).unwrap_or(0)
}

/// # Safety
///
/// Caller must ensure:
/// - `token_ptr` points to a valid readable region of at least
///   `token_len` bytes
/// - The memory region must not be aliased or mutated during the call
/// - The memory must outlive the duration of this call
/// - `token_len` must accurately reflect the size of the buffer;
///   mis-sizing causes out-of-bounds read with undefined behavior
///
/// The null-pointer and length checks at the start of this function
/// catch obvious invalid inputs but cannot validate that the pointer
/// addresses real memory. Safety is irreducibly a caller responsibility
/// at this C FFI boundary.
///
/// Per CERT-005 RSR-001: every pub extern "C" fn that dereferences
/// a raw pointer must be marked unsafe fn.
#[no_mangle]
pub unsafe extern "C" fn kirra_reset_state(token_ptr: *const u8, token_len: usize) -> i32 {
    if token_ptr.is_null() || token_len == 0 || token_len > 64 { return 0; }
    let key = match std::env::var("KIRRA_SUPERVISOR_RESET_KEY") {
        Ok(v) if !v.is_empty() => v.into_bytes(),
        _ => return 0,
    };
    let token = unsafe { std::slice::from_raw_parts(token_ptr, token_len) };
    // #103 DELTA 1: thread the REAL wall-clock into the reset so the cooldown /
    // brute-force timer actually advances. Previously this passed `0`, which
    // froze the timer — after 5 failed attempts `reset_cooldown_end_ms` became
    // 60000 and `0 < 60000` stayed true forever, so the intended 60 s cooldown
    // never elapsed. We use the SAME clock convention as the gateway reset path
    // (`gateway/mod.rs`: SystemTime since UNIX_EPOCH, ms) — one time convention
    // across both reset paths. `authenticated_manual_reset`'s signature is
    // unchanged (the caller supplies time, which is correct).
    if let Ok(mut g) = GLOBAL_GOVERNOR.lock() {
        reset_engine_at(&mut g.trust_engine, token, &key, supervisor_now_ms())
    } else { 0 }
}

// ---------------------------------------------------------------------------
// #103 DELTA 2 — DEFERRED (documented reservation; NO emission in this PR).
//
// A LockedOut clearance via supervisor reset is a safety-critical transition
// with no audit row today. The obvious fix — emit a signed event at this reset
// site — is BLOCKED by architecture: the signed-chain append primitive (in
// `src/audit_chain.rs`) and the SQLite chain live in the *verifier-service*
// subsystem, and neither kirra_core reset path (this FFI shim, or the
// `gateway/mod.rs` admin socket) carries an audit-chain handle. Reaching it
// would need cross-subsystem plumbing the design deliberately avoids.
//
// RESERVED audit vocabulary (for the future emitting path — NOT defined as code
// here, to avoid dead_code in a clippy-clean tree):
//   event types : SUPERVISOR_RESET_SUCCEEDED, SUPERVISOR_RESET_REJECTED
//   reject reasons (SUPERVISOR_RESET_REJECTED): COOLDOWN_ACTIVE,
//                  BRUTE_FORCE_SUSPECTED, INVALID_TOKEN
//   never log/record the token bytes — outcome + reason code only.
//
// FOLLOW-UP (#103): the signed clearance audit belongs in a verifier-service
// supervisor-reset route — that subsystem owns BOTH the fleet posture and the
// signed chain — pending a separate scoping of the kernel-trust-reset (this
// path) vs the fleet-posture-clearance distinction. Until then the #117 UL 4600
// clearance/reset SPI stays GAP (NOT emitted).
// ---------------------------------------------------------------------------

/// Wall-clock milliseconds since the UNIX epoch — the single time convention
/// shared by both supervisor-reset paths (mirrors `gateway/mod.rs`'s `now`).
fn supervisor_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Time-injectable core of the FFI reset, factored out so the cooldown /
/// brute-force timing is unit-testable without the C boundary, the process-wide
/// `GLOBAL_GOVERNOR`, or `KIRRA_SUPERVISOR_RESET_KEY` (which a test cannot set —
/// `std::env::set_var` in a multithreaded test is forbidden, INV-13). Returns
/// `1` on a successful reset, `0` on any rejection (fail-closed, matching the
/// C contract). The caller supplies `current_time_ms`; the
/// `authenticated_manual_reset` signature is unchanged.
fn reset_engine_at(
    engine: &mut RuntimeTrustEngine,
    token: &[u8],
    key: &[u8],
    current_time_ms: u64,
) -> i32 {
    engine
        .authenticated_manual_reset(token, key, current_time_ms)
        .map(|_| 1)
        .unwrap_or(0)
}

#[cfg(test)]
mod reset_clock_tests {
    use super::*;

    /// DELTA 1: the injected clock flows into the cooldown computation — after a
    /// brute-force lockout the cooldown end is set RELATIVE to the real
    /// timestamp (`T + 60_000`), not to a frozen `0`. With the old `now = 0`,
    /// `reset_cooldown_end_ms` would be `60_000` (a 1970 instant) and a probe at
    /// a real timestamp would NOT read as cooldown-active — the timer was inert.
    #[test]
    fn reset_threads_real_clock_into_cooldown_window() {
        let mut engine = RuntimeTrustEngine::new();
        let key = b"supervisor-key";

        // Five wrong tokens arm the brute-force counter.
        for _ in 0..5 {
            assert_eq!(reset_engine_at(&mut engine, b"wrong", key, 1_000), 0);
        }
        assert_eq!(engine.failed_reset_attempts, 5);

        // A real-ish wall-clock timestamp (~2024 in ms).
        let t: u64 = 1_700_000_000_000;
        // failed >= 5 → brute-force branch arms the cooldown RELATIVE to `t`.
        assert_eq!(reset_engine_at(&mut engine, key, key, t), 0);
        assert_eq!(
            engine.reset_cooldown_end_ms,
            t + 60_000,
            "cooldown end must be set relative to the injected real clock, not a frozen 0"
        );

        // A probe inside the 60 s window reads as cooldown-active (rejected) — a
        // behaviour that is only meaningful because the window is real-time
        // bounded. With the inert `now = 0`, `t + 59_999 < 60_000` would be false
        // and this branch would never be reached.
        assert!(t + 59_999 < engine.reset_cooldown_end_ms);
        assert_eq!(reset_engine_at(&mut engine, key, key, t + 59_999), 0);
    }

    /// The clock the production FFI path supplies is a real current wall clock,
    /// not the old hardcoded `0`.
    #[test]
    fn supervisor_clock_is_real_wall_clock_ms() {
        let now = supervisor_now_ms();
        assert!(
            now > 1_600_000_000_000,
            "supervisor reset must use real wall-clock ms (got {now}), never a frozen 0"
        );
    }
}

#[cfg(test)]
mod ffi_nonfinite_tests {
    use super::*;

    // #404: both C-ABI shims must fail-closed on non-finite input. The returns are
    // invariant of prior GLOBAL_GOVERNOR trust state (the non-finite guards short-
    // circuit before any trust-mode branching), so these are deterministic even
    // though they share the process-wide governor.

    #[test]
    fn rotate_velocity_rejects_nonfinite_to_zero() {
        // Clamps inline (NOT via `evaluate`): NaN.abs() > max is false, so without
        // the guard a NaN would be forwarded unclamped. Must command zero.
        assert_eq!(kirra_filter_rotate_velocity(f64::NAN, 0.05), 0.0);
        assert_eq!(kirra_filter_rotate_velocity(f64::INFINITY, 0.05), 0.0);
        assert_eq!(kirra_filter_rotate_velocity(f64::NEG_INFINITY, 0.05), 0.0);
    }

    #[test]
    fn move_velocity_rejects_nonfinite_to_finite_fallback() {
        // Fixed transitively by `evaluate`'s Priority-0 guard → contract fallback.
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let out = kirra_filter_move_velocity(bad, 0.05);
            assert!(out.is_finite(), "move shim must never return non-finite (got {out})");
            assert_eq!(out, 0.0);
        }
    }
}
