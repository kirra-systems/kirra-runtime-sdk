# ADR-0009: parko-tensorrt precision strategy — A1 (ort TRT EP) with A2 (native nvinfer) escalation

| Field | Value |
|---|---|
| Status | **Proposed** |
| Date | 2026-06-19 |
| Deciders | Project owner (pending) |
| Issues | #415 (remaining PARK-021 jetson-gated items), #414 (on-hardware validation, closed) |
| Code | `parko/crates/parko-tensorrt/src/lib.rs` (`Tf32Control`, `TrtPosture`, `park021_jetson_gated` #3) |
| Evidence | `tests/tf32_probe.rs`, `tests/equivalence_probe.rs` (Jetson Orin NX, JP6.2, ORT 1.23.0) |

## Context

`parko-tensorrt` runs inference on the Jetson via ort's **TensorRT execution
provider** (the A1 design: reuse the shared `OrtRunCore` path, differ only in the
EP/precision config). The safety-relevant precision question (PARK-021 jetson-gated
item #3): can we **guarantee fp32** for the governed decision, i.e. prevent TF32
from silently dropping mantissa bits on the Orin's Ampere (sm87) GPU?

Two facts constrain the answer:

1. **ort's TRT EP exposes no TF32 knob.** It offers `with_fp16` / `with_int8` only
   (confirmed in ort rc.11). `TrtPosture::full_precision_guaranteed()` therefore
   honestly returns `false` while TF32 is unenforceable — the backend does not claim
   full precision it cannot deliver.

2. **The out-of-band lever is empirically inert.** On the Orin (JP6.2, ORT 1.23.0,
   MNIST), `tests/tf32_probe.rs` — a one-shot differential that runs default-TF32 vs
   a child process with `NVIDIA_TF32_OVERRIDE=0` — measured **zero** change. And
   `tests/equivalence_probe.rs` measured TRT-vs-CPU max per-logit drift **2.98e-7**
   (~2.5 ULP, fp32-epsilon scale, ~3000× below TF32 ε ≈ 1e-3), i.e. **TF32 is not
   engaged for this model**. The override being a no-op is incidental — it is *not* a
   working TF32 control. TensorRT's TF32 is governed by its own build-time
   `BuilderFlag::kTF32` (default ON), which the cuBLAS/cuDNN env override does not
   reach.

Implication: for a *small* model TRT happens to run true fp32, so a **measured
decision-agreement** argument suffices. But a production-representative network with
large fp32 matmuls may have TRT select TF32 kernels, and **no lever in the A1 (ort
TRT EP) path can force them off**. If the safety case ever needs an *asserted* fp32
guarantee (not just measured agreement), A1 cannot provide it.

## Decision

Adopt a **two-stage, data-gated precision strategy**; do **not** build A2
speculatively.

- **A1 (current, default): ort TRT EP, full-precision config, TF32 UNENFORCED.**
  The safety argument rests on the **measured decision-agreement bound** (ADR
  context / #415 item #4): A1 is acceptable for a given model **iff** the
  TRT-vs-CPU-baseline decision agrees with margin on that model, demonstrating TF32
  (if engaged) does not move the governed decision. `full_precision_guaranteed()`
  stays `false`.

- **A2 (escalation): native `nvinfer` FFI backend that sets
  `BuilderFlag::kTF32 = false` at engine build** — the only reliable fp32 control.
  Adopt A2 **only when triggered**.

- **A2 trigger** (either condition):
  1. On the production-representative model, the decision-agreement probe shows
     TF32-scale drift (~1e-3) that threatens the decision margin; or
  2. the safety argument requires a *positive* fp32 guarantee rather than a
     measured-agreement argument.

## Consequences

- **A1 keeps the single-source ort path** (no FFI, no `unsafe`, reuses `OrtRunCore`)
  and is the right default while measured agreement holds.
- **A2 costs more:** a native `nvinfer` C++ FFI boundary (`unsafe`, TRT version
  coupling, more maintenance) in exchange for direct `kTF32`/precision-flag control.
- **The decision is evidence-driven:** the trigger is a measurement on the real
  model, not a guess. Until then the probes (`tf32_probe`, `equivalence_probe`) are
  the standing evidence, and `full_precision_guaranteed()` remains honestly `false`.

## Open questions (for the deciding session)

- The production-representative model + input distribution to run the trigger
  measurement against (MNIST is too small to exercise TF32 kernels).
- The numeric decision-agreement margin that distinguishes "A1 acceptable" from
  "A2 required" — seed from #415 item #4 (currently ~1e-4 proposed, ~300× over the
  observed fp32 floor).
