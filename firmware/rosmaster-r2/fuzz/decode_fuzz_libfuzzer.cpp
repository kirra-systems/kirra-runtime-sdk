// libFuzzer entry point for the R2CP frame decoder.
//
// Build with clang and -DR2_BUILD_FUZZERS=ON (see CMakeLists.txt); the target
// links protocol/src/wire.cpp compiled with -fsanitize=fuzzer,address,undefined.
// Run e.g. `r2_decode_fuzz -max_total_time=45 fuzz/corpus`.
//
// The shared oracle lives in fuzz/decode_fuzz.hpp and is also exercised
// deterministically on every PR by tests/test_main.cpp::test_decode_fuzz.

#include "decode_fuzz.hpp"

#include <cstddef>
#include <cstdint>
#include <cstdlib>

extern "C" int LLVMFuzzerTestOneInput(const std::uint8_t* data, std::size_t size) {
    // A logical-invariant violation is a finding; abort so libFuzzer records the
    // crashing input. Memory-safety violations are trapped by the sanitizers.
    if (!r2::protocol::fuzz::decode_one(data, size)) {
        std::abort();
    }
    return 0;
}
