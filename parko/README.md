# parko

An experimental Rust runtime for ML inference in robotics and edge applications.

**Status:** Early prototype. Public API may change without warning. Not yet suitable for production use.

## What this is

Parko combines three things that existing runtimes typically handle separately:

- **ML inference** — running ONNX (and eventually other) models against tensor inputs
- **Real-time control loops** — fixed-frequency tick scheduling, drop-frame detection, jitter measurement, and degraded-mode behavior
- **Safety governance integration** — designed to delegate command validation to the Aegis safety kernel (integration is roadmap, not present)

The intended use case is robotics and edge systems where ML perception drives actuator commands and runtime safety matters. The intended hardware targets include Linux servers (today), Jetson edge devices, and eventually edge NPUs (Qualcomm QNN, NXP eIQ, TI TIDL).

## What this is not

- Not a competitor to ONNX Runtime, TVM, or Modular MAX for general ML inference. Parko uses ONNX Runtime as a backend; it doesn't reimplement what those projects do well.
- Not a replacement for ROS2 or other robotics middleware. Parko provides a control loop primitive, not a full middleware stack.
- Not a safety-certified runtime. The integration with the Aegis safety kernel is designed-for but not yet implemented. The placeholder degraded-mode logic in the scheduler is explicitly marked as substitute-for-Aegis and is not a safety guarantee.
- Not yet validated on real hardware beyond CPU testing. Real benchmarks await Jetson hardware.

## Current capabilities

The workspace contains two crates:

**`parko-core`** — Core types and traits. No backends.
- `InferenceBackend` trait
- `TensorStorage` / `TensorBatch` types (input zero-copy supported; output zero-copy is roadmap)
- `RuntimeClock` with drift-free monotonic scheduling and overrun detection
- `CumulativeJitterEvaluator` for latency statistics (Welford's algorithm)
- `InferenceLoop` with one-tick-delayed actuator publication and degraded-mode detection
- `ControlLoop` with a clock-driven state machine

**`parko-onnx`** — ONNX Runtime backend, using the `ort` crate.
- One backend implementation: `OrtBackend` (CPU only)
- Integration test passing against MNIST-12

## Building

```bash
cargo check --workspace
cargo test --workspace
```
