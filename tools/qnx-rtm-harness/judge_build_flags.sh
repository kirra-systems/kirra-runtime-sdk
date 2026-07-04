# judge_build_flags.sh — the SINGLE SOURCE for the QNX governor-judge codegen
# flags (#790 F6). This file is `source`d by BOTH judge-build recipes:
#   - scripts/build_qnx_judge_artifact.sh — the shipped-artifact cargo/build-std recipe
#   - tools/qnx-rtm-harness/run_qnx_fdit.sh — the FDIT direct-rustc recipe
# so the reproducibility argument (#790 F2) never rests on three hand-kept copies
# drifting apart. The CMake host-test build (CMakeLists.txt) mirrors these values in
# CMake syntax and names THIS file as the canonical source (CMake cannot `source`
# shell); a drift there is caught by the host-native ctest lane (#790 F1).
#
# SAFETY: any change here is a codegen change to the SIGNED partition verdict core.
# Treat it as a safety change — re-run the FDIT/RTM matrix (QNX_MAPPING.md).
#
# Not executable and has no shebang on purpose: it only assigns shell variables.

# Canonical scalar values (the ONE place these numbers live).
KIRRA_JUDGE_EDITION=2021
KIRRA_JUDGE_CRATE_NAME=kirra_judge
KIRRA_JUDGE_PANIC=abort
KIRRA_JUDGE_OPT_LEVEL=2
KIRRA_JUDGE_DEBUGINFO=0
# #790 F2 — determinism hardening: a single codegen unit + fat LTO make the
# staticlib bytes a deterministic function of (source, toolchain, target) rather
# than of the parallel-codegen scheduling. Enforced end-to-end by the CI
# double-build `cmp` gate, not merely asserted.
KIRRA_JUDGE_CODEGEN_UNITS=1
KIRRA_JUDGE_LTO=fat

# Assembled `-C` flag array for the DIRECT-rustc consumer (run_qnx_fdit.sh); the
# CMake build mirrors exactly these `-C` flags.
KIRRA_JUDGE_RUSTC_CFLAGS=(
    -C "panic=${KIRRA_JUDGE_PANIC}"
    -C "opt-level=${KIRRA_JUDGE_OPT_LEVEL}"
    -C "debuginfo=${KIRRA_JUDGE_DEBUGINFO}"
    -C "codegen-units=${KIRRA_JUDGE_CODEGEN_UNITS}"
    -C "lto=${KIRRA_JUDGE_LTO}"
)
