// kirra_demo.cpp — minimal end-to-end demo of the shim (driver) → judge (checker)
// path, including a REPLAY injection (the corrected equal-sequence rule).
//
// EPIC #270, issue #271.

#include <cstdint>
#include <cstdio>

#include "kirra_ffi.h"
#include "kirra_shim.hpp"

namespace {

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

KirraContractView make_valid(const std::uint8_t *payload, std::uint32_t len) {
    KirraContractView v{};
    v.payload = payload;
    v.magic = KIRRA_CONTRACT_MAGIC;
    v.last_accepted_sequence = 1000;
    v.sequence = 1001; // strictly newer ⇒ valid
    v.now_monotonic_ns = 1000000;
    v.deadline_monotonic_ns = 10000000000ull;
    v.payload_len = len;
    v.commanded_velocity = 5000;
    v.integrity_ok = 1;
    v.header_torn = 0;
    return v;
}

} // namespace

int main() {
    const std::uint8_t payload[8] = {10, 20, 30, 40, 50, 60, 70, 80};
    const std::uint32_t crc = kirra::crc32_ieee(payload, sizeof(payload));

    std::printf("KIRRA QNX RTM harness — end-to-end demo (#271)\n\n");

    // 1) A valid command is admitted.
    KirraContractView ok = make_valid(payload, sizeof(payload));
    bool judged = false;
    kirra::ShimInput in_ok{&ok, payload, crc, 32};
    std::uint8_t v_ok = kirra::shim_process(in_ok, &judged);
    std::printf("valid  command (seq=%llu > last=%llu)  -> %s (judge called: %s)\n",
                static_cast<unsigned long long>(ok.sequence),
                static_cast<unsigned long long>(ok.last_accepted_sequence),
                verdict_name(v_ok), judged ? "yes" : "no");

    // 2) A REPLAY (sequence == last_accepted) is rejected — the corrected rule.
    KirraContractView replay = make_valid(payload, sizeof(payload));
    replay.sequence = replay.last_accepted_sequence; // 1000 == 1000
    kirra::ShimInput in_replay{&replay, payload, crc, 32};
    std::uint8_t v_replay = kirra::shim_process(in_replay, &judged);
    std::printf("replay command (seq=%llu == last=%llu) -> %s\n",
                static_cast<unsigned long long>(replay.sequence),
                static_cast<unsigned long long>(replay.last_accepted_sequence),
                verdict_name(v_replay));

    const bool ok_pass = (v_ok == KIRRA_VERDICT_OK);
    const bool replay_pass = (v_replay == KIRRA_VERDICT_SEQUENCE_REGRESS);
    std::printf("\ndemo: %s\n", (ok_pass && replay_pass) ? "PASS" : "FAIL");
    return (ok_pass && replay_pass) ? 0 : 1;
}
