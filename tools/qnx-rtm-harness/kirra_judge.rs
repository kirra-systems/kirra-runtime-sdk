// kirra_judge.rs — the Rust JUDGE (the checker) for the QNX RTM harness.
//
// EPIC #270, issue #271. NAMED kirra_judge.rs (NOT kirra_core.rs) deliberately:
// the repo already has `src/kirra_core.rs` (the governor), a different file — the
// name would collide under grep / confuse the concern split.
//
// Built by CMake invoking `rustc` DIRECTLY as a `staticlib` (no cargo,
// dependency-free) — mirroring the QNX cross-compile shape. `#![no_std]`,
// `panic = abort`, zero-alloc, integer-only: nothing here allocates, unwinds, or
// touches std.
//
// THE CONCERN SPLIT (see README + ADR-0006 Clause 3): the C++ shim is the DRIVER
// (memory/transport safety — tear detection, bounds, CRC). This judge is the
// CHECKER — it renders the CONTRACT verdict on a view the shim has already
// stabilized. Memory faults die in the driver; contract faults reach here.
//
// PROXY CONSTANT: `PROXY_MAX_COMMANDED_VELOCITY` is a clearly-labelled PROXY. The
// CERTIFIED kinematic envelope lives in the untouched talisman
// `src/gateway/kinematics_contract.rs` (`VehicleKinematicsContract`); this file
// imports NOTHING from it and its value must never be read as a certified bound.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]

use core::panic::PanicInfo;

// panic = abort: a panic in a no_std staticlib has nowhere to unwind to. The
// build also passes `-C panic=abort`, so this handler is the required shape and
// should be unreachable on the judge's branch-only paths.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    // No std::process::abort in no_std; loop is a valid divergent fallback.
    // `-C panic=abort` + the build means this is not reached in practice.
    loop {
        core::hint::spin_loop();
    }
}

// ---- ABI: must match kirra_ffi.h field-for-field, in order. ----------------

/// Contract header magic — "KIRRACON".
pub const KIRRA_CONTRACT_MAGIC: u64 = 0x4B49_5252_4143_4F4E;

/// Stable verdict codes — identical values to `kirra_ffi.h`.
pub const VERDICT_OK: u8 = 0;
pub const VERDICT_STALE_HEADER: u8 = 1;
pub const VERDICT_SEQUENCE_REGRESS: u8 = 2;
pub const VERDICT_DEADLINE_MISSED: u8 = 3;
// 4 = PAYLOAD_CORRUPT and 5 = PAYLOAD_OVERSIZE are SHIM-side and never produced here.
pub const VERDICT_KINEMATIC_LIMIT: u8 = 6;

/// PROXY kinematic envelope (magnitude of `commanded_velocity`, in the harness's
/// command unit of mm/s). NOT CERTIFIED — a proxy for ~22.35 m/s (the talisman's
/// `URBAN_ODD_SPEED_CAP_MPS`, ADR-0001). Referenced, never imported.
pub const PROXY_MAX_COMMANDED_VELOCITY: i32 = 22_350; // mm/s ≈ 22.35 m/s (PROXY)

/// `#[repr(C)]` — field order IDENTICAL to `kirra_ffi.h::KirraContractView`.
#[repr(C)]
pub struct KirraContractView {
    pub payload: *const u8, // const volatile uint8_t* on the C side (shim-owned)
    pub generation: u64,    // seqlock counter; the SHIM owns the seqlock, the judge ignores it
    pub magic: u64,
    pub sequence: u64,
    pub last_accepted_sequence: u64,
    pub now_monotonic_ns: u64,
    pub deadline_monotonic_ns: u64,
    pub payload_len: u32,
    pub commanded_velocity: i32,
    pub integrity_ok: u8,
    pub header_torn: u8,
}

// #778 F1 — ABI LAYOUT GATE (Rust side). The old CI "ABI assert" was a symbol-name
// grep (`nm | grep kirra_judge_assess`) that passes even if a field is reordered
// or resized — a broken-contract judge would then be checksummed, cosign-signed,
// and shipped as the partition verdict core. These compile-time asserts pin the
// documented LP64 layout (`kirra_ffi.h`: sizeof == 72, alignof == 8, one offset
// per field) INDEPENDENTLY of the C side, so drift on EITHER side fails the build.
// Gated on 64-bit: the layout (leading pointer + tail padding) is defined for LP64,
// which both QNX 8.0 judge tuples (aarch64 / x86_64) are.
#[cfg(target_pointer_width = "64")]
const _: () = {
    use core::mem::{align_of, offset_of, size_of};
    assert!(size_of::<KirraContractView>() == 72, "KirraContractView must be 72 bytes (LP64)");
    assert!(align_of::<KirraContractView>() == 8, "KirraContractView must be 8-byte aligned");
    assert!(offset_of!(KirraContractView, payload) == 0);
    assert!(offset_of!(KirraContractView, generation) == 8);
    assert!(offset_of!(KirraContractView, magic) == 16);
    assert!(offset_of!(KirraContractView, sequence) == 24);
    assert!(offset_of!(KirraContractView, last_accepted_sequence) == 32);
    assert!(offset_of!(KirraContractView, now_monotonic_ns) == 40);
    assert!(offset_of!(KirraContractView, deadline_monotonic_ns) == 48);
    assert!(offset_of!(KirraContractView, payload_len) == 56);
    assert!(offset_of!(KirraContractView, commanded_velocity) == 60);
    assert!(offset_of!(KirraContractView, integrity_ok) == 64);
    assert!(offset_of!(KirraContractView, header_torn) == 65);
};

/// Render the CONTRACT verdict for a stabilized contract view.
///
/// Check ORDER (fixed, documented): magic → sequence → deadline → integrity flag
/// → kinematic envelope. The first failing check wins; `VERDICT_OK` iff all pass.
///
/// The SEQUENCE rule (the corrected form): `sequence <= last_accepted_sequence ⇒
/// reject` with `VERDICT_SEQUENCE_REGRESS`. EQUAL is a REPLAY and rejects; only a
/// STRICTLY-NEWER sequence passes. (`<` would be a replay hole; not used. Mirrors
/// `tools/iceoryx2-spike/src/judge.rs`.)
///
/// # Safety
///
/// Per CERT-005 RSR-001 (`src/ffi.rs`): a `pub extern "C"` fn that dereferences a
/// raw pointer must be `unsafe fn`. The caller MUST ensure:
/// - `v` is either NULL, or points to a valid, readable, properly-aligned
///   `KirraContractView` that lives for the duration of the call;
/// - the `KirraContractView` is not concurrently mutated during the call (the
///   shim driver guarantees this by handing the judge a stabilized snapshot —
///   the judge reads only the scalar header fields, never `payload`).
///
/// The null check below is DEFENSE-IN-DEPTH (NULL ⇒ `VERDICT_STALE_HEADER`,
/// fail-closed), not a substitute for the caller contract: a non-null but invalid
/// pointer is still undefined behavior the caller must preclude.
#[no_mangle]
pub unsafe extern "C" fn kirra_judge_assess(v: *const KirraContractView) -> u8 {
    // Defense-in-depth: a NULL view is fail-closed, never a deref.
    if v.is_null() {
        return VERDICT_STALE_HEADER;
    }
    // SAFETY: `v` is non-null here; the caller contract (`# Safety`) guarantees it
    // points to a valid, aligned, non-aliased `KirraContractView` for this call.
    let view = unsafe { &*v };

    // 1. Header magic. A garbled/torn header is fail-closed StaleHeader. (The
    //    shim's generation seqlock catches torn writes upstream; the explicit
    //    `header_torn` flag is honored here too as belt-and-braces.)
    if view.magic != KIRRA_CONTRACT_MAGIC || view.header_torn != 0 {
        return VERDICT_STALE_HEADER;
    }

    // 2. Sequence monotonicity + replay. `sequence <= last_accepted ⇒ reject`
    //    (equal = replay, lower = regress). The judge owns replay rejection — the
    //    shim does NOT pre-filter equal sequences.
    if view.sequence <= view.last_accepted_sequence {
        return VERDICT_SEQUENCE_REGRESS;
    }

    // 3. Deadline freshness.
    if view.now_monotonic_ns > view.deadline_monotonic_ns {
        return VERDICT_DEADLINE_MISSED;
    }

    // 4. Upstream integrity assertion.
    if view.integrity_ok != 1 {
        return VERDICT_STALE_HEADER;
    }

    // 5. Kinematic envelope (PROXY bound; integer magnitude). `i32::MIN` has no
    //    positive abs, so it is treated as over-limit (fail-closed).
    let over = match view.commanded_velocity.checked_abs() {
        Some(mag) => mag > PROXY_MAX_COMMANDED_VELOCITY,
        None => true,
    };
    if over {
        return VERDICT_KINEMATIC_LIMIT;
    }

    VERDICT_OK
}
