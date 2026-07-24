"""gpu — Jetson CUDA/GPU acceleration + Ollama GPU offload (read-only).

Answers the one operational question that actually moves the needle on this
robot: **are the DOERS on the GPU?** The doer LLM (rabbit + mick, via Ollama)
and the parko ONNX/TensorRT inference backends are the components CUDA helps;
the KIRRA checker is deliberately deterministic CPU (WCET-bounded) and is NOT a
GPU consumer by design — so this module never asks the checker to "use CUDA."

The load-bearing check is `ollama GPU offload`: Ollama silently falls back to
CPU when its CUDA runtime is missing, and a model resident on the CPU is the #1
cause of a sluggish rabbit — visible here as "0 B in VRAM," not a crash.

Status policy (doctor.core): FAIL is reserved for "configured-but-broken."
Missing CUDA on a Jetson that must run parko inference is a WARN (actionable),
never FAIL — the parko backends fail closed on their own if the GPU is absent
(`parko-onnx` registers the CUDA EP `error_on_failure`). On a non-Tegra host
(a laptop / CI runner) the Jetson checks are informational PASS — the doctor
must not cry wolf where GPU accel was never expected.

Pure classifiers (`detect_tegra`, `classify_*`) take raw command/file output and
return check rows, so the safety-irrelevant-but-fiddly parsing is host-tested
(`robot/gpu_doctor_test.py`); `run` does only the read-only IO. stdlib only.
"""
from __future__ import annotations

import glob
import json
import os
import re
import urllib.request

from doctor.core import detail, run_cmd

NAME = "gpu"
DESCRIPTION = "Jetson CUDA/TensorRT + Ollama GPU offload (doers only; checker stays CPU)"
DEFAULT, HEAVY, TIMEOUT_S = True, False, 15

# Where JetPack / desktop CUDA drop their runtime .so; globbed, never assumed.
_CUDA_LIB_GLOBS = (
    "/usr/local/cuda*/lib64/libcudart.so*",
    "/usr/local/cuda*/targets/*/lib/libcudart.so*",
    "/usr/lib/aarch64-linux-gnu/libcudart.so*",
    "/usr/lib/x86_64-linux-gnu/libcudart.so*",
)
_TRT_LIB_GLOBS = (
    "/usr/lib/aarch64-linux-gnu/libnvinfer.so*",
    "/usr/lib/x86_64-linux-gnu/libnvinfer.so*",
)


# ---- pure parsers ------------------------------------------------------------

def parse_nvcc_version(nvcc_out: str):
    """CUDA toolkit version from `nvcc --version` output, or None."""
    m = re.search(r"release (\d+\.\d+)", nvcc_out or "")
    return m.group(1) if m else None


def parse_gpu_names(smi_l_out: str):
    """GPU names from `nvidia-smi -L` ('GPU 0: NAME (UUID: ...)'). The name may
    itself contain parens (a Jetson lists 'Orin (nvgpu)'), so split on '(UUID:',
    not the first '('."""
    return [m.group(1).strip()
            for m in re.finditer(r"GPU \d+:\s*(.+?)\s*\(UUID:", smi_l_out or "")]


def _mib(n) -> int:
    try:
        return int(n) // (1024 * 1024)
    except (TypeError, ValueError):
        return 0


# ---- pure classifiers (raw signal -> one check row) --------------------------

def detect_tegra(model_text: str, tegra_release_text: str):
    """(is_tegra, human_name). A Jetson is identified by /proc/device-tree/model
    naming (Jetson/Orin/Tegra) OR the mere presence of /etc/nv_tegra_release."""
    name = (model_text or "").strip().strip("\x00") or "unknown host"
    is_tegra = bool(
        re.search(r"jetson|orin|tegra|xavier|nano", model_text or "", re.I)
        or (tegra_release_text or "").strip()
    )
    return is_tegra, name


def classify_platform(is_tegra: bool, name: str):
    if is_tegra:
        return detail("platform", "PASS", f"Jetson/Tegra: {name}")
    return detail("platform", "PASS",
                  f"non-Tegra host ({name}) — Jetson GPU checks are informational here")


def classify_cuda_runtime(is_tegra: bool, cuda_libs, nvcc_version):
    if cuda_libs:
        base = os.path.basename(cuda_libs[0])
        v = f"nvcc {nvcc_version}" if nvcc_version else "nvcc not installed (runtime-only is fine)"
        return detail("cuda runtime", "PASS", f"CUDA runtime present: {base} ({v})")
    if not is_tegra:
        return detail("cuda runtime", "PASS",
                      "no system CUDA runtime — not applicable on a non-Tegra host")
    return detail(
        "cuda runtime", "WARN",
        "no system CUDA runtime (libcudart) found — parko ONNX-CUDA / TensorRT "
        "backends can't initialise (they fail closed). Note: Ollama bundles its "
        "own CUDA, so rabbit may still offload — see the ollama check",
        fix="install JetPack CUDA (sudo apt install nvidia-jetpack) — see docs/hardware/JETSON_CUDA_SETUP.md",
    )


def classify_tensorrt(is_tegra: bool, trt_libs):
    if trt_libs:
        return detail("tensorrt", "PASS", f"TensorRT present: {os.path.basename(trt_libs[0])}")
    if not is_tegra:
        return detail("tensorrt", "PASS", "non-Tegra host — TensorRT check skipped")
    # Optional per backend → informational, not a WARN (ONNX-CUDA works without it).
    return detail("tensorrt", "PASS",
                  "TensorRT (libnvinfer) not found — the parko-tensorrt backend is "
                  "unavailable; parko ONNX-CUDA still works if CUDA is present")


def classify_nvidia_smi(is_tegra: bool, rc: int, smi_l_out: str):
    names = parse_gpu_names(smi_l_out) if rc == 0 else []
    if names:
        return detail("nvidia-smi", "PASS", f"nvidia-smi sees: {names[0]}")
    if is_tegra:
        return detail("nvidia-smi", "PASS",
                      "nvidia-smi absent (normal on Jetson/L4T — use tegrastats); GPU is "
                      "confirmed via the cuda-runtime + ollama-offload checks")
    return detail("nvidia-smi", "PASS", "no NVIDIA GPU tooling (nvidia-smi absent)")


def classify_ollama_offload(reachable: bool, models, model_hint: str):
    """The load-bearing check: is the resident doer model on the GPU?
    `models` = the Ollama /api/ps `models` list (dicts with size / size_vram)."""
    if not reachable:
        return detail("ollama GPU offload", "PASS",
                      "Ollama not reachable at :11434 — offload check skipped (see the services module)")
    if not models:
        return detail(
            "ollama GPU offload", "UNKNOWN",
            "no model resident (Ollama idle-unloaded it) — can't confirm GPU offload",
            fix="warm the doer (send rabbit one utterance) then re-run, or pin it with Ollama keep_alive",
        )
    m = models[0]
    name = m.get("name") or m.get("model") or model_hint
    size, vram = int(m.get("size") or 0), int(m.get("size_vram") or 0)
    if size <= 0:
        return detail("ollama GPU offload", "UNKNOWN",
                      f"{name} is resident but Ollama reported no size — can't classify offload")
    if vram <= 0:
        return detail(
            "ollama GPU offload", "WARN",
            f"{name} is resident on the CPU (0 B in VRAM) — GPU offload is NOT active; "
            "this is the usual cause of slow rabbit/mick responses",
            fix="confirm Ollama has a CUDA runtime + a GPU build; check: journalctl -u ollama | grep -i gpu",
        )
    if vram >= size * 0.99:
        return detail("ollama GPU offload", "PASS", f"{name} fully on GPU ({_mib(vram)} MiB in VRAM)")
    pct = round(100 * vram / size)
    return detail(
        "ollama GPU offload", "WARN",
        f"{name} only partially offloaded ({pct}% — {_mib(vram)} of {_mib(size)} MiB on GPU); "
        "the remaining layers run on the CPU",
        fix="use a smaller / more-quantized model or free VRAM so the whole model fits",
    )


# ---- read-only IO ------------------------------------------------------------

def _read(path: str) -> str:
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            return f.read()
    except Exception:  # noqa: BLE001
        return ""


def _glob_any(patterns) -> list:
    out = []
    for p in patterns:
        out.extend(sorted(glob.glob(p)))
    return out


def _ollama_ps(ctx):
    """(reachable, models_list) from Ollama's GET /api/ps. Never raises."""
    env = ctx.get("env", {}) if isinstance(ctx, dict) else {}
    base = (env.get("KIRRA_OLLAMA_URL") or "http://127.0.0.1:11434").rstrip("/")
    try:
        with urllib.request.urlopen(f"{base}/api/ps", timeout=2.0) as r:
            body = json.loads(r.read().decode("utf-8"))
        models = body.get("models") if isinstance(body, dict) else None
        return True, (models if isinstance(models, list) else [])
    except Exception:  # noqa: BLE001
        return False, []


def run(ctx):
    env = ctx.get("env", {}) if isinstance(ctx, dict) else {}
    model_hint = (env.get("KIRRA_RABBIT_MODEL")
                  or ctx.get("robot_env", {}).get("KIRRA_RABBIT_MODEL")
                  or "the doer model")

    is_tegra, name = detect_tegra(_read("/proc/device-tree/model"), _read("/etc/nv_tegra_release"))
    details = [classify_platform(is_tegra, name)]

    _, nvcc_out, _ = run_cmd(["nvcc", "--version"], timeout_s=5)
    details.append(classify_cuda_runtime(is_tegra, _glob_any(_CUDA_LIB_GLOBS),
                                         parse_nvcc_version(nvcc_out)))
    details.append(classify_tensorrt(is_tegra, _glob_any(_TRT_LIB_GLOBS)))

    rc, smi_out, _ = run_cmd(["nvidia-smi", "-L"], timeout_s=5)
    details.append(classify_nvidia_smi(is_tegra, rc, smi_out))

    reachable, models = _ollama_ps(ctx)
    details.append(classify_ollama_offload(reachable, models, model_hint))

    return {"details": details}
