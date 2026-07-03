/* kirra_ffi.h — the C/C++ ⇄ Rust `extern "C"` ABI for the QNX RTM harness.
 *
 * EPIC #270, issue #271. This is the DOCUMENTED C/C++ integration boundary of
 * ADR-0006 Clause 3 — "the C ABI / FFI is retained ONLY as the documented
 * integration boundary for C/C++ components ... no longer the governor hot
 * path." Memory/transport safety is the C++ SHIM's job (the driver); the Rust
 * JUDGE renders the contract verdict on data the shim has already stabilized.
 *
 * The kinematic bound used by the judge is a clearly-labelled PROXY — the
 * CERTIFIED envelope lives in the untouched talisman
 * `src/gateway/kinematics_contract.rs`; this harness REFERENCES it, never
 * imports it, and its numbers must never be read as a certified bound.
 */
#ifndef KIRRA_FFI_H
#define KIRRA_FFI_H

#include <stdint.h>
#include <stddef.h>  /* offsetof — for the #778 F1 layout gate below */

/* #778 F1 — portable compile-time assert (C11 `_Static_assert` / C++ `static_assert`). */
#if defined(__cplusplus)
#  define KIRRA_STATIC_ASSERT(cond, msg) static_assert(cond, msg)
#  define KIRRA_ALIGNOF(t) alignof(t)
#elif defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L
#  define KIRRA_STATIC_ASSERT(cond, msg) _Static_assert(cond, msg)
#  define KIRRA_ALIGNOF(t) _Alignof(t)
#else
#  define KIRRA_STATIC_ASSERT(cond, msg) /* pre-C11: no static assert available */
#  define KIRRA_ALIGNOF(t) 8u            /* unused: asserts compile out pre-C11 */
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* Contract header magic — ASCII "KIRRACON" (big-endian spelling of the bytes). */
#define KIRRA_CONTRACT_MAGIC 0x4B49525241434F4EuLL

/* Stable verdict codes (uint8_t). Values are part of the ABI — do not renumber;
 * append only. Returned by both the shim driver and the Rust judge.
 *
 *   0 KIRRA_VERDICT_OK              all checks passed — admissible
 *   1 KIRRA_VERDICT_STALE_HEADER   bad/garbled header (magic mismatch or torn)
 *   2 KIRRA_VERDICT_SEQUENCE_REGRESS  sequence <= last_accepted (regress OR replay)
 *   3 KIRRA_VERDICT_DEADLINE_MISSED   now_monotonic_ns > deadline_monotonic_ns
 *   4 KIRRA_VERDICT_PAYLOAD_CORRUPT   shim CRC over the payload failed
 *   5 KIRRA_VERDICT_PAYLOAD_OVERSIZE  shim bounds reject (short-circuit; see note)
 *   6 KIRRA_VERDICT_KINEMATIC_LIMIT   |commanded_velocity| over the PROXY envelope
 */
#define KIRRA_VERDICT_OK               0u
#define KIRRA_VERDICT_STALE_HEADER     1u
#define KIRRA_VERDICT_SEQUENCE_REGRESS 2u
#define KIRRA_VERDICT_DEADLINE_MISSED  3u
#define KIRRA_VERDICT_PAYLOAD_CORRUPT  4u
#define KIRRA_VERDICT_PAYLOAD_OVERSIZE 5u
#define KIRRA_VERDICT_KINEMATIC_LIMIT  6u

/*
 * KirraContractView — the contract the judge assesses.
 *
 * Layout: NATURALLY ALIGNED, widest-first AFTER the leading pointer. NOTHING is
 * packed — `repr(C)` on the Rust side matches this field order and the compiler
 * inserts only natural tail padding. (On LP64: payload@0, generation@8, the five
 * u64 @16..55, payload_len@56, commanded_velocity@60, integrity_ok@64,
 * header_torn@65, 6 B tail pad → sizeof == 72, alignof == 8.)
 *
 * `payload` is `const volatile uint8_t*`: the SHIM obtains a coherent snapshot via
 * the GENERATION SEQLOCK (`generation`, below) and computes the CRC over this
 * region (volatile = the underlying bytes may be
 * concurrently written by an untrusted producer). By the time the JUDGE runs,
 * the shim has stabilized and bounds/CRC-checked the data; the judge reads only
 * the scalar header fields below (it does NOT walk `payload`).
 */
typedef struct KirraContractView {
    const volatile uint8_t *payload;     /* producer payload region (shim-owned) */
    uint64_t generation;                 /* seqlock: ODD=write in progress, EVEN=committed */
    uint64_t magic;                      /* must equal KIRRA_CONTRACT_MAGIC      */
    uint64_t sequence;                   /* this command's monotonic sequence    */
    uint64_t last_accepted_sequence;     /* highest already-accepted sequence    */
    uint64_t now_monotonic_ns;           /* judge's clock (monotonic domain)     */
    uint64_t deadline_monotonic_ns;      /* command deadline (same domain)       */
    uint32_t payload_len;                /* valid payload bytes; shim bounds-checks */
    int32_t  commanded_velocity;         /* extracted command scalar (see judge unit) */
    uint8_t  integrity_ok;               /* upstream integrity assertion (1 = set) */
    uint8_t  header_torn;                /* explicit upstream tear flag (1 = torn) */
} KirraContractView;

/*
 * #778 F1 — ABI LAYOUT GATE (C side). Every TU that includes this header now
 * pins the documented LP64 layout at COMPILE TIME, independently of the Rust
 * side (which pins the mirror in `kirra_judge.rs`). Reorder or resize a field on
 * either side and the build fails here — replacing the old symbol-name `nm` grep,
 * which passed through any struct-layout drift into the signed release artifact.
 * Gated on 64-bit (the leading pointer + tail padding make sizeof==72 an LP64
 * fact); both QNX 8.0 judge tuples are 64-bit.
 */
#if UINTPTR_MAX == 0xFFFFFFFFFFFFFFFFu
KIRRA_STATIC_ASSERT(sizeof(KirraContractView) == 72, "KirraContractView must be 72 bytes (LP64)");
KIRRA_STATIC_ASSERT(KIRRA_ALIGNOF(KirraContractView) == 8, "KirraContractView must be 8-byte aligned");
KIRRA_STATIC_ASSERT(offsetof(KirraContractView, payload) == 0, "payload@0");
KIRRA_STATIC_ASSERT(offsetof(KirraContractView, generation) == 8, "generation@8");
KIRRA_STATIC_ASSERT(offsetof(KirraContractView, magic) == 16, "magic@16");
KIRRA_STATIC_ASSERT(offsetof(KirraContractView, sequence) == 24, "sequence@24");
KIRRA_STATIC_ASSERT(offsetof(KirraContractView, last_accepted_sequence) == 32, "last_accepted_sequence@32");
KIRRA_STATIC_ASSERT(offsetof(KirraContractView, now_monotonic_ns) == 40, "now_monotonic_ns@40");
KIRRA_STATIC_ASSERT(offsetof(KirraContractView, deadline_monotonic_ns) == 48, "deadline_monotonic_ns@48");
KIRRA_STATIC_ASSERT(offsetof(KirraContractView, payload_len) == 56, "payload_len@56");
KIRRA_STATIC_ASSERT(offsetof(KirraContractView, commanded_velocity) == 60, "commanded_velocity@60");
KIRRA_STATIC_ASSERT(offsetof(KirraContractView, integrity_ok) == 64, "integrity_ok@64");
KIRRA_STATIC_ASSERT(offsetof(KirraContractView, header_torn) == 65, "header_torn@65");
#endif /* LP64 */

/*
 * The Rust JUDGE entry. Renders the CONTRACT verdict (magic → sequence →
 * deadline → integrity → kinematic) on a view the shim has already stabilized.
 * It dereferences `v`, so on the Rust side it is an `unsafe extern "C" fn` with a
 * documented `# Safety` caller contract (CERT-005 RSR-001). A NULL `v` returns
 * KIRRA_VERDICT_STALE_HEADER (fail-closed), never UB.
 *
 * NOTE: the judge does NOT produce PAYLOAD_OVERSIZE or PAYLOAD_CORRUPT — those
 * are the shim driver's responsibility and never cross this boundary (oversize
 * short-circuits in the shim before the judge is ever called).
 */
uint8_t kirra_judge_assess(const KirraContractView *v);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* KIRRA_FFI_H */
