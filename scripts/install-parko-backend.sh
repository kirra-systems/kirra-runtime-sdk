#!/usr/bin/env bash
#
# install-parko-backend.sh — FULL-STACK, TARGET-PARAMETERIZED install:
# KIRRA (governor/gateway) + OCCY (trajectory planner) + PARKO (per-silicon
# inference backend), composed into one bring-up. The per-silicon axis lives
# entirely in Parko (the backend matrix); Kirra and Occy are silicon-agnostic and
# authored once.
#
# GOAL — RUN-AND-GO READINESS. The install PATH is authored and ready for EVERY
# target now. The only thing that can be missing at install time is EXTERNAL:
# the hardware and (for vendor targets) the operator-supplied licensed SDK
# artifact. Never "we still have to build the install."
#
# TWO READINESS DIMENSIONS (kept distinct — see `--readiness`):
#   (1) INSTALL-PATH readiness — this script. READY NOW for all six targets.
#   (2) BACKEND-CODE readiness — done: ort-cpu, openvino; scaffold: tensorrt
#       (inference Jetson-gated); stub: qnn / ti-tidl / amd-vitis (PARK-027/028/030,
#       a separate code effort). A vendor target's PATH is ready; its BACKEND is
#       the remaining code gate — those are different things and this script says
#       so honestly per target.
#
# Per-target flow (identical shape, dispatched on BackendDescriptor; names align
# with scheduler `descriptor_vendor`):
#   [Kirra gateway] + [Occy bring-up]  (silicon-agnostic, composed once)
#     + select Parko target → acquire runtime/SDK → build Parko (right feature)
#       → apply posture → FAIL-CLOSED validate the backend loads
#     → common (chipset-independent) safety gates across the composed stack.
#
# FAIL-CLOSED: if the selected backend's runtime/EP isn't present, REFUSE — never
# silently substitute another backend (generalizes parko-tensorrt's
# .error_on_failure()). Selection is EXPLICIT; auto-detect only suggests.
# The common safety gates are NOT skippable.

set -euo pipefail

# ── presentation (mirrors install.sh) ────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'
BOLD='\033[1m'; NC='\033[0m'
info()    { echo -e "${BLUE}[INFO]${NC}  $*"; }
success() { echo -e "${GREEN}[OK]${NC}    $*"; }
warn()    { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error()   { echo -e "${RED}[ERROR]${NC} $*" >&2; }
fatal()   { error "$*"; exit 1; }
section() { echo ""; echo -e "${BOLD}━━━ $* ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"; }

# ── locations + per-target tunables (env-overridable) ─────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# The parko cargo workspace (repo_root/parko); the tensorrt load probe runs here.
PARKO_WORKSPACE_DIR="${PARKO_WORKSPACE_DIR:-${SCRIPT_DIR}/../parko}"
# TensorRT runtime acquisition (PARK-021 #6 / issue #414) — JetPack-coupled. The
# jp6/cu126 index serves the cp310 aarch64 onnxruntime-gpu wheel whose ORT 1.23.x
# matches parko's `ort = "=2.0.0-rc.11"`. Installed into an ISOLATED venv so the
# stock JetPack ORT (1.20) is left intact for vendor demos.
PARKO_TRT_VENV="${PARKO_TRT_VENV:-$HOME/parko-trt-venv}"
PARKO_TRT_ORT_VERSION="${PARKO_TRT_ORT_VERSION:-1.23.0}"
PARKO_TRT_PIP_INDEX="${PARKO_TRT_PIP_INDEX:-https://pypi.jetson-ai-lab.io/jp6/cu126}"

# ── the matrix: single source, aligned with scheduler descriptor strings ──────
ALL_TARGETS="ort-cpu openvino tensorrt qnn ti-tidl amd-vitis"

target_descriptor() { case "$1" in
    ort-cpu)   echo "Cpu" ;;        openvino)  echo "IntelOpenVino" ;;
    tensorrt)  echo "TensorRT" ;;   qnn)       echo "QualcommQnn" ;;
    ti-tidl)   echo "TiTidl" ;;     amd-vitis) echo "AmdVitis" ;;
    *) return 1 ;; esac; }

# BACKEND-CODE readiness (dimension 2). NOT the install-path readiness.
#   done     — backend implemented + validatable here (CPU anywhere; Intel on dev box)
#   scaffold — backend builds; real inference hardware-gated (Jetson)
#   stub     — backend not yet implemented (PARK-0xx); the install PATH is still ready
target_backend_code_status() { case "$1" in
    ort-cpu|openvino)  echo "done" ;;
    tensorrt)          echo "scaffold" ;;
    qnn)               echo "stub:PARK-027" ;;
    ti-tidl)           echo "stub:PARK-028" ;;
    amd-vitis)         echo "stub:PARK-030" ;;
    *) return 1 ;; esac; }

target_crate() { case "$1" in
    ort-cpu)   echo "parko-onnx" ;;     openvino) echo "parko-openvino" ;;
    tensorrt)  echo "parko-tensorrt" ;;
    qnn)       echo "parko-qnn (PARK-027)" ;;
    ti-tidl)   echo "parko-tidl (PARK-028)" ;;
    amd-vitis) echo "parko-vitis (PARK-030)" ;;
    *) return 1 ;; esac; }

target_feature() { case "$1" in
    ort-cpu)   echo "(parko-onnx crate)" ;;  openvino)  echo "backend-openvino" ;;
    tensorrt)  echo "backend-tensorrt" ;;     qnn)       echo "backend-qnn" ;;
    ti-tidl)   echo "backend-tidl" ;;         amd-vitis) echo "backend-amd" ;;
    *) return 1 ;; esac; }

# True when the runtime is a VENDOR-LICENSED SDK the operator must supply
# (--sdk-path). These are never auto-fetched.
target_needs_sdk() { case "$1" in
    qnn|ti-tidl|amd-vitis) echo "yes" ;;
    *) echo "no" ;; esac; }

target_runtime_note() { case "$1" in
    ort-cpu)   echo "ONNX Runtime CPU build (freely pullable)" ;;
    openvino)  echo "OpenVINO runtime (pip/apt; freely pullable)" ;;
    tensorrt)  echo "NVIDIA TensorRT-enabled ONNX Runtime (JetPack/L4T on the Jetson)" ;;
    qnn)       echo "Qualcomm QNN SDK (operator-supplied licensed artifact via --sdk-path)" ;;
    ti-tidl)   echo "TI TIDL / Processor SDK (operator-supplied licensed artifact via --sdk-path)" ;;
    amd-vitis) echo "AMD Vitis AI (operator-supplied licensed artifact via --sdk-path)" ;;
    *) return 1 ;; esac; }

target_posture() { case "$1" in
    ort-cpu)   echo "single-thread + GraphOptimizationLevel::Disable (bitwise-reproducible)" ;;
    openvino)  echo "ACCURACY + INFERENCE_PRECISION_HINT=f32 + LATENCY (mirrors ORT-CPU)" ;;
    tensorrt)  echo "fp16=false, int8=false, engine-cache on; TF32 UNENFORCED (Jetson-gated); decision-agreement posture" ;;
    qnn)       echo "full precision; QNN HTP posture defined with the backend (PARK-027)" ;;
    ti-tidl)   echo "full precision; TIDL posture defined with the backend (PARK-028)" ;;
    amd-vitis) echo "full precision; Vitis DPU posture defined with the backend (PARK-030)" ;;
    *) return 1 ;; esac; }

# Honest one-line external gate per target.
target_external_gate() { case "$1" in
    ort-cpu)   echo "none — ready now (CPU, anywhere)" ;;
    openvino)  echo "Intel silicon (dev box ok) — ready now there" ;;
    tensorrt)  echo "NVIDIA Jetson hardware (no license) — ready on hardware" ;;
    qnn)       echo "Qualcomm hardware + QNN SDK + backend code (PARK-027)" ;;
    ti-tidl)   echo "TI hardware + TIDL SDK + backend code (PARK-028)" ;;
    amd-vitis) echo "AMD hardware + Vitis AI + backend code (PARK-030)" ;;
    *) return 1 ;; esac; }

# ── arguments ─────────────────────────────────────────────────────────────────
TARGET=""; SDK_PATH=""
AUTO_DETECT=false; CONFIRMED=false; NON_INTERACTIVE=false; DRY_RUN=false
WITH_KIRRA=true; WITH_OCCY=true

usage() {
    cat <<EOF
Usage: sudo bash install-parko-backend.sh --target <TARGET> [OPTIONS]

Full-stack install: KIRRA (gateway) + OCCY (planner) + PARKO (per-silicon
backend). The per-silicon axis is the Parko --target; Kirra+Occy are
silicon-agnostic and composed once.

Targets (== scheduler descriptor strings) and readiness:
  ort-cpu    Cpu           path READY · backend DONE      · external: none (now)
  openvino   IntelOpenVino path READY · backend DONE      · external: Intel HW
  tensorrt   TensorRT      path READY · backend SCAFFOLD  · external: Jetson HW
  qnn        QualcommQnn   path READY · backend STUB(027) · external: HW+SDK+code
  ti-tidl    TiTidl        path READY · backend STUB(028) · external: HW+SDK+code
  amd-vitis  AmdVitis      path READY · backend STUB(030) · external: HW+SDK+code

Options:
  --target <name>    Parko backend target (EXPLICIT — recommended).
  --sdk-path <PATH>  Operator-supplied licensed SDK artifact (REQUIRED for the
                     vendor targets qnn/ti-tidl/amd-vitis). Never auto-fetched.
  --auto-detect      SUGGEST a target from hardware; requires --confirm.
  --confirm          Accept the auto-detected suggestion non-interactively.
  --parko-only       Install only the Parko backend (skip Kirra + Occy).
  --no-occy          Skip Occy bring-up (Kirra + Parko only).
  --non-interactive  No prompts.
  --dry-run          Print every step; acquire/build/install nothing. Runs
                     anywhere (no hardware/root). Used by the self-test.
  --readiness        Print the per-target readiness model (two dimensions) + exit.
  --list             Print the target matrix + exit.
  --help             This help.

FAIL-CLOSED: a selected backend whose runtime/EP is absent REFUSES — never
substitutes another backend. Common safety gates always run, never skipped.
EOF
}

print_matrix() {
    section "Parko backend / chipset target matrix"
    printf "%-10s %-14s %-16s %-13s %s\n" "TARGET" "DESCRIPTOR" "CRATE" "BACKEND-CODE" "RUNTIME"
    for t in $ALL_TARGETS; do
        printf "%-10s %-14s %-16s %-13s %s\n" \
            "$t" "$(target_descriptor "$t")" "$(target_crate "$t")" \
            "$(target_backend_code_status "$t")" "$(target_runtime_note "$t")"
    done
}

print_readiness() {
    section "Readiness model — install-path vs backend-code, per target (honest)"
    echo "Install-path readiness is READY NOW for ALL targets (this script)."
    echo "The remaining gates are EXTERNAL (hardware / licensed SDK) and, for the"
    echo "vendor targets, the BACKEND CODE itself. These are distinct dimensions:"
    echo ""
    printf "%-10s %-13s %-13s %s\n" "TARGET" "INSTALL-PATH" "BACKEND-CODE" "REMAINING EXTERNAL GATE"
    for t in $ALL_TARGETS; do
        printf "%-10s %-13s %-13s %s\n" \
            "$t" "READY" "$(target_backend_code_status "$t")" "$(target_external_gate "$t")"
    done
    echo ""
    echo "Summary: ort-cpu/openvino = ready now; tensorrt = ready on Jetson HW (no"
    echo "license); qnn/ti-tidl/amd-vitis = PATH ready, backend code + HW + SDK still"
    echo "required. A vendor target is NOT one-command-ready while its backend is a stub."
}

while [ $# -gt 0 ]; do
    case "$1" in
        --target) TARGET="${2:-}"; shift 2 ;;
        --target=*) TARGET="${1#*=}"; shift ;;
        --sdk-path) SDK_PATH="${2:-}"; shift 2 ;;
        --sdk-path=*) SDK_PATH="${1#*=}"; shift ;;
        --auto-detect) AUTO_DETECT=true; shift ;;
        --confirm) CONFIRMED=true; shift ;;
        --parko-only) WITH_KIRRA=false; WITH_OCCY=false; shift ;;
        --no-occy) WITH_OCCY=false; shift ;;
        --non-interactive) NON_INTERACTIVE=true; shift ;;
        --dry-run) DRY_RUN=true; shift ;;
        --readiness) print_readiness; exit 0 ;;
        --list) print_matrix; exit 0 ;;
        --help|-h) usage; exit 0 ;;
        # No --skip-safety-gates: the common gates are non-skippable by design.
        *) fatal "Unknown argument: $1 (see --help)" ;;
    esac
done

# ── selection (explicit; auto-detect only suggests) ───────────────────────────
auto_detect_suggest() {
    if command -v nvidia-smi >/dev/null 2>&1 || { [ -d /proc/device-tree ] && grep -qi nvidia /proc/device-tree/model 2>/dev/null; }; then
        echo "tensorrt"; return; fi
    if [ -d /dev/dri ] && grep -qi "GenuineIntel" /proc/cpuinfo 2>/dev/null; then
        echo "openvino"; return; fi
    echo "ort-cpu"
}

select_target() {
    if [ "$AUTO_DETECT" = true ]; then
        local s; s="$(auto_detect_suggest)"
        warn "Auto-detect is a SUGGESTION, not a decision (can mask misconfig / pick wrong silicon)."
        info "Suggested: ${BOLD}${s}${NC}"
        if [ "$CONFIRMED" = true ]; then TARGET="$s"; info "Accepted via --confirm."
        elif [ "$NON_INTERACTIVE" = true ]; then
            fatal "Auto-detect requires explicit confirmation. Re-run with --confirm or --target ${s}."
        else
            read -r -p "Use '${s}'? Type the target name to confirm: " a
            [ "$a" = "$s" ] || fatal "Not confirmed — refusing to guess. Pass --target explicitly."
            TARGET="$s"
        fi
    fi
    [ -n "$TARGET" ] || { usage; fatal "No target. Pass --target <name> (recommended) or --auto-detect."; }
    target_descriptor "$TARGET" >/dev/null 2>&1 || fatal "Unknown target '${TARGET}'. Valid: ${ALL_TARGETS}"
}

# ── full-stack composition: KIRRA + OCCY (silicon-agnostic) ───────────────────
install_kirra_gateway() {
    [ "$WITH_KIRRA" = true ] || { info "Skipping Kirra (--parko-only)."; return 0; }
    section "Compose: KIRRA gateway (silicon-agnostic)"
    info "The governor/gateway (kirra_verifier_service) is installed by install.sh —"
    info "authored once, identical for every silicon target."
    if [ "$DRY_RUN" = true ]; then
        info "[dry-run] would run: sudo bash install.sh --non-interactive"
    else
        info "Run: sudo bash install.sh   (see INSTALL.md)"
    fi
}

bringup_occy() {
    [ "$WITH_OCCY" = true ] || { info "Skipping Occy (--no-occy / --parko-only)."; return 0; }
    section "Compose: OCCY trajectory planner (silicon-agnostic ROS2)"
    info "Occy (Autoware-filled for the pilot) is a ROS2 trajectory planner — silicon-"
    info "agnostic; the same bring-up for every target. It registers with Kirra via"
    info "scripts/setup_ros2_fleet.sh and publishes plans the governor gates."
    if [ "$DRY_RUN" = true ]; then
        info "[dry-run] would: build the ROS2 planner workspace + register nodes with Kirra."
    fi
}

# ── PARKO per-target: acquire → build → posture → validate ────────────────────
acquire_runtime() {
    local t="$1"
    section "Parko 1/4 — acquire runtime/SDK (${t})"
    info "Runtime: $(target_runtime_note "$t")"
    if [ "$(target_needs_sdk "$t")" = "yes" ]; then
        # Vendor target: the operator SUPPLIES the licensed artifact. Authored,
        # ready-to-run — gated only on that external artifact being present.
        if [ -z "$SDK_PATH" ]; then
            fatal "Target '${t}' needs an operator-supplied licensed SDK: pass --sdk-path <ARTIFACT>. \
This is the PATH waiting on the EXTERNAL artifact — not a missing install (never auto-fetched)."
        fi
        if [ "$DRY_RUN" = true ]; then
            info "[dry-run] would install the ${t} backend SDK from operator artifact: ${SDK_PATH}"
        else
            [ -e "$SDK_PATH" ] || fatal "Supplied --sdk-path '${SDK_PATH}' not found. Provide the licensed artifact."
            info "Installing ${t} SDK from operator artifact: ${SDK_PATH}"
        fi
        return 0
    fi
    # tensorrt drives its own dry-run (it prints the real recipe); others use the
    # generic note.
    if [ "$DRY_RUN" = true ] && [ "$t" != "tensorrt" ]; then
        info "[dry-run] would acquire the ${t} runtime."; return 0
    fi
    case "$t" in
        ort-cpu)  info "Acquire ONNX Runtime CPU (v1.23.x) → ORT_DYLIB_PATH (see INSTALL.md)." ;;
        openvino) info "Acquire OpenVINO runtime (pip wheel >=2025.1 / apt) → LD_LIBRARY_PATH." ;;
        tensorrt) acquire_tensorrt_runtime ;;
    esac
}

# TensorRT-enabled ONNX Runtime — the on-Jetson recipe validated in PARK-021 #6
# (issue #414). Installs the version-matched wheel into an ISOLATED venv and exports
# ORT_DYLIB_PATH (+ the provider-lib dir on LD_LIBRARY_PATH) for the build/validate
# stages. JETSON-GATED: this is the NVIDIA Jetson path, no license required.
acquire_tensorrt_runtime() {
    info "TensorRT-enabled ONNX Runtime ${PARKO_TRT_ORT_VERSION} — Jetson-gated, isolated venv: ${PARKO_TRT_VENV}"
    info "Index: ${PARKO_TRT_PIP_INDEX} (JetPack-coupled: jp6/cu126 → cp310 aarch64 wheel; matches ort rc.11 = ORT 1.23.x)"
    if [ "$DRY_RUN" = true ]; then
        info "[dry-run] would: python3 -m venv ${PARKO_TRT_VENV}"
        info "[dry-run] would: ${PARKO_TRT_VENV}/bin/pip install --extra-index-url ${PARKO_TRT_PIP_INDEX} 'onnxruntime-gpu==${PARKO_TRT_ORT_VERSION}' 'numpy<2'"
        info "[dry-run] would: export ORT_DYLIB_PATH=<venv>/…/libonnxruntime.so.${PARKO_TRT_ORT_VERSION}"
        return 0
    fi
    # Jetson sanity (warn, don't hard-fail — the operator may know better). The
    # fail-closed load probe is the real gate: it REFUSES if the TRT EP is absent.
    if ! { [ -d /proc/device-tree ] && grep -qi nvidia /proc/device-tree/model 2>/dev/null; }; then
        warn "Host does not look like an NVIDIA Jetson (no 'nvidia' in device-tree model)."
        warn "The TensorRT runtime is Jetson-gated; proceeding, but the load probe will REFUSE if the TRT EP isn't present."
    fi
    command -v python3 >/dev/null 2>&1 || fatal "python3 not found — required to create the isolated ORT venv."
    if [ ! -d "$PARKO_TRT_VENV" ]; then
        info "Creating isolated venv: ${PARKO_TRT_VENV}"
        python3 -m venv "$PARKO_TRT_VENV" || fatal "venv creation failed at ${PARKO_TRT_VENV}."
    else
        info "Reusing existing venv: ${PARKO_TRT_VENV}"
    fi
    "$PARKO_TRT_VENV/bin/pip" install --quiet --upgrade pip \
        || fatal "pip self-upgrade failed in ${PARKO_TRT_VENV}."
    # numpy<2 — the prebuilt wheel is built against NumPy 1.x (PARK-021 #6 finding).
    info "Installing onnxruntime-gpu==${PARKO_TRT_ORT_VERSION} (+ numpy<2) from ${PARKO_TRT_PIP_INDEX}"
    "$PARKO_TRT_VENV/bin/pip" install --extra-index-url "$PARKO_TRT_PIP_INDEX" \
        "onnxruntime-gpu==${PARKO_TRT_ORT_VERSION}" 'numpy<2' \
        || fatal "Failed to install onnxruntime-gpu==${PARKO_TRT_ORT_VERSION} from ${PARKO_TRT_PIP_INDEX} (JetPack/index mismatch?)."
    # Locate the .so the ort crate will dlopen, and export it for build + validate.
    local so
    so="$(find "$PARKO_TRT_VENV" -name "libonnxruntime.so.${PARKO_TRT_ORT_VERSION}" 2>/dev/null | head -1)"
    [ -n "$so" ] && [ -e "$so" ] || fatal "Installed the wheel but could not locate libonnxruntime.so.${PARKO_TRT_ORT_VERSION} under ${PARKO_TRT_VENV}."
    export ORT_DYLIB_PATH="$so"
    # The TRT/CUDA provider libs sit beside it; put that dir on the loader path.
    export LD_LIBRARY_PATH="$(dirname "$so")${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    success "TensorRT ORT runtime ready: ORT_DYLIB_PATH=${ORT_DYLIB_PATH}"
}

build_parko() {
    local t="$1"
    section "Parko 2/4 — build (${t})"
    info "Crate: $(target_crate "$t") | feature: $(target_feature "$t")"
    case "$(target_backend_code_status "$t")" in
        stub:*) warn "Backend code is a STUB ($(target_backend_code_status "$t")) — the build target is reserved; \
the crate is implemented as a separate code effort. PATH is wired; this is the code gate." ;;
    esac
    if [ "$DRY_RUN" = true ]; then info "[dry-run] would build $(target_crate "$t")."; fi
}

apply_posture() {
    local t="$1"
    section "Parko 3/4 — posture config (${t})"
    info "Posture: $(target_posture "$t")"
    if [ "$DRY_RUN" = true ]; then info "[dry-run] would write the ${t} posture config."; fi
}

# Built-in strict TensorRT load probe (PARK-021 #6 / #414). Runs the parko-tensorrt
# positive_probe test in REQUIRE_EP mode against the ORT_DYLIB_PATH the acquire step
# exported; cargo's exit code is authoritative (the test fails — never self-skips —
# when the TRT EP is absent). Returns nonzero on any miss so the caller fail-closes.
run_tensorrt_strict_probe() {
    [ -n "${ORT_DYLIB_PATH:-}" ] && [ -e "${ORT_DYLIB_PATH:-}" ] \
        || { error "ORT_DYLIB_PATH unset/missing — the acquire step did not export the TensorRT ORT runtime."; return 1; }
    command -v cargo >/dev/null 2>&1 \
        || { error "cargo not found — required to run the tensorrt load probe."; return 1; }
    [ -f "${PARKO_WORKSPACE_DIR}/Cargo.toml" ] \
        || { error "parko workspace not found at ${PARKO_WORKSPACE_DIR} (set PARKO_WORKSPACE_DIR)."; return 1; }
    ( cd "$PARKO_WORKSPACE_DIR" \
        && PARKO_TRT_REQUIRE_EP=1 cargo test -p parko-tensorrt --test positive_probe -- --nocapture )
}

# FAIL-CLOSED backend-load validation. The single chokepoint for "the selected
# backend actually loaded". Generalizes parko-tensorrt's .error_on_failure().
validate_backend_loads() {
    local t="$1"; local code; code="$(target_backend_code_status "$t")"
    section "Parko 4/4 — FAIL-CLOSED backend-load validation (${t})"
    if [ "$DRY_RUN" = true ]; then
        info "[dry-run] would run the ${t} load probe and REFUSE on failure (no substitution)."
        return 0
    fi
    case "$code" in
        stub:*)
            # PATH ran (acquire/build/posture). FINAL validation defers to when the
            # backend CODE exists — a clearly-marked boundary, NOT a path gap.
            warn "Install PATH for '${t}' is ready and ran. FINAL backend-load validation is"
            warn "DEFERRED: the backend code is not yet implemented (${code}). This is the"
            warn "remaining CODE gate, distinct from the (present) hardware + SDK."
            fatal "Refusing to claim a VALIDATED backend for '${t}' while its code is a stub (fail-closed)." ;;
        done|scaffold)
            # Run the backend's own fail-closed load probe. Contract: the probe
            # exits 0 ONLY if the SELECTED backend's runtime/EP actually loaded
            # (parko-tensorrt already fail-closes via .error_on_failure(); ORT/OV
            # error/panic without their runtime). Any failure → REFUSE, never
            # substitute. The operator points PARKO_BACKEND_PROBE at the probe
            # (e.g. the crate's load check on a box with the runtime present).
            #
            # tensorrt ships a BUILT-IN strict probe (PARK-021 #6 / #414): the
            # positive_probe test in PARKO_TRT_REQUIRE_EP mode, run against the
            # ORT_DYLIB_PATH the acquire step exported. It exits nonzero unless the
            # TRT EP genuinely loaded (a self-skip becomes a hard failure), so an
            # operator need not wire PARKO_BACKEND_PROBE by hand on the Jetson.
            if [ -z "${PARKO_BACKEND_PROBE:-}" ] && [ "$t" = "tensorrt" ]; then
                info "Backend-load probe: built-in tensorrt strict load probe (positive_probe, PARKO_TRT_REQUIRE_EP=1)."
                if run_tensorrt_strict_probe; then
                    success "Backend 'tensorrt' load validated (TensorRT EP present, fail-closed)."
                else
                    fatal "Backend-load probe FAILED for 'tensorrt' — refusing (fail-closed; no substitution)."
                fi
                return 0
            fi
            if [ -n "${PARKO_BACKEND_PROBE:-}" ]; then
                info "Backend-load probe: ${PARKO_BACKEND_PROBE}"
                if "${PARKO_BACKEND_PROBE}"; then
                    success "Backend '${t}' load validated (runtime/EP present)."
                else
                    fatal "Backend-load probe FAILED for '${t}' — refusing (fail-closed; no substitution)."
                fi
            else
                fatal "No backend-load probe wired (PARKO_BACKEND_PROBE unset), so the '${t}' runtime/EP \
presence is unverified — refusing to claim a validated backend (fail-closed). For ort-cpu/openvino set \
PARKO_BACKEND_PROBE to the crate's load check on a box with the runtime; tensorrt is Jetson-gated. \
Use --dry-run to exercise the framework without a runtime."
            fi ;;
    esac
}

# ── common safety gates (chipset-independent; EVERY target; non-skippable) ────
gate() {
    if [ "$DRY_RUN" = true ]; then info "[dry-run] GATE: $1 — $2"; else info "GATE: $1 — $2"; fi
}
run_common_safety_gates() {
    local t="$1"
    section "Common safety gates — across the composed stack, NON-skippable"
    gate "backend-load"    "selected Parko backend loaded fail-closed — no silent substitution"
    gate "chokepoint"      "exactly ONE publisher on the motor command topic (Kirra gateway is the sole writer)"
    gate "envelope-config" "kinematic envelope + posture config present and parseable"
    gate "e-stop"          "emergency-stop path verified reachable and authoritative"
    gate "wheels-up smoke" "an over-limit Occy plan is clamped/denied by Kirra with the vehicle on stands"
    success "Common safety gates defined for the Kirra+Occy+${t} stack (refuse-to-proceed on deploy)."
}

# ── main ──────────────────────────────────────────────────────────────────────
main() {
    select_target
    section "Full-stack install — Parko target '${TARGET}' ($(target_descriptor "$TARGET"))"
    info "Backend-code: $(target_backend_code_status "$TARGET") | external gate: $(target_external_gate "$TARGET")"
    if [ "$DRY_RUN" = true ]; then warn "DRY-RUN: nothing acquired, built, or installed."; fi
    # Compose the silicon-agnostic layers first, then the per-silicon backend.
    install_kirra_gateway
    bringup_occy
    acquire_runtime         "$TARGET"
    build_parko             "$TARGET"
    apply_posture           "$TARGET"
    validate_backend_loads  "$TARGET"
    run_common_safety_gates "$TARGET"
    success "Full-stack flow complete for Kirra + Occy + Parko('${TARGET}')."
}

main
