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
// The SG-0N row IDs are PLACEHOLDERS; mapping them to the real kernel RTM IDs
// (REQUIREMENTS_TRACEABILITY.md SG-001..SG-016) is issue #272.

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
    const char *id;
    const char *name;
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

const Row kRows[] = {
    {"SG-00", "valid",                  KIRRA_VERDICT_OK,               true,  inj_valid},
    {"SG-01", "bad-magic",              KIRRA_VERDICT_STALE_HEADER,     true,  inj_bad_magic},
    {"SG-02", "sequence-regress",       KIRRA_VERDICT_SEQUENCE_REGRESS, true,  inj_seq_regress},
    {"SG-03", "deadline-missed",        KIRRA_VERDICT_DEADLINE_MISSED,  true,  inj_deadline},
    {"SG-04", "payload-corrupt (CRC)",  KIRRA_VERDICT_PAYLOAD_CORRUPT,  false, inj_payload_corrupt},
    {"SG-05", "payload-oversize",       KIRRA_VERDICT_PAYLOAD_OVERSIZE, false, inj_oversize},
    {"SG-06", "over-envelope",          KIRRA_VERDICT_KINEMATIC_LIMIT,  true,  inj_kinematic},
    {"SG-07", "replay (seq==last)",     KIRRA_VERDICT_SEQUENCE_REGRESS, true,  inj_replay},
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
    std::printf(" (#274) — host numbers are NEVER presented as WCET.  SG-0N IDs are\n");
    std::printf(" placeholders; real-RTM-ID mapping is #272.\n");
    std::printf("============================================================================\n");
}

} // namespace

int main() {
    print_banner();

    RowResult results[sizeof(kRows) / sizeof(kRows[0])];
    bool all_pass = true;

    std::printf("\n%-6s %-22s %-6s %-16s %10s %10s %10s\n",
                "id", "fault class", "ok", "verdict", "p50(ns)", "p99(ns)", "max(ns)");
    std::printf("----------------------------------------------------------------------------------\n");

    for (std::size_t i = 0; i < sizeof(kRows) / sizeof(kRows[0]); ++i) {
        results[i] = run_row(kRows[i]);
        const RowResult &r = results[i];
        const bool row_ok = r.pass && r.short_circuit_ok;
        if (!row_ok) all_pass = false;
        std::printf("%-6s %-22s %-6s %-16s %10llu %10llu %10llu\n",
                    kRows[i].id, kRows[i].name, row_ok ? "PASS" : "FAIL",
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

    // Machine-readable CSV (placeholder IDs; #272 remaps). Column order:
    // id,fault_class,result,expected_verdict,observed_verdict,p50_ns,p99_ns,max_ns
    std::printf("\nCSV:\n");
    std::printf("id,fault_class,result,expected_verdict,observed_verdict,p50_ns,p99_ns,max_ns\n");
    for (std::size_t i = 0; i < sizeof(kRows) / sizeof(kRows[0]); ++i) {
        const RowResult &r = results[i];
        const bool row_ok = r.pass && r.short_circuit_ok;
        std::printf("%s,%s,%s,%s,%s,%llu,%llu,%llu\n",
                    kRows[i].id, kRows[i].name, row_ok ? "PASS" : "FAIL",
                    verdict_name(kRows[i].expected), verdict_name(r.observed),
                    static_cast<unsigned long long>(r.p50),
                    static_cast<unsigned long long>(r.p99),
                    static_cast<unsigned long long>(r.max));
    }

    std::printf("\nGATE (verdict correctness): %s\n", all_pass ? "PASS" : "FAIL");
    return all_pass ? 0 : 1;
}
