# Doer (parko) build tuning

Per-chipset performance for the **doer** — the untrusted planning/inference side
(`parko-*`). The doer is the sanctioned place to chase chipset-specific speed:
it is bounded by the KIRRA checker, so a wrong or slow output is clamped, never
actuated unbounded. The **safety kernel** (the SDK workspace) is the opposite —
it stays uniform and keeps its own fixed-flag QNX build; never apply any of this
to it. See `../docs/PERFORMANCE_BUILD_TUNING.md` for the control-plane side.

Tiers, cheapest first.

## Tier A — the `dist` profile (source-identical, portable)

parko's `[profile.release]` is already aggressively tuned (`opt-level = 3`, fat
LTO, `codegen-units = 1`). `[profile.dist]` inherits it and adds a symbol strip,
mirroring the SDK so the shipping command is the same in both workspaces:

```bash
cd parko
cargo build --profile dist -p parko-ros2   # the doer node binary
```

Chipset-agnostic; keep it in the portable build.

## Tier B — `target-cpu` (opt-in, per deployment)

Set at the build invocation for a known target — never a committed global (it
breaks cross-arch + portable artifacts). The doer's geometric half (planning,
routing, RSS math) is CPU-bound and benefits directly:

```bash
# modern x86_64 baseline (AVX2/BMI2)
RUSTFLAGS="-C target-cpu=x86-64-v3" cargo build --profile dist -p parko-ros2

# aarch64 edge SoC — pin the actual core (example)
RUSTFLAGS="-C target-cpu=neoverse-n1" \
  cargo build --profile dist --target aarch64-unknown-linux-gnu -p parko-ros2
```

The deployment ISA for the doer is typically aarch64 (Orin/Thor-class) — pin the
SoC's core there.

## Tier C — PGO (opt-in)

Same shape as the SDK helper (`../scripts/pgo-build.sh`): instrument → run a
representative planning/perception workload → `llvm-profdata merge` → rebuild with
`-C profile-use`. Worth it for the doer's steady inner loop; use the rustc-bundled
`llvm-profdata`. Composes with Tier B via `EXTRA_RUSTFLAGS`.

## Tier D — the real per-silicon lever (backend + quantization)

Build flags (A–C) are the *cheap* win. The **large** per-silicon gains on the
doer are not build flags — they live in the inference backend:

- `parko-core/src/backend_selector.rs` + `backends/` (`openvino_stub`, `qnn_stub`,
  `amd_stub`) — the per-chip dispatch surface.
- `parko-onnx`, `parko-openvino`, `parko-tensorrt` — the vendor backends (all
  runtime-linked, so they build without the native SDK present).

The wins there: **quantization** (FP16 → INT8/FP8 on the bounded-vocabulary MLP),
**per-backend graph compilation** (TensorRT / OpenVINO / QNN / MIGraphX from one
ONNX IR), and **DLA / NPU / Hexagon offload**. This is a separate project (a doer
performance contract + calibration), tracked apart from this build-flag change.

## What NOT to do

- No global `target-cpu` / `[build] rustflags` (breaks cross-arch + portable builds).
- None of A–D on the SDK's safety-kernel / QNX judge build.
- Don't ship a `target-cpu=native` build as a portable artifact.
