#!/usr/bin/env bash
# Build and run the Kirra C quickstart against the cdylib (libkirra_verifier).
#
# Kirra's root crate builds a `cdylib` (Cargo.toml: crate-type = ["rlib","cdylib"]),
# so a C program links it directly and calls the C ABI declared in include/kirra.h.
#
#   ./examples/c/build_and_run.sh              # release cdylib
#   PROFILE=debug ./examples/c/build_and_run.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PROFILE="${PROFILE:-release}"
CC="${CC:-cc}"

case "$PROFILE" in
  release) CARGO_FLAGS="--release" ;;
  debug)   CARGO_FLAGS="" ;;
  *) echo "PROFILE must be 'release' or 'debug' (got '$PROFILE')" >&2; exit 2 ;;
esac

# 1. Build the shared library the C program links against.
( cd "$REPO_ROOT" && cargo build --lib --locked $CARGO_FLAGS )

LIB_DIR="$REPO_ROOT/target/$PROFILE"
OUT="$(mktemp -d)/kirra_ffi_demo"

# 2. Compile the C demo against include/kirra.h + the cdylib.
"$CC" -std=c11 -Wall -Wextra \
  -I "$REPO_ROOT/include" \
  "$REPO_ROOT/examples/c/kirra_ffi_demo.c" \
  -L "$LIB_DIR" -lkirra_verifier -lm \
  -o "$OUT"

# 3. Run it, pointing the loader at the freshly built cdylib.
echo "--- running $OUT (LD_LIBRARY_PATH=$LIB_DIR) ---"
LD_LIBRARY_PATH="$LIB_DIR${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" "$OUT"
