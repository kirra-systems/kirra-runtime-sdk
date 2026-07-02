# Q-1 — PTQ INT8 on one backend + the metric producers (detailed scope)

Status: **COMPLETE — measured on target** (Q-1a: #755/#756; Q-1b: #757; Orin
exit criteria met 2026-07-02, results in
`crates/parko-tensorrt/Q1B_ORIN.md` §"Measured results"). Elaborates phase **Q-1** of
`QUANTIZATION_DESIGN.md` §9. Q-0 landed the measuring stick
(`parko_core::perf_contract` — the contract + latency harness, with `quality`
and `admissibility` as *inputs*). Q-1 turns those two inputs into **real
producers** and adds the first sub-FP32 precision row. Owner: doer / parko.
Reviewers: doer + safety.

---

## 1. The reframing (read first)

The one-line Q-1 in the design note — *"PTQ INT8 on one backend; prove the
quality gate holds"* — hides two facts the code makes concrete:

**(a) The doer "model" is a native-Rust MLP, not an ONNX artifact.** The learned
planner is `Mlp<const M>` in `crates/kirra-planner/src/learned.rs` — a compact
2-layer tanh net scoring a fixed trajectory vocabulary (4 target speeds for
`LearnedPlanner`; 12 = 3 lateral offsets × 4 speeds for `LearnedManeuverPlanner`),
with hand-fit / seeded-ES weights. The parko backends (`OrtBackend`, `OvBackend`,
`TrtBackend`) load **ONNX files**. So "INT8 the model on a backend" presupposes an
**ONNX export of the planner that does not exist today**.

**(b) The two eval scalars live in a different workspace from the backend.** This
is the load-bearing structural fact for Q-1:

| Scalar | Where it is *produced* | Depends on |
|---|---|---|
| **admissibility** | **root** workspace (`crates/*`) | `LearnedPlanner::plan` → `validate_trajectory_slow` → `TrajectoryVerdict` |
| **plan quality** | **root** workspace (`crates/*`) | `LearnedPlanner` argmax vs. FP32/`Teacher` reference |
| **latency / precision** (the `EvalRow`) | **parko** workspace (separate `Cargo.toml`) | `InferenceBackend::run` on an ONNX model |

`parko-core` does **not** depend on `crates/kirra-planner`, and the root workspace
does not depend on parko. The quality/admissibility of the *doer's plans* is
inherently a root-side (planner + checker) computation; parko's
`perf_contract::EvalRow` is inherently a backend (latency + precision)
computation. **Q-1 must define the seam between them, not assume one.**

The consequence: the highest-value, fully-CI-testable part of Q-1 lives **entirely
in the root workspace** and needs no backend or hardware at all. That is Q-1a.

---

## 2. Decomposition: Q-1a (CI) and Q-1b (Orin-gated)

### Q-1a — the metric producers + a real in-Rust PTQ (no hardware, lands on CI)

The bulk of Q-1's value. Entirely root-workspace; deterministic; CI-green.

1. **Admissibility producer.** Generalize the ready-made test seam
   `crates/kirra-planner/tests/learned_doer_bounded_by_kirra.rs:58-64`
   (`kirra_verdict` → `admitted`) into a reusable function that runs a
   `PlanOutput` through `validate_trajectory_slow` and reduces the verdict, then
   an aggregator over a scenario set (the rate). `MickEvalSummary::acceptance_rate()`
   (`crates/kirra-planner/src/mick_capture.rs`) is essentially this already, scoped
   to Mick intents — the Q-1 producer is the un-scoped generalization.

2. **Plan-quality producer.** The net-new piece. Headline metric =
   **argmax-agreement rate** vs. the FP32 reference (see §3). Building blocks exist
   but are private in `learned.rs`: `progress_of()`, `chosen_index()` /
   `chosen_candidate()`, `teacher()`. Q-1a either lives in that module or exposes a
   minimal `pub` surface for them.

3. **A real in-Rust PTQ of the native MLP.** `Mlp<M>` is small enough to quantize
   *for real*, deterministically, in Rust: per-tensor int8 weights/activations with
   a scale derived from a calibration pass over the scenario corpus. This yields an
   **actual quantized-vs-FP32 comparison** of both metrics — on the real doer model,
   on CI, with **no external backend** — proving the whole quality-gate machinery
   end-to-end before any ONNX/hardware exists. This is a legitimate PTQ, not a mock;
   it also directly answers design-note §11's "is the 2-D maneuver planner more
   quantization-sensitive?" (run the same PTQ on both planners, compare argmax drift).

**Q-1a exit criteria (all on CI):**
- A `QuantEvalSummary` (root-side) reporting, for FP32-ref vs int8-PTQ over the
  corpus: argmax-agreement rate, admissibility rate (both `Accept|Clamp` and
  strict-`Accept`), mean progress ratio.
- A test proving the int8-PTQ planner stays **checker-admissible** at a rate within
  a budget of FP32 (the §4-quality-gate analogue, on the native model).
- A test proving argmax-agreement ≥ a floor on the corpus.
- No change to the checker; no `parko` dependency added to the root workspace.

### Q-1b — real TensorRT INT8 on the Orin (hardware-gated, `SKIPPED` in CI)

Runnable on your Jetson Orin NX; skipped where the GPU/EP is absent.

4. **ONNX export of the planner MLP.** A small offline step (Rust `Mlp<M>` →
   ONNX graph) so a backend can load it. Versioned artifact; the calibration table
   is produced once and reused (design-note §6).

5. **TensorRT INT8 path.** `TrtBackend` already threads a precision posture
   (`TrtPosture { int8, fp16 }`, pinned `false`) and already implements the real
   `warm_up` engine-build/cache hook (`parko/crates/parko-tensorrt/src/lib.rs`). Q-1b:
   - add an INT8 posture + calibration-table load into `warm_up`
     (the fail-closed build hook is the design-note §3/§6 plug-in point);
   - set `BackendCapabilities.supports_int8 = true` only where measured true;
   - assemble a real `EvalRow` (latency from `perf_contract::run_latency`;
     quality/admissibility from the Q-1a producers fed the exported model's outputs).

6. **Skip lane.** Reuse the existing idiom: probe tests self-skip via
   `eprintln!("SKIP: …")` + early return, and hard-fail under
   `PARKO_TRT_REQUIRE_EP=1` (see `parko/crates/parko-tensorrt/tests/*_probe.rs`). A Q-1b row is
   `SKIPPED` where the silicon/SDK is absent — **never silently passed**.

**Q-1b exit criteria (on the Orin, strict lane):** ✅ **MET 2026-07-02** — see
`crates/parko-tensorrt/Q1B_ORIN.md` §"Measured results" (fp32 + int8-qdq PASS,
fp16 latency-only; INT8 zero admissibility regression; notable finding: INT8 is
*slower* than FP32 at this model size — the contract measured it, selection
verdict for this model on Orin is FP32).
- FP32, FP16, and INT8 rows for the exported planner, each meeting or explicitly
  failing the §4 contract; INT8 within the admissibility-regression budget of FP32.
- Cross-precision comparison is **quality-based, not bit-based** (design-note §7.4:
  INT8 kernels are not bitwise-portable; `InferenceThreads::bitwise_reproducible`
  is FP-path only).

---

## 3. Metric definitions (the two calls Q-1 must nail)

**Admissibility = "accepted without an MRC" = `Accept | Clamp`.** A `Clamp` is a
per-pose kinematic derate, not a maximal-risk-condition maneuver; the doer's
proposal was still *admitted*. This matches the existing `admitted()` seam and
`MickEvalSummary`. The harness additionally reports the **strict-`Accept`** rate
(no derate) as a finer quality signal. `MRCFallback` and `Pending` are refusals.

**Plan quality (headline) = argmax-agreement rate vs. the FP32 reference.** The
design note §1 argues the doer output is a *ranking*, so the safety-relevant
question is "does quantization change which vocabulary entry is chosen?" —
argmax-agreement measures exactly that. **Secondary:** mean progress ratio
(`progress_of`), to catch a case where the argmax shifts to a still-admissible but
lower-progress entry. Both are higher-is-better and feed `EvalRow.quality` (the
harness reduces to a single scalar; the components are reported alongside).

Rationale for argmax-agreement over a regression error: a small per-score
perturbation that doesn't move the argmax is a **non-event** for a ranking doer
(and the alt is a checker-admissible member of the same vocabulary anyway), so an
MSE-style metric would over-penalize harmless perturbations.

---

## 4. The cross-workspace seam

Q-1a produces the scalars root-side; parko's `EvalRow` needs them parko-side.
Q-1 picks one of:

- **(A) Scorecard file (recommended).** The root-side `QuantEvalSummary` emits a
  small versioned JSON scorecard (quality + admissibility per precision); a
  top-level Q-1b runner reads it and joins it with the parko-side latency row into
  the final `EvalRow`. Keeps the two workspaces decoupled (no new cross-workspace
  Cargo dependency), matches how the mick eval already writes/reads a JSONL log.
- **(B) Shared lean crate.** Factor the metric *types* into a no-dep crate both
  workspaces depend on. Heavier; only worth it if the scalars need to flow live
  rather than as an offline artifact. Q-1 does not need live flow → defer.

Recommendation: **(A)**. The eval is an offline, reproducible build step
(design-note §2 non-goal: no on-vehicle calibration), so a file seam is the right
coupling.

---

## 5. Corpus

Reuse the deterministic scenario worlds that exist today:
`crates/kirra-mick/tests/scenario_suite.rs` (grounded-then-checked scenarios) and
the hand-built `world()` fixtures in the `kirra-planner` tests. Design-note §11
already flags "need a held-out set" — Q-1 **inherits** that gap, it does not create
it; the in-Rust PTQ is validated on the same corpus it calibrates on for Q-1a, with
a `TODO` to split a held-out set before any quality number is treated as a release
gate. Call this out explicitly rather than letting a train==test number read as a
guarantee.

## 6. Non-goals (unchanged from the design note)

- **No change to the checker.** `validate_vehicle_command`, the kinematic contract,
  the release seam — untouched. Q-1 lives entirely on the untrusted doer side.
- **No new backend crate.** Q-1b extends the existing `parko-tensorrt`.
- **No QAT.** PTQ only; QAT is spun up only if a specific model fails the PTQ
  quality gate (design-note §9), tracked separately.
- **No on-vehicle calibration.** Calibration is an offline, reproducible step; the
  Orin only *loads* the pre-quantized artifact.

## 7. Safety argument (inherited, restated for Q-1)

Quantization is a **quality** knob, not a **safety** knob: a worse int8 plan is
still bounded by the unchanged checker (clamped or MRC'd exactly as an FP32 plan
would be), and cannot widen the envelope. The in-Rust PTQ (Q-1a) and the TensorRT
INT8 (Q-1b) can only change *which* admissible vocabulary entry is chosen or make
the doer more conservative — never produce unsafe actuation. A chip that can't run
any int8 artifact meeting the contract **degrades** to a higher precision (worse
latency, never worse safety); the checker remains the sole fail-closed authority.

## 8. Suggested PR sequence

1. **This scope doc.**
2. **Q-1a metric producers** (root workspace): admissibility + quality producers +
   `QuantEvalSummary`, tests against the existing corpus. No PTQ yet.
3. **Q-1a in-Rust PTQ**: int8 quantization of `Mlp<M>` + the FP32-vs-int8
   comparison tests + the scorecard emitter (seam A).
4. **Q-1b TensorRT INT8** (parko workspace): ONNX export + calibration + INT8
   `warm_up` path + `EvalRow` assembly, behind the `PARKO_TRT_REQUIRE_EP` skip lane.
   Validated on the Orin, skipped in CI.

## See also

- `QUANTIZATION_DESIGN.md` — the Tier-D overview this elaborates (esp. §4 contract,
  §5 precision ladder, §6 IR→compile, §9 phasing).
- `parko/crates/parko-core/src/perf_contract.rs` — the Q-0 measuring stick Q-1 feeds.
- `crates/kirra-planner/src/learned.rs` — the MLP + vocabulary Q-1a quantizes.
- `crates/kirra-planner/tests/learned_doer_bounded_by_kirra.rs` — the admissibility
  seam Q-1a generalizes.
- `parko/crates/parko-tensorrt/src/lib.rs` — the `warm_up` + `TrtPosture` Q-1b extends.
