#!/bin/bash
# MC/DC code coverage measurement for Kirra Runtime SDK
# ISO 26262 ASIL-D prerequisite — CERT-001
set -e

echo "Running MC/DC coverage measurement..."

# Clean previous coverage data
find . -name "*.profraw" -delete
find . -name "*.profdata" -delete

# Build and run tests with coverage instrumentation
RUSTFLAGS="-C instrument-coverage" \
LLVM_PROFILE_FILE="kirra-%p-%m.profraw" \
  cargo test --workspace 2>&1

# Merge profile data
llvm-profdata merge -sparse *.profraw -o coverage.profdata

# Generate MC/DC report
llvm-cov report \
  --use-color \
  --ignore-filename-regex='/.cargo/registry' \
  --ignore-filename-regex='/rustup/toolchains' \
  --instr-profile=coverage.profdata \
  $(cargo test --workspace --no-run --message-format=json 2>/dev/null \
    | jq -r 'select(.profile.test == true) | .filenames[]' \
    | grep -v dSYM \
    | sed 's/^/--object /') \
  2>/dev/null

# Generate HTML report
llvm-cov show \
  --use-color \
  --ignore-filename-regex='/.cargo/registry' \
  --ignore-filename-regex='/rustup/toolchains' \
  --instr-profile=coverage.profdata \
  --format=html \
  --output-dir=coverage-report \
  $(cargo test --workspace --no-run --message-format=json 2>/dev/null \
    | jq -r 'select(.profile.test == true) | .filenames[]' \
    | grep -v dSYM \
    | sed 's/^/--object /') \
  2>/dev/null

echo "Coverage report generated at coverage-report/index.html"
