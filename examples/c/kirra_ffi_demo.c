/*
 * Kirra SDK quickstart (C) — the CHECKER bounding a DOER's proposals over the C ABI.
 *
 * The C integration boundary (ADR-0006 Clause 3) exposes the same safety-governor
 * behaviour as the Rust `governor_quickstart` example, through `include/kirra.h`.
 * A doer PROPOSES a velocity; `kirra_filter_move_velocity` BOUNDS it, fail-closed,
 * against a hard kinematic envelope compiled into `libkirra_verifier`.
 *
 * Build + run: see `examples/c/build_and_run.sh` (compiles against the cdylib).
 */

#include <stdio.h>
#include <math.h>
#include "kirra.h"

int main(void) {
    /* The doer's proposed linear velocities over 50 ms ticks. The last is a
     * corrupt NaN to exercise the fail-closed path. */
    const double dt = 0.05;
    const double proposals[] = {1.0, 1.8, 5.0 /* over-envelope */, NAN /* corrupt */};
    const size_t n = sizeof(proposals) / sizeof(proposals[0]);

    printf("proposed -> emitted   (trust score after tick)\n");
    printf("----------------------------------------------\n");
    for (size_t i = 0; i < n; ++i) {
        /* The returned scalar is what reaches the actuator: never non-finite,
         * never outside the envelope compiled into the governor. */
        double emitted = kirra_filter_move_velocity(proposals[i], dt);
        uint32_t trust = kirra_get_trust_score();

        if (!isfinite(emitted)) {
            fprintf(stderr, "FATAL: checker emitted a non-finite command\n");
            return 1;
        }
        printf("%8.2f -> %7.2f   (trust=%u)\n", proposals[i], emitted, trust);
    }

    return 0;
}
