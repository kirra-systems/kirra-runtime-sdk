// kirra_shim.cpp — the C++ SHIM (driver) implementation. See kirra_shim.hpp.
//
// Built with -fno-exceptions -fno-rtti -Wall -Werror (see CMakeLists.txt): no
// exceptions, no RTTI, fail-closed at every step.

#include "kirra_shim.hpp"

#include <atomic>

namespace kirra {

std::uint32_t crc32_ieee(const std::uint8_t *data, std::uint32_t len) noexcept {
    std::uint32_t crc = 0xFFFFFFFFu;
    for (std::uint32_t i = 0; i < len; ++i) {
        crc ^= data[i];
        for (int b = 0; b < 8; ++b) {
            const std::uint32_t mask = -(crc & 1u);
            crc = (crc >> 1) ^ (0xEDB88320u & mask);
        }
    }
    return ~crc;
}

namespace {

// Bounded seqlock retry budget. A persistently odd generation (writer wedged) or
// a churning generation exhausts this and FAILS CLOSED (StaleHeader) — never a
// stale-data accept. Mirrors `kirra-contract-channel`'s MAX_SNAPSHOT_RETRIES.
constexpr unsigned kMaxSeqlockRetries = 4;

// Obtain a coherent snapshot of the volatile shared header via the odd/even
// GENERATION SEQLOCK (HVCHAN-001 §3 steps 2-3 — the same mechanism as the
// certified-style reference in `crates/kirra-contract-channel`). The publisher
// makes `generation` ODD while writing and EVEN on commit. The reader loads the
// generation; if odd, a write is in progress; otherwise it copies the body and
// re-reads the generation, accepting only if it is UNCHANGED and EVEN.
//
// A real CPU ACQUIRE BARRIER (`std::atomic_thread_fence(acquire)`) orders the body
// copy between the two generation reads, so the tear detection is SOUND on
// weakly-ordered targets (aarch64) — unlike the prior `atomic_signal_fence`
// (compiler-only, no CPU barrier) double-read-compare, which was also ABA-prone.
// Retries are bounded; exhaustion ⇒ fail closed. `out` receives the stabilized,
// non-volatile snapshot the judge will see. Returns true iff the snapshot is
// coherent AND not flagged torn.
bool seqlock_acquire_snapshot(const volatile KirraContractView *hdr,
                              KirraContractView *out) noexcept {
    for (unsigned attempt = 0; attempt < kMaxSeqlockRetries; ++attempt) {
        const std::uint64_t g1 = hdr->generation;
        std::atomic_thread_fence(std::memory_order_acquire); // rmb after reading g1
        if ((g1 & 1u) != 0u) {
            continue; // odd ⇒ a write is in progress → retry
        }

        // Copy the whole struct into local, non-volatile memory.
        KirraContractView a;
        a.payload = hdr->payload;
        a.generation = g1;
        a.magic = hdr->magic;
        a.sequence = hdr->sequence;
        a.last_accepted_sequence = hdr->last_accepted_sequence;
        a.now_monotonic_ns = hdr->now_monotonic_ns;
        a.deadline_monotonic_ns = hdr->deadline_monotonic_ns;
        a.payload_len = hdr->payload_len;
        a.commanded_velocity = hdr->commanded_velocity;
        a.integrity_ok = hdr->integrity_ok;
        a.header_torn = hdr->header_torn;

        std::atomic_thread_fence(std::memory_order_acquire); // rmb before reading g2
        const std::uint64_t g2 = hdr->generation;
        if (g1 == g2) {
            // Unchanged and even ⇒ the copy did not race a writer.
            *out = a;
            // Belt-and-braces: an explicit upstream tear flag also fails closed.
            return a.header_torn == 0;
        }
        // generation moved across the copy ⇒ torn → retry.
    }
    return false; // bounded retry exhausted ⇒ fail closed
}

} // namespace

std::uint8_t shim_process(const ShimInput &in, bool *judge_was_called) noexcept {
    if (judge_was_called) {
        *judge_was_called = false;
    }

    // DRIVER step 1 — coherent snapshot via the generation seqlock.
    KirraContractView snap;
    if (!seqlock_acquire_snapshot(in.header, &snap)) {
        return KIRRA_VERDICT_STALE_HEADER;
    }

    // DRIVER step 2 — bounds. An oversize payload SHORT-CIRCUITS in the shim and
    // NEVER crosses the FFI (the judge is not called).
    if (snap.payload_len > in.max_payload_len) {
        return KIRRA_VERDICT_PAYLOAD_OVERSIZE;
    }

    // DRIVER step 3 — payload integrity (CRC). Computed in the driver over the
    // bounds-checked length; a mismatch fails closed without reaching the judge.
    const std::uint32_t actual_crc =
        crc32_ieee(in.payload, snap.payload_len);
    if (actual_crc != in.declared_crc) {
        return KIRRA_VERDICT_PAYLOAD_CORRUPT;
    }

    // Hand the JUDGE the stabilized snapshot (non-volatile). The judge reads only
    // the scalar header fields; null the payload pointer it never walks.
    snap.payload = nullptr;
    if (judge_was_called) {
        *judge_was_called = true;
    }
    return kirra_judge_assess(&snap);
}

} // namespace kirra
