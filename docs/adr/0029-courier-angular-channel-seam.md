# ADR-0029: Courier angular channel — binding the diff-drive yaw bound at the VehicleConfig ↔ S-PK1 seam

| Field | Value |
|---|---|
| Status | **Proposed (design note)** — ratified on merge. |
| Date | 2026-06-25 |
| Deciders | Project / safety-case owner |
| Safety goals | **SG3** (per-command kinematic envelope — extends it to the *angular* channel a bicycle model cannot express); **SG8** (degraded / converge-to-zero — adds the angular no-re-initiation analogue); **SG2** (containment is already drive-agnostic via S-PK1c — unchanged) |
| Cross-refs | ADR-0028 (sidewalk-courier ODD); ADR-0027 (platform-kinematics abstraction, S-PK1); `docs/safety/STAGE_S-PK1_PLATFORM_KINEMATICS.md`; `docs/safety/ASSUMPTIONS_OF_USE.md` (AOU-PLATFORM-GEOMETRY-001 — the DEPLOYMENT-PENDING this seam converts); `docs/CONTRACT_PROFILES.md` (cited-copy single-source rule); `VehicleConfig::courier()` (`crates/kirra-ros2-adapter/src/config.rs`); `DiffDrivePlatform` (`parko/crates/parko-kirra/src/platform.rs`); `AngularVelocityBound` / `STOP_EPSILON_RAD_S` (`parko/crates/parko-kirra/src/angular_bound.rs`, `lib.rs`) |

## Context

Two complementary work streams now describe the **same** sidewalk-courier robot, on **two
different axes**, with **no connection between them**:

- **#560 / ADR-0028 — `VehicleConfig::courier()`** (`crates/kirra-ros2-adapter`). The slow-loop
  checker's per-class profile: footprint **0.6 × 0.9 m**, wheelbase 0.5 m, max 3.0 / ODD 2.5 m/s,
  RSS lateral band 0.6 m. It covers the **spatial / scalar** side — SG2 containment (footprint),
  the linear envelope, and the RSS band. Correct for the behaviour shipped so far.
- **S-PK1b / ADR-0027 — `DiffDrivePlatform`** (`parko/crates/parko-kirra`). The differential-drive
  sibling under the `PlatformKinematics` abstraction: an explicit **angular-velocity** channel
  (`ClampAngularVelocity`, SOTIF-derived `AngularVelocityBound::omega_max(v)`, issue #136) and a
  native angular converge-to-zero gate (`STOP_EPSILON_RAD_S = 0.02`). It covers the **rotational**
  side — the axis a bicycle model cannot bound.

### The finding — the courier's angular channel is silently unbounded at creep / zero speed

`VehicleConfig::courier()` models the Rosmaster as a **small Ackermann bicycle**. The slow-loop
per-pose check (`validate_trajectory_slow` → `pose_pair_to_command` → `validate_vehicle_command`)
derives a steering angle from the bicycle model:

```
steering = atan2(Δheading · wheelbase,  v · Δt)
```

This is **undefined when `v ≈ 0` and `ω ≠ 0`** (in-place rotation: zero turning radius). The code
guards the singular denominator by **falling back to `steering = 0`** — so a courier turning in
place, or yawing hard while creeping, produces a `ProposedVehicleCommand` that looks like
*"stopped, going straight"*. `validate_vehicle_command` admits it, and the lateral-accel bound
(`a_lat = v²·tan(δ)/L`) is `0` at `v = 0` regardless. **The heading change is dropped; the angular
axis has no checker.** `VehicleConfig` today carries **no angular / yaw field at all**.

This is invisible for the behaviour shipped so far — #560's honest scope is "drives straight down
clear corridors and stops", and going straight the bicycle model degenerates fine (`δ = 0`). It
becomes a real, exercised gap the moment the Rosmaster **turns in place** — which, as a
differential-drive courier, is its primary cornering and yielding maneuver.

A nice tell that this is two unconnected representations of one robot: #560's courier footprint
`0.6 × 0.9` is **exactly** the `DiffDrivePlatform::centered_footprint(0.6, 0.9)` used in the
S-PK1b tests. Same robot, two halves, no seam.

## Decision

Adopt a **two-channel courier checker**: the courier's safety verdict is the **conjunction** of

1. **containment + RSS + linear envelope ← `VehicleConfig::courier()`** (unchanged — #560 is correct
   for these axes), and
2. **the angular (yaw-rate) channel ← the differential-drive model** — `AngularVelocityBound`'s
   velocity-dependent `omega_max(v)` plus the `STOP_EPSILON_RAD_S` converge-to-zero gate (parko's
   S-PK1b model of record, SOTIF-derived #136).

The bicycle model is retained for steering/lateral-accel **at speed** (where it is valid); the
diff-drive yaw bound is the authority on the **angular channel**, including the `v ≈ 0` in-place
regime the bicycle model drops.

### The cited-copy correspondence (one robot, two workspaces)

`crates/kirra-ros2-adapter` (SDK) cannot import `parko/` and vice-versa — they are
dependency-separated diverse-governor workspaces. Per the `docs/CONTRACT_PROFILES.md`
single-source-of-truth rule, the per-class numbers travel as **cited copies**, never imports. The
diff-drive **model of record** is parko's `AngularVelocityBound` (#136); the SDK courier checker
holds a cited copy of its parameters and `omega_max(v)` derivation, tagged with the source.

| Quantity | `VehicleConfig::courier()` (SDK) | `DiffDrivePlatform` / parko (record) |
|---|---|---|
| Footprint | 0.6 × 0.9 m | `centered_footprint(0.6, 0.9)` |
| Max linear speed | 3.0 m/s (ODD cap 2.5) | `max_speed_mps` (courier ≈ 1.5) |
| Linear stop epsilon | `STOP_EPSILON_MPS` | `stop_epsilon_mps` (0.05) |
| **Angular bound `ω_max(v)`** | **(new — cited copy)** | `AngularVelocityBound::urban_service_robot_reference()` (#136) |
| **Angular stop epsilon** | **(new — cited copy)** | `STOP_EPSILON_RAD_S = 0.02` |

### Phased realization

**Phase 1 — close the silent-drop gap in the SDK courier checker (testable here, frozen-AV-safe).**
- Add an **optional** diff-drive angular bound to `VehicleConfig` (e.g. `angular: Option<…>` carrying
  the `omega_max(v)` parameters + `stop_epsilon_rad_s`). `courier()` sets it; the Ackermann profiles
  (`default_urban` / `delivery_av`) leave it **`None`** → the per-pose path is **byte-identical** to
  today, so the **robotaxi / AV path stays frozen**.
- In `validate_trajectory_slow`, when the angular bound is `Some`, add a per-segment check on
  `ω = Δheading / Δt`: refuse `|ω| > ω_max(v_segment)`, and apply the angular converge-to-zero /
  no-re-initiation rule under Degraded — mapped onto the existing slow-loop verdict (`Clamp` /
  `MRCFallback`), fail-closed. The bicycle steering/lateral-accel check is unchanged.
- Tests: in-place rotation at a sane `ω` → admitted (now *checked*, not silently passed); in-place
  rotation at an excessive `ω` → refused (today's bug); a `default_urban` trajectory verdict
  **byte-identical** to before (frozen proof). SAFETY tag + regenerate the traceability matrix.

**Phase 2 — the live diff-drive deployment (converts AOU-PLATFORM-GEOMETRY-001 DEPLOYMENT-PENDING). LANDED.**
- Wire the ROS node so the Rosmaster's `(v, ω)` `cmd_vel` is bounded by parko's `DiffDrivePlatform`
  as the **live per-command checker** (the angular model of record) and by `validate_platform_containment`
  for SG2 (already proven), supplied the courier footprint/limits. This is the production node S-PK1
  named as pending; it lives in the ROS runtime and is not unit-testable in CI without ROS.
- **Realization** (`parko/crates/parko-ros2`): a new `CourierPlatformProfile` (`platform_profile.rs`)
  holds the courier's footprint + the SOTIF angular `PlatformParams` and builds the courier-parameterized
  `KirraGovernor` (`with_platform_params`) and the `DiffDrivePlatform<KirraGovernor>` checker. The finding
  Phase 2 surfaced: the angular bound was **already enforced live** (`KirraGovernor::nominal_angular_clamp`
  → `omega_max(v)`), but the stock node built `KirraGovernor::new()` = `PlatformParams::conservative_default()`
  (ω_max(0) ≈ 0.20 rad/s) — a generic bound for an *uncharacterized* platform. The profile parameterizes
  the live governor (BOTH comparator arms, so they agree by construction) with the courier's
  `urban_service_robot_reference()` envelope (ω_max(0) ≈ 0.833 rad/s). `ParkoNodeConfig::platform_profile`
  is `Option<…>`, **default `None`** → the node keeps the conservative default, byte-identical to
  pre-Phase-2 (fail-safe: an unprofiled deployment gets the *tighter* generic bound). The pure profile +
  config are unit-tested in parko's stable lane (in-place-rotation admit/clamp at the courier bound, the
  SG2 containment seam, the profile-vs-default envelope gap); only the ~10-line node-binary governor swap
  is `required-features = ["ros2"]` and built by the ROS CI lane.
- **Cited-copy correspondence:** in `parko/` we are the **model of record** — the profile uses parko's own
  `urban_service_robot_reference()` directly (no copy). The SDK `VehicleConfig::courier()` holds the cited
  COPY (Phase 1), gated by `courier_angular_bound_matches_parko_record`; the 0.6 × 0.9 m footprint matches
  both sides.

Phase 1 is the concrete next code step. Phase 2 is the deployment it unblocks; both share the same
cited-copy angular numbers, so they cannot diverge silently.

## Consequences

- The courier checker becomes **honest on the angular axis**: an in-place rotation is bounded by a
  diff-drive yaw model, not silently dropped by a degenerate bicycle term.
- The **AV / robotaxi path is untouched and frozen** — Ackermann profiles carry **no** angular bound
  (`None`), so their per-pose verdict is byte-identical; the existing frozen-value test is extended
  to assert the robotaxi has no angular channel.
- Conformant to the doer-checker invariants (CLAUDE.md): purely **additive** (the Nominal
  WCET-critical `validate_vehicle_command` path is unchanged), **fail-closed** (absent angular
  config → today's behaviour; a fault → an MRC-floor verdict, never a relaxation), and
  **cited-copy** across the workspace boundary (no new cross-workspace import).
- Follow-ups (tracked, not in this ADR): Phase 2 deployment node; whether the SDK should copy the
  full `omega_max(v)` derivation vs a conservative scalar ceiling for the slow loop; a CI
  equivalence check that the SDK cited copy still matches parko's `AngularVelocityBound` record.

## Status

**Accepted — ratified on merge** (as with ADR-0011..0028). Records the angular-channel seam, the finding,
and the phased plan. **Phase 1** (the SDK angular bound) landed in #563. **Phase 2** (the live parko-ros2
`CourierPlatformProfile` + `DiffDrivePlatform` checker) is realized here; the ros2-gated node-binary swap
is the deployment step built by the ROS CI lane.
