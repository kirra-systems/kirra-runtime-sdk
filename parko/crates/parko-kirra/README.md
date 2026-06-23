# parko-kirra

A `SafetyGovernor` implementation for parko-core, backed by the Kirra runtime SDK's vehicle kinematics contract.

## What this does

Implements parko-core's `SafetyGovernor` trait by calling `kirra_verifier::gateway::kinematics_contract::validate_vehicle_command` with the active vehicle kinematics contract profile (nominal or MRC fallback).

When attached to a `parko_core::InferenceLoop` via `with_governor()`, proposed control commands are evaluated against the Kirra contract before being passed to actuators. Commands exceeding the contract's bounds are clamped to safe values; commands violating hard invariants result in a full stop.

## Usage

```rust
use std::sync::Arc;
use tokio::sync::mpsc;
use parko_core::backend::ModelHandle;
use parko_core::scheduler::InferenceLoop;
use parko_kirra::KirraGovernor;

let backend = Arc::new(/* your InferenceBackend */);
let model: ModelHandle = backend.load_model("model.onnx").unwrap();
let (tx, _rx) = mpsc::channel(10);

let governor = KirraGovernor::nominal();
let loop_engine = InferenceLoop::new(backend, model, tx)
    .with_governor(Box::new(governor));
```

`KirraGovernor::nominal()` uses the `nominal_reference_profile`. For degraded operation, use `KirraGovernor::mrc_fallback()`. For posture-driven selection, use `KirraGovernor::for_posture(posture)`.

## Important limitation: angular velocity is not enforced

parko's `ControlCommand` uses a differential-drive Twist model: (`linear_velocity`, `angular_velocity`) in m/s and rad/s. Kirra's `ProposedVehicleCommand` uses a bicycle/Ackermann model: (`linear_velocity_mps`, `steering_angle_deg`). These are semantically different control representations measured in different units.

This adapter currently enforces only the linear velocity dimension. The steering angle dimension is set to zero in the call to Kirra. This means angular velocity bounds are not currently checked by this governor.

A differential-drive robot that requires angular velocity bounds enforcement should either:

1. Add a second governor specifically for angular velocity bounds, composed with this one.
2. Extend this adapter with a wheelbase-dependent kinematic bicycle conversion from `angular_velocity` to effective `steering_angle_deg`. This requires the robot's wheelbase as a configuration parameter and introduces conversion error.

Option 2 is future work.

## What's bridged and what isn't

**Bridged:**
- Linear velocity proposed-vs-current bound checking
- Linear acceleration bound checking (Kirra computes from `current_velocity_mps` and `delta_time_s`)
- Both nominal and MRC fallback contract profiles
- Posture-driven profile selection via `KirraGovernor::for_posture()`

**Not bridged:**
- Angular velocity / steering angle dimension (see above)
- Kirra's audit chain, posture engine, federation, attestation
- Kirra's `LockedOut` posture handling — `for_posture(LockedOut)` falls back to MRC profile, but commands should not be routed at all in `LockedOut` state; this is the caller's responsibility

## Tests

```bash
cd parko && cargo test -p parko-kirra 2>&1
```

The integration tests verify:
- Low-velocity commands pass through as `Allow`
- Excessive linear velocity (65.0 m/s) is clamped to a value within bounds
- The MRC fallback profile constructs and evaluates without panic

The tests use property-based assertions (e.g., "clamped value is strictly less than input") rather than asserting specific numeric values, so they remain correct if the underlying Kirra contract profiles are tuned.

## License

Apache-2.0
