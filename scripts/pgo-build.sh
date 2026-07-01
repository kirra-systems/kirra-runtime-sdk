#!/usr/bin/env bash
# scripts/pgo-build.sh — Profile-Guided Optimization for the control-plane binary.
#
# OPT-IN, per-deployment tuning of the UNTRUSTED control plane. It does NOT touch
# the safety element: the QNX judge / checker WCET evidence is built separately by
# tools/qnx-rtm-harness/run_qnx_fdit.sh with fixed, PGO-free flags. Never PGO the
# cert build. See docs/PERFORMANCE_BUILD_TUNING.md.
#
# Three phases: instrument -> run a representative workload -> rebuild with the
# collected profile.
#
# Usage:
#   scripts/pgo-build.sh <workload-driver>
#
#   <workload-driver> is a script YOU provide that drives the RUNNING instrumented
#   service with representative traffic and then returns. This script starts the
#   instrumented binary, runs the driver against it (KIRRA_VERIFIER_ADDR is
#   exported), stops it, then rebuilds. PGO optimizes for what the workload
#   exercises, so make it representative (a realistic route mix, not a microbench).
#
# Env:
#   BIN               binary name (default: kirra_verifier_service)
#   EXTRA_RUSTFLAGS   stacked onto both phases, e.g. "-C target-cpu=x86-64-v3"
#   KIRRA_VERIFIER_ADDR   listen addr for the instrumented run (default 127.0.0.1:8099)
set -euo pipefail

BIN="${BIN:-kirra_verifier_service}"
WORKLOAD="${1:?usage: scripts/pgo-build.sh <workload-driver-script>}"
EXTRA_RUSTFLAGS="${EXTRA_RUSTFLAGS:-}"
ADDR="${KIRRA_VERIFIER_ADDR:-127.0.0.1:8099}"
PGO_DIR="$(pwd)/target/pgo-data"

# Use the rustc-BUNDLED llvm-profdata so its LLVM version matches rustc's (a
# system llvm-profdata will refuse a version-mismatched raw profile).
SYSROOT="$(rustc --print sysroot)"
PROFDATA="$(find "$SYSROOT" -name 'llvm-profdata' -type f 2>/dev/null | head -1 || true)"
if [[ -z "$PROFDATA" ]]; then
    echo "ERROR: rustc-bundled llvm-profdata not found."
    echo "       Install it:  rustup component add llvm-tools-preview"
    exit 1
fi

echo "== PGO phase 1/3: build instrumented $BIN"
rm -rf "$PGO_DIR"
RUSTFLAGS="-C profile-generate=${PGO_DIR} ${EXTRA_RUSTFLAGS}" \
    cargo build --profile dist --bin "$BIN"

echo "== PGO phase 2/3: run representative workload against the instrumented binary"
KIRRA_VERIFIER_ADDR="$ADDR" ./target/dist/"$BIN" &
SVC_PID=$!
# give it a moment to bind, then drive it; always tear the service down.
sleep 2
trap 'kill "$SVC_PID" 2>/dev/null || true' EXIT
KIRRA_VERIFIER_ADDR="$ADDR" "$WORKLOAD"
kill "$SVC_PID" 2>/dev/null || true
wait "$SVC_PID" 2>/dev/null || true
trap - EXIT

echo "== PGO phase 3/3: merge profile + rebuild with -C profile-use"
"$PROFDATA" merge -o "${PGO_DIR}/merged.profdata" "${PGO_DIR}"
RUSTFLAGS="-C profile-use=${PGO_DIR}/merged.profdata -C llvm-args=-pgo-warn-missing-function ${EXTRA_RUSTFLAGS}" \
    cargo build --profile dist --bin "$BIN"

echo
echo "PGO build complete: target/dist/${BIN}"
echo "  profile data: ${PGO_DIR}/merged.profdata"
[[ -n "$EXTRA_RUSTFLAGS" ]] && echo "  stacked flags: ${EXTRA_RUSTFLAGS}"
