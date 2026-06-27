// rtm_harness.cpp — the automated FDIT / RTM fault-injection harness.
//
// EPIC #270, issue #271. Injects EIGHT fault classes through the SHIM (driver) →
// JUDGE (checker) path, one ROW per class, each row's name claiming EXACTLY what
// that row injects. The GATE is VERDICT CORRECTNESS ONLY: a wrong verdict on any
// row exits non-zero (so `ctest` / the build fails).
//
// HONESTY: the per-row p50/p99/max are INDICATIVE host timing (FDIT shape), NOT
// certified WCET. Certified WCET must be measured on the QNX target under FIFO
// scheduling (#274). Host numbers are never presented as WCET.
//
// RTM TRACING (#272, this change): the local `SG-0N` is the HARNESS ROW INDEX —
// NOT a kernel RTM identifier. The `rtm_id`/`tr_id` fields are the bridge to the
// real kernel RTM (docs/safety/REQUIREMENTS_TRACEABILITY.md, AEGIS-RTM-001). The
// honest grounded result (per-row JUSTIFICATION in QNX_MAPPING.md): only the
// over-envelope row has a genuine RTM home — SG-001/TR-001 (PROXY bound,
// reject-not-clamp). The other six transport-contract fault classes (frame magic,
// sequence monotonicity, message deadline, payload CRC, payload bounds, replay)
// have NO matching kernel TR — they are honest NO-RTM-ID gaps, candidate new TRs
// for the EPIC #270 transport-contract lane. The valid row is the clean-accept
// CONTROL. The gaps ARE evidence (they feed the RTM gap report); never force-fit.

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <ctime>
#include <vector>

#include "kirra_ffi.h"
#include "kirra_shim.hpp"

namespace {

constexpr std::uint32_t kMaxPayloadLen = 32;
constexpr std::uint32_t kWarmup = 1000;
constexpr std::uint32_t kIters = 8000;

const char *verdict_name(std::uint8_t v) {
    switch (v) {
        case KIRRA_VERDICT_OK: return "Ok";
        case KIRRA_VERDICT_STALE_HEADER: return "StaleHeader";
        case KIRRA_VERDICT_SEQUENCE_REGRESS: return "SequenceRegress";
        case KIRRA_VERDICT_DEADLINE_MISSED: return "DeadlineMissed";
        case KIRRA_VERDICT_PAYLOAD_CORRUPT: return "PayloadCorrupt";
        case KIRRA_VERDICT_PAYLOAD_OVERSIZE: return "PayloadOversize";
        case KIRRA_VERDICT_KINEMATIC_LIMIT: return "KinematicLimit";
        default: return "??";
    }
}

std::uint64_t now_ns() {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return static_cast<std::uint64_t>(ts.tv_sec) * 1000000000ull +
           static_cast<std::uint64_t>(ts.tv_nsec);
}

// A baseline VALID contract (each row clones + mutates this).
struct Baseline {
    std::uint8_t payload[16] = {1, 2, 3, 4, 5, 6, 7, 8,
                                9, 10, 11, 12, 13, 14, 15, 16};
    KirraContractView view{};
    std::uint32_t crc = 0;

    Baseline() {
        crc = kirra::crc32_ieee(payload, sizeof(payload));
        view.payload = payload;
        view.generation = 2;                         // EVEN ⇒ committed (seqlock)
        view.magic = KIRRA_CONTRACT_MAGIC;
        view.last_accepted_sequence = 1000;
        view.sequence = 1001;                       // strictly newer ⇒ valid
        view.now_monotonic_ns = 1000000;            // 1 ms
        view.deadline_monotonic_ns = 10000000000ull; // far future
        view.payload_len = sizeof(payload);          // 16 ≤ 32
        view.commanded_velocity = 5000;              // < PROXY 22350 mm/s
        view.integrity_ok = 1;
        view.header_torn = 0;
    }
};

// Mutator: takes the baseline view + payload + crc by value, injects a fault.
struct Row {
    const char *id;   // LOCAL harness row index (SG-0N) — NOT a kernel RTM id.
    const char *name; // exactly what this row injects (honest fault name).
    // RTM bridge (#272). `rtm_display` is the human column; `rtm_id`/`tr_id` are
    // the CSV cells. A genuine hit carries SG-NNN/TR-NNN; an honest gap carries
    // "NO-RTM-ID"/"CANDIDATE"; the no-fault control carries "CONTROL"/"NONE".
    // See QNX_MAPPING.md for the per-row justification.
    const char *rtm_display;
    const char *rtm_id;
    const char *tr_id;
    std::uint8_t expected;
    // Whether the Rust JUDGE is invoked. FALSE for the rows the SHIM driver
    // rejects on its own (oversize bounds AND CRC corruption) — they never cross
    // the FFI. The harness asserts this, demonstrating the concern split.
    bool expect_judge_called;
    // Mutates the per-iteration working copy.
    void (*inject)(KirraContractView &v, std::uint8_t *payload, std::uint32_t &crc);
};

void inj_valid(KirraContractView &, std::uint8_t *, std::uint32_t &) {}
void inj_bad_magic(KirraContractView &v, std::uint8_t *, std::uint32_t &) {
    v.magic = 0xDEADBEEFDEADBEEFull;
}
void inj_seq_regress(KirraContractView &v, std::uint8_t *, std::uint32_t &) {
    v.sequence = 999; // strictly LOWER than last_accepted (1000)
}
void inj_deadline(KirraContractView &v, std::uint8_t *, std::uint32_t &) {
    v.deadline_monotonic_ns = 0; // now (1ms) > 0 ⇒ missed
}
void inj_payload_corrupt(KirraContractView &, std::uint8_t *payload, std::uint32_t &) {
    payload[0] ^= 0xFF; // flip a byte WITHOUT recomputing the declared CRC
}
void inj_oversize(KirraContractView &v, std::uint8_t *, std::uint32_t &) {
    v.payload_len = kMaxPayloadLen + 1; // 33 > 32 ⇒ shim short-circuit
}
void inj_kinematic(KirraContractView &v, std::uint8_t *, std::uint32_t &) {
    v.commanded_velocity = 99999; // > PROXY 22350 mm/s
}
void inj_replay(KirraContractView &v, std::uint8_t *, std::uint32_t &) {
    v.sequence = 1000; // EQUAL to last_accepted ⇒ replay ⇒ SequenceRegress
}
void inj_torn_generation(KirraContractView &v, std::uint8_t *, std::uint32_t &) {
    v.generation = 3; // ODD ⇒ write-in-progress ⇒ seqlock rejects (StaleHeader),
                      // in the SHIM, before the FFI (judge NOT called).
}

// Verdicts + injection unchanged from #284; only the RTM-mapping cells are added.
// Mapping is GROUNDED in REQUIREMENTS_TRACEABILITY.md / SAFETY_GOALS.md — only the
// over-envelope row evidences a real kernel TR (SG-001/TR-001, proxy); the rest
// are honest NO-RTM-ID transport-contract gaps (QNX_MAPPING.md justifies each).
const Row kRows[] = {
    {"SG-00", "valid",                 "CONTROL",       "CONTROL",   "NONE",      KIRRA_VERDICT_OK,               true,  inj_valid},
    {"SG-01", "bad-magic",             "NO-RTM-ID",     "NO-RTM-ID", "CANDIDATE", KIRRA_VERDICT_STALE_HEADER,     true,  inj_bad_magic},
    {"SG-02", "sequence-regress",      "NO-RTM-ID",     "NO-RTM-ID", "CANDIDATE", KIRRA_VERDICT_SEQUENCE_REGRESS, true,  inj_seq_regress},
    {"SG-03", "deadline-missed",       "NO-RTM-ID",     "NO-RTM-ID", "CANDIDATE", KIRRA_VERDICT_DEADLINE_MISSED,  true,  inj_deadline},
    {"SG-04", "payload-corrupt (CRC)", "NO-RTM-ID",     "NO-RTM-ID", "CANDIDATE", KIRRA_VERDICT_PAYLOAD_CORRUPT,  false, inj_payload_corrupt},
    {"SG-05", "payload-oversize",      "NO-RTM-ID",     "NO-RTM-ID", "CANDIDATE", KIRRA_VERDICT_PAYLOAD_OVERSIZE, false, inj_oversize},
    {"SG-06", "over-envelope",         "SG-001/TR-001", "SG-001",    "TR-001",    KIRRA_VERDICT_KINEMATIC_LIMIT,  true,  inj_kinematic},
    {"SG-07", "replay (seq==last)",    "NO-RTM-ID",     "NO-RTM-ID", "CANDIDATE", KIRRA_VERDICT_SEQUENCE_REGRESS, true,  inj_replay},
    {"SG-08", "torn-write (odd gen)",  "NO-RTM-ID",     "NO-RTM-ID", "CANDIDATE", KIRRA_VERDICT_STALE_HEADER,     false, inj_torn_generation},
};

struct RowResult {
    bool pass;
    std::uint8_t observed;
    std::uint64_t p50, p99, max;
    bool short_circuit_ok; // judge-call expectation held every iteration
};

std::uint64_t pct(std::vector<std::uint64_t> &sorted, double p) {
    if (sorted.empty()) return 0;
    std::size_t idx = static_cast<std::size_t>((p / 100.0) * (sorted.size() - 1) + 0.5);
    if (idx >= sorted.size()) idx = sorted.size() - 1;
    return sorted[idx];
}

RowResult run_row(const Row &row) {
    RowResult r{true, row.expected, 0, 0, 0, true};
    std::vector<std::uint64_t> times;
    times.reserve(kIters);

    for (std::uint32_t i = 0; i < kWarmup + kIters; ++i) {
        Baseline base; // fresh per iteration (payload mutations don't leak)
        KirraContractView v = base.view;
        std::uint32_t crc = base.crc;
        row.inject(v, base.payload, crc);
        v.payload = base.payload;

        kirra::ShimInput in{&v, base.payload, crc, kMaxPayloadLen};
        bool judge_called = false;

        const std::uint64_t t0 = now_ns();
        const std::uint8_t verdict = kirra::shim_process(in, &judge_called);
        const std::uint64_t t1 = now_ns();

        if (i >= kWarmup) {
            times.push_back(t1 - t0);
        }
        r.observed = verdict;
        if (verdict != row.expected) r.pass = false;
        if (judge_called != row.expect_judge_called) r.short_circuit_ok = false;
    }

    std::sort(times.begin(), times.end());
    r.p50 = pct(times, 50.0);
    r.p99 = pct(times, 99.0);
    r.max = times.empty() ? 0 : times.back();
    return r;
}

void print_banner() {
    std::printf("============================================================================\n");
    std::printf(" KIRRA QNX RTM harness (#271) — C++ shim (driver) -> Rust judge (checker)\n");
    std::printf(" PASS GATE = VERDICT CORRECTNESS ONLY.  Timing is INDICATIVE (host FDIT).\n");
    std::printf(" Certified WCET must be measured on the QNX target under FIFO scheduling\n");
    std::printf(" (#274) — host numbers are NEVER presented as WCET.  The RTM column maps\n");
    std::printf(" each row to the kernel RTM (#272); SG-0N is the LOCAL row index, not an\n");
    std::printf(" RTM id.  Only over-envelope hits a real TR (SG-001, proxy); the rest are\n");
    std::printf(" honest NO-RTM-ID transport-contract gaps (see QNX_MAPPING.md).\n");
    std::printf("============================================================================\n");
}

} // namespace

int main() {
    print_banner();

    RowResult results[sizeof(kRows) / sizeof(kRows[0])];
    bool all_pass = true;

    std::printf("\n%-6s %-22s %-14s %-6s %-16s %10s %10s %10s\n",
                "row", "fault class", "rtm", "ok", "verdict", "p50(ns)", "p99(ns)", "max(ns)");
    std::printf("-------------------------------------------------------------------------------------------------\n");

    for (std::size_t i = 0; i < sizeof(kRows) / sizeof(kRows[0]); ++i) {
        results[i] = run_row(kRows[i]);
        const RowResult &r = results[i];
        const bool row_ok = r.pass && r.short_circuit_ok;
        if (!row_ok) all_pass = false;
        std::printf("%-6s %-22s %-14s %-6s %-16s %10llu %10llu %10llu\n",
                    kRows[i].id, kRows[i].name, kRows[i].rtm_display,
                    row_ok ? "PASS" : "FAIL",
                    verdict_name(r.observed),
                    static_cast<unsigned long long>(r.p50),
                    static_cast<unsigned long long>(r.p99),
                    static_cast<unsigned long long>(r.max));
        if (!r.pass) {
            std::printf("   ! expected %s, observed %s\n",
                        verdict_name(kRows[i].expected), verdict_name(r.observed));
        }
        if (!r.short_circuit_ok) {
            std::printf("   ! judge-call expectation violated (oversize must short-circuit)\n");
        }
    }

    // Machine-readable CSV. The RTM gap report (RTM_GAP_REPORT.md) has NO per-test
    // CSV evidence-table format to match — its tables are markdown (goal | ASIL |
    // description | tests), no timing columns — so this is the PROPOSED grounded
    // format (flagged proposed in QNX_MAPPING.md), per #272's instruction. Columns:
    // harness_row is the LOCAL index; rtm_id/tr_id are the kernel-RTM bridge;
    // wcet_status is constant TBD-QNX-TARGET (host timing is never WCET, #274).
    std::printf("\nCSV (proposed format — RTM_GAP_REPORT.md has no per-test CSV table to match):\n");
    std::printf("harness_row,rtm_id,tr_id,fault_class,verdict_expected,verdict_observed,"
                "pass,p50_ns,p99_ns,max_ns,wcet_status\n");
    for (std::size_t i = 0; i < sizeof(kRows) / sizeof(kRows[0]); ++i) {
        const RowResult &r = results[i];
        const bool row_ok = r.pass && r.short_circuit_ok;
        std::printf("%s,%s,%s,%s,%s,%s,%s,%llu,%llu,%llu,%s\n",
                    kRows[i].id, kRows[i].rtm_id, kRows[i].tr_id, kRows[i].name,
                    verdict_name(kRows[i].expected), verdict_name(r.observed),
                    row_ok ? "PASS" : "FAIL",
                    static_cast<unsigned long long>(r.p50),
                    static_cast<unsigned long long>(r.p99),
                    static_cast<unsigned long long>(r.max),
                    "TBD-QNX-TARGET");
    }

    std::printf("\nGATE (verdict correctness): %s\n", all_pass ? "PASS" : "FAIL");
    return all_pass ? 0 : 1;
}
