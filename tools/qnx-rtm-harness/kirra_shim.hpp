// kirra_shim.hpp — the C++ SHIM (the driver) for the QNX RTM harness.
//
// EPIC #270, issue #271. The concern split (see README + ADR-0006 Clause 3):
// the SHIM owns MEMORY/TRANSPORT safety — odd/even generation-seqlock tear
// detection on the header, bounds rejection (oversize SHORT-CIRCUITS here and
// NEVER crosses the FFI), and the in-place CRC over the payload. The Rust JUDGE
// (`kirra_judge_assess`) renders the CONTRACT verdict on the stabilized snapshot
// the shim hands it. Memory faults die in the driver; contract faults go to the
// judge.
//
// The shim does NOT pre-filter equal sequences — replay rejection is the JUDGE's
// responsibility (`sequence <= last_accepted`). (An old comment claiming
// shim-side equal-sequence filtering was the original defect; it is not
// reintroduced here.)
#ifndef KIRRA_SHIM_HPP
#define KIRRA_SHIM_HPP

#include <cstdint>

#include "kirra_ffi.h"

namespace kirra {

// CRC-32 (IEEE 802.3, reflected, poly 0xEDB88320), table-less. Kept in-shim so
// the harness can compute correct CRCs (and corrupt them) with the same routine.
std::uint32_t crc32_ieee(const std::uint8_t *data, std::uint32_t len) noexcept;

// What the shim driver consumes for one assessment.
struct ShimInput {
    // The SHARED header, qualified volatile: its bytes may be concurrently
    // written by an untrusted producer. The shim reads it under the odd/even
    // generation seqlock to obtain a coherent snapshot before trusting any field.
    const volatile KirraContractView *header;
    const std::uint8_t *payload;       // payload bytes (header->payload_len valid)
    std::uint32_t declared_crc;        // CRC the producer claims over the payload
    std::uint32_t max_payload_len;     // bounds limit (oversize ⇒ short-circuit)
};

// Process one input through the DRIVER, then (only if it survives the driver) the
// JUDGE. Returns a KIRRA_VERDICT_* code.
//
// `judge_was_called` (out, optional) reports whether the Rust judge was invoked —
// the harness asserts it is FALSE for the oversize row (the short-circuit proof
// that an oversize payload never crosses the FFI).
std::uint8_t shim_process(const ShimInput &in, bool *judge_was_called) noexcept;

} // namespace kirra

#endif // KIRRA_SHIM_HPP
