//! # C ABI — the safety-governor integration boundary (ADR-0006 Clause 3)
//!
//! The stable C entry points declared in [`include/kirra.h`](https://github.com/kirra-systems/kirra-runtime-sdk/blob/main/include/kirra.h).
//! A C/C++ integrator PROPOSES a scalar command; the governor BOUNDS it,
//! fail-closed, against a hard kinematic envelope. All functions operate on one
//! process-wide governor (`GLOBAL_GOVERNOR`) and are safe to call from any thread
//! (an internal mutex serialises access); a poisoned lock fails closed to `0.0`.
//!
//! See `examples/c/kirra_ffi_demo.c` for a linked, runnable consumer, and the Rust
//! `governor_quickstart` example for the equivalent in-process path.

use crate::kinematics_contract::KinematicContract;
use crate::kirra_core::{KirraKernelGovernor, RuntimeTrustEngine};
use crate::{SafetyContract, SafetyGovernor};
use std::sync::{LazyLock, Mutex};

static GLOBAL_GOVERNOR: LazyLock<Mutex<KirraKernelGovernor<KinematicContract>>> =
    LazyLock::new(|| {
        let contract = KinematicContract {
            max_linear_velocity: 2.0,
            max_angular_velocity: 1.0,
            max_linear_acceleration: 10.0,
            fallback_linear_speed: 0.0,
        };
        Mutex::new(KirraKernelGovernor::new(contract, 0.0, -2.0, 2.0))
    });

/// Bound a proposed LINEAR velocity (m/s) against the governor's envelope and
/// rate-of-change limits, over a timestep `dt` (seconds). Returns the sanitized
/// scalar to send to the actuator — ALWAYS finite and inside the envelope. A
/// non-finite proposal, a non-positive `dt`, or a poisoned lock fails closed to the
/// contract fallback (`0.0`).
#[no_mangle]
pub extern "C" fn kirra_filter_move_velocity(proposed_velocity: f64, dt: f64) -> f64 {
    GLOBAL_GOVERNOR
        .lock()
        .map(|mut g| g.evaluate(proposed_velocity, dt).sanitized_scalar)
        .unwrap_or(0.0)
}

// --- Structured verdict (WS-2 SDK: the verdict struct) ---------------------
//
// `kirra_filter_move_velocity` returns only the bounded scalar; a C integrator
// cannot tell WHY it was bounded (a clean passthrough vs an envelope clamp vs a
// fail-closed rejection). `kirra_check_move_velocity` returns both — the same
// sanitized scalar plus a stable reason code — as a `#[repr(C)]` struct BY VALUE.
// No raw pointers, so no `unsafe`: the ABI passes the small {f64,i32} aggregate
// per the platform C convention, and the fail-closed contract is unchanged.

/// Stable C verdict reason codes (mirror `include/kirra.h`'s `KIRRA_VERDICT_*`).
/// Frozen wire values — only APPEND new codes, never renumber.
pub const KIRRA_VERDICT_PASSTHROUGH: i32 = 0;
pub const KIRRA_VERDICT_ENVELOPE_CLAMP: i32 = 1;
pub const KIRRA_VERDICT_RATE_CLAMP: i32 = 2;
pub const KIRRA_VERDICT_NONFINITE_REJECTED: i32 = 3;
pub const KIRRA_VERDICT_INVALID_DT_REJECTED: i32 = 4;
pub const KIRRA_VERDICT_DEGRADED_POSTURE_CLAMP: i32 = 5;
pub const KIRRA_VERDICT_DEGRADED_DECEL_HOLD: i32 = 6;
pub const KIRRA_VERDICT_SHADOW_HOLD: i32 = 7;
pub const KIRRA_VERDICT_LOCKOUT_FALLBACK: i32 = 8;
/// FFI-only sentinel: the process governor lock was poisoned; `sanitized_value`
/// is the fail-closed `0.0`. Not a `MitigationCode` (no verdict was computed).
pub const KIRRA_VERDICT_LOCK_POISONED: i32 = 9;

/// A governed-command verdict for the C ABI: the sanitized scalar to actuate plus
/// the reason it was (or was not) bounded. `#[repr(C)]`, returned by value.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KirraVerdict {
    /// The scalar to send to the actuator — ALWAYS finite and inside the envelope
    /// (identical to `kirra_filter_move_velocity` for the same input).
    pub sanitized_value: f64,
    /// One of the `KIRRA_VERDICT_*` codes.
    pub code: i32,
}

/// The stable C reason code for a `MitigationCode`. Exhaustive (no wildcard): a
/// new verdict variant will fail to compile here until it is given a code — the
/// C ABI can never silently drop a new mitigation reason.
#[must_use]
fn mitigation_to_code(m: &crate::MitigationCode) -> i32 {
    use crate::MitigationCode as M;
    match m {
        M::PassthroughUnrestrictedNormal => KIRRA_VERDICT_PASSTHROUGH,
        M::EnvelopeClampTakesPriority => KIRRA_VERDICT_ENVELOPE_CLAMP,
        M::RateClampEnforced { .. } => KIRRA_VERDICT_RATE_CLAMP,
        M::NonfiniteInputRejectedFailsafe => KIRRA_VERDICT_NONFINITE_REJECTED,
        M::InvalidTimeDeltaRejectedFailsafe => KIRRA_VERDICT_INVALID_DT_REJECTED,
        M::DegradedPostureClamp { .. } => KIRRA_VERDICT_DEGRADED_POSTURE_CLAMP,
        M::DegradedDecelToStopHold { .. } => KIRRA_VERDICT_DEGRADED_DECEL_HOLD,
        M::ShadowModeHoldEnforced { .. } => KIRRA_VERDICT_SHADOW_HOLD,
        M::CriticalLockoutFallback => KIRRA_VERDICT_LOCKOUT_FALLBACK,
    }
}

/// Bound a proposed LINEAR velocity (m/s) over `dt` (seconds) and return a
/// structured [`KirraVerdict`] — the sanitized scalar plus WHY it was bounded.
/// `sanitized_value` is byte-identical to [`kirra_filter_move_velocity`] for the
/// same input; a poisoned lock fails closed to `{0.0, KIRRA_VERDICT_LOCK_POISONED}`.
#[no_mangle]
pub extern "C" fn kirra_check_move_velocity(proposed_velocity: f64, dt: f64) -> KirraVerdict {
    match GLOBAL_GOVERNOR.lock() {
        Ok(mut g) => {
            let r = g.evaluate(proposed_velocity, dt);
            KirraVerdict {
                sanitized_value: r.sanitized_scalar,
                code: mitigation_to_code(&r.mitigation),
            }
        }
        Err(_) => KirraVerdict {
            sanitized_value: 0.0,
            code: KIRRA_VERDICT_LOCK_POISONED,
        },
    }
}

/// Bound a proposed ANGULAR velocity (rad/s) to the governor's `max_angular_rate`.
/// Returns the clamped rate — ALWAYS finite. A non-finite proposal fails closed to
/// `0.0` and decays trust; an over-limit proposal is clamped to the bound and decays
/// trust; an in-bound proposal passes through. If the lock is poisoned it returns
/// `0.0` WITHOUT touching trust (the engine was never acquired) — fail-closed.
#[no_mangle]
pub extern "C" fn kirra_filter_rotate_velocity(proposed_angular: f64, _dt: f64) -> f64 {
    if let Ok(mut g) = GLOBAL_GOVERNOR.lock() {
        let max = g.contract.max_angular_rate();
        // Fail-closed on non-finite input: this shim clamps inline (it does NOT go
        // through `evaluate`), and `NaN.abs() > max` is `false`, so an unguarded
        // `NaN` would be forwarded to the actuator unclamped (#404). Command zero
        // angular rate and decay trust. (`kirra_filter_move_velocity` is fixed
        // transitively — it routes through `evaluate`'s Priority-0 guard.)
        if !crate::governor_guard::all_finite(&[proposed_angular]) {
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
    } else {
        0.0
    }
}

/// The governor's current trust score (0–100). Safe ticks raise it; clamps and
/// fail-closed rejections decay it. A poisoned lock reads as `0` (fail-closed).
#[no_mangle]
pub extern "C" fn kirra_get_trust_score() -> u32 {
    GLOBAL_GOVERNOR
        .lock()
        .map(|g| g.trust_engine.current_score)
        .unwrap_or(0)
}

// --- Posture query (WS-2 SDK: posture query) -------------------------------
//
// A C integrator can read the governor's current operating posture — the
// trust-mode band the score has settled into — to alert or take its own action
// (e.g. surface a Degraded/LockedOut banner) without inferring it from the
// per-command verdict stream.

/// Stable C posture codes (mirror `include/kirra.h`'s `KIRRA_POSTURE_*`). Frozen
/// wire values — ordered most-permissive (0) to most-restrictive (3), append-only.
pub const KIRRA_POSTURE_NOMINAL: i32 = 0;
pub const KIRRA_POSTURE_CONSTRAINED: i32 = 1;
pub const KIRRA_POSTURE_SHADOW: i32 = 2;
pub const KIRRA_POSTURE_LOCKED_OUT: i32 = 3;

/// The stable C posture code for a `TrustMode`. Exhaustive (no wildcard): a new
/// trust mode won't compile until it is mapped.
#[must_use]
fn trust_mode_to_posture(m: crate::TrustMode) -> i32 {
    use crate::TrustMode as T;
    match m {
        T::FullAutonomy => KIRRA_POSTURE_NOMINAL,
        T::ConstrainedAdvisory => KIRRA_POSTURE_CONSTRAINED,
        T::ShadowMode => KIRRA_POSTURE_SHADOW,
        T::LockedOut => KIRRA_POSTURE_LOCKED_OUT,
    }
}

/// The governor's current operating posture as a `KIRRA_POSTURE_*` code. A
/// poisoned lock fails closed to the MOST-RESTRICTIVE posture
/// (`KIRRA_POSTURE_LOCKED_OUT`) — never a permissive default — so a consumer that
/// gates on the posture stops rather than proceeds when the state is unreadable.
#[no_mangle]
pub extern "C" fn kirra_posture() -> i32 {
    GLOBAL_GOVERNOR
        .lock()
        .map(|g| trust_mode_to_posture(g.trust_engine.mode))
        .unwrap_or(KIRRA_POSTURE_LOCKED_OUT)
}

// --- Envelope config query (WS-2 SDK: envelope config) ---------------------
//
// The hard kinematic envelope is COMPILED into the library; a C integrator
// currently cannot discover the bounds it is being held to. `kirra_envelope`
// reports them, so a caller can pre-clamp its own proposals (or display the
// limits) instead of learning them only by getting clamped.

/// The governor's hard kinematic envelope + rate limits, reported to C. All
/// fields are the SAME bounds `kirra_check_move_velocity` / `_filter_*` enforce.
/// `#[repr(C)]`, returned by value.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KirraEnvelope {
    /// Max linear velocity (m/s) — the upper envelope bound.
    pub max_linear_velocity_mps: f64,
    /// Min linear velocity (m/s) — the lower envelope bound (symmetric: `-max`).
    pub min_linear_velocity_mps: f64,
    /// Max angular velocity (rad/s).
    pub max_angular_velocity_radps: f64,
    /// Max linear acceleration (m/s²) — the rate-of-change limit.
    pub max_linear_acceleration_mps2: f64,
    /// The fail-closed fallback linear velocity (m/s) commanded on a rejection.
    pub fallback_linear_velocity_mps: f64,
}

/// Report the governor's compiled hard envelope + rate limits as a
/// [`KirraEnvelope`]. A poisoned lock fails closed to an ALL-ZERO envelope: a
/// zero `max_linear_velocity` admits only a stop (0.0), so a consumer that
/// pre-clamps to this envelope halts rather than proceeds when it is unreadable.
#[no_mangle]
pub extern "C" fn kirra_envelope() -> KirraEnvelope {
    match GLOBAL_GOVERNOR.lock() {
        Ok(g) => KirraEnvelope {
            max_linear_velocity_mps: g.contract.max_bound(),
            min_linear_velocity_mps: g.contract.min_bound(),
            max_angular_velocity_radps: g.contract.max_angular_rate(),
            max_linear_acceleration_mps2: g.contract.max_rate(),
            fallback_linear_velocity_mps: g.contract.fallback(),
        },
        Err(_) => KirraEnvelope {
            max_linear_velocity_mps: 0.0,
            min_linear_velocity_mps: 0.0,
            max_angular_velocity_radps: 0.0,
            max_linear_acceleration_mps2: 0.0,
            fallback_linear_velocity_mps: 0.0,
        },
    }
}

// --- Release-token verify (WS-2 SDK: release-token verify) -----------------
//
// The actuator's verify-before-release gate (HVCHAN §3 step 7) over the C ABI:
// given a 96-byte release token, the 32-byte digest of the command the caller is
// ABOUT to actuate, and the 32-byte governor verifying key, confirm the governor
// approved exactly those bytes and the signature verifies. Delegates to the ONE
// canonical `verify_release_over_digest` — no crypto is re-implemented here.

/// The release token approves this digest AND its signature verifies — RELEASE.
pub const KIRRA_RELEASE_OK: i32 = 0;
/// The token's digest does not match the command about to be actuated (stale /
/// substituted approval) — DO NOT release.
pub const KIRRA_RELEASE_DIGEST_MISMATCH: i32 = 1;
/// The signature does not verify against the governor key (forged / tampered /
/// wrong signer) — DO NOT release.
pub const KIRRA_RELEASE_SIGNATURE_INVALID: i32 = 2;
/// A malformed argument (null pointer, wrong length, or a `governor_vk` that is
/// not a valid Ed25519 point) — fail-closed, DO NOT release.
pub const KIRRA_RELEASE_BAD_ARGS: i32 = -1;

/// # Safety
///
/// Verify a governor release token before actuating a command (HVCHAN §3 step 7).
/// Returns `KIRRA_RELEASE_OK` (0) ONLY if the token approves `digest_ptr` and its
/// signature verifies against `vk_ptr`; every other outcome is a non-zero
/// fail-closed code (`KIRRA_RELEASE_*`). Release ONLY on `== KIRRA_RELEASE_OK`.
///
/// - `token_ptr` must address `token_len` readable bytes; `token_len` must be 96
///   (`digest[32] || signature[64]`).
/// - `digest_ptr` must address `digest_len` readable bytes; `digest_len` must be
///   32 — the SHA-256 digest of the command the caller is about to actuate.
/// - `vk_ptr` must address `vk_len` readable bytes; `vk_len` must be 32 — the
///   governor Ed25519 verifying key.
/// - None of the regions may be aliased/mutated during the call and each must
///   outlive it. The null + length checks catch obvious misuse but cannot validate
///   that a pointer addresses real memory — irreducibly a caller responsibility at
///   the C boundary.
///
/// Per CERT-005 RSR-001: every pub extern "C" fn that dereferences a raw pointer
/// must be marked unsafe fn.
#[no_mangle]
pub unsafe extern "C" fn kirra_verify_release_token(
    token_ptr: *const u8,
    token_len: usize,
    digest_ptr: *const u8,
    digest_len: usize,
    vk_ptr: *const u8,
    vk_len: usize,
) -> i32 {
    if token_ptr.is_null()
        || token_len != 96
        || digest_ptr.is_null()
        || digest_len != 32
        || vk_ptr.is_null()
        || vk_len != 32
    {
        return KIRRA_RELEASE_BAD_ARGS;
    }
    let mut token_arr = [0u8; 96];
    let mut digest_arr = [0u8; 32];
    let mut vk_arr = [0u8; 32];
    // SAFETY: non-null + exact-length verified above; caller owns validity/outlives.
    token_arr.copy_from_slice(unsafe { std::slice::from_raw_parts(token_ptr, 96) });
    digest_arr.copy_from_slice(unsafe { std::slice::from_raw_parts(digest_ptr, 32) });
    vk_arr.copy_from_slice(unsafe { std::slice::from_raw_parts(vk_ptr, 32) });

    let vk = match ed25519_dalek::VerifyingKey::from_bytes(&vk_arr) {
        Ok(k) => k,
        Err(_) => return KIRRA_RELEASE_BAD_ARGS, // not a valid Ed25519 point
    };
    let token = crate::governor_release::ReleaseToken::from_bytes(&token_arr);
    match crate::governor_release::verify_release_over_digest(&token, &digest_arr, &vk) {
        Ok(()) => KIRRA_RELEASE_OK,
        Err(crate::governor_release::ReleaseDenied::DigestMismatch) => {
            KIRRA_RELEASE_DIGEST_MISMATCH
        }
        Err(crate::governor_release::ReleaseDenied::SignatureInvalid) => {
            KIRRA_RELEASE_SIGNATURE_INVALID
        }
    }
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
    if token_ptr.is_null() || token_len == 0 || token_len > 64 {
        return 0;
    }
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
    } else {
        0
    }
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

    /// DELTA 1: the injected clock flows into the cooldown computation — the
    /// threshold-th failed attempt arms the cooldown RELATIVE to the real
    /// timestamp (`T + 60_000`), not to a frozen `0`. With the old `now = 0`,
    /// `reset_cooldown_end_ms` would be `60_000` (a 1970 instant) and a probe at
    /// a real timestamp would NOT read as cooldown-active — the timer was inert.
    #[test]
    fn reset_threads_real_clock_into_cooldown_window() {
        let mut engine = RuntimeTrustEngine::new();
        let key = b"supervisor-key";

        // A real-ish wall-clock timestamp (~2024 in ms).
        let t: u64 = 1_700_000_000_000;
        // Five wrong tokens at `t`: the threshold-th arms the cooldown at `t + 60s`.
        for _ in 0..5 {
            assert_eq!(reset_engine_at(&mut engine, b"wrong", key, t), 0);
        }
        assert_eq!(engine.failed_reset_attempts, 5);
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

    /// M2 (permanent-lockout fix): once the brute-force cooldown has been SERVED,
    /// the CORRECT token must succeed. The pre-fix code cleared the failed-attempt
    /// counter only on a successful compare — which was unreachable, because
    /// `failed >= threshold` returned BRUTE_FORCE (re-arming the cooldown) before
    /// the compare, forever. The counter is persisted across restarts, so this was
    /// an unrecoverable lockout of a legitimate supervisor.
    #[test]
    fn served_cooldown_admits_correct_token() {
        let mut engine = RuntimeTrustEngine::new();
        let key = b"supervisor-key";
        let t: u64 = 1_700_000_000_000;

        // Trip the brute-force cooldown.
        for _ in 0..5 {
            assert_eq!(reset_engine_at(&mut engine, b"wrong", key, t), 0);
        }
        assert_eq!(engine.reset_cooldown_end_ms, t + 60_000);

        // Correct token DURING the window is still blocked (throttle intact).
        assert_eq!(reset_engine_at(&mut engine, key, key, t + 30_000), 0);

        // Correct token AFTER the window succeeds — the lockout is recoverable.
        assert_eq!(
            reset_engine_at(&mut engine, key, key, t + 60_001),
            1,
            "a served cooldown must admit the correct token (no permanent lockout)"
        );
        assert_eq!(
            engine.failed_reset_attempts, 0,
            "success clears the counter"
        );
        assert_eq!(
            engine.reset_cooldown_end_ms, 0,
            "success clears the cooldown"
        );
        assert_eq!(engine.mode, crate::TrustMode::FullAutonomy);
    }

    /// A served cooldown grants a FRESH attempt budget, not a single try: after the
    /// window, wrong tokens count from zero again and only re-arm the cooldown once
    /// the threshold is hit anew.
    #[test]
    fn served_cooldown_grants_fresh_attempt_window() {
        let mut engine = RuntimeTrustEngine::new();
        let key = b"supervisor-key";
        let t: u64 = 1_700_000_000_000;

        for _ in 0..5 {
            assert_eq!(reset_engine_at(&mut engine, b"wrong", key, t), 0);
        }
        let after = t + 60_001;
        // First wrong attempt after the window: counter restarts at 1, no re-arm.
        assert_eq!(reset_engine_at(&mut engine, b"wrong", key, after), 0);
        assert_eq!(engine.failed_reset_attempts, 1);
        assert_eq!(
            engine.reset_cooldown_end_ms, 0,
            "one failure must not re-arm the cooldown"
        );
    }

    /// A restart must NOT bypass the throttle (Copilot #819). The gateway persists
    /// `failed_reset_attempts` but NOT the in-memory `reset_cooldown_end_ms`, so a
    /// reboot presents `failed == threshold` with `cooldown_end == 0`. That state
    /// must ARM a fresh cooldown and reject — not clear the counter and grant an
    /// immediate fresh window — otherwise an attacker who can force restarts skips
    /// the wait. Recovery is still possible once the armed window is served.
    #[test]
    fn restart_persisted_lockout_arms_cooldown_not_bypass() {
        let mut engine = RuntimeTrustEngine::new();
        let key = b"supervisor-key";
        let t: u64 = 1_700_000_000_000;

        // Simulate the post-restart load: counter restored from disk, cooldown lost.
        engine.failed_reset_attempts = 5;
        engine.reset_cooldown_end_ms = 0;

        // Correct token immediately after restart: throttle must NOT be skipped.
        assert_eq!(reset_engine_at(&mut engine, key, key, t), 0);
        assert_eq!(
            engine.reset_cooldown_end_ms,
            t + 60_000,
            "a restart at threshold must ARM a cooldown, not clear the counter"
        );
        assert_eq!(
            engine.failed_reset_attempts, 5,
            "counter held until the cooldown is served"
        );

        // Within the window: still blocked.
        assert_eq!(reset_engine_at(&mut engine, key, key, t + 59_999), 0);

        // Once served: the correct token recovers (no permanent lockout).
        assert_eq!(reset_engine_at(&mut engine, key, key, t + 60_001), 1);
        assert_eq!(engine.failed_reset_attempts, 0);
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
mod verdict_tests {
    use super::*;
    use crate::MitigationCode as M;

    /// Every `MitigationCode` maps to its stable, DISTINCT C code — exhaustively,
    /// so a new verdict variant cannot silently collide or default.
    #[test]
    fn mitigation_codes_are_stable_and_distinct() {
        let cases = [
            (M::PassthroughUnrestrictedNormal, KIRRA_VERDICT_PASSTHROUGH),
            (M::EnvelopeClampTakesPriority, KIRRA_VERDICT_ENVELOPE_CLAMP),
            (
                M::RateClampEnforced { max_rate: 1.0 },
                KIRRA_VERDICT_RATE_CLAMP,
            ),
            (
                M::NonfiniteInputRejectedFailsafe,
                KIRRA_VERDICT_NONFINITE_REJECTED,
            ),
            (
                M::InvalidTimeDeltaRejectedFailsafe,
                KIRRA_VERDICT_INVALID_DT_REJECTED,
            ),
            (
                M::DegradedPostureClamp {
                    cap_min: -1.0,
                    cap_max: 1.0,
                },
                KIRRA_VERDICT_DEGRADED_POSTURE_CLAMP,
            ),
            (
                M::DegradedDecelToStopHold { held: 0.5 },
                KIRRA_VERDICT_DEGRADED_DECEL_HOLD,
            ),
            (
                M::ShadowModeHoldEnforced { retained: 0.2 },
                KIRRA_VERDICT_SHADOW_HOLD,
            ),
            (M::CriticalLockoutFallback, KIRRA_VERDICT_LOCKOUT_FALLBACK),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for (m, expected) in cases {
            assert_eq!(
                mitigation_to_code(&m),
                expected,
                "code for {m:?} must be stable"
            );
            assert!(seen.insert(expected), "code {expected} must be distinct");
        }
        // The poisoned-lock sentinel is distinct from every mapped code.
        assert!(!seen.contains(&KIRRA_VERDICT_LOCK_POISONED));
    }

    /// The verdict's `sanitized_value` matches the scalar `kirra_filter_move_velocity`
    /// returns for the same input — the struct adds a reason, never a different value.
    /// Uses the non-finite input, whose fail-closed result is invariant of the shared
    /// `GLOBAL_GOVERNOR` state (the P0 guard short-circuits before any trust branch).
    #[test]
    fn verdict_value_matches_the_scalar_filter_nonfinite() {
        let v = kirra_check_move_velocity(f64::NAN, 0.05);
        assert_eq!(
            v.code, KIRRA_VERDICT_NONFINITE_REJECTED,
            "a NaN demand is a fail-closed rejection"
        );
        assert!(
            v.sanitized_value.is_finite(),
            "the verdict value is never non-finite"
        );
        assert_eq!(
            v.sanitized_value,
            kirra_filter_move_velocity(f64::NAN, 0.05)
        );
    }

    /// A non-positive timestep is a fail-closed rejection with the invalid-dt code
    /// (also state-invariant — the dt guard runs before any trust branch).
    #[test]
    fn verdict_reports_invalid_dt() {
        let v = kirra_check_move_velocity(1.0, 0.0);
        assert_eq!(v.code, KIRRA_VERDICT_INVALID_DT_REJECTED);
        assert!(v.sanitized_value.is_finite());
    }

    /// Every `TrustMode` maps to its stable, DISTINCT, correctly-ORDERED posture
    /// code (0 most-permissive → 3 most-restrictive) — exhaustively.
    #[test]
    fn posture_codes_are_stable_ordered_and_distinct() {
        use crate::TrustMode as T;
        assert_eq!(
            trust_mode_to_posture(T::FullAutonomy),
            KIRRA_POSTURE_NOMINAL
        );
        assert_eq!(
            trust_mode_to_posture(T::ConstrainedAdvisory),
            KIRRA_POSTURE_CONSTRAINED
        );
        assert_eq!(trust_mode_to_posture(T::ShadowMode), KIRRA_POSTURE_SHADOW);
        assert_eq!(
            trust_mode_to_posture(T::LockedOut),
            KIRRA_POSTURE_LOCKED_OUT
        );
        // Ordered most-permissive → most-restrictive, and all distinct.
        assert!(
            KIRRA_POSTURE_NOMINAL < KIRRA_POSTURE_CONSTRAINED
                && KIRRA_POSTURE_CONSTRAINED < KIRRA_POSTURE_SHADOW
                && KIRRA_POSTURE_SHADOW < KIRRA_POSTURE_LOCKED_OUT
        );
    }

    /// The FFI posture query never returns garbage: always a valid, in-range
    /// posture code (regardless of the shared `GLOBAL_GOVERNOR` state other tests
    /// leave it in). A poisoned lock would fail closed to LOCKED_OUT, still in range.
    #[test]
    fn ffi_posture_is_always_a_valid_code() {
        let p = kirra_posture();
        assert!(
            (KIRRA_POSTURE_NOMINAL..=KIRRA_POSTURE_LOCKED_OUT).contains(&p),
            "posture must be a valid KIRRA_POSTURE_* code, got {p}"
        );
    }

    /// The envelope query reports the compiled FFI governor's bounds. The contract
    /// is immutable, so this is deterministic regardless of the shared trust state
    /// other tests leave `GLOBAL_GOVERNOR` in.
    #[test]
    fn envelope_reports_the_compiled_bounds() {
        let e = kirra_envelope();
        assert_eq!(e.max_linear_velocity_mps, 2.0, "compiled linear envelope");
        assert_eq!(e.min_linear_velocity_mps, -2.0, "symmetric lower bound");
        assert_eq!(e.max_angular_velocity_radps, 1.0);
        assert_eq!(e.max_linear_acceleration_mps2, 10.0);
        assert_eq!(e.fallback_linear_velocity_mps, 0.0);
    }

    /// Structural invariants that hold for ANY reported envelope — including the
    /// all-zero fail-closed envelope on a poisoned lock: all finite, symmetric
    /// (`min == -max`), and non-negative bounds. (The stronger "positive/usable"
    /// accel limit is a success-path property, asserted in
    /// `envelope_reports_the_compiled_bounds`, not a universal API guarantee.)
    #[test]
    fn envelope_is_finite_and_symmetric() {
        let e = kirra_envelope();
        for v in [
            e.max_linear_velocity_mps,
            e.min_linear_velocity_mps,
            e.max_angular_velocity_radps,
            e.max_linear_acceleration_mps2,
            e.fallback_linear_velocity_mps,
        ] {
            assert!(v.is_finite(), "every envelope field must be finite");
        }
        assert_eq!(
            e.min_linear_velocity_mps, -e.max_linear_velocity_mps,
            "symmetric envelope"
        );
        assert!(
            e.max_linear_velocity_mps >= 0.0,
            "max velocity is non-negative"
        );
        assert!(
            e.max_angular_velocity_radps >= 0.0,
            "max angular rate is non-negative"
        );
        assert!(
            e.max_linear_acceleration_mps2 >= 0.0,
            "accel limit is non-negative (0 in the fail-closed envelope)"
        );
    }

    /// The reported envelope agrees with what the checker enforces: a demand well
    /// ABOVE `max_linear_velocity_mps` comes back at or below it (envelope-clamped),
    /// never above the reported bound.
    #[test]
    fn reported_envelope_bounds_the_actual_verdict() {
        let e = kirra_envelope();
        let v = kirra_check_move_velocity(e.max_linear_velocity_mps + 100.0, 1.0);
        assert!(
            v.sanitized_value <= e.max_linear_velocity_mps + 1e-9,
            "the checker must not emit above the reported envelope ({} > {})",
            v.sanitized_value,
            e.max_linear_velocity_mps
        );
    }
}

#[cfg(test)]
mod release_token_ffi_tests {

    use super::*;
    use crate::governor_release::{contract_digest, issue_release_token};
    use ed25519_dalek::SigningKey;
    use kirra_contract_channel::GovernorContractView;

    fn gov_key() -> SigningKey {
        SigningKey::from_bytes(&[42u8; 32])
    }
    fn view(cmd: &[u8]) -> GovernorContractView {
        GovernorContractView::new_command(2, 1, 100, 10_000, cmd).unwrap()
    }

    /// An honest token, verified against the digest of the SAME command and the
    /// governor's key, releases (`KIRRA_RELEASE_OK`).
    #[test]
    fn honest_token_releases() {
        let sk = gov_key();
        let v = view(b"steer:1.5");
        let token = issue_release_token(&v, &sk).to_bytes();
        let digest = contract_digest(&v);
        let vk = sk.verifying_key().to_bytes();
        let code = unsafe {
            kirra_verify_release_token(token.as_ptr(), 96, digest.as_ptr(), 32, vk.as_ptr(), 32)
        };
        assert_eq!(code, KIRRA_RELEASE_OK);
    }

    /// A token for one command, verified against the digest of a DIFFERENT command,
    /// is a digest mismatch (the anti-substitution check) — no release.
    #[test]
    fn digest_mismatch_denies() {
        let sk = gov_key();
        let token = issue_release_token(&view(b"steer:1.5"), &sk).to_bytes();
        let other_digest = contract_digest(&view(b"steer:9.9")); // different bytes
        let vk = sk.verifying_key().to_bytes();
        let code = unsafe {
            kirra_verify_release_token(
                token.as_ptr(),
                96,
                other_digest.as_ptr(),
                32,
                vk.as_ptr(),
                32,
            )
        };
        assert_eq!(code, KIRRA_RELEASE_DIGEST_MISMATCH);
    }

    /// A tampered signature (a flipped byte) fails the crypto verify — no release.
    #[test]
    fn tampered_signature_denies() {
        let sk = gov_key();
        let v = view(b"steer:1.5");
        let mut token = issue_release_token(&v, &sk).to_bytes();
        token[64] ^= 0x01; // flip a byte in the signature half (digest[32]||sig[64])
        let digest = contract_digest(&v);
        let vk = sk.verifying_key().to_bytes();
        let code = unsafe {
            kirra_verify_release_token(token.as_ptr(), 96, digest.as_ptr(), 32, vk.as_ptr(), 32)
        };
        assert_eq!(code, KIRRA_RELEASE_SIGNATURE_INVALID);
    }

    /// The right token + digest but the WRONG governor key does not verify.
    #[test]
    fn wrong_governor_key_denies() {
        let sk = gov_key();
        let v = view(b"steer:1.5");
        let token = issue_release_token(&v, &sk).to_bytes();
        let digest = contract_digest(&v);
        let other_vk = SigningKey::from_bytes(&[7u8; 32])
            .verifying_key()
            .to_bytes();
        let code = unsafe {
            kirra_verify_release_token(
                token.as_ptr(),
                96,
                digest.as_ptr(),
                32,
                other_vk.as_ptr(),
                32,
            )
        };
        assert_eq!(code, KIRRA_RELEASE_SIGNATURE_INVALID);
    }

    /// Malformed arguments fail closed to `KIRRA_RELEASE_BAD_ARGS`, never OK.
    #[test]
    fn malformed_args_fail_closed() {
        let sk = gov_key();
        let v = view(b"steer:1.5");
        let token = issue_release_token(&v, &sk).to_bytes();
        let digest = contract_digest(&v);
        let vk = sk.verifying_key().to_bytes();
        // Wrong token length.
        assert_eq!(
            unsafe {
                kirra_verify_release_token(token.as_ptr(), 95, digest.as_ptr(), 32, vk.as_ptr(), 32)
            },
            KIRRA_RELEASE_BAD_ARGS
        );
        // Wrong digest length.
        assert_eq!(
            unsafe {
                kirra_verify_release_token(token.as_ptr(), 96, digest.as_ptr(), 31, vk.as_ptr(), 32)
            },
            KIRRA_RELEASE_BAD_ARGS
        );
        // Null token pointer.
        assert_eq!(
            unsafe {
                kirra_verify_release_token(
                    std::ptr::null(),
                    96,
                    digest.as_ptr(),
                    32,
                    vk.as_ptr(),
                    32,
                )
            },
            KIRRA_RELEASE_BAD_ARGS
        );
        // A correct-LENGTH but non-decodable Ed25519 key (not a valid curve point)
        // also fails closed — the key parse rejects it before any verify.
        let bad_vk = INVALID_ED25519_KEY;
        assert!(
            ed25519_dalek::VerifyingKey::from_bytes(&bad_vk).is_err(),
            "the fixture must be a genuinely invalid Ed25519 key encoding"
        );
        assert_eq!(
            unsafe {
                kirra_verify_release_token(
                    token.as_ptr(),
                    96,
                    digest.as_ptr(),
                    32,
                    bad_vk.as_ptr(),
                    32,
                )
            },
            KIRRA_RELEASE_BAD_ARGS
        );
    }

    /// A 32-byte value that is NOT a valid compressed Edwards point (its `y` has
    /// no `x` on the curve), so `VerifyingKey::from_bytes` rejects it. The
    /// `is_err()` guard in the test above ensures this can never silently become a
    /// valid key.
    const INVALID_ED25519_KEY: [u8; 32] = [2u8; 32];
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
            assert!(
                out.is_finite(),
                "move shim must never return non-finite (got {out})"
            );
            assert_eq!(out, 0.0);
        }
    }
}
