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

    /* Release-token verify (HVCHAN step 7): a precomputed honest token — the
     * governor's signed approval of the digest of command "steer:1.5" — verifies
     * against that digest + the governor key; a flipped signature byte does not.
     * (Generated in Rust: SigningKey::from_bytes([42;32]) over the command view.) */
    const uint8_t token[96] = {
        0x18, 0x88, 0x3b, 0x8e, 0x78, 0x44, 0x34, 0x13, 0xc5, 0x1a, 0xd5, 0x46, 0xe4, 0x70, 0x89, 0x25,
        0x05, 0xf2, 0x00, 0xe8, 0x9f, 0x81, 0xdd, 0xaa, 0xb9, 0xf6, 0x27, 0xef, 0x97, 0x34, 0x22, 0x93,
        0xb2, 0x2f, 0x97, 0x36, 0xe3, 0x64, 0xaa, 0x0d, 0x80, 0x8f, 0xbd, 0xa3, 0x6c, 0xea, 0xc4, 0x10,
        0xe4, 0x6f, 0x66, 0xa6, 0x71, 0xe5, 0x63, 0xb9, 0xfe, 0x00, 0xdf, 0x82, 0x2c, 0xf7, 0x11, 0x4a,
        0x89, 0xdd, 0x82, 0x68, 0xd2, 0x17, 0x0c, 0xa7, 0x3c, 0xab, 0x83, 0xa4, 0x62, 0x40, 0xc7, 0x7c,
        0x40, 0xec, 0x1d, 0x23, 0x8c, 0x40, 0x91, 0xbf, 0x47, 0x9b, 0x08, 0x35, 0xda, 0xe2, 0x53, 0x02};
    const uint8_t digest[32] = {
        0x18, 0x88, 0x3b, 0x8e, 0x78, 0x44, 0x34, 0x13, 0xc5, 0x1a, 0xd5, 0x46, 0xe4, 0x70, 0x89, 0x25,
        0x05, 0xf2, 0x00, 0xe8, 0x9f, 0x81, 0xdd, 0xaa, 0xb9, 0xf6, 0x27, 0xef, 0x97, 0x34, 0x22, 0x93};
    const uint8_t gov_vk[32] = {
        0x19, 0x7f, 0x6b, 0x23, 0xe1, 0x6c, 0x85, 0x32, 0xc6, 0xab, 0xc8, 0x38, 0xfa, 0xcd, 0x5e, 0xa7,
        0x89, 0xbe, 0x0c, 0x76, 0xb2, 0x92, 0x03, 0x34, 0x03, 0x9b, 0xfa, 0x8b, 0x3d, 0x36, 0x8d, 0x61};

    printf("\nrelease-token verify:\n");
    int ok = kirra_verify_release_token(token, 96, digest, 32, gov_vk, 32);
    printf("  honest token  -> code %d (%s)\n", ok, ok == KIRRA_RELEASE_OK ? "RELEASE" : "denied");
    if (ok != KIRRA_RELEASE_OK) {
        fprintf(stderr, "FATAL: an honest release token failed to verify\n");
        return 1;
    }
    uint8_t tampered[96];
    for (size_t i = 0; i < 96; ++i) tampered[i] = token[i];
    tampered[64] ^= 0x01; /* flip a signature byte */
    int bad = kirra_verify_release_token(tampered, 96, digest, 32, gov_vk, 32);
    printf("  tampered token -> code %d (%s)\n", bad,
           bad == KIRRA_RELEASE_OK ? "RELEASE" : "denied (fail-closed)");
    if (bad == KIRRA_RELEASE_OK) {
        fprintf(stderr, "FATAL: a tampered release token was accepted\n");
        return 1;
    }

    return 0;
}
