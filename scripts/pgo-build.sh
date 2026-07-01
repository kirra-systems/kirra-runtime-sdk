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
#   KIRRA_ADMIN_TOKEN     REQUIRED — the service fails closed at startup without a
#                         non-empty admin token (SG-008), so the instrumented run
#                         below cannot start without it.
set -euo pipefail

# Resolve paths from the script location, not the caller's cwd, so this works from
# anywhere. The workload arg is resolved to absolute BEFORE we cd into the repo.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

BIN="${BIN:-kirra_verifier_service}"
WORKLOAD_ARG="${1:?usage: scripts/pgo-build.sh <workload-driver-script>}"
# `|| true` so a non-existent workload dir doesn't trip `set -e` inside the
# command substitution (which would exit silently, with cd's stderr suppressed,
# before the explicit check below). A failed cd leaves WORKLOAD_DIR empty.
WORKLOAD_DIR="$(cd "$(dirname "$WORKLOAD_ARG")" 2>/dev/null && pwd || true)"
WORKLOAD="${WORKLOAD_DIR}/$(basename "$WORKLOAD_ARG")"
[[ -n "$WORKLOAD_DIR" && -x "$WORKLOAD" ]] || { echo "ERROR: workload driver not found or not executable: $WORKLOAD_ARG" >&2; exit 1; }
EXTRA_RUSTFLAGS="${EXTRA_RUSTFLAGS:-}"
ADDR="${KIRRA_VERIFIER_ADDR:-127.0.0.1:8099}"
PGO_DIR="${REPO_ROOT}/target/pgo-data"

# The service fails closed at startup without a non-empty admin token (SG-008);
# the phase-2 instrumented run must actually boot, so require it up front.
: "${KIRRA_ADMIN_TOKEN:?ERROR: set KIRRA_ADMIN_TOKEN (non-empty) — the service fails closed at startup without it (SG-008)}"

# Run everything from the repo root so target/dist paths resolve regardless of cwd.
cd "$REPO_ROOT"

# Use the rustc-BUNDLED llvm-profdata so its LLVM version matches rustc's (a
# system llvm-profdata will refuse a version-mismatched raw profile). It lives
# under the sysroot's per-host rustlib bin, NOT the toolchain's top-level bin, so
# `rustup which llvm-profdata` cannot resolve it — address it by exact path first,
# then fall back to a search for an EXECUTABLE match.
SYSROOT="$(rustc --print sysroot)"
HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
PROFDATA="${SYSROOT}/lib/rustlib/${HOST_TRIPLE}/bin/llvm-profdata"
if [[ ! -x "$PROFDATA" ]]; then
    PROFDATA="$(find "$SYSROOT" -name 'llvm-profdata' -type f -perm -u+x 2>/dev/null | head -1 || true)"
fi
if [[ -z "$PROFDATA" || ! -x "$PROFDATA" ]]; then
    echo "ERROR: rustc-bundled llvm-profdata not found or not executable." >&2
    echo "       Install it:  rustup component add llvm-tools-preview" >&2
    exit 1
fi

echo "== PGO phase 1/3: build instrumented $BIN"
rm -rf "$PGO_DIR"
RUSTFLAGS="-C profile-generate=${PGO_DIR} ${EXTRA_RUSTFLAGS}" \
    cargo build --profile dist --bin "$BIN"

echo "== PGO phase 2/3: run representative workload against the instrumented binary"
KIRRA_VERIFIER_ADDR="$ADDR" "${REPO_ROOT}/target/dist/${BIN}" &
SVC_PID=$!
# Arm the teardown BEFORE anything that can fail/interrupt (the sleep, the
# workload), so the background service is never left running on an abort.
trap 'kill "$SVC_PID" 2>/dev/null || true' EXIT
# give it a moment to bind, then drive it.
sleep 2
KIRRA_VERIFIER_ADDR="$ADDR" "$WORKLOAD"
kill "$SVC_PID" 2>/dev/null || true
wait "$SVC_PID" 2>/dev/null || true
trap - EXIT

echo "== PGO phase 3/3: merge profile + rebuild with -C profile-use"
# `llvm-profdata merge` expects .profraw files (not a directory). Expand them and
# fail clearly if the instrumented run produced none (e.g. the workload didn't
# actually exercise the binary, or it never started).
shopt -s nullglob
PROFRAWS=("${PGO_DIR}"/*.profraw)
shopt -u nullglob
if (( ${#PROFRAWS[@]} == 0 )); then
    echo "ERROR: no .profraw files in ${PGO_DIR} — did the instrumented run exercise ${BIN}?" >&2
    exit 1
fi
"$PROFDATA" merge -o "${PGO_DIR}/merged.profdata" "${PROFRAWS[@]}"
RUSTFLAGS="-C profile-use=${PGO_DIR}/merged.profdata -C llvm-args=-pgo-warn-missing-function ${EXTRA_RUSTFLAGS}" \
    cargo build --profile dist --bin "$BIN"

echo
echo "PGO build complete: target/dist/${BIN}"
echo "  profile data: ${PGO_DIR}/merged.profdata"
[[ -n "$EXTRA_RUSTFLAGS" ]] && echo "  stacked flags: ${EXTRA_RUSTFLAGS}"
