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

// Stabilize the volatile shared header by reading it TWICE and comparing the
// fields. Equal copies ⇒ no torn write occurred across the reads. `out` receives
// the stabilized, non-volatile snapshot the judge will see.
//
// Returns true if the snapshot is coherent (and not flagged torn); false on a
// detected tear. A real producer racing the reader would surface here; the
// injected `header_torn` flag is honored as a belt-and-braces signal.
bool stabilize_header(const volatile KirraContractView *hdr,
                      KirraContractView *out) noexcept {
    // First read.
    KirraContractView a;
    a.payload = hdr->payload;
    a.magic = hdr->magic;
    a.sequence = hdr->sequence;
    a.last_accepted_sequence = hdr->last_accepted_sequence;
    a.now_monotonic_ns = hdr->now_monotonic_ns;
    a.deadline_monotonic_ns = hdr->deadline_monotonic_ns;
    a.payload_len = hdr->payload_len;
    a.commanded_velocity = hdr->commanded_velocity;
    a.integrity_ok = hdr->integrity_ok;
    a.header_torn = hdr->header_torn;

    // Prevent the compiler from coalescing the two volatile reads.
    std::atomic_signal_fence(std::memory_order_acq_rel);

    // Second read.
    KirraContractView b;
    b.payload = hdr->payload;
    b.magic = hdr->magic;
    b.sequence = hdr->sequence;
    b.last_accepted_sequence = hdr->last_accepted_sequence;
    b.now_monotonic_ns = hdr->now_monotonic_ns;
    b.deadline_monotonic_ns = hdr->deadline_monotonic_ns;
    b.payload_len = hdr->payload_len;
    b.commanded_velocity = hdr->commanded_velocity;
    b.integrity_ok = hdr->integrity_ok;
    b.header_torn = hdr->header_torn;

    const bool coherent =
        a.payload == b.payload && a.magic == b.magic && a.sequence == b.sequence &&
        a.last_accepted_sequence == b.last_accepted_sequence &&
        a.now_monotonic_ns == b.now_monotonic_ns &&
        a.deadline_monotonic_ns == b.deadline_monotonic_ns &&
        a.payload_len == b.payload_len &&
        a.commanded_velocity == b.commanded_velocity &&
        a.integrity_ok == b.integrity_ok && a.header_torn == b.header_torn;

    *out = a;
    return coherent && a.header_torn == 0;
}

} // namespace

std::uint8_t shim_process(const ShimInput &in, bool *judge_was_called) noexcept {
    if (judge_was_called) {
        *judge_was_called = false;
    }

    // DRIVER step 1 — tear detection on the header (double-read).
    KirraContractView snap;
    if (!stabilize_header(in.header, &snap)) {
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
