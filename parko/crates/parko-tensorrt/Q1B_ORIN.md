# Q-1b on the Jetson Orin — run commands

The on-target half of Q-1 (`parko/QUANTIZATION_Q1_SCOPE.md` §2, Q-1b): run the
doer performance-contract matrix (FP32 / FP16 / INT8-QDQ) on the Orin. Everything
below self-skips off-Jetson; `PARKO_TRT_REQUIRE_EP=1` makes a skip a hard failure
(use it on the Orin so nothing passes silently).

## 0. One-time setup on the Orin

Same as the existing PARK-021 probes: a TensorRT-enabled ONNX Runtime (JetPack 6.x,
e.g. `onnxruntime-gpu` 1.23.x in a venv), and export its dylib:

```bash
export ORT_DYLIB_PATH=<venv>/lib/python3.*/site-packages/onnxruntime/capi/libonnxruntime.so.1.23.0
```

## 1. Artifacts (already checked in — regenerate only if the model changed)

The FP32 + INT8-QDQ planner models and the scorecard live at
`artifacts/doer-eval/` (repo root), produced by the ROOT workspace and pinned by
the `artifact_drift` test:

```bash
# only needed after changing the scorer / PTQ / corpus:
cargo run -p kirra-doer-eval --example export_artifacts
```

The QDQ model's `QuantizeLinear`/`DequantizeLinear` nodes carry the in-Rust PTQ
calibration (design note §6: one calibration, every backend) — TensorRT INT8
needs **no separate calibration table**.

## 2. Validate the INT8 engine builds (the probe)

```bash
cd parko
PARKO_TRT_REQUIRE_EP=1 \
  cargo test -p parko-tensorrt --test int8_qdq_probe -- --nocapture
```

Expected: `INT8-QDQ PROBE PASSED — engine built (...ms), scores [...]`.

## 3. Run the contract matrix

```bash
cd parko
PARKO_TRT_REQUIRE_EP=1 \
  cargo run --release -p parko-tensorrt --example orin_eval
```

Prints one row per precision: engine-build time, p50/p99/max latency, engine SHA,
and the contract PASS/FAIL for the fp32 and int8-qdq rows (quality/admissibility
joined from the scorecard; FP16 is latency-only/informational for now).

Knobs (env): `KIRRA_EVAL_ITERS` (default 1000), `KIRRA_EVAL_WARMUP` (100),
`KIRRA_P99_BUDGET_NS` (default = the documented `PerfContract::illustrative()`
placeholder — real per-class budgets are design-note §11), `KIRRA_DOER_ARTIFACTS`
(artifact dir override).

## 4. Optionally: round-trip the export through the Orin's ORT

```bash
# root workspace — verifies the exported bytes against the Rust scorer:
KIRRA_DOER_EVAL_REQUIRE_ORT=1 cargo test -p kirra-doer-eval --test onnx_roundtrip
```

## Honesty notes

- Latency here is **on-target-indicative**, not a WCET/FTTI claim (see
  `docs/safety/WCET_MEASUREMENT_METHODOLOGY.md` — host/target discipline).
- The contract thresholds are the **illustrative placeholder** until per-class
  numbers land (design note §11).
- All of this tunes the **untrusted doer**; the checker is unchanged and bounds
  the INT8 planner's proposals exactly as it bounds FP32 ones.

---

## Measured results — Jetson Orin NX 16GB (2026-07-02)

Q-1b exit criteria: **MET**. Bench: Orin NX 16GB, JetPack 6 (`jp6/cu126`),
onnxruntime-gpu **1.23.0** (venv, `load-dynamic`), `ort` rc.11, strict lane
(`PARKO_TRT_REQUIRE_EP=1`), artifacts as checked in at `artifacts/doer-eval/`.

### Probe (step 2)

`INT8-QDQ PROBE PASSED` — cold INT8 engine build **48 ms**, engine SHA
`2fbc1a6f…9052e5f1`, scores `[0.0448861, -0.4265421, -1.3937072, -3.0795364]`,
argmax `0`, stable across runs.

### Contract matrix (step 3; iters=1000, warmup=100)

| row | engine build | p50 | p99 | max | contract |
|---|---|---|---|---|---|
| fp32 | 19 ms | 97 µs | 115 µs | 157 µs | **PASS** (quality 1.000, admissibility 1.000) |
| fp16 | 1 ms | 97 µs | 134 µs | 175 µs | informational (latency-only by design) |
| int8-qdq | 1 ms | 146 µs | 163 µs | 185 µs | **PASS** (quality 1.000, admissibility 1.000) |

All rows are ~60–90× under the placeholder 10 ms p99 budget
(`PerfContract::illustrative()` — real per-class budgets are design-note §11).

### Findings

1. **The calibration is consistent across silicon (design note §6, measured).**
   The TensorRT INT8 engine's scores agree with the same QDQ artifact run
   through CPU ONNX Runtime to ~1e-7, identical argmax — one calibration
   artifact, two very different backends, the same decision. Engine SHA matched
   between the probe and the eval run (deterministic build).
2. **FP16 is a no-op at this model size.** The fp16 row produced a
   byte-identical engine to fp32 (same SHA): TensorRT judged reduced precision
   pointless for a 4→8→4 MLP. Expected; recorded so nobody reads the fp16 row
   as a win.
3. **INT8 is SLOWER than FP32 on this model (146 µs vs 97 µs p50)** — the Q/DQ
   overhead dominates a net with almost no compute. This is the contract
   framework doing its job: "closed the gap" is measured, not assumed, and for
   THIS model on THIS chip the selection verdict is **FP32**. INT8 earns its row
   when a real-sized net goes through the same pipeline (Q-2+ / larger doer).

Honesty: all latency figures are on-target-indicative (pinning/isolation not
controlled), NOT a WCET/FTTI claim.
