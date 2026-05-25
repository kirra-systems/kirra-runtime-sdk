# parko

An experimental Rust runtime for ML inference in robotics and edge applications.

**Status:** Early prototype. Public API may change without warning. Not yet suitable for production use.

## What this is

Parko combines three things that existing runtimes typically handle separately:

- **ML inference** — running ONNX (and eventually other) models against tensor inputs
- **Real-time control loops** — fixed-frequency tick scheduling, drop-frame detection, jitter measurement, and degraded-mode behavior
- **Safety governance integration** — pluggable safety governors via the `SafetyGovernor` trait, with a working adapter to the Aegis kinematics contract via the `parko-aegis` crate. Posture-driven contract selection (Nominal → nominal_reference_profile, Degraded → mrc_fallback_profile) is verified end-to-end by integration tests. Linear velocity dimension only; see limitations.

The intended use case is robotics and edge systems where ML perception drives actuator commands and runtime safety matters. The intended hardware targets include Linux servers (today), Jetson edge devices, and eventually edge NPUs (Qualcomm QNN, NXP eIQ, TI TIDL).

## What this is not

- Not a competitor to ONNX Runtime, TVM, or Modular MAX for general ML inference. Parko uses ONNX Runtime as a backend; it doesn't reimplement what those projects do well.
- Not a replacement for ROS2 or other robotics middleware. Parko provides a control loop primitive, not a full middleware stack.
- Not a safety-certified runtime. The integration with the Aegis safety kernel is partially implemented for linear velocity bounds (via parko-aegis); broader safety properties are not yet bridged. The placeholder degraded-mode logic in the scheduler is explicitly marked as substitute-for-Aegis and is not a safety guarantee.
- Not yet validated on real hardware beyond CPU testing. Real benchmarks await Jetson hardware.

## Current capabilities

The workspace contains three crates:

**`parko-core`** — Core types and traits. No backends.
- `InferenceBackend` trait
- `TensorStorage` / `TensorBatch` types (input zero-copy supported; output zero-copy is roadmap)
- `RuntimeClock` with drift-free monotonic scheduling and overrun detection
- `CumulativeJitterEvaluator` for latency statistics (Welford's algorithm)
- `InferenceLoop` with one-tick-delayed actuator publication, degraded-mode detection, and optional `SafetyGovernor`
- `ControlLoop` with a clock-driven state machine
- `SafetyGovernor` trait for pluggable command envelope enforcement
- 33 unit and integration tests

**`parko-onnx`** — ONNX Runtime backend, using the `ort` crate.
- One backend implementation: `OrtBackend` (CPU only)
- Integration test passing against MNIST-12

**`parko-aegis`** — Aegis safety kernel adapter.
- Implements `SafetyGovernor` via the Aegis kinematics contract
- Enforces linear velocity bounds; selects nominal or MRC fallback profile by `FleetPosture`
- 3 integration tests against real Aegis contract profiles. Posture-driven divergence (Nominal clamps to 35.0 m/s, Degraded MRC clamps to 5.0 m/s) is verified by parko-core's test_posture_divergence integration tests.

## Building

```bash
cargo check --workspace
cargo test --workspace
```

## Running parko-onnx tests

parko-onnx uses the `ort` crate with the `load-dynamic` feature. This requires `libonnxruntime.so` to be available at runtime, pointed to by `ORT_DYLIB_PATH`.

Version matters. `ort 2.0.0-rc.12` is compiled against `ORT_API_VERSION = 24`. ONNX Runtime follows the convention `v1.N.x → API version N`, so you need ONNX Runtime v1.24.x. Older versions will cause a deadlock in the ort error handling path (a known bug in ort 2.0.0-rc.12 that requires the matching runtime to avoid).

Install ONNX Runtime v1.24.2 to a known location:

```bash
curl -L -o /tmp/onnxruntime-1.24.2.tgz \
  https://github.com/microsoft/onnxruntime/releases/download/v1.24.2/onnxruntime-linux-x64-1.24.2.tgz
mkdir -p ~/.local/onnxruntime
tar -xzf /tmp/onnxruntime-1.24.2.tgz -C ~/.local/onnxruntime --strip-components=1
```

Run the tests with `ORT_DYLIB_PATH` set:

```bash
cd parko && ORT_DYLIB_PATH=$HOME/.local/onnxruntime/lib/libonnxruntime.so cargo test -p parko-onnx 2>&1
```

## Running parko-aegis tests

parko-aegis depends on the aegis-runtime-sdk at the repository root. No additional setup is required beyond a standard cargo test.

```bash
cd parko && cargo test -p parko-aegis 2>&1
```

## Design notes

### Why a separate trait abstraction for backends and governors

Existing ONNX-related Rust crates couple the inference API tightly to the underlying ONNX Runtime API. Parko's `InferenceBackend` trait abstracts over the backend, so the same `InferenceLoop` and `ControlLoop` code can run with future implementations against Qualcomm QNN, NXP eIQ, or other NPU SDKs without changes to the control logic.

The same plugin pattern applies to safety: `SafetyGovernor` is a trait in parko-core, with `AegisGovernor` (in parko-aegis) as one implementation. A future TÜV-certified, ISO 26262 SEooC, or custom governor would slot in the same way. Parko-core has no Aegis dependency; the bridge lives only in parko-aegis.

This is an investment in flexibility, paid for in indirection. For a single-backend, single-platform deployment, the trait abstraction is overhead. For a multi-backend, multi-governor roadmap, it's the foundation.

### Why `Mutex<Session>` in parko-onnx

ONNX Runtime sessions are not `Sync` — concurrent inference on a single session is not safe. The `InferenceBackend` trait requires `Send + Sync`, so the ORT backend wraps the session in a `Mutex`. This serializes inference (one call at a time per backend instance), which is acceptable for single-model deployments. Multi-model or true concurrent inference would require a different design (session pool, thread-local sessions).

### Why the timing primitives are soft real-time

`RuntimeClock` uses `tokio::time::sleep`, which has millisecond-class precision on Linux but no hard real-time guarantees. Suitable for control rates up to ~100Hz on a quiet system. For sub-millisecond timing, the clock would need to be replaced with `clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, ...)` and `SCHED_FIFO` scheduling. This is documented in the code so it doesn't get silently overlooked.

### Governor precedence over built-in degraded-mode

If a `SafetyGovernor` is attached via `InferenceLoop::with_governor()`, it runs before the built-in degraded-mode logic. The governor's clamp output feeds into the built-in logic, which may further restrict the value. The lower bound wins. The built-in degraded-mode logic remains as a fallback for callers without a governor.

## Known limitations

- Pipelining of inference and command publication is not implemented; `InferenceLoop` is one-tick-delayed publication, not pipelined inference.
- Thermal monitoring reads `/sys/class/thermal/thermal_zone0/temp` and returns `None` on platforms without that path. Non-Linux platforms have no thermal awareness today.
- The built-in degraded-mode policy clamps linear velocity to a hardcoded ceiling (1.5 m/s) when no `SafetyGovernor` is attached. When a governor is attached via `with_governor()`, the built-in clamp is suppressed and the governor has full authority over command modification.
- The `Mutex<Session>` serialization means parko-onnx is single-inference-at-a-time per backend instance. Multiple models or concurrent inference would require multiple backend instances.
- ONNX Runtime via `ort 2.0.0-rc.12` has a re-entrant Once-lock deadlock in its error handling path when `ORT_API_VERSION` mismatch occurs. Pinning to the matching runtime version (v1.24.x) avoids triggering it, but the bug exists.
- parko-aegis bridges only the linear velocity dimension to the Aegis kinematics contract. Angular velocity is not currently enforced. See `crates/parko-aegis/README.md` for details.

## License

Apache-2.0

## Relationship to Aegis

Parko is developed alongside Aegis but is a separate experiment. Parko-core has no dependency on Aegis. The parko-aegis adapter crate depends on aegis-runtime-sdk and implements parko-core's `SafetyGovernor` trait, bridging to the existing Aegis kinematics contract. This integration enforces linear velocity bounds; broader safety properties (steering, audit chain, posture engine, federation) are not yet bridged.

Parko's existence does not change Aegis's roadmap or commercial focus.
