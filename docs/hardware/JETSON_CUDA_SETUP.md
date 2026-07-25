# Jetson (Orin NX) CUDA setup — getting the DOERS on the GPU

Companion to the `gpu` diagnostics module (`robot/doctor/modules/gpu.py`). This
is the runbook its WARNs point at. It configures CUDA acceleration for the
components that actually benefit, and is explicit about the one component that
must **not** move to the GPU.

Run every step's verification with:

```bash
robot/kirra_doctor.py --module gpu --verbose
```

## Who benefits from CUDA (and who doesn't)

| Component | GPU? | Why |
|---|---|---|
| **rabbit** | ✅ big win | Its doer LLM is Ollama (`gemma3:4b`). GPU offload is the fix for slow spoken responses. Whisper.cpp STT can use CUDA too. |
| **mick** | ✅ (same path) | `mick_service` calls the same Ollama; mick itself is Rust/CPU. |
| **parko inference** | ✅ real code path | `parko-onnx` `cuda` feature (ORT CUDA EP) + `parko-tensorrt`. **Fail-closed**: a missing GPU/driver makes the CUDA EP registration error, never a silent CPU run. |
| **Taj** | ⚪ little today | Shipped Taj is geometric lidar (CPU). Benefits only with learned perception. |
| **occy** | ⚪ not yet | Today's doer scorer is a tiny MLP; `DOER_MODEL_SCALEUP.md` measured INT8 *slower* than FP32 (no compute to accelerate). Revisit with the real-sized M-lane doer. |
| **kirra (checker)** | 🔴 **stays CPU by design** | The checker is deterministic + WCET-bounded — that is the safety invariant. GPU CUDA is not bitwise-reproducible (`parko-onnx` says so in-code). Moving the checker to CUDA would break the safety argument, not help it. **Do not.** |

So the goal is: **doers on the GPU, checker on the CPU.** The highest-leverage,
lowest-risk first target is Ollama (rabbit + mick).

> ⚠️ These steps run on the Orin, not in CI. Treat any timing as indicative until
> measured on the target — the same rule as the WCET methodology.

## Step 0 — baseline: what does the box already have?

On a Jetson, "installing CUDA" means **JetPack** (the L4T BSP), not a desktop
CUDA installer. First see what's present:

```bash
robot/kirra_doctor.py --module gpu --verbose      # platform + cuda + tensorrt + ollama offload
cat /etc/nv_tegra_release                          # L4T / JetPack release line
dpkg -l | grep -i nvidia-jetpack                   # the JetPack meta-package
```

If `platform` doesn't report a Jetson, you're on the wrong host.

## Step 1 — CUDA + TensorRT runtime (parko inference)

If the `cuda runtime` check WARNs (no `libcudart`), install the JetPack runtime:

```bash
sudo apt update
sudo apt install nvidia-jetpack        # pulls CUDA, cuDNN, TensorRT for this L4T
```

Re-run the doctor: `cuda runtime` and `tensorrt` should go PASS with versions.
(You do **not** need `nvcc` at runtime — the runtime `.so`s are enough; `nvcc` is
only for building.)

## Step 2 — Ollama on the GPU (the rabbit + mick win)

The `ollama GPU offload` check is the load-bearing one: Ollama **silently falls
back to CPU** if it can't find a CUDA runtime, and a model resident on the CPU is
the usual cause of a sluggish rabbit — the check reports it as `0 B in VRAM`.

1. Use a CUDA-enabled Ollama build for arm64/Jetson (the official install script
   detects the Tegra GPU; if you containerize, use a `jetson-containers` Ollama
   image). After install, warm the model and check offload:
   ```bash
   ollama run "$KIRRA_RABBIT_MODEL" "hi" >/dev/null      # load it
   curl -s localhost:11434/api/ps | python3 -m json.tool  # size_vram > 0 == on GPU
   robot/kirra_doctor.py --module gpu --verbose
   ```
   `ollama GPU offload` → **PASS "fully on GPU"** is the target. If it stays on
   CPU, check `journalctl -u ollama | grep -i gpu` for "no compatible GPUs" (a
   CUDA-runtime / build problem, back to Step 1).

2. **Keep it resident** (removes the cold-reload stall on the first utterance
   after idle) — set a long/pinned `keep_alive` on the Ollama service, e.g.
   `Environment=OLLAMA_KEEP_ALIVE=-1`. This pairs with the rabbit responsiveness
   work (streaming to TTS + a `num_predict` cap).

3. If offload is only **partial** (some layers on CPU), the model doesn't fully
   fit VRAM — use a smaller / more-quantized `KIRRA_RABBIT_MODEL` (and re-run the
   `rabbit_model_smoketest.py` + re-pin the digest before trusting the swap).

## Step 3 — Whisper.cpp STT on the GPU (optional)

If STT latency matters, build whisper.cpp with CUDA (`GGML_CUDA=1`) so the
transcription step uses the iGPU too. Verify by timing a canned clip through the
PTT path. Piper TTS stays CPU (fine — it's cheap).

## Step 4 — parko inference backends

Build the parko inference crate with the GPU backend once CUDA is present:

- `parko-onnx` with `--features cuda` → the ORT CUDA EP (`build_cuda_session`,
  registered `error_on_failure` — **fail-closed**, no silent CPU fallback).
- `parko-tensorrt` for the TensorRT path (needs the `tensorrt` check PASS).

These are opt-in; the CPU backends remain the default and the checker path is
unaffected. See `parko/QNX_BACKEND_SELECTION.md` and `parko/crates/parko-tensorrt/Q1B_ORIN.md`.

## Step 5 — confirm, and leave the checker alone

```bash
robot/kirra_doctor.py --module gpu --verbose      # doers green
```

Do **not** add a GPU execution path to the KIRRA checker / governor / fence. Its
determinism and WCET bound are the safety case; keep it on the CPU.
