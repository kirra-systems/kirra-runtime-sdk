# Doer quantization & per-silicon inference — design note (Tier D)

Status: **scoping / design** (not implementation). Elaborates Tier D of
`BUILD_TUNING.md` — the large per-silicon win on the doer that is *not* a build
flag. Owner: doer / parko. Reviewers: doer + safety.

---

## 1. Why (and why it's safe to be aggressive)

The doer (`parko-*`: learned planners + perception inference) is the one place in
the system where chipset-specific optimization has real leverage — and it is the
place where it is **safe** to chase it, because the doer is **bounded by the KIRRA
checker**. A quantized model can only make the doer's *proposals* worse (lower
plan quality, more conservative), never unsafe: an out-of-envelope proposal is
clamped or MRC'd by the checker exactly as an FP32 one would be. So quantization
error is a **quality** knob, not a **safety** knob. That asymmetry is the whole
license for this work — and the load-bearing invariant of this note.

Two things make our models unusually quantization-friendly:

- They are **small and bounded**. The learned planners score a **fixed trajectory
  vocabulary** with a compact MLP distilled from a `Teacher` (`SafetyAware` /
  `ProgressOnly`) — not a 10B-param end-to-end driving transformer. INT8 on a
  ranking MLP costs accuracy the checker already tolerates.
- The output is a **ranking**, not a precise regression. The doer picks the
  best-scoring vocabulary entry; small per-score perturbations rarely change the
  argmax, and when they do, the alternative is still a checker-admissible member
  of the same vocabulary.

## 2. Goals / non-goals

**Goals**
- Hit each target chip's loop-rate deadline with a checker-admissible plan, at the
  lowest precision that holds plan quality (FP16 / INT8, later FP8 / INT4).
- One model definition → per-backend compiled/quantized artifacts, selected at
  runtime by silicon.
- A **doer performance contract** that turns "closed the gap on chip X" into a
  CI pass/fail, not a FLOPS race.

**Non-goals**
- **No change to the checker.** The kinematic contract / `validate_vehicle_command`
  and the release seam are untouched. This note lives entirely on the untrusted side.
- **No new backend crate.** Uses the existing `parko-onnx` / `parko-openvino` /
  `parko-tensorrt` + `BackendSelector`.
- **No online / on-vehicle retraining or calibration.** Calibration is an offline,
  reproducible build step; the vehicle only *loads* a pre-quantized artifact.
- Not a training-accuracy project — we quantize an already-trained/distilled net.

## 3. What already exists (anchor points)

`crates/parko-core/src/backend.rs` + `crates/parko-core/src/backend_selector.rs`
already provide most of the plumbing — this note fills the gaps, it does not
rebuild the frame:

| Exists today | Role for quantization |
|---|---|
| `PrecisionMode { FP32, FP16, INT8 }` (`#[non_exhaustive]`) | The precision ladder. Extend with `FP8` / `INT4` later (non_exhaustive = additive). |
| `BackendCapabilities { supports_int8, supports_fp16, .. }` | Per-backend capability negotiation — the input to precision policy. |
| `ModelHandle { expected_precision, input_shapes, output_shapes }` | A loaded model already declares its precision. |
| `BackendDescriptor::{Cpu, Cuda, TensorRT, QualcommQnn, TiTidl, IntelOpenVino, AmdVitis}` | The per-silicon dispatch key (PARK-020/027/028/029/030). |
| `InferenceBackend::warm_up` (fail-closed) | Where engine build / precision compile happens; a failed build ⇒ node refuses to start. TensorRT already builds its engine here. |
| `BackendSelector` (PARK-022) | Resolves a descriptor → concrete backend; the natural home for precision-aware selection. |
| `InferenceThreads::bitwise_reproducible` | Determinism knob — relevant to the eval harness's reproducibility. |

**Gaps to fill (this note's scope):** (a) an offline **calibration/quantization
build step** that produces the INT8 artifact + calibration table; (b) a
**precision-selection policy** (caps × quality budget → `PrecisionMode` with a
fallback ladder); (c) a **plan-quality guardrail** that proves a quantized model
stays checker-admissible; (d) the **performance contract**; (e) the **eval harness**.

## 4. The doer performance contract (the acceptance gate)

Don't chase FLOPS parity with the premium chip; define a contract and make
"closed the gap on chip X" = **meets the contract on X**. Per deployment class:

- **Latency** — p99 of one planning tick ≤ the slow-loop budget; fast-loop path
  (if used) ≤ its budget. Measured on-target, pinned, warmed.
- **Plan quality** — a scalar vs. the `SafetyAware` teacher: e.g. progress ratio
  and/or vocabulary-argmax agreement rate, on a held-out scenario set.
- **Admissibility** — fraction of proposed plans the checker accepts without MRC
  must not regress beyond a small budget vs. the FP32 reference.

A (chip, backend, precision) row **passes** iff it meets all three. This is the
CI artifact that replaces subjective "is it fast enough?".

## 5. Precision ladder & selection policy

Offline we produce, per model, the artifacts a target can use; at runtime the
selector picks the lowest precision that (a) the backend supports and (b) passed
the performance contract for that chip.

```
FP32  (reference / correctness oracle)
  └─ FP16  (near-lossless; default where supported)
       └─ INT8 (PTQ w/ calibration; QAT only if PTQ misses the quality gate)
            └─ FP8 / INT4  (later; behind the non_exhaustive PrecisionMode)
```

- **PTQ first** (post-training quantization + a calibration set). Cheap; usually
  enough for a ranking MLP.
- **QAT only if** PTQ fails the §4 quality gate for a given model — heavier, needs
  a training loop, tracked as a separate spike.
- **Selection** extends `BackendSelector`: given the resolved `BackendDescriptor`,
  its `BackendCapabilities`, and a per-chip **precision allow-list** (the rows that
  passed the contract), choose the artifact. If no artifact meets the contract on a
  chip, gracefully **degrade** to the highest precision that runs (worse latency,
  never worse safety) and log it — the doer keeps running and the checker still
  bounds the output. (Degradation, not "fail-closed": the checker, not the selector,
  is the fail-closed safety authority — the doer is availability-preserving here.)

## 6. Portable IR → per-backend compile

One exported **ONNX** graph per model is the portable IR. Each backend compiles /
quantizes it its own way, in `warm_up` (fail-closed, cached):

| Chip | `BackendDescriptor` | Backend | Precision path |
|---|---|---|---|
| NVIDIA DRIVE / Jetson | `TensorRT` / `Cuda` | parko-tensorrt / parko-onnx | TensorRT EP, FP16/INT8 (DLA offload) |
| Intel CPU/iGPU/NPU | `IntelOpenVino` | parko-openvino | OpenVINO FP16/INT8 |
| Qualcomm | `QualcommQnn` | (QNN backend, PARK-027) | Hexagon INT8 |
| TI | `TiTidl` | (TIDL, PARK-028) | INT8 |
| AMD | `AmdVitis` | (Vitis, PARK-030) | INT8 |
| Portable CPU | `Cpu` | parko-onnx | FP32/FP16 |

The calibration table is produced once (offline) and reused by every INT8 backend
so the quantization is consistent across silicon.

## 7. Safety argument (the part that must be airtight)

1. **The checker is unchanged and remains the sole safety authority.** Nothing in
   this note touches `validate_vehicle_command`, the kinematic contract, or the
   release seam.
2. **Quantization is monotone-in-safety-relaxation = 0.** It can change *which*
   admissible plan is chosen, or make the doer more conservative, or (worst case)
   produce a worse plan — all of which the checker still bounds. It cannot widen
   the envelope.
3. **A quantized-model failure is fail-safe.** If a chip can't run any artifact
   that meets the contract, the doer runs slower / more conservatively or the
   fallback kicks in; the checker clamps/MRCs exactly as before. There is no path
   where quantization error → unsafe actuation.
4. **Determinism where it's claimed.** Any reproducibility claim rides on
   `InferenceThreads::bitwise_reproducible`; INT8 kernels are generally *not*
   bitwise-portable across silicon, so the eval harness compares **quality**, not
   bit-equality, across backends.

This is why the doer, not the checker, is the correct home for chipset-specific
code — the split the whole system is built on.

## 8. Eval harness

A `(chip, backend, precision, model)` matrix runner that reports, per row:
`p50/p99 latency`, the §4 quality metric, and the admissibility rate — vs. the
FP32 reference. Output is the pass/fail contract table. Hardware/CI-gated like the
existing backend crates (runtime-linked; a row is `SKIPPED` where the silicon/SDK
is absent, never silently passed). Reuses the existing bench scaffolding.

## 9. Phasing

- **Q-0 — reference + contract.** Land the performance-contract definition + the
  eval harness with the FP32 reference and FP16 (near-lossless) rows. No new
  precision yet; establishes the measuring stick.
- **Q-1 — PTQ INT8 (one backend).** Calibration build step + INT8 on the
  best-supported backend (TensorRT or OpenVINO). Prove the quality gate holds.
- **Q-2 — precision-aware selection.** Extend `BackendSelector` with the per-chip
  allow-list + fail-closed fallback.
- **Q-3 — fan out** INT8 to QNN / TIDL / Vitis as those backends land
  (PARK-027/028/030).
- **Q-4 — FP8/INT4** (extend `PrecisionMode`) where hardware supports it, if the
  contract shows headroom.
- **QAT** — only spun up for a specific model that fails PTQ's quality gate.

## 10. Risks & mitigations

| Risk | Mitigation |
|---|---|
| INT8 drops plan quality below the contract | PTQ→QAT ladder; per-model gate; fall back to FP16, never ship a failing row. |
| Calibration set unrepresentative → skewed quantization | Calibration drawn from the same distribution as the eval scenarios; treat it as a versioned artifact. |
| Cross-silicon numeric divergence read as a "bug" | Harness compares quality, not bits; document that INT8 kernels aren't bitwise-portable. |
| Engine-build latency at startup | Already handled by the fail-closed `warm_up` (build+cache before serving). |
| Scope creep into the checker | Hard non-goal (§2); the checker is untouched and reviewed separately. |

## 11. Open questions

- Which deployment class(es) set the *first* contract numbers (courier / delivery-av
  / robotaxi)?
- Is `LearnedManeuverPlanner` (2-D vocabulary) more quantization-sensitive than the
  speed-only planner? (Likely yes — argmax over a bigger space; may need QAT first.)
- Do we want a fast-loop/slow-loop *split* precision (INT8 fast-loop, FP16
  slow-loop), or one precision per model?
- Calibration-artifact provenance: where stored, how versioned/signed alongside the
  model?

## See also

- `BUILD_TUNING.md` — Tiers A–C (build-flag wins) that this Tier-D note extends.
- `crates/parko-core/src/backend.rs`, `crates/parko-core/src/backend_selector.rs` — the backend abstraction anchored above.
- `../docs/PERFORMANCE_BUILD_TUNING.md` — the control-plane (SDK) tuning counterpart.
