#!/usr/bin/env python3
"""gpu_doctor_test — host tests for the doctor `gpu` module. CI-safe: no
hardware, no GPU, no network beyond a localhost-refused connect. The pure
classifiers are exercised with synthetic command/file output; the live `run`
is smoke-tested for a bounded, valid, non-raising result on a GPU-less host.
Runner style matches rabbit_voice_test.py / kirra_doctor_test.py (assert-based,
stdlib only).
"""
from __future__ import annotations

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from doctor import core  # noqa: E402
from doctor.modules import gpu  # noqa: E402

_VALID = set(core.STATUSES)


# ---- pure parsers ------------------------------------------------------------

def test_parse_nvcc_version() -> None:
    out = ("nvcc: NVIDIA (R) Cuda compiler driver\n"
           "Cuda compilation tools, release 12.2, V12.2.140\n")
    assert gpu.parse_nvcc_version(out) == "12.2"
    assert gpu.parse_nvcc_version("") is None
    assert gpu.parse_nvcc_version("garbage") is None


def test_parse_gpu_names() -> None:
    out = "GPU 0: NVIDIA GeForce RTX 3090 (UUID: GPU-abc)\nGPU 1: Orin (nvgpu) (UUID: GPU-def)\n"
    assert gpu.parse_gpu_names(out) == ["NVIDIA GeForce RTX 3090", "Orin (nvgpu)"]
    assert gpu.parse_gpu_names("") == []


# ---- tegra detection ---------------------------------------------------------

def test_detect_tegra_from_model() -> None:
    is_tegra, name = gpu.detect_tegra("NVIDIA Jetson Orin NX Engineering Reference\x00", "")
    assert is_tegra
    assert "Orin" in name and "\x00" not in name


def test_detect_tegra_from_release_file_alone() -> None:
    # A generic model string but nv_tegra_release present is still a Jetson.
    is_tegra, _ = gpu.detect_tegra("", "# R36 (release), REVISION: 3.0\n")
    assert is_tegra


def test_detect_non_tegra() -> None:
    is_tegra, name = gpu.detect_tegra("Dell Inc. XPS 15", "")
    assert not is_tegra
    assert "XPS" in name


# ---- cuda runtime ------------------------------------------------------------

def test_cuda_present_passes() -> None:
    d = gpu.classify_cuda_runtime(True, ["/usr/local/cuda-12.2/lib64/libcudart.so.12"], "12.2")
    assert d["status"] == "PASS" and "12.2" in d["info"]


def test_cuda_absent_on_tegra_warns() -> None:
    d = gpu.classify_cuda_runtime(True, [], None)
    assert d["status"] == "WARN" and "fix" in d
    assert "bundles its own CUDA" in d["info"]  # the Ollama caveat is surfaced


def test_cuda_absent_off_tegra_is_not_applicable() -> None:
    d = gpu.classify_cuda_runtime(False, [], None)
    assert d["status"] == "PASS"  # never cry wolf on a laptop / CI runner


# ---- tensorrt (informational) -----------------------------------------------

def test_tensorrt_present_passes() -> None:
    d = gpu.classify_tensorrt(True, ["/usr/lib/aarch64-linux-gnu/libnvinfer.so.8"])
    assert d["status"] == "PASS" and "TensorRT present" in d["info"]


def test_tensorrt_absent_is_informational_not_warn() -> None:
    # Optional per backend — ONNX-CUDA works without it, so absence never WARNs.
    assert gpu.classify_tensorrt(True, [])["status"] == "PASS"
    assert gpu.classify_tensorrt(False, [])["status"] == "PASS"


# ---- nvidia-smi (never FAIL) -------------------------------------------------

def test_nvidia_smi_names_pass() -> None:
    d = gpu.classify_nvidia_smi(False, 0, "GPU 0: NVIDIA A100 (UUID: GPU-x)\n")
    assert d["status"] == "PASS" and "A100" in d["info"]


def test_nvidia_smi_absent_on_tegra_is_normal() -> None:
    d = gpu.classify_nvidia_smi(True, -1, "")
    assert d["status"] == "PASS" and "tegrastats" in d["info"]


# ---- the load-bearing ollama-offload classifier ------------------------------

def test_ollama_unreachable_skips_without_alarm() -> None:
    d = gpu.classify_ollama_offload(False, [], "gemma3:4b")
    assert d["status"] == "PASS" and "skipped" in d["info"]


def test_ollama_idle_is_unknown() -> None:
    d = gpu.classify_ollama_offload(True, [], "gemma3:4b")
    assert d["status"] == "UNKNOWN" and "fix" in d


def test_ollama_on_cpu_warns() -> None:
    # size_vram == 0 → resident on CPU → the slow-rabbit smoking gun.
    models = [{"name": "gemma3:4b", "size": 3_000_000_000, "size_vram": 0}]
    d = gpu.classify_ollama_offload(True, models, "gemma3:4b")
    assert d["status"] == "WARN" and "0 B in VRAM" in d["info"] and "fix" in d


def test_ollama_fully_on_gpu_passes() -> None:
    models = [{"name": "gemma3:4b", "size": 3_000_000_000, "size_vram": 3_000_000_000}]
    d = gpu.classify_ollama_offload(True, models, "gemma3:4b")
    assert d["status"] == "PASS" and "fully on GPU" in d["info"]


def test_ollama_partial_offload_warns() -> None:
    models = [{"name": "gemma3:4b", "size": 4_000_000_000, "size_vram": 1_000_000_000}]
    d = gpu.classify_ollama_offload(True, models, "gemma3:4b")
    assert d["status"] == "WARN" and "partially offloaded" in d["info"]


def test_ollama_resident_but_no_size_is_unknown() -> None:
    d = gpu.classify_ollama_offload(True, [{"name": "x", "size": 0, "size_vram": 0}], "x")
    assert d["status"] == "UNKNOWN"


# ---- live run smoke (GPU-less host: bounded, valid, never raises) ------------

def test_run_is_bounded_and_valid_on_this_host() -> None:
    ctx = {"env": dict(os.environ), "robot_env": {}, "robot_env_path": "/none",
           "here": "/none", "repo": "/none"}
    out = gpu.run(ctx)
    details = out["details"]
    # One row per probe: platform, cuda, tensorrt, nvidia-smi, ollama.
    assert len(details) == 5
    for d in details:
        assert d["status"] in _VALID, d
    # No probe on a plain host may FAIL (FAIL is 'configured-but-broken').
    assert all(d["status"] != "FAIL" for d in details)


def test_module_integrates_with_the_runner() -> None:
    # Through the real isolation runner: valid schema-v1 module result, read-only.
    ctx = {"env": {}, "robot_env": {}, "robot_env_path": "/none", "here": "/none", "repo": "/none"}
    res = core.run_module(gpu, ctx)
    assert res["name"] == "gpu"
    assert res["status"] in _VALID
    assert res["metadata"]["read_only"] is True


def _run_all() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    print("gpu doctor host tests (no hardware):")
    failed = 0
    for t in tests:
        try:
            t()
            print(f"  ok   {t.__name__}")
        except AssertionError as e:
            failed += 1
            print(f"  FAIL {t.__name__}: {e}")
    print(f"\n{len(tests) - failed}/{len(tests)} passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(_run_all())
