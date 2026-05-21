# Aegis v2.0.0 — AV Kinematics Flight Envelope Protection

This document freezes the deterministic, physical safety invariants used to gate motion
planning outputs before they reach vehicle actuators. Aegis does not generate trajectories;
it intercepts proposed commands and either passes, clamps, or drops them based on hard
physical contracts.

---

## 1. Safety Kernel Invariants

Aegis operates on an uncompromising execution contract for every `ProposedVehicleCommand`
that arrives at the actuator policy layer:

- **Passive Monitoring vs. Active Enforcement**: Aegis does not generate trajectories; it
  intercepts proposed `CmdVel` or actuator targets. The perception and planning stacks own
  their own reliability; Aegis owns admissibility.

- **Fail-Closed Clamping**: Commands exceeding hard limits are either smoothly clamped to
  the maximum safe boundary (`EnforceAction::Clamp*`) or dropped entirely
  (`EnforceAction::DenyBreach`), depending on the severity of the violation and active fleet
  posture rules. The system never silently passes a physically inadmissible command.

- **Dynamic Lateral Envelope**: Maximum allowable steering angles are decoupled from static
  thresholds and vary dynamically based on current forward velocity to prevent high-speed
  rollover events. The bicycle model constraint `a_lat ≤ threshold` is evaluated per-command,
  not per-configuration.

- **Posture-Aware Profile Selection**: The active `VehicleKinematicsContract` is selected by
  the fleet posture engine, not by the planner. A degraded sensor node propagates upward
  through the dependency DAG and forces the vehicle into the MRC profile automatically.

---

## 2. Bicycle Model Lateral Acceleration

The dynamic steering envelope uses the standard kinematic bicycle model approximation:

```
a_lat = (v² × |tan(δ)|) / L
```

Where:
- `v`     = forward velocity (m/s)
- `δ`     = front wheel steering angle (radians)
- `L`     = vehicle wheelbase (m)
- `a_lat` = implied lateral acceleration (m/s²)

When `a_lat > max_lateral_accel_mps2`, the steering angle is back-solved to the maximum
safe value for the current velocity:

```
δ_max = atan((a_lat_max × L) / v²)
```

This is enforced even if the absolute steering angle is within `max_steering_deg`, because
a geometrically valid steering angle becomes dynamically unsafe at high speed.

---

## 3. Verification Pipeline Order

Every `validate_vehicle_command` call runs checks in a strict, ordered sequence. A check
that fires returns immediately — later checks are not evaluated. The order is intentional:
non-physical inputs are rejected before physics-based checks, and linear bounds are
evaluated before lateral bounds.

| Priority | Check | Action on Breach |
| :---: | :--- | :--- |
| 1 | Non-physical time delta (`dt ≤ 0`) | `DenyBreach("INVALID_TIME_DELTA")` |
| 2 | Linear velocity exceeds `max_speed_mps` | `ClampLinear(max × sign(v))` |
| 3 | Implied acceleration exceeds `max_accel_mps2` | `ClampLinear(safe_speed)` |
| 4 | Implied deceleration exceeds `max_brake_mps2` | `ClampLinear(safe_speed)` |
| 5 | Steering rate exceeds `max_steering_rate_deg_s` | `ClampSteering(safe_angle)` |
| 6 | Bicycle model `a_lat > max_lateral_accel_mps2` | `ClampSteering(dynamic_max)` |
| — | All checks pass | `Allow` |

---

## 4. Posture Dependency Mapping

The kinematic allowance matrix maps directly to the system's runtime posture state.
Profile selection is performed by the actuator policy layer, which reads `SharedPostureCache`
on each command evaluation.

| Posture State | Kinematics Profile Active | Action on Envelope Breach |
| :--- | :--- | :--- |
| `Nominal` | `nominal_reference_profile()` | Smooth clamping to profile limits |
| `Degraded` | `mrc_fallback_profile()` | Hard clamping to MRC safe-haven values |
| `LockedOut` | No profile evaluated | Command dropped; actuators commanded to safe stop |

**MRC Fallback Profile Rationale**: The Minimal Risk Condition profile reduces `max_speed_mps`
to 5.0 m/s (~11 mph) and `max_lateral_accel_mps2` to 1.5 m/s², constraining the vehicle to
a controlled, low-energy state from which a graceful stop can always be achieved. It does not
bring the vehicle to an immediate halt; it creates a bounded degradation corridor toward
safe haven.

---

## 5. Safe Stop vs. MRC vs. Emergency Stop

These three concepts are distinct:

- **Safe Stop** (`LockedOut`): Commanded by Aegis when the fleet posture is `LockedOut`.
  All actuator write commands are dropped. The vehicle transitions to a controlled halt.
  Not a crash stop — braking is still governed by `max_brake_mps2`.

- **MRC (Minimal Risk Condition)** (`Degraded`): The vehicle continues to operate but under
  the `mrc_fallback_profile()`. Planning continues; Aegis enforces the tighter envelope.
  The system can recover to `Nominal` if degraded nodes recover.

- **Emergency Brake** (external, out of scope): A separate hard-wired electrical interlock
  layer. Aegis does not replace this; it operates upstream of it.

---

## 6. Out-of-Scope Invariants

The following safety concerns are explicitly **not** handled by the kinematics contract
module and belong in other system layers:

- **Obstacle detection and collision avoidance** — perception + planning stack responsibility
- **Lane boundary enforcement** — HD map + trajectory planner responsibility
- **Traffic law compliance** — behavioral planning stack responsibility
- **Emergency braking (hardware interlock)** — physical brake controller, not software

Aegis enforces that the command is *physically admissible for the vehicle platform*. Whether
the command is *contextually correct for the environment* is the planner's problem.

---

## 7. Profile Definitions (Reference)

### Nominal Reference Profile

| Parameter | Value | Rationale |
| :--- | :--- | :--- |
| `max_speed_mps` | 35.0 (~78 mph) | Upper operational bound for highway driving |
| `max_accel_mps2` | 2.5 | Comfortable linear acceleration, ~0.25g |
| `max_brake_mps2` | 4.5 | Service braking limit; emergency layer separate |
| `max_steering_deg` | 35.0 | Maximum low-speed wheel articulation |
| `max_steering_rate_deg_s` | 45.0 | Physical steering rack rate limit |
| `min_follow_distance_m` | 2.0 | Absolute close-proximity buffer |
| `max_lateral_accel_mps2` | 3.5 | ~0.36g lateral G limit, prevents rollover/skid |
| `wheelbase_m` | 2.8 | Standard mid-size vehicle wheelbase |

### MRC Fallback Profile

| Parameter | Value | Rationale |
| :--- | :--- | :--- |
| `max_speed_mps` | 5.0 (~11 mph) | Safe, limp-home crawling speed |
| `max_accel_mps2` | 1.0 | Highly subdued acceleration curve |
| `max_brake_mps2` | 3.0 | Gradual slowdown profile |
| `max_steering_deg` | 15.0 | Restricts high-amplitude maneuvering |
| `max_steering_rate_deg_s` | 20.0 | Slow, deliberate steering changes only |
| `min_follow_distance_m` | 5.0 | Expanded safety margins during degradation |
| `max_lateral_accel_mps2` | 1.5 | ~0.15g, minimizes side-slip risk |
| `wheelbase_m` | 2.8 | Unchanged (physical constant) |

---

## 8. Extension Points

The following extensions are planned for subsequent milestones but intentionally excluded
from v2.0.0 to keep the safety kernel small and auditable:

- **Multi-axle and articulated vehicle profiles** — separate `wheelbase_m` per axle group
- **Road surface coefficient integration** — dynamic `max_lateral_accel_mps2` scaling via μ
- **Speed zone map injection** — geo-fenced `max_speed_mps` override per route segment
- **Tire model integration** — replace bicycle model approximation with Pacejka Magic Formula
  for high-fidelity lateral force estimation

These will be introduced as additive, non-breaking extensions to `VehicleKinematicsContract`.
