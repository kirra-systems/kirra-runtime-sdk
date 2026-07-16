# R2CP decoder fuzzing

Fuzz harness for the untrusted R2CP frame decoder (`protocol/src/wire.cpp`:
`decode` / `decode_authenticated`, which internally exercise COBS de-framing and
the CRC32C check). Any party on the serial link can inject bytes, so the decode
path is the primary attack surface.

## Oracle

`decode_fuzz.hpp::decode_one(data, size)` feeds one arbitrary buffer through both
decode entry points and checks the load-bearing invariants:

- a rejection (`status != ok`) never releases a partially-populated `Frame`
  (output stays a zero-initialised `Frame{}`);
- an accepted frame is bounded (`payload_length <= kMaximumPayload`) and never
  carries the `AUTH_TAG` flag on the unauthenticated path;
- no out-of-bounds access or UB (caught by ASAN/UBSAN).

## Two drivers, one oracle

- **Per-PR, deterministic** — `tests/test_main.cpp::test_decode_fuzz` runs a
  fixed-seed sweep (raw-random buffers + byte-mutated valid frames) through
  `decode_one` inside the sanitized `host-verification` ctest lane. Reproducible,
  so any discovered crash is replayable.
- **Coverage-guided** — `decode_fuzz_libfuzzer.cpp` is a libFuzzer target built
  with `-DR2_BUILD_FUZZERS=ON` (Clang only) and run for a bounded time by the
  `decoder-fuzz` CI job, seeded from `corpus/`.

## Run locally

```sh
# deterministic sweep (part of the normal test build)
cmake -S firmware/rosmaster-r2 -B build -DR2_ENABLE_SANITIZERS=ON
cmake --build build && ctest --test-dir build

# coverage-guided libFuzzer run
cmake -S firmware/rosmaster-r2 -B build-fuzz \
  -DCMAKE_CXX_COMPILER=clang++-18 -DR2_BUILD_FUZZERS=ON \
  -DR2_BUILD_TESTS=OFF -DR2_BUILD_SIMULATION=OFF
cmake --build build-fuzz
build-fuzz/r2_decode_fuzz -max_total_time=60 firmware/rosmaster-r2/fuzz/corpus
```

`corpus/seed_*.bin` are valid encoded frames (plain, authenticated, and an empty
`hello`) used to seed coverage.
