/*
 * kirra.h — Kirra safety-governor C ABI (ADR-0006 Clause 3).
 *
 * The C/C++ integration boundary for the Kirra runtime safety governor. A caller
 * PROPOSES a scalar actuator command; the governor BOUNDS it, fail-closed, against
 * a hard kinematic envelope and rate-of-change limits compiled into the library.
 * The doer is never trusted for safety — these functions are the invariant.
 *
 * All functions operate on ONE process-wide governor and are thread-safe (an
 * internal mutex serialises access). Every function fails CLOSED: any error path
 * (non-finite input, invalid timestep, poisoned lock) yields the safe value
 * (0.0 / 0), never an unclamped or non-finite command.
 *
 * Link against the cdylib `libkirra_verifier` (Cargo crate-type includes cdylib);
 * see examples/c/ for a runnable demo.
 */
#ifndef KIRRA_H
#define KIRRA_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * Bound a proposed LINEAR velocity against the envelope + rate limits.
 *
 * @param demand  proposed linear velocity (m/s).
 * @param dt      timestep since the previous command (seconds); must be > 0.
 * @return the sanitized velocity to actuate — ALWAYS finite and within the
 *         envelope. A non-finite @p demand, a non-positive @p dt, or an internal
 *         lock failure returns the fail-closed fallback (0.0).
 */
double kirra_filter_move_velocity(double demand, double dt);

/*
 * Verdict reason codes returned in KirraVerdict.code. Stable wire values — only
 * appended, never renumbered. A poisoned internal lock yields LOCK_POISONED with
 * a fail-closed value of 0.0.
 */
#define KIRRA_VERDICT_PASSTHROUGH             0  /* in envelope + rate: unchanged */
#define KIRRA_VERDICT_ENVELOPE_CLAMP          1  /* clamped to the hard envelope  */
#define KIRRA_VERDICT_RATE_CLAMP              2  /* clamped to the rate-of-change */
#define KIRRA_VERDICT_NONFINITE_REJECTED      3  /* NaN/Inf demand: fail-closed   */
#define KIRRA_VERDICT_INVALID_DT_REJECTED     4  /* dt <= 0: fail-closed          */
#define KIRRA_VERDICT_DEGRADED_POSTURE_CLAMP  5  /* bounded inside a reduced cap  */
#define KIRRA_VERDICT_DEGRADED_DECEL_HOLD     6  /* decel-to-stop-and-hold        */
#define KIRRA_VERDICT_SHADOW_HOLD             7  /* shadow mode: last value held  */
#define KIRRA_VERDICT_LOCKOUT_FALLBACK        8  /* LockedOut: fallback commanded */
#define KIRRA_VERDICT_LOCK_POISONED           9  /* internal lock poisoned        */

/**
 * A governed-command verdict: the sanitized scalar to actuate plus WHY it was
 * bounded. Returned by value from kirra_check_move_velocity.
 */
typedef struct KirraVerdict {
    double sanitized_value; /* what to actuate — ALWAYS finite, inside envelope */
    int32_t code;           /* one of the KIRRA_VERDICT_* codes                 */
} KirraVerdict;

/**
 * Bound a proposed LINEAR velocity AND report the reason.
 *
 * Same fail-closed bounding as kirra_filter_move_velocity — sanitized_value is
 * identical for the same input — but also returns a KIRRA_VERDICT_* code so the
 * caller can tell a clean passthrough from a clamp or a fail-closed rejection.
 *
 * @param demand  proposed linear velocity (m/s).
 * @param dt      timestep since the previous command (seconds); must be > 0.
 * @return a KirraVerdict; a poisoned internal lock yields {0.0, LOCK_POISONED}.
 */
KirraVerdict kirra_check_move_velocity(double demand, double dt);

/**
 * Bound a proposed ANGULAR velocity to the governor's maximum angular rate.
 *
 * @param angular_demand  proposed angular velocity (rad/s).
 * @param dt              timestep (seconds); currently unused by the angular clamp.
 * @return the clamped angular rate — ALWAYS finite. A non-finite input returns 0.0
 *         and decays the trust score; an over-limit input is clamped to the bound and
 *         decays trust. If the internal lock cannot be acquired it returns 0.0
 *         WITHOUT touching trust (fail-closed).
 */
double kirra_filter_rotate_velocity(double angular_demand, double dt);

/**
 * @return the governor's current trust score, 0–100. Safe ticks raise it; clamps
 *         and fail-closed rejections decay it. An internal lock failure reads 0.
 */
uint32_t kirra_get_trust_score(void);

/*
 * Governor operating-posture codes returned by kirra_posture(). Ordered
 * most-permissive (0) to most-restrictive (3); stable, append-only.
 */
#define KIRRA_POSTURE_NOMINAL      0  /* full autonomy — commands pass          */
#define KIRRA_POSTURE_CONSTRAINED  1  /* constrained/advisory — reduced cap      */
#define KIRRA_POSTURE_SHADOW       2  /* shadow — no new motion authored         */
#define KIRRA_POSTURE_LOCKED_OUT   3  /* locked out — fallback only              */

/**
 * @return the governor's current operating posture as a KIRRA_POSTURE_* code
 *         (the trust-mode band the score has settled into). An internal lock
 *         failure fails CLOSED to the most-restrictive posture
 *         (KIRRA_POSTURE_LOCKED_OUT), never a permissive default.
 */
int32_t kirra_posture(void);

/**
 * The governor's compiled hard kinematic envelope + rate limits — the SAME bounds
 * kirra_check_move_velocity / kirra_filter_* enforce. Returned by value.
 */
typedef struct KirraEnvelope {
    double max_linear_velocity_mps;      /* upper envelope bound              */
    double min_linear_velocity_mps;      /* lower envelope bound (== -max)    */
    double max_angular_velocity_radps;   /* angular-rate ceiling             */
    double max_linear_acceleration_mps2; /* rate-of-change limit             */
    double fallback_linear_velocity_mps; /* fail-closed fallback command     */
} KirraEnvelope;

/**
 * @return the governor's compiled envelope, so a caller can pre-clamp its own
 *         proposals or display the limits. An internal lock failure fails CLOSED
 *         to an ALL-ZERO envelope (max velocity 0 admits only a stop).
 */
KirraEnvelope kirra_envelope(void);

/*
 * Release-token verify result codes returned by kirra_verify_release_token().
 * RELEASE ONLY on KIRRA_RELEASE_OK (0); every other code is fail-closed.
 */
#define KIRRA_RELEASE_OK                 0  /* token approves the digest + valid sig */
#define KIRRA_RELEASE_DIGEST_MISMATCH    1  /* approval was for different bytes       */
#define KIRRA_RELEASE_SIGNATURE_INVALID  2  /* forged / tampered / wrong signer       */
#define KIRRA_RELEASE_BAD_ARGS          -1  /* null / wrong length / invalid key      */

/**
 * Verify a governor release token before actuating a command (HVCHAN step 7).
 *
 * Confirms the governor approved EXACTLY the command about to be actuated and the
 * signature verifies against the governor key. No crypto is done by the caller.
 *
 * @param token_ptr   96 bytes: digest(32) || Ed25519 signature(64).
 * @param token_len   must be 96.
 * @param digest_ptr  32 bytes: the SHA-256 digest of the command about to actuate.
 * @param digest_len  must be 32.
 * @param vk_ptr      32 bytes: the governor Ed25519 verifying key.
 * @param vk_len      must be 32.
 * @return KIRRA_RELEASE_OK (0) only if the token approves @p digest_ptr and the
 *         signature verifies; otherwise a non-zero fail-closed KIRRA_RELEASE_*
 *         code. Release ONLY on KIRRA_RELEASE_OK.
 *
 * @warning The caller owns pointer validity: each pointer must address its stated
 *          length of valid, non-aliased bytes that outlive the call.
 */
int32_t kirra_verify_release_token(const uint8_t *token_ptr, size_t token_len,
                                   const uint8_t *digest_ptr, size_t digest_len,
                                   const uint8_t *vk_ptr, size_t vk_len);

/**
 * Authenticated supervisor reset of the governor's trust state.
 *
 * Requires the KIRRA_SUPERVISOR_RESET_KEY environment variable to be set and the
 * presented @p token_ptr to match it. Rate-limited: repeated failures arm a
 * brute-force cooldown. The token bytes are never logged.
 *
 * @param token_ptr  pointer to @p token_len readable bytes (the reset token).
 * @param token_len  token length in bytes; must be in [1, 64].
 * @return 1 on a successful reset, 0 on ANY rejection (bad/oversized token,
 *         null pointer, unset key, active cooldown) — fail-closed.
 *
 * @warning The caller owns pointer validity: @p token_ptr must address
 *          @p token_len valid, non-aliased bytes that outlive the call.
 */
int kirra_reset_state(const uint8_t *token_ptr, size_t token_len);

#ifdef __cplusplus
}
#endif

#endif /* KIRRA_H */
