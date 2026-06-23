#!/usr/bin/env bash
# Build and run the Kirra native C++ FFI integration test.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KIRRA_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

echo "Building Kirra shared library..."
cargo build --release --manifest-path "${KIRRA_DIR}/Cargo.toml"

LIB_PATH="${KIRRA_DIR}/target/release"
INCLUDE_PATH="${KIRRA_DIR}/include"
TEST_SRC="${KIRRA_DIR}/tests/native_test.cpp"
TEST_BIN="${KIRRA_DIR}/target/native_test"

echo "Compiling native test..."
g++ -std=c++17 -o "${TEST_BIN}" "${TEST_SRC}" \
    -I "${INCLUDE_PATH}" \
    -L "${LIB_PATH}" \
    -lkirra_verifier \
    -Wl,-rpath,"${LIB_PATH}"

echo "Running native test..."
"${TEST_BIN}"
echo "Native test passed."
