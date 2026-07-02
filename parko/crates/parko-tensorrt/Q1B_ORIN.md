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
