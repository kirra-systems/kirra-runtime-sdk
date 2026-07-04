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
# REPRODUCIBILITY (#790 F2): the codegen flags (codegen-units=1 + fat LTO, from
# the single-source judge_build_flags.sh) make the staticlib bytes a deterministic
# function of (source, toolchain, target). This is ENFORCED, not asserted: the CI
# double-build `cmp` gate rebuilds and byte-compares the staticlibs. PROVENANCE.txt
# (the only intentionally build-varying file) is excluded from that comparison.
#
# TUPLE SEMANTICS (#790 F4): current stable (1.94) ships the `*-nto-qnx800`
# tuples; a later toolchain RENAMED them (`aarch64-unknown-qnx` / `x86_64-pc-qnx`).
# The script prefers the qnx800 spelling and, when it substitutes a renamed tuple,
# does NOT trust the name — it asserts the RESOLVED target's `target-spec-json`
# really is QNX 8.0 (`os == nto`, `env == nto80`). A tuple that is not genuinely
# QNX 8.0 is a HARD STOP (fail-closed), never a silently-mislabelled qnx800 tarball.
#
# Usage:
#   scripts/build_qnx_judge_artifact.sh [out-dir]     # default: dist-qnx
# Env:
#   KIRRA_QNX_TARGETS         space-separated tuples
#                             (default: "x86_64-pc-nto-qnx800 aarch64-unknown-nto-qnx800")
#   KIRRA_QNX_ALLOW_NON_QNX8  set to 1 to DOWNGRADE the QNX-8.0 semantic assertion
#                             from a hard stop to a warning (experimentation only;
#                             NEVER in the release workflow — it would ship a
#                             mislabelled judge core).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HARNESS_DIR="$REPO_ROOT/tools/qnx-rtm-harness"
JUDGE_SRC="$HARNESS_DIR/kirra_judge.rs"
FFI_HEADER="$HARNESS_DIR/kirra_ffi.h"
OUT="${1:-dist-qnx}"
TARGETS="${KIRRA_QNX_TARGETS:-x86_64-pc-nto-qnx800 aarch64-unknown-nto-qnx800}"

# #790 F6 — the codegen flags come from the ONE canonical fragment, shared with
# run_qnx_fdit.sh (and mirrored by CMakeLists.txt). No second copy to drift.
# shellcheck source=tools/qnx-rtm-harness/judge_build_flags.sh
source "$HARNESS_DIR/judge_build_flags.sh"

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
# run_qnx_fdit.sh fallback). Detect which we have and RECORD it (#790 F3 — the
# compiler mode is provenance, not an incidental).
BOOT=()
BOOTSTRAP_MODE="nightly (native -Z)"
if ! rustc --version | grep -q nightly; then
    BOOT=(env RUSTC_BOOTSTRAP=1)
    BOOTSTRAP_MODE="stable + RUSTC_BOOTSTRAP=1"
fi

# Resolve a requested tuple against this toolchain, following the upstream
# rename (nto-qnx800 → version-less qnx tuples on 2026 nightlies). Emits a
# CI-visible ::warning:: on substitution; the QNX-8.0 semantic assertion below
# is what actually guards correctness.
resolve_target() {
    local want="$1" renamed=""
    rustc --print target-list | grep -qx "$want" && { echo "$want"; return 0; }
    case "$want" in
        x86_64-pc-nto-qnx800)       renamed="x86_64-pc-qnx" ;;
        aarch64-unknown-nto-qnx800) renamed="aarch64-unknown-qnx" ;;
    esac
    if [[ -n "$renamed" ]] && rustc --print target-list | grep -qx "$renamed"; then
        echo "::warning::qnx tuple '$want' is gone from this toolchain — substituting its upstream rename '$renamed'; verifying it is still QNX 8.0" >&2
        echo "$renamed"
        return 0
    fi
    return 1
}

# #790 F4 — semantic equivalence of a resolved tuple: assert it REALLY is QNX 8.0
# via target-spec-json (`os == nto`, `env == nto80`), independent of the tuple's
# NAME. `env` carries the version (nto80 = 8.0, nto71 = 7.1), so this rejects both
# a rename to a non-QNX target and a wrong-version fallback. Hard stop unless
# KIRRA_QNX_ALLOW_NON_QNX8=1 (experimentation only).
spec_field() {
    "${BOOT[@]}" rustc -Z unstable-options --print target-spec-json --target "$1" 2>/dev/null \
        | sed -n "s/^[[:space:]]*\"$2\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" | head -1
}
assert_is_qnx8() {
    local tgt="$1" os env
    os="$(spec_field "$tgt" os)"
    env="$(spec_field "$tgt" env)"
    if [[ "$os" == "nto" && "$env" == "nto80" ]]; then
        echo "   qnx-8.0 semantics ok: os=$os env=$env"
        return 0
    fi
    local msg="resolved tuple '$tgt' is NOT QNX 8.0 (os='$os' env='$env'; want os=nto env=nto80)"
    if [[ "${KIRRA_QNX_ALLOW_NON_QNX8:-0}" == "1" ]]; then
        echo "::warning::$msg — proceeding because KIRRA_QNX_ALLOW_NON_QNX8=1 (NOT for release)" >&2
        return 0
    fi
    echo "::error::$msg — refusing to ship a mislabelled qnx800 judge core (#790 F4)" >&2
    return 1
}

# Throwaway cargo wrapper (the judge is a single dependency-free .rs built by
# rustc in the harness; -Zbuild-std is cargo-level, so wrap it). The leading
# empty [workspace] detaches it from the repo workspace. The [profile.release]
# codegen keys come from judge_build_flags.sh (#790 F6) — codegen-units + LTO are
# the #790 F2 determinism flags.
CRATE="$(mktemp -d)"
trap 'rm -rf "$CRATE"' EXIT
mkdir -p "$CRATE/src"
cp "$JUDGE_SRC" "$CRATE/src/lib.rs"
cat > "$CRATE/Cargo.toml" <<EOF
[workspace]

[package]
name = "${KIRRA_JUDGE_CRATE_NAME}"
version = "0.0.0"
edition = "${KIRRA_JUDGE_EDITION}"

[lib]
crate-type = ["staticlib"]

[profile.release]
panic = "${KIRRA_JUDGE_PANIC}"
opt-level = ${KIRRA_JUDGE_OPT_LEVEL}
debug = false
codegen-units = ${KIRRA_JUDGE_CODEGEN_UNITS}
lto = "${KIRRA_JUDGE_LTO}"
EOF

BUILT_TARGETS=""
SPEC_LINES=""
for want in $TARGETS; do
    tgt="$(resolve_target "$want")" || { echo "::error::no usable tuple for '$want' on $(rustc --version)" >&2; exit 2; }
    assert_is_qnx8 "$tgt" || exit 2   # #790 F4 hard stop
    echo "== building judge staticlib for $tgt (-Zbuild-std=core, $(rustc --version | cut -d' ' -f2))"
    # NOTE: NOT --offline. Although the throwaway crate is dependency-free, cargo's
    # -Zbuild-std resolves the sysroot workspace (core/std/compiler_builtins), whose
    # own manifests pull registry crates (e.g. `cfg-if`); a cold CI cache has none of
    # them, so --offline fails resolution (#790 F9 follow-up). Reproducibility is
    # unaffected — those versions are pinned by the toolchain's sysroot lockfile, so
    # the fetched set is a deterministic function of the pinned toolchain.
    ( cd "$CRATE" && "${BOOT[@]}" cargo build --release -Z build-std=core --target "$tgt" )
    lib="$CRATE/target/$tgt/release/libkirra_judge.a"
    [[ -f "$lib" ]] || { echo "ERROR: expected $lib missing" >&2; exit 1; }

    # Acceptance check (#790 F8): EVERY archive member must be ELF for the TARGET
    # machine, not the host — a silent host-arch fallback in even ONE member would
    # ship a wrong artifact that only fails at partition link time. (The prior
    # `grep -m1` checked only the first member.)
    case "$tgt" in
        x86_64-*)  mach="Advanced Micro Devices X86-64" ;;
        aarch64-*) mach="AArch64" ;;
        *)         mach="" ;;
    esac
    if [[ -n "$mach" ]]; then
        machines="$(readelf -h "$lib" 2>/dev/null | grep 'Machine:' || true)"
        total="$(grep -c 'Machine:' <<<"$machines" || true)"
        matched="$(grep -c "$mach" <<<"$machines" || true)"
        if [[ "${total:-0}" -lt 1 || "$total" != "$matched" ]]; then
            echo "ERROR: $tgt machine check failed — $matched/$total members are '$mach'" >&2
            readelf -h "$lib" 2>/dev/null | grep 'Machine:' >&2 || true
            exit 1
        fi
        echo "   machine ok: all $total members are $mach"
    fi
    # The kirra_ffi.h ABI entry point must be exported, or the shim has nothing to
    # link against. Anchor the symbol name (#790 F8: ' T kirra_judge_assess$' — an
    # unanchored grep would also match e.g. kirra_judge_assess_v2). nm exits
    # non-zero on the archive's rmeta member while still printing real symbols, so
    # capture first and let pipefail judge the grep, not nm's complaint.
    syms="$(nm "$lib" 2>/dev/null || true)"
    grep -qE ' T kirra_judge_assess$' <<<"$syms" \
        || { echo "ERROR: $tgt archive does not export kirra_judge_assess (ABI break?)" >&2; exit 1; }
    echo "   abi ok: kirra_judge_assess exported"
    cp "$lib" "$OUT/libkirra_judge-$tgt.a"
    BUILT_TARGETS="${BUILT_TARGETS:+$BUILT_TARGETS }$tgt"
    # #790 F3 — record the resolved spec per target for provenance.
    SPEC_LINES="${SPEC_LINES}target-spec[$tgt] : os=$(spec_field "$tgt" os) env=$(spec_field "$tgt" env) arch=$(spec_field "$tgt" arch) llvm-target=$(spec_field "$tgt" llvm-target)
"
done

cp "$FFI_HEADER" "$OUT/kirra_ffi.h"
# #790 F7 — ship the licence with the artifact (the repo has COPYRIGHT, no LICENSE).
# NO `|| true`: a missing licence is a packaging error, not something to swallow.
cp "$REPO_ROOT/COPYRIGHT" "$OUT/COPYRIGHT"

# #790 F3 — SLSA-style provenance generated from VARIABLES (not hand-maintained
# prose): compiler mode, exact toolchain, build-std fact, resolved target specs,
# CI run id, and a per-staticlib sha256 (so the reproducibility cmp has a record).
STATICLIB_HASHES="$(cd "$OUT" && sha256sum libkirra_judge-*.a 2>/dev/null || echo 'unknown')"
cat > "$OUT/PROVENANCE.txt" <<EOF
kirra QNX governor-judge artifact (WS-5.1)
==========================================
source        : tools/qnx-rtm-harness/kirra_judge.rs (+ kirra_ffi.h, the ABI)
requested     : $TARGETS
targets       : $BUILT_TARGETS
commit        : $(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo unknown)
compiler-mode : $BOOTSTRAP_MODE
toolchain     : $(rustc -vV 2>/dev/null | tr '\n' ';' | sed 's/;$//')
build-std     : yes (-Z build-std=core; tier-3 nto tuples ship no prebuilt core)
codegen       : cargo --release staticlib; panic=${KIRRA_JUDGE_PANIC},
                opt-level=${KIRRA_JUDGE_OPT_LEVEL}, debuginfo=${KIRRA_JUDGE_DEBUGINFO},
                codegen-units=${KIRRA_JUDGE_CODEGEN_UNITS}, lto=${KIRRA_JUDGE_LTO} (#790 F2 determinism)
${SPEC_LINES}ci-run        : ${GITHUB_RUN_ID:-local}/${GITHUB_RUN_ATTEMPT:-0} on ${GITHUB_WORKFLOW:-local-shell}
dependencies  : NONE beyond rust core/compiler_builtins (the judge is #![no_std],
                zero-alloc, dependency-free by design — its SBOM is the toolchain line).
sha256        :
$(printf '%s\n' "$STATICLIB_HASHES" | sed 's/^/                /')

Integration   : link against the C++ shim per tools/qnx-rtm-harness/README.md
                (ADR-0006 Clause 3 — the shim is the memory/transport DRIVER,
                this judge is the contract CHECKER).
NO TIMING CLAIM: this is a host-built functional artifact. WCET/FTTI evidence
                comes ONLY from QNX-target-under-SCHED_FIFO runs
                (AOU-HW-QNX-TARGET-001; docs/safety/WCET_MEASUREMENT_METHODOLOGY.md).
EOF

echo "== artifact contents ($OUT):"
ls -l "$OUT"
