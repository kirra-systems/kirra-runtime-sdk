/*
 * Kirra SDK quickstart (C) — the CHECKER bounding a DOER's proposals over the C ABI.
 *
 * The C integration boundary (ADR-0006 Clause 3) exposes the same safety-governor
 * behaviour as the Rust `governor_quickstart` example, through `include/kirra.h`.
 * A doer PROPOSES a velocity; `kirra_check_move_velocity` BOUNDS it, fail-closed,
 * against a hard kinematic envelope compiled into `libkirra_verifier`, and reports
 * WHY it was bounded via a KirraVerdict struct.
 *
 * Build + run: see `examples/c/build_and_run.sh` (compiles against the cdylib).
 */

#include <stdio.h>
#include <math.h>
#include <inttypes.h>
#include "kirra.h"

/* Human label for a KIRRA_POSTURE_* code (integrators would branch on the int). */
static const char *posture_label(int32_t code) {
    switch (code) {
        case KIRRA_POSTURE_NOMINAL:     return "nominal";
        case KIRRA_POSTURE_CONSTRAINED: return "constrained";
        case KIRRA_POSTURE_SHADOW:      return "shadow";
        case KIRRA_POSTURE_LOCKED_OUT:  return "locked-out";
        default:                        return "unknown";
    }
}

/* Human label for a KIRRA_VERDICT_* code (integrators would branch on the int). */
static const char *verdict_label(int32_t code) {
    switch (code) {
        case KIRRA_VERDICT_PASSTHROUGH:            return "passthrough";
        case KIRRA_VERDICT_ENVELOPE_CLAMP:         return "envelope-clamp";
        case KIRRA_VERDICT_RATE_CLAMP:             return "rate-clamp";
        case KIRRA_VERDICT_NONFINITE_REJECTED:     return "nonfinite-rejected";
        case KIRRA_VERDICT_INVALID_DT_REJECTED:    return "invalid-dt-rejected";
        case KIRRA_VERDICT_DEGRADED_POSTURE_CLAMP: return "degraded-posture-clamp";
        case KIRRA_VERDICT_DEGRADED_DECEL_HOLD:    return "degraded-decel-hold";
        case KIRRA_VERDICT_SHADOW_HOLD:            return "shadow-hold";
        case KIRRA_VERDICT_LOCKOUT_FALLBACK:       return "lockout-fallback";
        case KIRRA_VERDICT_LOCK_POISONED:          return "lock-poisoned";
        default:                                   return "unknown";
    }
}

int main(void) {
    /* The doer's proposed linear velocities over 50 ms ticks. The last is a
     * corrupt NaN to exercise the fail-closed path. */
    const double dt = 0.05;
    const double proposals[] = {1.0, 1.8, 5.0 /* over-envelope */, NAN /* corrupt */};
    const size_t n = sizeof(proposals) / sizeof(proposals[0]);

    /* Discover the bounds we're being held to, up front. */
    KirraEnvelope env = kirra_envelope();
    printf("envelope: linear [%.2f, %.2f] m/s, angular <= %.2f rad/s, accel <= %.2f m/s^2\n\n",
           env.min_linear_velocity_mps, env.max_linear_velocity_mps,
           env.max_angular_velocity_radps, env.max_linear_acceleration_mps2);

    printf("proposed -> emitted   [reason]                (trust score after tick)\n");
    printf("----------------------------------------------------------------------\n");
    for (size_t i = 0; i < n; ++i) {
        /* The verdict carries both the sanitized scalar (what reaches the actuator:
         * never non-finite, never outside the envelope) AND the reason code. */
        KirraVerdict v = kirra_check_move_velocity(proposals[i], dt);
        uint32_t trust = kirra_get_trust_score();

        if (!isfinite(v.sanitized_value)) {
            fprintf(stderr, "FATAL: checker emitted a non-finite command\n");
            return 1;
        }
        printf("%8.2f -> %7.2f   [%-22s] (trust=%" PRIu32 ", posture=%s)\n",
               proposals[i], v.sanitized_value, verdict_label(v.code),
               trust, posture_label(kirra_posture()));
    }

    return 0;
}
