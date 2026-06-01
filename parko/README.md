# parko

An experimental Rust runtime for ML inference in robotics and edge applications.

**Status:** Early prototype. Public API may change without warning. Not yet suitable for production use.

## What this is

Parko combines three things that existing runtimes typically handle separately:

- **ML inference** — running ONNX (and eventually other) models against tensor inputs
- **Real-time control loops** — fixed-frequency tick scheduling, drop-frame detection, jitter measurement, and degraded-mode behavior
- **Safety governance integration** — pluggable safety governors via the `SafetyGovernor` trait, with a working adapter to the Kirra kinematics contract via the `parko-kirra` crate. Posture-driven contract selection (Nominal → nominal_reference_profile, Degraded → mrc_fallback_profile) is verified end-to-end by integration tests. Linear velocity dimension only; see limitations.

The intended use case is robotics and edge systems where ML perception drives actuator commands and runtime safety matters. The intended hardware targets include Linux servers (today), Jetson edge devices, and eventually edge NPUs (Qualcomm QNN, NXP eIQ, TI TIDL).

## What this is not

- Not a competitor to ONNX Runtime, TVM, or Modular MAX for general ML inference. Parko uses ONNX Runtime as a backend; it doesn't reimplement what those projects do well.
- Not a replacement for ROS2 or other robotics middleware. Parko provides a control loop primitive, not a full middleware stack.
- Not a safety-certified runtime. The integration with the Kirra safety kernel is partially implemented for linear velocity bounds (via parko-kirra); broader safety properties are not yet bridged. The placeholder degraded-mode logic in the scheduler is explicitly marked as substitute-for-Kirra and is not a safety guarantee.
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

**`parko-openvino`** — OpenVINO backend, using the `openvino` crate.
- One backend implementation: `OvBackend` (CPU device, ingests ONNX directly)
- Integration tests including a **cross-backend numerical-equivalence
  test** against the same MNIST-12 fixture parko-onnx uses — the first
  evidence that the `InferenceBackend` abstraction works for two
  independent runtimes (the core of the vendor-neutral thesis).

### Backend status

| Backend | Crate | Hardware | Runtime install | Status |
|---|---|---|---|---|
| ONNX Runtime CPU | `parko-onnx` | any x86 CPU | `libonnxruntime.so` + `ORT_DYLIB_PATH` (v1.24.2) | ✅ full |
| Intel OpenVINO | `parko-openvino` | any x86 Intel CPU | `libopenvino_c.so` from the Intel apt repo (`openvino-2024.x`) | ✅ full (CPU; `cargo build` does not require the toolkit) |
| TensorRT (NVIDIA) | — | NVIDIA GPU | — | planned (PARK-020) |
| Qualcomm QNN | — | Qualcomm NPU | — | planned (PARK-027) |
| TI TIDL | — | TI hardware | — | planned (PARK-028) |
| AMD Vitis | — | AMD hardware | — | planned (PARK-030) |

**`parko-kirra`** — Kirra safety kernel adapter.
- Implements `SafetyGovernor` via the Kirra kinematics contract
- Enforces linear velocity bounds; selects nominal or MRC fallback profile by `FleetPosture`
- 3 integration tests against real Kirra contract profiles. Posture-driven divergence (Nominal clamps to 35.0 m/s, Degraded MRC clamps to 5.0 m/s) is verified by parko-core's test_posture_divergence integration tests.

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

## Running parko-openvino tests

parko-openvino uses the `openvino` crate with the `runtime-linking`
feature — the analog of ort's `load-dynamic`. This requires
`libopenvino_c.so` to be available at runtime; `cargo build` does not
need the toolkit installed.

The recommended install path is the Intel public apt repository (see
the [OpenVINO 2024 install guide](https://docs.openvino.ai/2024/get-started/install-openvino/install-openvino-linux.html)):

```bash
wget -qO - https://apt.repos.intel.com/intel-gpg-keys/GPG-PUB-KEY-INTEL-SW-PRODUCTS.PUB \
  | sudo gpg --dearmor -o /usr/share/keyrings/intel.gpg
echo "deb [signed-by=/usr/share/keyrings/intel.gpg] https://apt.repos.intel.com/openvino/2024 ubuntu22 main" \
  | sudo tee /etc/apt/sources.list.d/intel-openvino-2024.list
sudo apt-get update && sudo apt-get install -y openvino-2024.5.0
```

Run the tests with the library path exported:

```bash
cd parko && \
  LD_LIBRARY_PATH=/opt/intel/openvino/runtime/lib/intel64 \
  OPENVINO_LIB_PATH=/opt/intel/openvino/runtime/lib/intel64 \
  cargo test -p parko-openvino
```

The integration suite includes a **cross-backend equivalence test**
(`ort_ov_output_equivalence_on_mnist`) that loads the same MNIST-12
ONNX file in both `OrtBackend` and `OvBackend`, runs the same input,
and asserts the outputs match within `EQUIV_TOL = 1e-3` per element.
This is the first numerical-equivalence check across backends — it
seeds the planned model-validation tooling (a generic harness that
can swap any two `InferenceBackend` impls).

## Running parko-kirra tests

parko-kirra depends on the kirra-runtime-sdk at the repository root. No additional setup is required beyond a standard cargo test.

```bash
cd parko && cargo test -p parko-kirra 2>&1
```

## SensorInputMapping library — camera + odometry

`parko-ros2::sensor_mapping` ships **pre-tested mappings for the two most common Parko inputs**: camera images and odometry/state. Integrators pick a config and plug a mapping into the Parko node — no hand-written normalization, channel-order, layout, or quaternion code required.

### Camera

```rust
use parko_ros2::{
    CameraConfig, CameraEncoding, CameraLayout, CameraMapping,
    CameraNormalization, CameraResize,
};

let mapping = CameraMapping::new(CameraConfig {
    encoding:      CameraEncoding::Bgr8,         // sensor_msgs/Image is typically bgr8
    target_height: 224, target_width: 224,       // model input dims
    resize:        CameraResize::Nearest,        // M1 default; bilinear is a future feature
    normalization: CameraNormalization::MeanStd {
        mean: vec![0.485, 0.456, 0.406],         // ImageNet
        std:  vec![0.229, 0.224, 0.225],
    },
    layout:        CameraLayout::Nchw,           // PyTorch / ONNX
    tensor_name:   "input".to_string(),
});
```

**Configurable surface:**

| Field | Choices | Notes |
|---|---|---|
| `encoding` | `Rgb8`, `Bgr8`, `Mono8` | The output is **always RGB-ordered** for 3-channel encodings — `Bgr8` source bytes are channel-swapped on the way out, eliminating the classic-bug rgb-vs-bgr confusion |
| `normalization` | `Unit01` (`[0,1]`), `SignedUnit` (`[-1,1]`), `MeanStd { mean, std }` | Per-channel mean/std required for ImageNet-style models |
| `layout` | `Nchw` (PyTorch/ONNX), `Nhwc` (TensorFlow/TFLite) | Match the model's input contract |
| `resize` | `Nearest` | Bilinear is the next addition |
| `target_height`, `target_width` | u32 | The output is resized to these dims via the configured algorithm |
| `tensor_name` | String | Must match the model's input-node name |

**Defaults chosen + rationale:**
- **Resize: nearest-neighbour** — simplest to test exactly, no interpolation artifacts. Bilinear is the obvious next addition and will be feature-gated so existing nearest-resize models don't drift.
- **Layout: NCHW** — matches the MNIST-12 ONNX fixture and the dominant PyTorch/ONNX convention; NHWC is supplied for TFLite users.

**Errors:** the pure transform returns `Result<TensorBatch, CameraMappingError>`. The trait-level `to_frame` falls back to a zero tensor + a `tracing::error!` so the downstream tick pipeline's staleness / governor MRC path kicks in — fail-closed by construction.

### Odometry

```rust
use parko_ros2::{OdomConfig, OdomMapping, OdomOrientation};

let mapping = OdomMapping::new(OdomConfig {
    include_position:         true,
    include_orientation:      Some(OdomOrientation::Yaw),   // planar control default
    include_linear_velocity:  true,
    include_angular_velocity: true,
    tensor_name:              "state".to_string(),
});
```

**Configurable surface:**

| Field | Choices | Notes |
|---|---|---|
| `include_position` | bool | (x, y, z) — 3 floats |
| `include_orientation` | `None`, `Some(Yaw)`, `Some(FullEuler)`, `Some(Quaternion)` | 0, 1, 3, or 4 floats respectively. **`Yaw` is the planar-control default** and matches `kirra-ros2-adapter::geometry::quat_to_yaw` |
| `include_linear_velocity` | bool | (vx, vy, vz) — 3 floats |
| `include_angular_velocity` | bool | (wx, wy, wz) — 3 floats |
| `tensor_name` | String | Must match the model's input-node name |

**Output vector layout** (each block present only if its toggle is on, in this fixed order):

```
[ pos.x, pos.y, pos.z,    {orientation block},    vlin.x, vlin.y, vlin.z,    vang.x, vang.y, vang.z ]
```

**Quaternion convention:** ROS `(x, y, z, w)`. The conversion uses Tait–Bryan ZYX intrinsic Euler — same convention the `kirra-ros2-adapter::geometry::quat_to_yaw` helper uses, so the adapter and parko-ros2 agree on what "yaw" means.

### Plugging into the Parko node

```rust
use std::sync::Arc;
let mapping = Arc::new(mapping);   // Arc<CameraMapping> or Arc<OdomMapping>
// hand `mapping` to `parko_ros2::node::run_node(.., mapping, ..)`
```

The trait dispatch keeps the pipeline generic — the same `run_node` accepts any `SensorInputMapping` impl.

### What's tested now vs. what needs ROS

| Component | Tested on stable today | Requires ROS / runtime |
|---|---|---|
| `CameraMapping::to_tensor` (pure transform) | ✅ rgb vs bgr channel order, NCHW vs NHWC, all 3 normalizations, mono, resize up + down, all 3 error paths | — |
| `OdomMapping::to_tensor` (pure transform) | ✅ quaternion → yaw (positive + negative), full Euler, raw quaternion, field selection, vector layout | — |
| `sensor_msgs/Image` → `OwnedCameraSample` shim | — | ros2 feature (next-milestone wiring) |
| `nav_msgs/Odometry` → `OdomSample` shim | — | ros2 feature (next-milestone wiring) |

## Design notes

### Why a separate trait abstraction for backends and governors

Existing ONNX-related Rust crates couple the inference API tightly to the underlying ONNX Runtime API. Parko's `InferenceBackend` trait abstracts over the backend, so the same `InferenceLoop` and `ControlLoop` code can run with future implementations against Qualcomm QNN, NXP eIQ, or other NPU SDKs without changes to the control logic.

The same plugin pattern applies to safety: `SafetyGovernor` is a trait in parko-core, with `KirraGovernor` (in parko-kirra) as one implementation. A future TÜV-certified, ISO 26262 SEooC, or custom governor would slot in the same way. Parko-core has no Kirra dependency; the bridge lives only in parko-kirra.

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
- parko-kirra bridges only the linear velocity dimension to the Kirra kinematics contract. Angular velocity is not currently enforced. See `crates/parko-kirra/README.md` for details.

## License

Apache-2.0

## Relationship to Kirra

Parko is developed alongside Kirra but is a separate experiment. Parko-core has no dependency on Kirra. The parko-kirra adapter crate depends on kirra-runtime-sdk and implements parko-core's `SafetyGovernor` trait, bridging to the existing Kirra kinematics contract. This integration enforces linear velocity bounds; broader safety properties (steering, audit chain, posture engine, federation) are not yet bridged.

Parko's existence does not change Kirra's roadmap or commercial focus.
