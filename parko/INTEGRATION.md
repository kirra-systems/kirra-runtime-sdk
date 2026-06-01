# Integrating a Learned Controller Under Parko

A single-document guide for external integrators (robotics startups,
research labs, pilot partners) deploying a trained model under
Parko's safety governance. This is a **developer guide** — for the
safety case (HARA, SG-derivation, SOTIF arguments) see
`docs/safety/`.

---

## 1. Overview

**Parko is a safety-governed runtime for learned controllers.** You
bring a trained model (ONNX today); Parko runs it inside a control
loop with a safety governor that bounds every actuator command the
model produces.

### Scope boundary (read first)

Parko's job is **safety bounds on the model's output**:

- **Parko enforces:** linear-velocity envelope (ODD speed cap, accel /
  decel, lateral acceleration), angular-velocity envelope (SOTIF
  rollover / sweep / FTTI), RSS safe-distance, posture-driven MRC
  derate, fail-closed responses to staleness / NaN / inference
  errors.
- **Parko does NOT:** plan, replace, or guarantee the **correctness**
  of the model. **Task performance is the integrator's
  responsibility.** The governor catches commands that violate the
  envelope; it does not make a bad policy into a good one.

A clamped or denied command is the governor saying "the model asked
for something outside the safety envelope". That's a signal — it
means the trained policy needs to be retrained, or the envelope is
mis-specified, or both. Treat clamp / deny as diagnostic information,
not as part of the control loop the model can rely on.

### The stack

```
sensor topic ──▶ SensorInputMapping ──▶ InferenceBackend ──▶ ControlCommand
                  (CameraMapping /        (OrtBackend /     │  (linear + angular m/s, rad/s)
                   OdomMapping /            OvBackend)      │
                   custom impl)                             ▼
                                                  ┌─────────────────────┐
                                                  │ parko-kirra         │
                                                  │ KirraGovernor or    │
                                                  │ GovernorComparator  │
                                                  │ (dual-governor      │
                                                  │  lockstep + audit)  │
                                                  └─────────────────────┘
                                                            │
                                                            ▼ EnforcementAction:
                                                  Allow / ClampLinearVelocity /
                                                  ClampAngularVelocity / ClampMotion / Deny
                                                            │
                                                            ▼ map to OutgoingTwist
                                                            ▼ publish geometry_msgs/Twist
                                                  command topic ──▶ vehicle
```

---

## 2. The model contract

### 2.1 Input — sensor → tensor

Your model receives a `parko_core::backend::TensorBatch` produced by
a `SensorInputMapping`. The trait is in
`parko/crates/parko-ros2/src/sensor_mapping.rs`:

```rust
pub trait SensorInputMapping: Send + Sync {
    type Sample;
    fn to_frame(&self, frame_id: u64, timestamp_ms: u64, sample: &Self::Sample)
        -> parko_core::sensor::SensorFrame;
}
```

Parko ships two configurable mappings out of the box. Pick the one
that matches your sensor; subclass the trait for anything else.

**Camera (`CameraMapping`)** — `CameraConfig` fields:

| Field | Type | Choices / values |
|---|---|---|
| `encoding` | `CameraEncoding` | `Rgb8` / `Bgr8` / `Mono8` |
| `target_height`, `target_width` | `u32` | Model's expected H × W |
| `resize` | `CameraResize` | `Nearest` (bilinear is planned) |
| `normalization` | `CameraNormalization` | `Unit01` / `SignedUnit` / `MeanStd { mean, std }` |
| `layout` | `CameraLayout` | `Nchw` / `Nhwc` |
| `tensor_name` | `String` | Model's input-node name |

Channel-order invariant: `Bgr8` source bytes are channel-swapped on
the way out, so the output tensor is **always RGB-ordered** for
3-channel encodings.

**Odometry (`OdomMapping`)** — `OdomConfig` fields:

| Field | Type | Notes |
|---|---|---|
| `include_position` | `bool` | adds 3 floats (x, y, z) |
| `include_orientation` | `Option<OdomOrientation>` | `None` / `Yaw` (1 float) / `FullEuler` (3) / `Quaternion` (4) |
| `include_linear_velocity` | `bool` | adds 3 floats |
| `include_angular_velocity` | `bool` | adds 3 floats |
| `tensor_name` | `String` | |

Output-vector layout (fixed; each block only if its toggle is on):
`pos.x, pos.y, pos.z, {orientation block}, vlin.x, vlin.y, vlin.z, vang.x, vang.y, vang.z`.

Yaw conversion uses Tait–Bryan ZYX (the same convention the Occy
adapter uses).

### 2.2 Output — tensor → ControlCommand

The model must emit two named output tensors:

| Tensor name | Shape | Contents |
|---|---|---|
| `cmd_vel_linear`  | one f32 | desired forward velocity, m/s |
| `cmd_vel_angular` | one f32 | desired yaw rate, rad/s (positive = CCW) |

The scheduler reads these names directly
(`parko/crates/parko-core/src/scheduler.rs::parse_inference_to_command`).
Non-finite values (`NaN` / `±Inf`) are caught at parse time and the
loop emits a stopped command for that tick — the model never poisons
the actuator.

The resulting `parko_core::commands::ControlCommand` shape is the
2-D Twist subset (linear forward velocity + yaw rate); maps to
`geometry_msgs/Twist` on the ROS 2 side via
`parko_ros2::command_mapping::OutgoingTwist`.

### 2.3 Model format

Today the supported model format on every shipped backend is **ONNX**.

| Backend crate | Type name | Hardware | Status |
|---|---|---|---|
| `parko-onnx` | `OrtBackend` | any x86 CPU | ✅ shipping — uses `ort 2.0.0-rc.12` + ONNX Runtime v1.24.2 dlopen'd at runtime |
| `parko-openvino` | `OvBackend` | any x86 Intel CPU | ✅ shipping — uses `openvino = 0.11` with the `runtime-linking` feature; OpenVINO 2024.x |
| TensorRT (NVIDIA) | — | NVIDIA GPU | **planned** (PARK-020); not built |
| Qualcomm QNN | — | Qualcomm NPU | **planned** (PARK-027); not built |
| TI TIDL | — | TI hardware | **planned** (PARK-028); not built |
| AMD Vitis | — | AMD hardware | **planned** (PARK-030); not built |

Cross-backend equivalence: `parko-openvino` includes the
`ort_ov_output_equivalence_on_mnist` test (loads the same ONNX
model in both backends, asserts outputs match within
`EQUIV_TOL = 1e-3` per element on a deterministic non-trivial input).

---

## 3. The governance contract

Every tick the inference loop produces a `ControlCommand`. Before it
reaches the actuator topic it passes through a `SafetyGovernor`
(parko-core trait, `parko-kirra::KirraGovernor` is the shipping
implementation) which returns one of:

| `EnforcementAction` | Meaning | Effect |
|---|---|---|
| `Allow` | command satisfies the envelope | published as-is |
| `ClampLinearVelocity(v)` | linear-axis violation | linear axis overridden, angular unchanged |
| `ClampAngularVelocity(w)` | angular-axis violation | angular axis overridden, linear unchanged |
| `ClampMotion { linear, angular }` | both axes need bounding | per-axis override; `None` = unconstrained on that axis |
| `Deny { reason }` | hard safety violation (e.g. LockedOut posture) | published as `ControlCommand::stopped` |

### 3.1 Linear-axis enforcement

The linear axis is bridged through the Kirra kinematics contract
(`kirra_runtime_sdk::gateway::kinematics_contract::validate_vehicle_command`).
The relevant fields of `VehicleKinematicsContract`:

| Field | Purpose |
|---|---|
| `max_speed_mps` | vehicle physical max |
| `odd_speed_cap_mps: Option<f64>` | ODD operational ceiling — for an urban deployment set to `URBAN_ODD_SPEED_CAP_MPS = 22.35` (50 mph). The enforced max is `min(max_speed_mps, odd_speed_cap_mps)` |
| `max_accel_mps2`, `max_brake_mps2` | rate-of-change ceilings |
| `max_steering_deg`, `max_steering_rate_deg_s` | steering envelope (Ackermann path) |
| `max_lateral_accel_mps2`, `wheelbase_m` | bicycle-model lateral-accel check |
| `width_m`, `length_m`, `overhang_front_m`, `overhang_rear_m` | footprint (used by SG2 containment) |

ODD speed cap source: `docs/safety/SPEED_ENVELOPE.md` (KIRRA-OCCY-SPEED-001)
and `docs/safety/OCCY_SPEED_CAP_VALIDATION.md` (KIRRA-OCCY-SPEED-VAL-001).

### 3.2 Angular-axis enforcement (SOTIF — DRAFT)

The angular axis is bounded natively on the parko side via
`parko_kirra::AngularVelocityBound`. The bound is **v-dependent**:

```
ω_max(v) = min(ω_rollover(v), ω_sweep, ω_ftti)
```

Per-platform parameters in `PlatformParams`:

| Field | Purpose |
|---|---|
| `track_width_m`     | static stability factor (rollover) |
| `cog_height_m`      | static stability factor (rollover) |
| `robot_extent_m`    | sweep / contact bound (bounding-circle radius) |
| `v_edge_safe_mps`   | safe contact velocity (ISO/TS 15066) |
| `theta_max_rad`     | max heading change per FTTI |
| `ftti_s`            | fault-tolerant time interval |
| `mrc_posture_factor`| MRC derate factor on `v_edge_safe` + `theta_max` |

Derivation, assumptions, citations, and worked numbers:
**`docs/safety/ANGULAR_VELOCITY_SOTIF.md`** (KIRRA-OCCY-ANGULAR-SOTIF-001).
That document carries a **DRAFT — pending formal safety-engineer review**
banner; do not treat the numbers as a validated safety claim.

### 3.3 RSS safe-distance

`parko_kirra::KirraGovernor` carries an `RssState { safe: bool, … }`
updated via `update_rss_state`. An unsafe RSS state applies the MRC
profile (same path as `SafetyPosture::Degraded`) — a sensor gap is
recoverable, not a hard stop. Per ADL-001.

### 3.4 Posture-driven profile

`parko_core::safety::SafetyPosture` has three states; the governor's
response per state:

| Posture | Governor behaviour |
|---|---|
| `Nominal` | Full envelope: Kirra kinematics contract on linear, SOTIF bound on angular |
| `Degraded` | MRC profile: linear clamped to `MRC_VELOCITY_CEILING_MPS = 5.0`, angular derated by `mrc_posture_factor` |
| `LockedOut` | Hard stop — every command returns `Deny { reason: "LockedOut: hard stop" }` |

### 3.5 GovernorComparator (dual-governor lockstep)

For ASIL-D decomposition (CERT-006) you can wrap two `KirraGovernor`
instances in a `GovernorComparator`:

```rust
let comparator = GovernorComparator::new(KirraGovernor::new(), KirraGovernor::new());
```

On agreement: returns the primary's verdict and decays the divergence
accumulator. On disagreement on either axis (linear OR angular,
post-CERT-006 v3): commands a most-restrictive reconciliation
(`reconcile` function), records a `ComparatorDivergence` event,
escalates to `LockedOut` if the accumulator reaches the lockout
level. The `comparator_adapter::ComparatorAsGovernor` newtype in
parko-ros2 lets you hand a `GovernorComparator` to the
`InferenceLoop` (which expects a `SafetyGovernor`).

### Key point (re-state)

> **The model should produce in-bounds commands.** The governor is
> the safety net, not the primary controller. A clamped or denied
> command is information that the model asked for something the
> safety case forbids — train against the envelope, don't rely on
> the governor to "fix" outputs.

---

## 4. Platform configuration

### 4.1 Kinematics contract (linear axis)

Built per-platform via `kirra_ros2_adapter::config::VehicleConfig`
or directly via `VehicleKinematicsContract`. For an urban AV the
adapter's `VehicleConfig::default_urban()` produces a contract with
`odd_speed_cap_mps = Some(URBAN_ODD_SPEED_CAP_MPS)` (22.35 m/s) and
the reference vehicle geometry (1.85 × 4.8 m). For a small mobile
robot construct your own `VehicleKinematicsContract`.

A startup helper `warn_if_missing_odd_cap()` logs WARN when the ODD
cap is missing — a deployment without an ODD cap is loud, not silent.

### 4.2 Angular bound (PlatformParams)

Two presets in `parko_kirra::angular_bound`:

- `PlatformParams::conservative_default()` — for an
  uncharacterised platform. Tight bound that fails toward safe
  (~0.2 rad/s at v=0). Used by `KirraGovernor::new()` when no
  `with_platform_params` is called.
- `PlatformParams::urban_service_robot_reference()` — worked example
  in the SOTIF doc (~TurtleBot-4 scale, ~0.833 rad/s at v=0).

Override via `KirraGovernor::with_platform_params(my_params)`.

### 4.3 Posture source

The `kirra-ros2-adapter` binary (Occy path) reads three env vars
to wire the verifier's `/system/posture/stream` SSE feed into the
shared `PostureTracker`:

| Env var | Required when | Behaviour |
|---|---|---|
| `KIRRA_POSTURE_STREAM_URL`   | gates the source | unset → `NoSource` (M1 default, posture stays Nominal) |
| `KIRRA_ADMIN_TOKEN`          | required when URL is set | missing → `ConfiguredNoTransport` (posture seeds **Degraded** — fail-closed; not silently Nominal) |
| `KIRRA_POSTURE_CLIENT_ID`    | optional | defaults to `kirra-ros2-adapter` |

Fail-closed contract (see `crates/kirra-ros2-adapter/src/bin/kirra_ros2_adapter_node.rs::classify_posture_source`):

| Env state | Decision | Posture floor |
|---|---|---|
| URL unset | `NoSource` | Nominal (verifier-less deployment) |
| URL set + admin token present | `Live` + spawn SSE task | live posture from the verifier |
| URL set + admin token missing | `ConfiguredNoTransport` + WARN | **Degraded** (held until the operator fixes the config + restarts) |

The state machine itself is `kirra_runtime_sdk::posture_tracker::PostureTracker`
in the kernel — shared between the adapter and parko-ros2.

`POSTURE_STALENESS_TIMEOUT_MS = 6_000` (the staleness derate threshold).

**Parko-ros2 binary status (M2b code present, transport planned):**
the shared `PostureTracker` and the `ParkoPostureState` wrapper live
in `parko/crates/parko-ros2/src/posture_state.rs`, and the tick
pipeline's `run_pipeline_tick_with_posture_state` consumes it.
**The parko-ros2 binary currently passes a static
`SafetyPosture::Nominal`** to `run_node`; the SSE-subscriber wiring
(M2c) is planned. To run with live posture today, use the Occy
adapter's transport on the verifier side and a separate path on the
parko side.

### 4.4 ParkoNodeConfig

`parko_ros2::ParkoNodeConfig` (defaults shown):

| Field | Default | Purpose |
|---|---|---|
| `sensor_topic`  | `~/input/observation` | ROS 2 sensor-input topic |
| `command_topic` | `~/output/cmd_vel`    | ROS 2 actuator-output topic |
| `tick_period_s` | `0.05` (20 Hz) | inference-loop period; `delta_time_s` for the governor |
| `sensor_staleness_budget_ms` | `200` | beyond this, the tick publishes MRC instead of running inference |

---

## 5. Deployment steps

1. **Pick a backend.** Export your trained model to ONNX. Choose
   `parko-onnx` (CPU/ONNX Runtime — set `ORT_DYLIB_PATH`) or
   `parko-openvino` (Intel CPU/OpenVINO — set `OPENVINO_LIB_PATH`).
2. **Choose or implement a `SensorInputMapping`.**
   - Camera observation? `parko_ros2::CameraMapping::new(CameraConfig { … })`.
   - Odometry / state observation? `parko_ros2::OdomMapping::new(OdomConfig { … })`.
   - Custom sensor? Implement the `SensorInputMapping` trait against your message type.
3. **Configure the kinematics contract** (`VehicleKinematicsContract`):
   set `max_speed_mps`, `odd_speed_cap_mps`, the accel / brake bounds,
   and the platform geometry. For urban AV deployments,
   `kirra_ros2_adapter::config::VehicleConfig::default_urban()` does
   this for you.
4. **Configure `PlatformParams`** for the angular bound: pass real
   numbers for `track_width_m`, `cog_height_m`, `robot_extent_m`, plus
   the safety budgets (`v_edge_safe_mps`, `theta_max_rad`, `ftti_s`).
   Use `PlatformParams::conservative_default()` if you have not yet
   characterised the platform — it fails toward safe.
5. **Configure the posture source** (optional). If you have a Kirra
   verifier reachable, set `KIRRA_POSTURE_STREAM_URL` +
   `KIRRA_ADMIN_TOKEN`. Without these, the node runs at the Nominal
   default. The Occy adapter consumes this today; the parko-ros2
   binary will when M2c lands.
6. **Wire the ROS 2 topics.** `ParkoNodeConfig::sensor_topic` (the
   adapter subscribes here — `std_msgs/msg/Float32MultiArray`
   wire format today, JSON shape `{ "data": [f32, ...],
   "stamp_ms": u64 }`); `ParkoNodeConfig::command_topic` (the adapter
   publishes here — `geometry_msgs/msg/Twist`).
7. **Run.** `cargo run -p parko-ros2 --features ros2,onnx-backend
   --bin parko_ros2_node` with the relevant env vars set
   (`PARKO_MODEL_PATH`, `ORT_DYLIB_PATH`).

The binary's entry point is
`parko/crates/parko-ros2/src/bin/parko_ros2_node.rs`. It builds the
backend, the `InferenceLoop` with `GovernorComparator` attached, the
sensor mapping, and calls `parko_ros2::node::run_node`.

---

## 6. Fail-closed behaviour

Every failure path produces a safe outgoing twist instead of a
crash, panic, or stale output. The integrator must understand which
events fire which response so the model's reaction to "the actuator
went quiet" is correct.

| Event | Detection site | Response |
|---|---|---|
| **Sensor input older than `sensor_staleness_budget_ms`** | `tick_pipeline.rs::run_pipeline_tick` (staleness check) | `OutgoingTwist::stopped` + `TickError::StaleSensorInput`; **no inference runs** on a stale frame |
| **Non-finite model output (`NaN` / `±Inf`)** | `scheduler.rs::parse_inference_to_command` (parko-core) | scheduler emits a `PostureSnapshot { active_command: stopped, active_state_degraded: true }`; tick publishes stopped twist |
| **Backend `InferenceLoop::tick` returns `Err`** (model handle invalid, runtime error) | `run_pipeline_tick` | `OutgoingTwist::stopped` + `TickError::InferenceError` |
| **Defence-in-depth NaN at the mapper** (a non-finite value leaked past the governor) | `command_mapping.rs::enforce_outgoing_twist` | `OutgoingTwist::stopped` |
| **Governor `Deny` (e.g. LockedOut posture)** | `KirraGovernor::evaluate` → scheduler stamps `ControlCommand::stopped` | published twist is zero/zero |
| **Comparator divergence escalation** | `GovernorComparator::evaluate` accumulator → `LockedOut` | → `Deny` → stopped twist |
| **Backend `load_model` failure at startup** | binary `main` | process exits with a non-zero status + clear `eprintln`; never a silent no-op |
| **Posture source unreachable (M1b)** | adapter SSE subscriber + `PostureTracker` staleness watchdog | derates Nominal → Degraded after `POSTURE_STALENESS_TIMEOUT_MS = 6_000` ms; `LockedOut` is sticky-toward-safe |
| **Posture source misconfigured (URL set, token missing)** | `classify_posture_source` (adapter) | `ConfiguredNoTransport` → `Degraded` held until corrected (fail-closed; never silently Nominal) |

---

## 7. What's built vs. what's planned

| Component | Status |
|---|---|
| `parko-core` `InferenceBackend` trait + scheduler | ✅ shipping |
| `parko-onnx::OrtBackend` (CPU/ONNX) | ✅ shipping (`ORT_DYLIB_PATH`, v1.24.2 pin) |
| `parko-openvino::OvBackend` (Intel CPU/OpenVINO) | ✅ shipping (`OPENVINO_LIB_PATH`, 2024.x) + cross-backend equivalence test |
| `parko-ros2` node (r2r-based; `--features ros2`) | ✅ shipping |
| `SensorInputMapping`: `CameraMapping`, `OdomMapping`, `VectorMapping` | ✅ shipping |
| `OutgoingTwist` command mapping | ✅ shipping |
| `KirraGovernor` linear / angular / RSS / posture | ✅ shipping |
| `GovernorComparator` dual-governor lockstep | ✅ shipping |
| Kirra ODD speed cap (`URBAN_ODD_SPEED_CAP_MPS = 22.35`) | ✅ shipping |
| `AngularVelocityBound` SOTIF derivation (#136) | ✅ shipping — **DRAFT, pending formal safety-engineer review** |
| Shared `PostureTracker` (kernel, M2b) | ✅ shipping |
| `ParkoPostureState` + `run_pipeline_tick_with_posture_state` | ✅ shipping (tick path consumes it) |
| **Live posture transport wired into the parko-ros2 binary** | **planned (M2c)** — today the binary passes `SafetyPosture::Nominal` statically; the M1b SSE subscriber pattern is the template |
| **Hardware inference backends** (TensorRT, QNN, TIDL, Vitis) | **planned** — abstractions exist in `parko-core::BackendDescriptor` enum; no implementations yet |
| **LiDAR / radar sensor mappings** | **planned** — `VectorMapping` is the trait-impl template; integrators implement `SensorInputMapping` for their wire type today |
| **`sensor_msgs/Image` → `OwnedCameraSample` extraction shim** | **planned** (M2 sensor-library follow-up) — today the JSON shape `{ "data": [...], "stamp_ms": u64 }` is the wire format the parko-ros2 node subscribes to |
| **`nav_msgs/Odometry` → `OdomSample` extraction shim** | **planned** (same follow-up) |
| **Bilinear camera resize** | **planned** — `CameraResize::Nearest` is the only variant today |

---

## 8. Worked end-to-end example

A camera-input policy on a small mobile robot, urban deployment.

### 8.1 Sensor mapping

```rust
use parko_ros2::{CameraConfig, CameraEncoding, CameraLayout,
                 CameraMapping, CameraNormalization, CameraResize};

let camera_mapping = CameraMapping::new(CameraConfig {
    encoding:      CameraEncoding::Bgr8,    // ROS sensor_msgs/Image default
    target_height: 224, target_width: 224,
    resize:        CameraResize::Nearest,
    normalization: CameraNormalization::MeanStd {
        mean: vec![0.485, 0.456, 0.406],     // ImageNet
        std:  vec![0.229, 0.224, 0.225],
    },
    layout:        CameraLayout::Nchw,       // PyTorch / ONNX
    tensor_name:   "input".to_string(),       // the model's input-node name
});
```

### 8.2 Kinematics contract

```rust
use kirra_runtime_sdk::gateway::kinematics_contract::{
    URBAN_ODD_SPEED_CAP_MPS, VehicleKinematicsContract,
};

let contract = VehicleKinematicsContract {
    max_speed_mps:           2.0,                              // small mobile robot
    odd_speed_cap_mps:       Some(URBAN_ODD_SPEED_CAP_MPS),    // 22.35 (won't bind on this platform)
    max_accel_mps2:          1.5,
    max_brake_mps2:          2.0,
    max_steering_deg:        45.0,
    max_steering_rate_deg_s: 90.0,
    min_follow_distance_m:   0.3,
    max_lateral_accel_mps2:  2.0,
    wheelbase_m:             0.2,
    width_m:                 0.5,
    length_m:                0.6,
    overhang_front_m:        0.2,
    overhang_rear_m:         0.2,
};
```

### 8.3 Angular bound (PlatformParams)

```rust
use parko_kirra::PlatformParams;

let platform = PlatformParams::urban_service_robot_reference();
// Or build your own:
// let platform = PlatformParams {
//     track_width_m:      0.50,
//     cog_height_m:       0.40,
//     robot_extent_m:     0.30,
//     v_edge_safe_mps:    0.25,
//     theta_max_rad:      0.087,
//     ftti_s:             0.10,
//     mrc_posture_factor: 0.5,
// };
```

This produces `ω_max(0) ≈ 0.833 rad/s` and `ω_max(v=5 m/s) ≈ 0.833`
(sweep binds across the operating range; see
`docs/safety/ANGULAR_VELOCITY_SOTIF.md` §4 for the worked table).

### 8.4 Governor + InferenceLoop

```rust
use parko_kirra::{GovernorComparator, KirraGovernor};
use parko_ros2::ComparatorAsGovernor;

let primary = KirraGovernor::new().with_platform_params(platform.clone());
let shadow  = KirraGovernor::new().with_platform_params(platform);
let comparator = GovernorComparator::new(primary, shadow);

// InferenceLoop construction lives in the binary's `build_loop`:
// let infer = InferenceLoop::new(backend, model, actuator_tx)
//     .with_governor(ComparatorAsGovernor(comparator))
//     .with_tick_period(0.05);
```

### 8.5 Run

```bash
# CPU/ONNX path
export PARKO_MODEL_PATH=/path/to/policy.onnx
export ORT_DYLIB_PATH=$HOME/.local/onnxruntime/lib/libonnxruntime.so
source /opt/ros/humble/setup.bash

cd parko && cargo run --bin parko_ros2_node \
    --features ros2,onnx-backend
```

The binary then subscribes to `~/input/observation`, drives the
loop at 20 Hz, and publishes gated `geometry_msgs/Twist` to
`~/output/cmd_vel`. Posture defaults to `Nominal` until M2c wires
the live source.

### 8.6 What the governor will do

For a model that emits `(cmd_vel_linear=1.0, cmd_vel_angular=0.5)`
at this platform configuration:

- Linear 1.0 m/s is under the platform's 2.0 m/s `max_speed_mps`
  AND under the 22.35 m/s ODD cap → linear axis passes.
- Angular 0.5 rad/s is under the urban-reference 0.833 rad/s
  sweep bound → angular axis passes.
- Verdict: `EnforcementAction::Allow`. The Twist published to
  `~/output/cmd_vel` carries `linear.x = 1.0`, `angular.z = 0.5`.

If the model emits `(2.5, 0.5)` instead, the linear axis exceeds
the 2.0 m/s `max_speed_mps` and the governor returns
`ClampLinearVelocity(2.0)`; the published Twist carries `linear.x = 2.0`,
`angular.z = 0.5`. A retrained policy that stops asking for 2.5 m/s
is the right response — the governor is the safety net, not a
substitute for a well-trained model.

---

## 9. Accuracy cross-check

Every interface named in this document was confirmed against the
code at branch `feat/parko-integration-spec`, off main `8eadf83`.

**Confirmed (in code today):**

| Name | Location |
|---|---|
| `InferenceBackend` trait, `load_model`, `run`, `capabilities`, `descriptor` | `parko/crates/parko-core/src/backend.rs:113` |
| `OrtBackend` | `parko/crates/parko-onnx/src/lib.rs:16` |
| `OvBackend` | `parko/crates/parko-openvino/src/lib.rs::OvBackend` |
| `SensorInputMapping` trait, `to_frame` | `parko/crates/parko-ros2/src/sensor_mapping.rs:24` |
| `CameraMapping`, `CameraConfig` (encoding / target_height / target_width / resize / normalization / layout / tensor_name) | same file `:208`, `:281` |
| `CameraEncoding::{Rgb8, Bgr8, Mono8}`; `CameraNormalization::{Unit01, SignedUnit, MeanStd}`; `CameraLayout::{Nchw, Nhwc}`; `CameraResize::Nearest` | same file |
| `OdomMapping`, `OdomConfig`, `OdomOrientation::{Yaw, FullEuler, Quaternion}` | same file `:455`, `:485` |
| `OutgoingTwist`, `enforce_outgoing_twist` | `parko/crates/parko-ros2/src/command_mapping.rs:21`, `:52` |
| `parse_inference_to_command` reading `cmd_vel_linear` + `cmd_vel_angular` | `parko/crates/parko-core/src/scheduler.rs:295`, `:302` |
| `KirraGovernor`, `with_platform_params`, `with_angular_bounds`, `update_rss_state` | `parko/crates/parko-kirra/src/lib.rs` |
| `GovernorComparator::new`, `evaluate`, `reconcile` | `parko/crates/parko-kirra/src/comparator.rs` |
| `ComparatorAsGovernor` newtype | `parko/crates/parko-ros2/src/comparator_adapter.rs` |
| `PlatformParams` + `conservative_default()` + `urban_service_robot_reference()` + `AngularVelocityBound` | `parko/crates/parko-kirra/src/angular_bound.rs:50`, `:191` |
| `URBAN_ODD_SPEED_CAP_MPS = 22.35` | `src/gateway/kinematics_contract.rs:43` |
| `MRC_VELOCITY_CEILING_MPS = 5.0` | `parko/crates/parko-kirra/src/lib.rs:54` |
| `VehicleKinematicsContract` field set (max_speed_mps / odd_speed_cap_mps / max_accel_mps2 / max_brake_mps2 / max_steering_deg / max_steering_rate_deg_s / min_follow_distance_m / max_lateral_accel_mps2 / wheelbase_m / width_m / length_m / overhang_front_m / overhang_rear_m) | `src/gateway/kinematics_contract.rs:54` |
| `VehicleConfig::default_urban`, `warn_if_missing_odd_cap` | `crates/kirra-ros2-adapter/src/config.rs` |
| `PostureTracker`, `POSTURE_STALENESS_TIMEOUT_MS = 6_000` | `src/posture_tracker.rs:57` |
| `ParkoPostureState`, `fleet_to_safety` | `parko/crates/parko-ros2/src/posture_state.rs` |
| `ParkoNodeConfig` fields (sensor_topic / command_topic / tick_period_s / sensor_staleness_budget_ms) | `parko/crates/parko-ros2/src/config.rs:13` |
| Default topics `~/input/observation`, `~/output/cmd_vel` | same file `:57`, `:58` |
| Wire types `std_msgs/msg/Float32MultiArray` (sensor), `geometry_msgs/msg/Twist` (command) | `parko/crates/parko-ros2/src/node.rs:71`, `:78` |
| Env vars `KIRRA_POSTURE_STREAM_URL`, `KIRRA_ADMIN_TOKEN`, `KIRRA_POSTURE_CLIENT_ID`, `PARKO_MODEL_PATH`, `ORT_DYLIB_PATH`, `OPENVINO_LIB_PATH` | adapter binary `:319-336`; parko-ros2 binary `:131` |
| `classify_posture_source`, `PostureSourceDecision::{NoSource, Live, ConfiguredNoTransport}` | adapter binary `:287` ff |
| `TickError::{StaleSensorInput, InferenceError}` | `parko/crates/parko-ros2/src/tick_pipeline.rs:43` |
| `EnforcementAction::{Allow, ClampLinearVelocity, ClampAngularVelocity, ClampMotion, Deny}` | `parko/crates/parko-core/src/safety.rs` |
| `SafetyPosture::{Nominal, Degraded, LockedOut}` | same file |
| `run_pipeline_tick`, `run_pipeline_tick_with_posture_state` | `parko/crates/parko-ros2/src/tick_pipeline.rs:85`, `:109` |
| `parko_ros2::node::run_node` | `parko/crates/parko-ros2/src/node.rs:44` |

**Marked as planned, NOT in code today:**

- Live posture transport wired into the parko-ros2 binary
  (status confirmed via
  `parko/crates/parko-ros2/src/bin/parko_ros2_node.rs:140`: the
  binary still passes `SafetyPosture::Nominal` statically).
- Hardware inference backends (`BackendDescriptor::{TensorRT,
  QualcommQnn, TiTidl, AmdVitis}` enum variants exist in
  `parko-core::backend`; **no `InferenceBackend` impls**).
- `CameraResize::Bilinear` (enum has `Nearest` only today).
- `sensor_msgs/Image` and `nav_msgs/Odometry` ROS extraction shims
  (the wire format the parko-ros2 node currently reads is the JSON
  `Float32MultiArray` shape — see node.rs).

**No invented APIs.** Every type/field/method/env var/topic in §§
1–8 either points at the code via the table above or is explicitly
flagged "planned" in §7.

---

## 10. Document control

| Field | Value |
|---|---|
| Author | engineering (Parko team) |
| Status | First-edition developer guide |
| Cross-refs | `docs/safety/ANGULAR_VELOCITY_SOTIF.md`, `docs/safety/SPEED_ENVELOPE.md`, `docs/safety/PARKO_OCCY_TOPOLOGY.md` |
| Code branch at time of writing | `feat/parko-integration-spec` (off `main@8eadf83`) |
