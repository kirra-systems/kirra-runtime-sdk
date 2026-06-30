// tools/qnx-rtm-harness/wcet_measure.cpp
//
// Objective 1 (#274) — per-verdict WCET measurement of the no_std judge
// `kirra_judge_assess` on a QNX SDP 8.0 target under SCHED_FIFO.
//
// DRAFT — a defined build, not yet run. Not wired into CMakeLists (it is gated
// behind the QNX cross-build; see docs/safety/WCET_QNX_BRINGUP.md). Compiles on a
// host for a smoke run, but a host/Linux number is INDICATIVE only — ONLY a
// QNX-target-under-FIFO number is a WCET claim.
//
// BOUNDARIES (see WCET_QNX_BRINGUP.md):
//   * A Jetson cannot run QNX (L4T/Linux, doer-side). The QNX target is separate.
//   * Phase I = QNX SDP 8.0 on an aarch64 eval board (preferred) / x86_64 VM
//     (fallback) — feasibility, not cert-grade. Phase II = NVIDIA DRIVE + QNX OS
//     for Safety + Ferrocene-qualified Rust — the certified number.
//
// WCET PATH: the OK/admissible view. The judge runs magic → sequence → deadline
// → integrity → kinematic IN ORDER and returns early on the first failure, so the
// LONGEST path is the all-pass (admissible) case. Timing a failing case would
// time a short-circuit, not the WCET.

#include "kirra_ffi.h"

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <string_view>
#include <vector>

#include <ctime>
#include <pthread.h>
#include <sched.h>

namespace {

constexpr std::size_t WARMUP_ITERS  = 10'000;
constexpr std::size_t MEASURE_ITERS = 1'000'000;

// Build the OK/admissible view — the WCET path (no early short-circuit).
KirraContractView make_admissible_view(const std::uint8_t *payload, std::uint32_t len) {
    KirraContractView v{};
    v.payload                = payload;
    v.magic                  = KIRRA_CONTRACT_MAGIC;
    v.sequence               = 2;            // > last_accepted → not a regress/replay
    v.last_accepted_sequence = 1;
    v.now_monotonic_ns       = 1'000;        // now < deadline → not missed
    v.deadline_monotonic_ns  = 1'000'000;
    v.payload_len            = len;
    v.commanded_velocity     = 0;            // within the PROXY envelope → no kinematic limit
    v.integrity_ok           = 1;
    v.header_torn            = 0;
    return v;
}

// Monotonic nanoseconds. clock_gettime self-overhead (~tens of ns) is inside each
// sample and is roughly constant, so the reported MAX is a CONSERVATIVE (slightly
// over) WCET — safe for a bound. On QNX prefer ClockCycles() + SYSPAGE
// cycles_per_sec for finer resolution, and subtract the measured clock overhead.
std::uint64_t now_ns() {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return static_cast<std::uint64_t>(ts.tv_sec) * 1'000'000'000ull
         + static_cast<std::uint64_t>(ts.tv_nsec);
}

// Raise to SCHED_FIFO max priority and pin to one isolated core. Returns TRUE
// iff SCHED_FIFO was actually granted — the caller uses this to label the row
// honestly (FIFO is REQUIRED for a WCET claim; without it the run is INDICATIVE).
//   * POSIX form (works on a Linux eval VM; isolate the core with isolcpus=<cpu>).
//   * On QNX, ALSO set the runmask via
//       ThreadCtl(_NTO_TCTL_RUNMASK_GET_AND_SET_INHERIT, &mask)
//     and SchedSet() the FIFO priority. SCHED_FIFO requires privilege.
bool enter_rt(int cpu) {
    struct sched_param sp{};
    sp.sched_priority = sched_get_priority_max(SCHED_FIFO);
    const bool fifo_granted =
        pthread_setschedparam(pthread_self(), SCHED_FIFO, &sp) == 0;
    if (!fifo_granted) {
        std::fprintf(stderr,
            "WARN: SCHED_FIFO not granted (need privilege) — timing is INDICATIVE only\n");
    }
#if defined(__linux__)
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(cpu, &set);
    pthread_setaffinity_np(pthread_self(), sizeof(set), &set);
#else
    (void)cpu;  // QNX: pin via ThreadCtl(_NTO_TCTL_RUNMASK_GET_AND_SET_INHERIT, ...)
#endif
    return fifo_granted;
}

} // namespace

int main(int argc, char **argv) {
    // The target triple is passed in (e.g. `x86_64-pc-nto-qnx800`) so the emitted
    // row names the ACTUAL target, never a placeholder. Absent → flagged below.
    const char *target = (argc > 1) ? argv[1] : "<TARGET_TRIPLE_TBD>";

    const bool fifo_granted = enter_rt(/*cpu=*/1);

    static const std::uint8_t payload[8] = {0, 1, 2, 3, 4, 5, 6, 7};
    KirraContractView v = make_admissible_view(payload, sizeof(payload));

    // Guard: the WCET path MUST return OK, else we are timing a short-circuit.
    if (kirra_judge_assess(&v) != KIRRA_VERDICT_OK) {
        std::fprintf(stderr, "FATAL: admissible view did not return OK — wrong WCET path\n");
        return 2;
    }

    // Warm caches / branch predictors. `volatile` sink defeats DCE; the extern
    // "C" call into the separately-linked staticlib cannot be inlined or elided.
    volatile std::uint8_t sink = 0;
    for (std::size_t i = 0; i < WARMUP_ITERS; ++i)
        sink = static_cast<std::uint8_t>(sink ^ kirra_judge_assess(&v));

    std::vector<std::uint64_t> samples;
    samples.reserve(MEASURE_ITERS);
    for (std::size_t i = 0; i < MEASURE_ITERS; ++i) {
        const std::uint64_t t0 = now_ns();
        sink = static_cast<std::uint8_t>(sink ^ kirra_judge_assess(&v));
        const std::uint64_t t1 = now_ns();
        samples.push_back(t1 - t0);
    }
    (void)sink;

    std::sort(samples.begin(), samples.end());
    const std::uint64_t mn   = samples.front();
    const std::uint64_t med  = samples[samples.size() / 2];
    const std::uint64_t p999 = samples[(samples.size() * 999) / 1000];
    const std::uint64_t mx   = samples.back();

    std::printf("WCET kirra_judge_assess  n=%zu  min=%lluns  med=%lluns  p99.9=%lluns  MAX=%lluns\n",
                MEASURE_ITERS,
                (unsigned long long)mn, (unsigned long long)med,
                (unsigned long long)p999, (unsigned long long)mx);

    // Honest status token (the methodology's INVARIANT: host timing is INDICATIVE,
    // never WCET — only a QNX-target run UNDER FIFO is a WCET claim):
    //   * non-QNX build           → HOST-INDICATIVE        (never a WCET number)
    //   * QNX build, FIFO denied   → QNX-TARGET-INDICATIVE  (FIFO is required)
    //   * QNX build, FIFO granted  → QNX-TARGET-MEASURED    (the real row)
    // The `sched` column reflects what was ACTUALLY granted, not what we asked for.
    const char *sched = fifo_granted ? "SCHED_FIFO" : "SCHED_OTHER";
#if defined(__QNXNTO__)
    const char *status = fifo_granted ? "QNX-TARGET-MEASURED" : "QNX-TARGET-INDICATIVE";
#else
    const char *status = "HOST-INDICATIVE";
#endif

    // A MEASURED row with a placeholder triple would be unfilable — warn loudly.
    if (std::string_view(status) == "QNX-TARGET-MEASURED"
        && std::string_view(target) == "<TARGET_TRIPLE_TBD>") {
        std::fprintf(stderr,
            "WARN: target triple not supplied — pass it as argv[1] "
            "(e.g. x86_64-pc-nto-qnx800) so the MEASURED row names the real target\n");
    }

    // CSV row — replaces the harness placeholder `wcet_status = TBD-QNX-TARGET`.
    std::printf("metric,target,sched,n,warmup,max_ns,p999_ns,wcet_status\n");
    std::printf("kirra_judge_assess,%s,%s,%zu,%zu,%llu,%llu,%s\n",
                target, sched, MEASURE_ITERS, WARMUP_ITERS,
                (unsigned long long)mx, (unsigned long long)p999, status);
    return 0;
}
