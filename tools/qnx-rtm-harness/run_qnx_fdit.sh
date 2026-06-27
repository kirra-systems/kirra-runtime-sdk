#!/usr/bin/env bash
# run_qnx_fdit.sh — Phase-I QNX on-target FDIT + WCET bring-up (#274, EPIC #270).
#
# Cross-builds, on an Ubuntu host with QNX SDP 8.0, the no_std verdict JUDGE
# (libkirra_judge.a) + the C++ shim/harness/demo + wcet_measure for an x86_64 QNX
# target, then tells you how to run them ON the QNX target. The host build path is
# untouched (this script is the QNX path only).
#
# It owns the part that varies most — getting rustc to emit `core` for the nto
# target — and tries, in order:
#   1) direct `rustc --target <nto>` (works if your rustc ships a prebuilt core,
#      e.g. a QNX-vendor / Ferrocene toolchain);
#   2) `cargo -Z build-std=core --target <nto>` (upstream nightly + rust-src);
#   3) if neither knows the tuple, prints the custom target.json next step.
# The C++ side is always qcc/q++ via qnx.toolchain.cmake.
#
# Prereqs: `source ~/qnx800/qnxsdp-env.sh` first (sets QNX_HOST/QNX_TARGET + qcc).
#
# Env overrides:
#   KIRRA_QNX_ARCH        x86_64 (default) | aarch64
#   KIRRA_RUSTC_TARGET    override the rustc tuple (default per arch)
#   KIRRA_QNX_QCC_VARIANT override the qcc -V variant (default per arch)
#   KIRRA_RUST_TARGET_JSON path to a custom target spec (forces the build-std path)
set -euo pipefail

: "${QNX_HOST:?source qnxsdp-env.sh first (QNX_HOST unset)}"
: "${QNX_TARGET:?source qnxsdp-env.sh first (QNX_TARGET unset)}"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD="$HERE/build-qnx"
JUDGE_SRC="$HERE/kirra_judge.rs"
JUDGE_LIB="$BUILD/libkirra_judge.a"

ARCH="${KIRRA_QNX_ARCH:-x86_64}"
case "$ARCH" in
    x86_64)
        RUST_TARGET="${KIRRA_RUSTC_TARGET:-x86_64-pc-nto-qnx800}"
        QCC_VARIANT="${KIRRA_QNX_QCC_VARIANT:-gcc_ntox86_64}" ;;
    aarch64*)
        RUST_TARGET="${KIRRA_RUSTC_TARGET:-aarch64-unknown-nto-qnx800}"
        QCC_VARIANT="${KIRRA_QNX_QCC_VARIANT:-gcc_ntoaarch64le}" ;;
    *) echo "unknown KIRRA_QNX_ARCH=$ARCH (use x86_64 | aarch64)" >&2; exit 2 ;;
esac

# A custom target.json forces tuple = the file and the build-std path.
if [[ -n "${KIRRA_RUST_TARGET_JSON:-}" ]]; then
    RUST_TARGET="$KIRRA_RUST_TARGET_JSON"
fi

mkdir -p "$BUILD"
echo "=============================================================="
echo " QNX FDIT/WCET bring-up (#274)"
echo "   arch         : $ARCH"
echo "   rustc target : $RUST_TARGET"
echo "   qcc variant  : $QCC_VARIANT"
echo "   QNX_TARGET   : $QNX_TARGET"
echo "=============================================================="

# ---- 1. Build the no_std judge staticlib for the QNX target ------------------
build_judge_direct() {
    rustc --print target-list 2>/dev/null | grep -qx "$RUST_TARGET" || return 1
    echo "[judge] rustc knows '$RUST_TARGET' — trying a direct cross-build (prebuilt core)…"
    rustc --edition 2021 --target "$RUST_TARGET" \
          --crate-type staticlib --crate-name kirra_judge \
          -C panic=abort -C opt-level=2 -C debuginfo=0 \
          -o "$JUDGE_LIB" "$JUDGE_SRC" 2>"$BUILD/rustc_direct.log"
}

build_judge_buildstd() {
    command -v cargo >/dev/null || { echo "[judge] cargo not found for -Zbuild-std fallback" >&2; return 1; }
    echo "[judge] falling back to cargo -Z build-std=core (nightly + rust-src)…"
    local C="$BUILD/judge-crate"
    mkdir -p "$C/src"
    cp "$JUDGE_SRC" "$C/src/lib.rs"
    # The leading empty [workspace] table DETACHES this throwaway crate from the
    # repo's parent Cargo workspace (else cargo refuses to build it).
    cat > "$C/Cargo.toml" <<EOF
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
    # build-std needs the rust-src component for the active toolchain.
    if ! rustc --print sysroot >/dev/null 2>&1 \
         || [[ ! -d "$(rustc --print sysroot)/lib/rustlib/src/rust" ]]; then
        echo "[judge] NOTE: rust-src not found — run 'rustup component add rust-src' \
(and on stable, this uses RUSTC_BOOTSTRAP=1)." >&2
    fi
    local tgt="$RUST_TARGET"
    local log="$BUILD/cargo_buildstd.log"
    # Prefer an installed nightly; else RUSTC_BOOTSTRAP=1 lets stable accept -Z.
    if rustc +nightly --version >/dev/null 2>&1; then
        ( cd "$C" && cargo +nightly build --release \
                -Z build-std=core --target "$tgt" ) 2>&1 | tee "$log" || return 1
    else
        ( cd "$C" && RUSTC_BOOTSTRAP=1 cargo build --release \
                -Z build-std=core --target "$tgt" ) 2>&1 | tee "$log" || return 1
    fi
    local base; base="$(basename "${tgt%.json}")"
    cp "$C/target/$base/release/libkirra_judge.a" "$JUDGE_LIB"
}

echo "## 1/3  Building the no_std judge for $RUST_TARGET"
if build_judge_direct; then
    echo "[judge] ok: direct rustc cross-build → $JUDGE_LIB"
elif build_judge_buildstd; then
    echo "[judge] ok: cargo build-std cross-build → $JUDGE_LIB"
else
    cat >&2 <<EOF

[judge] FAILED to build the judge for '$RUST_TARGET'.
        Logs:
          direct rustc : $BUILD/rustc_direct.log
          cargo build-std : $BUILD/cargo_buildstd.log
        Most common causes:
          * rust-src missing  → rustup component add rust-src
          * no nightly + stable too old for -Z build-std
        If rustc does NOT list the tuple at all (it DID here), supply a custom
        target spec instead:
          rustc +nightly -Z unstable-options --target x86_64-pc-nto-qnx710 \\
                --print target-spec-json > ~/nto-qnx800.json
          # set "os":"nto", llvm triple ...-nto-qnx8.0.0, linker qcc, then:
          KIRRA_RUST_TARGET_JSON=~/nto-qnx800.json $0
        (See docs/safety/WCET_QNX_BRINGUP.md §1 and docs/adr/KIRRA_QNX_CROSSCOMPILE.md.)
EOF
    exit 1
fi

# ---- 2. Cross-build the C++ shim/harness/demo/wcet with qcc ------------------
echo "## 2/3  Configuring + cross-building C++ (qcc) against the prebuilt judge"
cmake -S "$HERE" -B "$BUILD" \
      -DCMAKE_TOOLCHAIN_FILE="$HERE/qnx.toolchain.cmake" \
      -DKIRRA_QNX_TARGET=ON \
      -DKIRRA_QNX_QCC_VARIANT="$QCC_VARIANT" \
      -DKIRRA_JUDGE_LIB_PREBUILT="$JUDGE_LIB"
cmake --build "$BUILD" -j

# ---- 3. How to run on the QNX target ----------------------------------------
cat <<EOF

## 3/3  Built nto binaries (these will NOT run on this Ubuntu host):
   $BUILD/rtm_harness     FDIT/RTM matrix — exits non-zero on ANY wrong verdict
   $BUILD/wcet_measure    SCHED_FIFO per-verdict WCET row (run as root for FIFO)
   $BUILD/kirra_demo      end-to-end demo incl. a replay

On the x86_64 QNX target (copy them over, e.g. scp / a shared image), run:

   ./rtm_harness && echo "FDIT: every verdict correct on QNX (gate PASS)"
   ./kirra_demo
   ./wcet_measure         # run as root so SCHED_FIFO is granted (else INDICATIVE)

Acceptance (WCET_QNX_BRINGUP.md §4): rtm_harness PASS byte-identically on-target,
and wcet_measure's MAX < 100 µs replaces 'TBD-QNX-TARGET' with 'QNX-TARGET-MEASURED'.
Paste both outputs back and we'll fold the CSV row into QNX_MAPPING.md.
EOF
