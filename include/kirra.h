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

/**
 * Bound a proposed ANGULAR velocity to the governor's maximum angular rate.
 *
 * @param angular_demand  proposed angular velocity (rad/s).
 * @param dt              timestep (seconds); currently unused by the angular clamp.
 * @return the clamped angular rate — ALWAYS finite. A non-finite input (or lock
 *         failure) returns 0.0; an over-limit input is clamped to the bound. Both
 *         off-nominal cases decay the trust score.
 */
double kirra_filter_rotate_velocity(double angular_demand, double dt);

/**
 * @return the governor's current trust score, 0–100. Safe ticks raise it; clamps
 *         and fail-closed rejections decay it. An internal lock failure reads 0.
 */
uint32_t kirra_get_trust_score(void);

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
