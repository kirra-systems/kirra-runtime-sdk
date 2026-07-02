# Doer model scale-up (the M-lane) — detailed scope

Status: **scoping / design** (M-0). Owner: doer / parko. Reviewers: doer + safety.

The Q-lane (Q-0…Q-2, complete and measured — `QUANTIZATION_Q1_SCOPE.md`,
`crates/parko-tensorrt/Q1B_ORIN.md`) built the full per-silicon pipeline:
contract → metric producers → in-Rust PTQ → ONNX/QDQ export → TensorRT INT8 →
precision ladder. Its measured verdict was honest and unflattering to the model:
**the current doer scorer is a toy** — a 4→8→4 MLP over a 4-entry vocabulary —
so INT8 ran *slower* than FP32 (Q/DQ overhead on a net with no compute) and FP16
was a byte-identical no-op. The pipeline is ahead of its payload. The M-lane
closes that gap: a **real-sized doer scorer**, through the SAME pipeline,
re-measured.

---

## 1. What "real-sized" means here (and what it does NOT promise)

**Shape.** Real Hydra-MDP scores a large fixed trajectory vocabulary from an
environment encoding. The M-lane scales our miniature to that shape:

| Axis | Today (`learned.rs`) | M-lane target |
|---|---|---|
| Features | 4 scalars | ~32-dim scene encoding (ego kinematics, goal, corridor geometry, K=4 nearest objects with relative pos/vel) |
| Vocabulary | 4 speeds (or 3×4 maneuvers) | **~256 candidates** (16 lateral offsets × 16 speed profiles, maneuver-shaped materialization) |
| Scorer | 4→8→4, ~100 params | **32→256→256→256, ~140k params** (~0.3 MFLOPs/tick) |
| Training | seeded (1+1)-ES, at construction time | seeded **in-Rust SGD backprop** (mini-batch + momentum), OFFLINE, weights shipped as a versioned artifact |

**The honest INT8 expectation — read before buying.** ~0.3 MFLOPs is real
compute for a scorer but still small for an Orin GPU: a single-inference TRT row
is kernel-launch-dominated, so **INT8 may STILL not beat FP32 on the TRT rows**.
Where INT8 plausibly wins first is the **CPU (OrtBackend) rows** — a millisecond-
scale matmul chain quantizes profitably on CPU — and, later, conv-shaped
perception on DLA (the Taj/PARK detector lane, out of scope here). The contract
measures it either way; the M-lane's value does NOT hinge on the INT8 verdict
flipping:

1. **A non-toy planner.** Richer world encoding + a 256-way vocabulary makes the
   doer itself real — route-around choices come from the net, not from 3
   hand-picked offsets.
2. **Design-note §11's open question, answered at scale.** "Is the 2-D maneuver
   planner more quantization-sensitive?" — a 256-way argmax is where PTQ
   sensitivity actually shows; the Q-1a producers measure it directly.
3. **The pipeline exercised at scale, end-to-end** — PTQ, export, artifacts,
   Orin matrix — on a model whose numbers mean something.

## 2. Phases

- **M-0 — this scope.**
- **M-1 — the model + trainer** (root workspace, `kirra-planner`):
  - `learned_v2`: the scene **feature encoder** (fixed 32-dim, zero-padded object
    slots — absent objects are zeros, fail-safe), the **vocabulary**
    (offset × speed-profile grid; candidates materialized with the existing
    maneuver easing shape so any checker rejection is about the hazard, not the
    geometry), and the **N-layer MLP scorer** (tanh hidden layers, linear head).
  - **In-Rust training**: mini-batch SGD + momentum backprop (no deps, f64
    accumulation, seeded xorshift — same determinism ethos as the ES trainer),
    distilling the SAME `Teacher` signals (`SafetyAware` / `ProgressOnly`) over
    synthetic scenes. A **gradient-check unit test** (finite differences vs.
    backprop on a tiny config) proves the math on CI.
  - **Offline trainer binary** emits a **versioned weights artifact**
    (`artifacts/doer-eval/planner_v2_weights.bin`, ~0.5 MB, magic + version +
    dims header). **Checked in; CI validates the LOADED planner's behavior**
    (admissibility + teacher-agreement floors over the corpus) rather than
    retraining — training in CI would be slow and float-reproducibility across
    architectures is not guaranteed; regeneration is documented and seeded.
    This mirrors the design-note §2 non-goal: the vehicle *loads* a pre-trained
    artifact.
  - `ScoredPlanner` impl → drops into `evaluate_corpus` / the Q-1a harness
    UNCHANGED. `PlanOutput::safe_stop` untouched; the nominal output must stay
    checker-admissible (the doer-checker invariant).
- **M-2 — pipeline regeneration** (both workspaces):
  - Generalize the in-Rust PTQ (`quantize_int8`) from the fixed 2-layer `Mlp` to
    N-layer chains (same per-tensor symmetric int8 + calibration pass).
  - Generalize the ONNX export (`kirra-doer-eval::onnx`) from the fixed 2-layer
    topology to layer chains (fp32 + QDQ; same hand-encoded writer, same
    round-trip verification through real ORT).
  - Regenerate `artifacts/doer-eval/*`: v2 model artifacts + scorecard rows
    (v2 alongside v1 — the v1 rows remain the small-model baseline), drift tests
    extended.
- **M-3 — Orin re-measure**: same `orin_eval` runner over the v2 artifacts
  (plus a CPU-row comparison via OrtBackend, where the INT8 story is expected
  first); results recorded in `Q1B_ORIN.md`-style; the precision-ladder evidence
  for this deployment updated to whatever the contract says.

## 3. Decisions recorded

- **Planner, not detector.** The detector path (`parko-core::detector`, PARK-02a)
  is the *perception* lane: no trajectory ⇒ no admissibility axis ⇒ a different
  eval design. It is the natural home for conv-scale INT8/DLA wins later, but it
  is not "the doer model" this pipeline was built to measure.
- **Weights are an artifact, not code.** Checked in, versioned, behavior-gated
  (load + floors), never retrained in CI. Same artifact discipline as the ONNX
  exports (provenance question from design-note §11 gets its first concrete
  answer: in-repo, drift-tested, regeneration documented).
- **Vocabulary admissibility.** Candidate materialization stays kinematically
  continuous and horizon-capped (`MAX_TRAJECTORY_HORIZON`), like today: the
  checker rejects a candidate for its *hazard*, never its shape. A trained net
  whose nominal pick is inadmissible is caught by the Q-1a admissibility floor.
- **Teachers unchanged.** `SafetyAware` / `ProgressOnly` remain the distillation
  signals — the misalignment-detection story (the harness catches a
  progress-only net) must keep holding at scale.
- **The checker is untouched.** Same non-goal as every phase before it.

## 4. Exit criteria

- **M-1 (CI):** gradient-check passes; the loaded v2 planner meets explicit
  admissibility + teacher-agreement floors on the corpus; the misaligned
  (`ProgressOnly`) v2 net is still caught by both metrics.
- **M-2 (CI):** v2 PTQ + export round-trip green through real ORT (identical
  argmax in-Rust vs QDQ-ONNX); artifacts drift-pinned.
- **M-3 (Orin, strict lane):** the full matrix re-measured on v2; the
  fp32-vs-int8 verdict recorded WHATEVER it is; the deployment ladder updated
  from the evidence.

## 5. Suggested PR sequence

1. This scope doc.
2. M-1: encoder + vocabulary + MLP + backprop trainer + gradient check (no
   artifact yet — the net trains at test time on a REDUCED config to prove the
   loop, minutes-bounded).
3. M-1: the trainer binary + checked-in weights artifact + behavior-gate tests.
4. M-2: PTQ + export generalization + regenerated artifacts.
5. M-3: Orin session + results doc.

## See also

- `QUANTIZATION_DESIGN.md` §11 (the quantization-sensitivity question this
  answers) / §9 (Q-4 FP8/INT4, which waits on this model's headroom data).
- `QUANTIZATION_Q1_SCOPE.md` — the pipeline this feeds, unchanged.
- `crates/kirra-planner/src/learned.rs` — the miniature this scales.
- `crates/parko-tensorrt/Q1B_ORIN.md` — the baseline (v1) measured results.
