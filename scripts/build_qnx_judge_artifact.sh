#!/usr/bin/env bash
# build_qnx_judge_artifact.sh — WS-5.1: the QNX governor-judge artifact build.
#
# Cross-compiles the EPIC-#270 governor JUDGE (`tools/qnx-rtm-harness/
# kirra_judge.rs` — the #![no_std], zero-alloc, core-only contract checker)
# for the QNX 8.0 targets, producing one staticlib per target plus a
# provenance manifest. This is the recipe→pipeline promotion of the
# `-Zbuild-std=core` lane from KIRRA_QNX_RUNBOOK.md / run_qnx_fdit.sh:
# because the judge is core-only and a STATICLIB needs no final link, the
# cross-build requires NO proprietary QNX SDP — upstream nightly + rust-src
# is enough, so it runs in plain CI.
#
# What this artifact IS: the portable verdict core (judge) the C++ shim
# links against on the QNX safety partition (ADR-0006 Clause 3 boundary).
# What it is NOT: a WCET/timing claim. Host-built artifacts carry no timing
# evidence — only QNX-target-under-SCHED_FIFO runs do (AOU-HW-QNX-TARGET-001,
# docs/safety/WCET_MEASUREMENT_METHODOLOGY.md). PROVENANCE.txt states this.
#
# Toolchain note: the ACTIVE toolchain is used. On stable, -Zbuild-std is
# unlocked with RUSTC_BOOTSTRAP=1 — the same documented fallback as
# run_qnx_fdit.sh, and deliberately so here: current stable (1.94) still
# ships the `*-nto-qnx800` tuples, while nightly 1.98 has RENAMED them to
# `aarch64-unknown-qnx` / `x86_64-pc-qnx`. The script prefers the qnx800
# spelling and falls back to the renamed tuple LOUDLY when stable picks up
# the rename, so the pipeline degrades with a visible message, never a
# silent wrong artifact.
#
# Usage:
#   scripts/build_qnx_judge_artifact.sh [out-dir]     # default: dist-qnx
# Env:
#   KIRRA_QNX_TARGETS   space-separated tuples
#                       (default: "x86_64-pc-nto-qnx800 aarch64-unknown-nto-qnx800")
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
JUDGE_SRC="$REPO_ROOT/tools/qnx-rtm-harness/kirra_judge.rs"
FFI_HEADER="$REPO_ROOT/tools/qnx-rtm-harness/kirra_ffi.h"
OUT="${1:-dist-qnx}"
TARGETS="${KIRRA_QNX_TARGETS:-x86_64-pc-nto-qnx800 aarch64-unknown-nto-qnx800}"

mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"

# rust-src is required for -Zbuild-std (core is compiled from source for the
# tier-3 nto tuples — rustup ships no prebuilt core/std for them).
SYSROOT="$(rustc --print sysroot)"
if [[ ! -d "$SYSROOT/lib/rustlib/src/rust" ]]; then
    echo "ERROR: rust-src missing — run: rustup component add rust-src" >&2
    exit 2
fi

# -Z flags need a nightly compiler OR RUSTC_BOOTSTRAP=1 on stable (the
# run_qnx_fdit.sh fallback). Detect which we have.
BOOT=()
if ! rustc --version | grep -q nightly; then
    BOOT=(env RUSTC_BOOTSTRAP=1)
fi

# Resolve a requested tuple against this toolchain, following the upstream
# rename (nto-qnx800 → version-less qnx tuples on 2026 nightlies).
resolve_target() {
    local want="$1" renamed=""
    rustc --print target-list | grep -qx "$want" && { echo "$want"; return 0; }
    case "$want" in
        x86_64-pc-nto-qnx800)       renamed="x86_64-pc-qnx" ;;
        aarch64-unknown-nto-qnx800) renamed="aarch64-unknown-qnx" ;;
    esac
    if [[ -n "$renamed" ]] && rustc --print target-list | grep -qx "$renamed"; then
        echo "NOTE: tuple '$want' is gone from this toolchain — using its upstream rename '$renamed'" >&2
        echo "$renamed"
        return 0
    fi
    return 1
}

# Throwaway cargo wrapper (the judge is a single dependency-free .rs built by
# rustc in the harness; -Zbuild-std is cargo-level, so wrap it). The leading
# empty [workspace] detaches it from the repo workspace. Same technique as
# run_qnx_fdit.sh::build_judge_buildstd — one recipe, two entry points.
CRATE="$(mktemp -d)"
trap 'rm -rf "$CRATE"' EXIT
mkdir -p "$CRATE/src"
cp "$JUDGE_SRC" "$CRATE/src/lib.rs"
cat > "$CRATE/Cargo.toml" <<'EOF'
[workspace]

[package]
name = "kirra_judge"
version = "0.0.0"
edition = "2021"

[lib]
crate-type = ["staticlib"]

[profile.release]
panic = "abort"
opt-level = 2
debug = false
EOF

BUILT_TARGETS=""
for want in $TARGETS; do
    tgt="$(resolve_target "$want")" || { echo "ERROR: no usable tuple for '$want' on $(rustc --version)" >&2; exit 2; }
    echo "== building judge staticlib for $tgt (-Zbuild-std=core, $(rustc --version | cut -d' ' -f2))"
    ( cd "$CRATE" && "${BOOT[@]}" cargo build --release -Z build-std=core --target "$tgt" )
    lib="$CRATE/target/$tgt/release/libkirra_judge.a"
    [[ -f "$lib" ]] || { echo "ERROR: expected $lib missing" >&2; exit 1; }

    # Acceptance check: the archive's objects must be ELF for the TARGET
    # machine, not the host (a silent host-arch fallback would ship a wrong
    # artifact that only fails at partition link time).
    case "$tgt" in
        x86_64-*)  mach="Advanced Micro Devices X86-64" ;;
        aarch64-*) mach="AArch64" ;;
        *)         mach="" ;;
    esac
    if [[ -n "$mach" ]]; then
        got="$(readelf -h "$lib" 2>/dev/null | grep -m1 'Machine:' || true)"
        echo "$got" | grep -q "$mach" \
            || { echo "ERROR: $tgt archive machine check failed: '$got' (want '$mach')" >&2; exit 1; }
        echo "   machine ok: ${got#*Machine:}"
    fi
    # The kirra_ffi.h ABI entry point must be exported, or the shim has
    # nothing to link against. (nm exits non-zero on the archive's rmeta
    # member while still printing real symbols — capture first so pipefail
    # judges the grep, not nm's complaint.)
    syms="$(nm "$lib" 2>/dev/null || true)"
    grep -q " T kirra_judge_assess" <<<"$syms" \
        || { echo "ERROR: $tgt archive does not export kirra_judge_assess (ABI break?)" >&2; exit 1; }
    echo "   abi ok: kirra_judge_assess exported"
    cp "$lib" "$OUT/libkirra_judge-$tgt.a"
    BUILT_TARGETS="${BUILT_TARGETS:+$BUILT_TARGETS }$tgt"
done

cp "$FFI_HEADER" "$OUT/kirra_ffi.h"

# Provenance: what built this, from what, and what it does NOT claim.
cat > "$OUT/PROVENANCE.txt" <<EOF
kirra QNX governor-judge artifact (WS-5.1)
==========================================
source      : tools/qnx-rtm-harness/kirra_judge.rs (+ kirra_ffi.h, the ABI)
requested   : $TARGETS
commit      : $(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo unknown)
targets     : $BUILT_TARGETS
toolchain   : $(rustc --version)
build       : cargo -Zbuild-std=core --release (staticlib; panic=abort, opt-level=2)
dependencies: NONE beyond rust core/compiler_builtins (the judge is
              #![no_std], zero-alloc, dependency-free by design — its SBOM
              is this toolchain line).

Integration : link against the C++ shim per tools/qnx-rtm-harness/README.md
              (ADR-0006 Clause 3 — the shim is the memory/transport DRIVER,
              this judge is the contract CHECKER).
NO TIMING CLAIM: this is a host-built functional artifact. WCET/FTTI
              evidence comes ONLY from QNX-target-under-SCHED_FIFO runs
              (AOU-HW-QNX-TARGET-001; docs/safety/WCET_MEASUREMENT_METHODOLOGY.md).
EOF

echo "== artifact contents ($OUT):"
ls -l "$OUT"
