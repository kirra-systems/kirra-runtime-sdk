#pragma once

// Shared fuzz oracle for the R2CP frame decoder.
//
// `decode_one()` feeds one arbitrary input buffer through the untrusted decode
// entry points (decode / decode_authenticated) and checks the load-bearing
// safety invariants. It is driven two ways:
//   • fuzz/decode_fuzz_libfuzzer.cpp — a libFuzzer target (clang, -fsanitize=fuzzer;
//     built with -DR2_BUILD_FUZZERS=ON) for coverage-guided / deep fuzzing.
//   • tests/test_main.cpp::test_decode_fuzz — a deterministic seeded sweep that
//     runs on every PR inside the sanitized (ASAN/UBSAN) host-verification lane.
//
// Memory-safety violations (OOB read/write, UB) are caught independently by
// ASAN/UBSAN as process crashes; `decode_one` additionally checks the logical
// invariants and returns false if any is violated.

#include "r2/protocol/wire.hpp"

#include <array>
#include <cstddef>
#include <cstdint>

namespace r2::protocol::fuzz {

// True iff `frame` is a pristine, default-constructed Frame — i.e. carries no
// application data. decode()/decode_authenticated() both zero-initialise their
// output frame before doing anything, so any non-ok status MUST leave this true
// (the "never partially populate on rejection" contract).
[[nodiscard]] inline bool frame_is_default(const Frame& frame) noexcept {
    if (frame.type != MessageType::hello) {
        return false;
    }
    if (frame.flags != 0U || frame.sequence != 0U || frame.source_time_us != 0U ||
        frame.payload_length != 0U) {
        return false;
    }
    for (const std::uint8_t byte : frame.payload) {
        if (byte != 0U) {
            return false;
        }
    }
    return true;
}

// Runs one fuzz iteration over `data`/`size`. Returns true if every invariant
// held. Never allocates, never throws.
[[nodiscard]] inline bool decode_one(const std::uint8_t* data,
                                     const std::size_t size) noexcept {
    // A fixed, non-trivial per-link key. decode_authenticated must never accept
    // arbitrary/forged bytes under it, and must fail closed on every rejection.
    std::array<std::uint8_t, kMacKeySize> key{};
    for (std::size_t i = 0U; i < key.size(); ++i) {
        key[i] = static_cast<std::uint8_t>(0xA5U ^ static_cast<std::uint8_t>(i));
    }

    bool invariants_hold = true;

    // 1) Unauthenticated decode path.
    {
        Frame out{};
        const DecodeStatus status = decode(data, size, out);
        if (status != DecodeStatus::ok) {
            // No partially-populated frame is ever released on a rejection.
            invariants_hold = invariants_hold && frame_is_default(out);
        } else {
            // A released frame is bounded, and never carries the AUTH_TAG flag
            // (authenticated frames must go through decode_authenticated()).
            invariants_hold = invariants_hold &&
                              (out.payload_length <= kMaximumPayload) &&
                              ((out.flags & kFlagAuthTag) == 0U);
        }
    }

    // 2) Authenticated decode path under a provisioned key.
    {
        Frame out{};
        const DecodeStatus status = decode_authenticated(data, size, key, out);
        if (status != DecodeStatus::ok) {
            invariants_hold = invariants_hold && frame_is_default(out);
        } else {
            // On success the tag is stripped and the AUTH_TAG flag cleared.
            invariants_hold = invariants_hold &&
                              (out.payload_length <= kMaximumPayload) &&
                              ((out.flags & kFlagAuthTag) == 0U);
        }
    }

    // 3) crc32c must be total over any span (exercised under the sanitizers).
    if (size > 0U) {
        static_cast<void>(crc32c(data, size));
    }

    return invariants_hold;
}

}  // namespace r2::protocol::fuzz
