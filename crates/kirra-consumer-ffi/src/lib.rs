//! ADR-0033 decision (c) — the C-ABI surface over the ONE verifying motor
//! consumer (`kirra-actuation-consumer::MotorConsumer`), for the Python
//! Rosmaster consumer on the Yahboom X3.
//!
//! # Why this crate exists
//!
//! The verify core is Rust (`RosReleaseGate` + `MotorConsumer`), the motor
//! driver is Python (`Rosmaster_Lib.set_car_motion`). ADR-0033 settled that the
//! token verification, the sequence watermark, the freshness rule, the refusal
//! taxonomy, and the SS-002 liveness/decel-to-stop must live in **one Rust
//! implementation** — never re-implemented in Python (two-sources-of-truth is
//! the failure this rule exists to prevent). So the Python node NEVER sees a
//! key, a signature, a watermark, or a ramp: it hands raw wire bytes to this
//! ABI and is told, per frame and per tick, *whether* and *what* to actuate.
//!
//! This crate adds **no verification logic**. It instantiates `MotorConsumer<S>`
//! with a [`CaptureSerial`] seam that records the twist the core decided to
//! write (and applies the demo-envelope clamp), then marshals the outcome across
//! the boundary. `unsafe` lives ONLY here, at the C boundary — the verify core
//! stays `#![forbid(unsafe_code)]`.
//!
//! # The wire frame Python passes in
//!
//! - `payload`: the 32-byte [`RosTwistPayload`] image (`ROS_TWIST_PAYLOAD_LEN`).
//! - `token`: the 96-byte [`ReleaseToken`] image, or absent (`NULL`, len 0) for
//!   an unsigned command → refused `NoToken`, no motor write.
//!
//! # Decision, not action
//!
//! On a full gate pass the core writes the (clamped) twist into `CaptureSerial`;
//! this ABI copies it into the out-struct with `write = 1`. Python then calls
//! `set_car_motion(linear, 0.0, angular)`. On any refusal `write = 0` and the
//! motor is not touched. The starve path ([`kirra_consumer_on_tick`]) likewise
//! returns the next decel-ramp twist to write (or `write = 0` for silence /
//! never-released).

// The verify core is `forbid(unsafe_code)`; this is the C boundary, so unsafe is
// unavoidable and DELIBERATELY isolated here. Same discipline as `src/ffi.rs`
// and the QNX judge: every `unsafe fn` documents its caller contract.
#![allow(clippy::missing_safety_doc)] // each fn has a prose `# Safety` section.

use std::os::raw::c_char;

use ed25519_dalek::VerifyingKey;
use kirra_actuation_consumer::{
    ConsumerConfig, FrameOutcome, MotorConsumer, MotorSerial, RosReleaseRefusal,
    KEY_MISMATCH_ALARM_EXPLANATION,
};
use kirra_release_token::ros_twist::ROS_TWIST_PAYLOAD_LEN;
use kirra_release_token::{ReleaseDenied, ReleaseToken};

/// Wire lengths the Python side must present (re-exported as constants so the
/// ctypes binding and any C consumer agree with the Rust truth).
pub const KIRRA_PAYLOAD_LEN: usize = ROS_TWIST_PAYLOAD_LEN; // 32
pub const KIRRA_TOKEN_LEN: usize = 96;
pub const KIRRA_VK_LEN: usize = 32;

// ---------------------------------------------------------------------------
// The capture serial seam: the demo-envelope clamp + a one-slot record of the
// twist the core decided to write on the LAST call. NOT the motor — the motor
// is Python. `Infallible` error type (a record never fails), so the core's
// `SerialError` outcome is structurally unreachable here (documented at the ABI).
// ---------------------------------------------------------------------------

/// Records the (clamped) twist the core wrote on the most recent
/// `on_frame`/`on_tick`, and applies the demo-envelope magnitude clamp.
///
/// The clamp is a **backstop**, not the safety mechanism (Kirra's checker is the
/// authority). Its bounds are config-injected with NO default — a caller must
/// supply `vx_max`/`vz_max`; the constructor fails closed (returns NULL) if they
/// are not finite and > 0. `v_y` has no channel here (skid-steer demo): the
/// consumer core carries only (linear, angular), and Python passes `v_y = 0`.
struct CaptureSerial {
    vx_max: f64,
    vz_max: f64,
    /// The twist written on the last core call, already clamped. `None` = the
    /// last call wrote nothing (refusal / silence / never-released).
    pending: Option<(f64, f64)>,
}

impl CaptureSerial {
    fn clamp_mag(v: f64, max: f64) -> f64 {
        // max is validated finite & > 0 at construction. A finite v is clamped
        // to [-max, max]; a non-finite v cannot arrive (the gate refuses
        // non-finite payloads before any write), but clamp defensively to 0.
        if !v.is_finite() {
            0.0
        } else if v > max {
            max
        } else if v < -max {
            -max
        } else {
            v
        }
    }
}

impl MotorSerial for CaptureSerial {
    type Error = core::convert::Infallible;
    fn write_twist(&mut self, linear_mps: f64, angular_rad_s: f64) -> Result<(), Self::Error> {
        self.pending = Some((
            Self::clamp_mag(linear_mps, self.vx_max),
            Self::clamp_mag(angular_rad_s, self.vz_max),
        ));
        Ok(())
    }
}

/// The opaque handle Python holds. One consumer = one actuation path (the same
/// single-owner discipline as the gate); not thread-safe by design.
pub struct KirraConsumer {
    inner: MotorConsumer<CaptureSerial>,
}

// ---------------------------------------------------------------------------
// C ABI result structs — `#[repr(C)]`, scalar-only, no pointers into Rust state.
// ---------------------------------------------------------------------------

/// The outcome of one `on_frame`. `kind`: 0 = Released, 1 = SerialError
/// (unreachable with the capture seam; mapped for completeness), 2 = Refused.
/// `refusal_code` is meaningful iff `kind == 2`.
#[repr(C)]
pub struct KirraFrameResult {
    pub kind: i32,
    pub refusal_code: i32,
    /// 1 → Python must actuate `(linear, angular)`; 0 → do not touch the motor.
    pub write: u8,
    pub sequence: u64,
    pub linear: f64,
    pub angular: f64,
}

/// The outcome of one `on_tick` (the liveness clock / decel-to-stop ramp).
#[repr(C)]
pub struct KirraTickResult {
    /// 1 → Python must actuate `(linear, angular)` (the next ramp step); 0 →
    /// output silence (nothing to write this tick).
    pub write: u8,
    pub linear: f64,
    pub angular: f64,
}

/// A snapshot of the consumer's health surface (the #892 loud-diagnostic data).
#[repr(C)]
pub struct KirraHealth {
    pub releases: u64,
    pub refusals_total: u64,
    pub no_token: u64,
    pub digest_mismatch: u64,
    pub signature_invalid: u64,
    pub undecodable: u64,
    pub stale: u64,
    pub sequence_not_advanced: u64,
    pub consecutive_signature_invalid: u32,
    /// 1 → the DISTINCT key-mismatch alarm is latched (sustained
    /// SignatureInvalid). Python must surface [`kirra_consumer_alarm_explanation`]
    /// loudly and keep the safe stop.
    pub key_mismatch_alarm: u8,
    pub silent: u8,
}

// Refusal-class codes (wire-stable; mirror `RosReleaseRefusal`).
const REF_NO_TOKEN: i32 = 0;
const REF_DIGEST_MISMATCH: i32 = 1;
const REF_SIGNATURE_INVALID: i32 = 2;
const REF_UNDECODABLE: i32 = 3;
const REF_STALE: i32 = 4;
const REF_SEQUENCE_NOT_ADVANCED: i32 = 5;

fn refusal_code(r: &RosReleaseRefusal) -> i32 {
    match r {
        RosReleaseRefusal::NoToken => REF_NO_TOKEN,
        RosReleaseRefusal::Denied(ReleaseDenied::DigestMismatch) => REF_DIGEST_MISMATCH,
        RosReleaseRefusal::Denied(ReleaseDenied::SignatureInvalid) => REF_SIGNATURE_INVALID,
        RosReleaseRefusal::Undecodable(_) => REF_UNDECODABLE,
        RosReleaseRefusal::Stale { .. } => REF_STALE,
        RosReleaseRefusal::SequenceNotAdvanced { .. } => REF_SEQUENCE_NOT_ADVANCED,
    }
}

// ---------------------------------------------------------------------------
// Constructor / destructor
// ---------------------------------------------------------------------------

/// Construct a consumer. FAIL-CLOSED: returns NULL (never a half-configured
/// handle) if the pinned key is malformed, the liveness/decel config is invalid
/// (see `ConsumerConfig`), or the demo-envelope bounds are not finite and > 0.
/// There is deliberately NO default for any physical number — the caller
/// (deployment) must supply them.
///
/// # Safety
/// - `vk` must point to exactly [`KIRRA_VK_LEN`] readable bytes (the raw
///   Ed25519 public key). The pointer is only read for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn kirra_consumer_new(
    vk: *const u8,
    freshness_window_ms: u64,
    control_period_ms: u64,
    missed_periods: u32,
    stop_decel_mps2: f64,
    vx_max: f64,
    vz_max: f64,
) -> *mut KirraConsumer {
    if vk.is_null() {
        return core::ptr::null_mut();
    }
    // Demo-envelope bounds are required and fail-closed (no silent default).
    if !(vx_max.is_finite() && vx_max > 0.0 && vz_max.is_finite() && vz_max > 0.0) {
        return core::ptr::null_mut();
    }
    // SAFETY: caller guarantees `vk` points to KIRRA_VK_LEN readable bytes.
    let vk_bytes: [u8; KIRRA_VK_LEN] = {
        let mut b = [0u8; KIRRA_VK_LEN];
        core::ptr::copy_nonoverlapping(vk, b.as_mut_ptr(), KIRRA_VK_LEN);
        b
    };
    let Ok(governor_vk) = VerifyingKey::from_bytes(&vk_bytes) else {
        // A non-canonical / invalid public key → refuse (never pin garbage).
        return core::ptr::null_mut();
    };
    let cfg = ConsumerConfig {
        freshness_window_ms,
        control_period_ms,
        missed_periods,
        stop_decel_mps2,
    };
    let serial = CaptureSerial {
        vx_max,
        vz_max,
        pending: None,
    };
    match MotorConsumer::new(governor_vk, cfg, serial) {
        Ok(inner) => Box::into_raw(Box::new(KirraConsumer { inner })),
        // ConfigError (bad decel / deadline) → fail closed, NULL.
        Err(_) => core::ptr::null_mut(),
    }
}

/// Free a consumer handle. Safe to call with NULL (no-op).
///
/// # Safety
/// - `h` must be a pointer returned by [`kirra_consumer_new`] and not already
///   freed. After this call `h` is dangling; the caller must not reuse it.
#[no_mangle]
pub unsafe extern "C" fn kirra_consumer_free(h: *mut KirraConsumer) {
    if !h.is_null() {
        // SAFETY: caller guarantees `h` came from Box::into_raw and is live.
        drop(Box::from_raw(h));
    }
}

// ---------------------------------------------------------------------------
// The chokepoint: on_frame / on_tick
// ---------------------------------------------------------------------------

/// Ingest one wire frame. Writes the decision into `*out`. `token` may be NULL
/// (with `token_len == 0`) for an unsigned command → refused `NoToken`.
///
/// # Safety
/// - `h` is a live handle from [`kirra_consumer_new`].
/// - `payload` points to exactly [`KIRRA_PAYLOAD_LEN`] readable bytes.
/// - `token` is either NULL, or points to exactly [`KIRRA_TOKEN_LEN`] readable
///   bytes; `token_len` must be `KIRRA_TOKEN_LEN` when `token` is non-NULL.
/// - `out` points to a writable [`KirraFrameResult`].
///
/// All pointers are used only for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn kirra_consumer_on_frame(
    h: *mut KirraConsumer,
    payload: *const u8,
    token: *const u8,
    token_len: usize,
    now_ms: u64,
    out: *mut KirraFrameResult,
) {
    if out.is_null() {
        return;
    }
    // Fail-closed default FIRST (Copilot #901): a caller that reuses an output
    // struct must never observe a stale `write=1` from a previous call when
    // this call bails on a NULL handle/payload. Refused, no write, no motion.
    // SAFETY: caller guarantees `out` is a writable KirraFrameResult.
    core::ptr::write(
        out,
        KirraFrameResult {
            kind: 2,
            refusal_code: -1,
            write: 0,
            sequence: 0,
            linear: 0.0,
            angular: 0.0,
        },
    );
    if h.is_null() || payload.is_null() {
        return; // *out already says "refused / do not actuate".
    }
    // SAFETY: caller guarantees `h` is a live handle.
    let consumer = &mut *h;

    // SAFETY: caller guarantees KIRRA_PAYLOAD_LEN readable bytes at `payload`.
    let mut payload_bytes = [0u8; KIRRA_PAYLOAD_LEN];
    core::ptr::copy_nonoverlapping(payload, payload_bytes.as_mut_ptr(), KIRRA_PAYLOAD_LEN);

    // Reconstruct the optional token. A malformed length is treated as "no
    // usable token" → NoToken refusal (fail-closed; never a partial read).
    let token_obj: Option<ReleaseToken> = if token.is_null() || token_len != KIRRA_TOKEN_LEN {
        None
    } else {
        // SAFETY: caller guarantees KIRRA_TOKEN_LEN readable bytes at `token`.
        let mut tb = [0u8; KIRRA_TOKEN_LEN];
        core::ptr::copy_nonoverlapping(token, tb.as_mut_ptr(), KIRRA_TOKEN_LEN);
        Some(ReleaseToken::from_bytes(&tb))
    };

    // Clear the capture slot so we can tell whether THIS call wrote.
    consumer.reset_capture();
    let outcome = consumer
        .inner
        .on_frame(&payload_bytes, token_obj.as_ref(), now_ms);

    let result = match outcome {
        FrameOutcome::Released { sequence } => {
            let (linear, angular) = consumer.pending().unwrap_or((0.0, 0.0));
            KirraFrameResult {
                kind: 0,
                refusal_code: -1,
                write: 1,
                sequence,
                linear,
                angular,
            }
        }
        FrameOutcome::SerialError => KirraFrameResult {
            kind: 1,
            refusal_code: -1,
            write: 0,
            sequence: 0,
            linear: 0.0,
            angular: 0.0,
        },
        FrameOutcome::Refused(r) => KirraFrameResult {
            kind: 2,
            refusal_code: refusal_code(&r),
            write: 0,
            sequence: 0,
            linear: 0.0,
            angular: 0.0,
        },
    };
    // SAFETY: caller guarantees `out` is a writable KirraFrameResult.
    core::ptr::write(out, result);
}

/// The liveness clock — call once per control period. Drives SS-002: past the
/// deadline with no valid release → an ACTIVE decel-to-zero ramp (never
/// hold-last), then silence. Writes the next ramp step (or "no write") to `*out`.
///
/// # Safety
/// - `h` is a live handle; `out` points to a writable [`KirraTickResult`].
#[no_mangle]
pub unsafe extern "C" fn kirra_consumer_on_tick(
    h: *mut KirraConsumer,
    now_ms: u64,
    out: *mut KirraTickResult,
) {
    if out.is_null() {
        return;
    }
    // Fail-closed default first (Copilot #901): stale-struct reuse must read
    // as "no write", never a leftover ramp step.
    // SAFETY: caller guarantees `out` is a writable KirraTickResult.
    core::ptr::write(
        out,
        KirraTickResult {
            write: 0,
            linear: 0.0,
            angular: 0.0,
        },
    );
    if h.is_null() {
        return; // *out already says "no write".
    }
    // SAFETY: caller guarantees `h` is a live handle.
    let consumer = &mut *h;
    consumer.reset_capture();
    consumer.inner.on_tick(now_ms);
    let result = match consumer.pending() {
        Some((linear, angular)) => KirraTickResult {
            write: 1,
            linear,
            angular,
        },
        None => KirraTickResult {
            write: 0,
            linear: 0.0,
            angular: 0.0,
        },
    };
    // SAFETY: caller guarantees `out` is a writable KirraTickResult.
    core::ptr::write(out, result);
}

/// Snapshot the health surface into `*out`.
///
/// # Safety
/// - `h` is a live handle; `out` points to a writable [`KirraHealth`].
#[no_mangle]
pub unsafe extern "C" fn kirra_consumer_health(h: *const KirraConsumer, out: *mut KirraHealth) {
    if out.is_null() {
        return;
    }
    // Zeroed snapshot first (Copilot #901): a NULL handle must not leave stale
    // counters/alarm state visible in a reused struct.
    // SAFETY: caller guarantees `out` is a writable KirraHealth.
    core::ptr::write(
        out,
        KirraHealth {
            releases: 0,
            refusals_total: 0,
            no_token: 0,
            digest_mismatch: 0,
            signature_invalid: 0,
            undecodable: 0,
            stale: 0,
            sequence_not_advanced: 0,
            consecutive_signature_invalid: 0,
            key_mismatch_alarm: 0,
            silent: 0,
        },
    );
    if h.is_null() {
        return; // *out is the zeroed snapshot.
    }
    // SAFETY: caller guarantees `h` is a live handle.
    let health = (*h).inner.health();
    let refusals = health.refusals;
    let result = KirraHealth {
        releases: health.releases,
        refusals_total: refusals.total(),
        no_token: refusals.no_token,
        digest_mismatch: refusals.digest_mismatch,
        signature_invalid: refusals.signature_invalid,
        undecodable: refusals.undecodable,
        stale: refusals.stale,
        sequence_not_advanced: refusals.sequence_not_advanced,
        consecutive_signature_invalid: health.consecutive_signature_invalid,
        key_mismatch_alarm: u8::from(health.key_mismatch_alarm),
        silent: u8::from(health.silent),
    };
    // SAFETY: caller guarantees `out` is a writable KirraHealth.
    core::ptr::write(out, result);
}

/// The latched key-mismatch alarm's operator sentence, as a static
/// NUL-terminated C string (valid for the program lifetime; never freed by the
/// caller). Returned regardless of latch state so callers can display it; check
/// `KirraHealth::key_mismatch_alarm` to know WHEN to display it.
#[no_mangle]
pub extern "C" fn kirra_consumer_alarm_explanation() -> *const c_char {
    // A 'static &str with an appended NUL, built once.
    ALARM_EXPLANATION_CSTR.as_ptr().cast::<c_char>()
}

// KEY_MISMATCH_ALARM_EXPLANATION with a trailing NUL, as a byte slice usable as
// a C string. A const check below pins that the source sentence has no interior
// NUL (which would truncate it at the boundary).
const fn _no_interior_nul() {
    let bytes = KEY_MISMATCH_ALARM_EXPLANATION.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        assert!(bytes[i] != 0, "alarm sentence must have no interior NUL");
        i += 1;
    }
}
const _: () = _no_interior_nul();

// Build the NUL-terminated image at compile time.
const ALARM_LEN: usize = KEY_MISMATCH_ALARM_EXPLANATION.len();
static ALARM_EXPLANATION_CSTR: [u8; ALARM_LEN + 1] = {
    let src = KEY_MISMATCH_ALARM_EXPLANATION.as_bytes();
    let mut buf = [0u8; ALARM_LEN + 1];
    let mut i = 0;
    while i < ALARM_LEN {
        buf[i] = src[i];
        i += 1;
    }
    // buf[ALARM_LEN] stays 0 → the terminator.
    buf
};

impl CaptureSerial {
    fn pending(&self) -> Option<(f64, f64)> {
        self.pending
    }
}

impl KirraConsumer {
    /// Clear the capture slot before a core call, so a subsequent `pending()`
    /// read reflects ONLY what this call wrote. Reaches the seam through
    /// `MotorConsumer::serial_mut` — the seam only, never the gate/watermark.
    fn reset_capture(&mut self) {
        self.inner.serial_mut().pending = None;
    }
    fn pending(&self) -> Option<(f64, f64)> {
        self.inner.serial().pending()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use kirra_release_token::ros_twist::{issue_ros_release, RosTwistPayload};

    fn sk() -> SigningKey {
        SigningKey::from_bytes(&[42u8; 32])
    }

    unsafe fn new_consumer(vk: [u8; 32]) -> *mut KirraConsumer {
        kirra_consumer_new(
            vk.as_ptr(),
            /* freshness_window_ms */ 200,
            /* control_period_ms   */ 100,
            /* missed_periods      */ 3,
            /* stop_decel_mps2     */ 1.0,
            /* vx_max              */ 0.15,
            /* vz_max              */ 0.4,
        )
    }

    fn frame(seq: u64, issued: u64, lin: f64, ang: f64) -> ([u8; 32], [u8; 96]) {
        let p = RosTwistPayload {
            sequence: seq,
            issued_at_ms: issued,
            linear_mps: lin,
            angular_rad_s: ang,
        };
        (p.encode(), issue_ros_release(&p, &sk()).to_bytes())
    }

    #[test]
    fn valid_frame_releases_and_clamps_to_the_demo_envelope() {
        unsafe {
            let h = new_consumer(sk().verifying_key().to_bytes());
            assert!(!h.is_null());
            // Command ABOVE the demo envelope (0.5 m/s, 2.0 rad/s) — the clamp
            // must saturate it to (0.15, 0.4). (The checker is authority; this
            // is the defense-in-depth backstop.)
            let (p, t) = frame(1, 10_000, 0.5, 2.0);
            let mut out = std::mem::zeroed::<KirraFrameResult>();
            kirra_consumer_on_frame(h, p.as_ptr(), t.as_ptr(), 96, 10_050, &mut out);
            assert_eq!(out.kind, 0, "valid frame must Release");
            assert_eq!(out.write, 1);
            assert_eq!(out.sequence, 1);
            assert!(
                (out.linear - 0.15).abs() < 1e-12,
                "linear clamped to vx_max"
            );
            assert!(
                (out.angular - 0.4).abs() < 1e-12,
                "angular clamped to vz_max"
            );
            kirra_consumer_free(h);
        }
    }

    #[test]
    fn unsigned_frame_is_refused_no_write() {
        unsafe {
            let h = new_consumer(sk().verifying_key().to_bytes());
            let (p, _t) = frame(1, 10_000, 0.1, 0.1);
            let mut out = std::mem::zeroed::<KirraFrameResult>();
            // token NULL, len 0 → NoToken.
            kirra_consumer_on_frame(h, p.as_ptr(), core::ptr::null(), 0, 10_000, &mut out);
            assert_eq!(out.kind, 2, "unsigned → Refused");
            assert_eq!(out.refusal_code, REF_NO_TOKEN);
            assert_eq!(out.write, 0, "refusal must not actuate");
            kirra_consumer_free(h);
        }
    }

    #[test]
    fn wrong_key_latches_the_key_mismatch_alarm() {
        unsafe {
            let h = new_consumer(sk().verifying_key().to_bytes());
            let wrong = SigningKey::from_bytes(&[9u8; 32]);
            for k in 0..10u64 {
                let p = RosTwistPayload {
                    sequence: k + 1,
                    issued_at_ms: 10_000 + k,
                    linear_mps: 0.1,
                    angular_rad_s: 0.0,
                };
                let t = issue_ros_release(&p, &wrong).to_bytes();
                let mut out = std::mem::zeroed::<KirraFrameResult>();
                kirra_consumer_on_frame(
                    h,
                    p.encode().as_ptr(),
                    t.as_ptr(),
                    96,
                    10_000 + k,
                    &mut out,
                );
                assert_eq!(out.refusal_code, REF_SIGNATURE_INVALID);
                assert_eq!(out.write, 0);
            }
            let mut hp = std::mem::zeroed::<KirraHealth>();
            kirra_consumer_health(h, &mut hp);
            assert_eq!(hp.key_mismatch_alarm, 1, "sustained bad-sig must latch");
            assert_eq!(hp.signature_invalid, 10);
            // The alarm sentence is reachable and non-empty.
            let s = kirra_consumer_alarm_explanation();
            let cstr = std::ffi::CStr::from_ptr(s);
            assert!(cstr.to_bytes().len() > 40);
            kirra_consumer_free(h);
        }
    }

    #[test]
    fn starvation_ramps_to_zero_then_goes_silent() {
        unsafe {
            let h = new_consumer(sk().verifying_key().to_bytes());
            let (p, t) = frame(1, 10_000, 0.15, 0.0);
            let mut out = std::mem::zeroed::<KirraFrameResult>();
            kirra_consumer_on_frame(h, p.as_ptr(), t.as_ptr(), 96, 10_000, &mut out);
            assert_eq!(out.kind, 0);

            // Tick past the 300 ms deadline: expect a strictly decreasing ramp
            // to zero, then silence (write=0).
            let mut saw_ramp = false;
            let mut last = 0.15f64;
            let mut reached_zero = false;
            for k in 1..=20u64 {
                let mut tk = std::mem::zeroed::<KirraTickResult>();
                kirra_consumer_on_tick(h, 10_000 + 300 + k * 100, &mut tk);
                if tk.write == 1 {
                    saw_ramp = true;
                    assert!(tk.linear.abs() <= last + 1e-12, "ramp must not increase");
                    last = tk.linear;
                    if tk.linear == 0.0 {
                        reached_zero = true;
                    }
                }
            }
            assert!(saw_ramp, "starvation must produce an active stop ramp");
            assert!(reached_zero, "ramp must reach zero");
            let mut hp = std::mem::zeroed::<KirraHealth>();
            kirra_consumer_health(h, &mut hp);
            assert_eq!(hp.silent, 1, "after the ramp, the consumer is silent");
            kirra_consumer_free(h);
        }
    }

    /// Copilot #901: an early bail (NULL handle/payload) must OVERWRITE `*out`
    /// with a fail-closed "do not actuate" result — a caller reusing an output
    /// struct must never observe a stale `write=1` / stale health from the
    /// previous call.
    #[test]
    fn null_inputs_overwrite_out_with_fail_closed_defaults() {
        unsafe {
            let h = new_consumer(sk().verifying_key().to_bytes());
            // Seed `out` with a stale RELEASED decision, then call with a NULL
            // handle: the stale write=1 must be clobbered to refused/no-write.
            let (p, t) = frame(1, 10_000, 0.1, 0.0);
            let mut out = std::mem::zeroed::<KirraFrameResult>();
            kirra_consumer_on_frame(h, p.as_ptr(), t.as_ptr(), 96, 10_000, &mut out);
            assert_eq!(out.write, 1, "precondition: stale struct holds write=1");
            kirra_consumer_on_frame(
                core::ptr::null_mut(),
                p.as_ptr(),
                t.as_ptr(),
                96,
                10_000,
                &mut out,
            );
            assert_eq!(
                out.write, 0,
                "NULL handle must fail closed, not leak stale write=1"
            );
            assert_eq!(out.kind, 2, "NULL handle reads as Refused");

            // Same for a NULL payload.
            kirra_consumer_on_frame(h, core::ptr::null(), t.as_ptr(), 96, 10_000, &mut out);
            assert_eq!(out.write, 0, "NULL payload must fail closed");

            // on_tick with NULL handle: stale ramp step must read as no-write.
            let mut tk = KirraTickResult {
                write: 1,
                linear: 0.15,
                angular: 0.0,
            };
            kirra_consumer_on_tick(core::ptr::null_mut(), 10_000, &mut tk);
            assert_eq!(
                tk.write, 0,
                "NULL handle tick must not leak a stale ramp step"
            );

            // health with NULL handle: zeroed snapshot, no stale alarm.
            let mut hp = std::mem::zeroed::<KirraHealth>();
            hp.key_mismatch_alarm = 1; // stale
            hp.releases = 99;
            kirra_consumer_health(core::ptr::null(), &mut hp);
            assert_eq!(
                hp.key_mismatch_alarm, 0,
                "NULL handle must zero the snapshot"
            );
            assert_eq!(hp.releases, 0);
            kirra_consumer_free(h);
        }
    }

    #[test]
    fn bad_config_and_bad_key_fail_closed_null() {
        unsafe {
            // Bad vx_max (0.0) → NULL.
            let vk = sk().verifying_key().to_bytes();
            let h = kirra_consumer_new(vk.as_ptr(), 200, 100, 3, 1.0, 0.0, 0.4);
            assert!(h.is_null(), "zero vx_max must fail closed");
            // Bad decel → NULL.
            let h2 = kirra_consumer_new(vk.as_ptr(), 200, 100, 3, -1.0, 0.15, 0.4);
            assert!(h2.is_null(), "negative decel must fail closed");
            // Bad control period (0) → NULL (ConsumerConfig::InvalidDeadline).
            let h3 = kirra_consumer_new(vk.as_ptr(), 200, 0, 3, 1.0, 0.15, 0.4);
            assert!(h3.is_null(), "zero control period must fail closed");
            // NULL key pointer → NULL (never dereferenced). A malformed key
            // encoding likewise returns NULL via the `from_bytes` Err arm.
            let h4 = kirra_consumer_new(core::ptr::null(), 200, 100, 3, 1.0, 0.15, 0.4);
            assert!(h4.is_null(), "null key pointer must fail closed");
        }
    }
}
