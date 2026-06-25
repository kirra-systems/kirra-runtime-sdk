# Occy doer bridge — drive the robot to a goal, governed by KIRRA

This is the **doer**: the piece that makes real Occy drive the robot to a goal, with Taj
perceiving and KIRRA governing. It closes the loop "Mick/Occy proposes → KIRRA disposes" on
hardware. The five pieces working together (Mick · Taj · Occy · KIRRA, with Parko as the
Phase-2 perception upgrade):

```
  goal (RViz / Mick) ─/goal_pose─┐
  /odom (pose, speed) ───────────┤
  /scan (lidar) ──▶ Taj :8101 (corridor + objects) ─┐
                                 ▼                   ▼
                         occy_doer ──▶ Occy :8100 /plan (proposes + KIRRA slow-loop)
                                 │
                          pure-pursuit → Twist
                                 ▼
            /cmd_vel_raw ─▶ cmd_vel_interceptor [Taj speed cap + KIRRA fast-loop] ─/cmd_vel─▶ wheels
```

Each tick (`occy_doer`, default 5 Hz):
1. read the robot pose + speed (`/odom`) and the current goal (`/goal_pose`),
2. POST the latest scan to **Taj** → the geometric corridor (left/right polylines) + objects,
3. transform the goal into the base frame, extend the corridor behind the robot (footprint
   containment), and POST `{ego, goal, corridor, objects, vehicle}` to **Occy** `/plan`,
4. turn Occy's **KIRRA-validated** trajectory into a Twist (pure pursuit) on `/cmd_vel_raw`.

**Occy only PROPOSES; KIRRA DISPOSES — twice:** the planner runs the slow-loop checker
(`validate_trajectory_slow`) and returns a verdict; the `cmd_vel_interceptor` then re-checks
every command with the fast-loop kinematic governor + the Taj speed cap. The doer is
**fail-soft**: no goal, a stale scan, a service error, or a refused plan all publish a zero
Twist (hold) — and even if it didn't, the interceptor + governor are the safety authority.

## Run it

```bash
# sidecars + verifier (systemd, or scripts/orin_bringup.sh --serve, or the launch starts them)
ros2 launch kirra_safety kirra_with_robot.launch.py \
    kirra_token:=$KIRRA_ADMIN_TOKEN \
    use_occy_doer:=true use_perception_cap:=true
# then publish a goal — in RViz click "2D Goal Pose", or:
ros2 topic pub --once /goal_pose geometry_msgs/PoseStamped \
    '{header: {frame_id: odom}, pose: {position: {x: 2.0, y: 0.0}}}'
```

The robot drives toward the goal down the clear corridor and stops on arrival (or before an
obstacle). Prereqs: the Yahboom/Rosmaster base + lidar drivers (publishing `/scan`, `/odom`,
subscribing `/cmd_vel`), and the Occy planner sidecar (+ Taj). The launch starts the Rust
sidecars itself unless `start_sidecars:=false`.

## Robot sizing (important)

The checker judges a **vehicle footprint**. The planner's default is an urban car (4.8 m) —
which cannot fit a robot-scale lidar corridor, so KIRRA would MRC every plan. `occy_doer`
therefore tells the planner the robot's real size via the `/plan` request's `vehicle` block.
Defaults are Rosmaster-class; tune them to your chassis:

| param | default | meaning |
|---|---|---|
| `wheelbase_m` | 0.2 | axle-to-axle |
| `half_length_m` / `half_width_m` | 0.18 / 0.15 | bumper-to-centre half extents |
| `max_speed_mps` | 1.2 | doer cruise / checker max |
| `max_steering_deg` | 30 | steering limit (Ackermann) |
| `corridor_back_m` | 0.5 | how far to extend the corridor behind the robot (footprint containment) |
| `lookahead_m` | 0.8 | pure-pursuit lookahead |
| `vehicle_class` | `courier` | per-class checker profile (`courier` = small robot, `robotaxi` = the frozen AV) |
| `rss_lateral_alignment_tolerance_m` | 0.6 | per-class RSS lateral band — robot "lane" width, not the car's 4 m |
| `lateral_clearance_target_m` | 0.6 | how much room the DOER (Occy) demands before proposing a pass |

The `vehicle_class` selects a **sibling profile** in the checker via the single
`VehicleConfig::for_class()` selector (`courier` / `delivery-av` / `robotaxi`), the slow-loop
counterpart of the fast-loop `VehicleClass` — per [`docs/CONTRACT_PROFILES.md`](../CONTRACT_PROFILES.md)
and **[ADR-0028](../adr/0028-sidewalk-courier-odd.md)** (the sidewalk-courier ODD: a pedestrian-space
class, not a shrunk car — creep + assured-clear-distance + impact-energy, *not* RSS car-following).
The robotaxi numbers are **frozen and unchanged** (proven by
`default_urban_rss_band_is_the_frozen_robotaxi_value`), so the courier profile **cannot regress the
AV path** — the only difference is the numbers.

## What it does today (honest scope)

Verified end to end (real `taj_service` + `planner_service` + the real `doer_core` decision):

| scene | result |
|---|---|
| clear corridor, goal ahead | **DRIVE** ~1.2 m/s (Occy `Motion`/`Clamp`) |
| obstacle dead-ahead | **HOLD** — Occy proposes a controlled stop |
| bending corridor | Occy **proposes** the turn (`path_maxy≠0`) but the car-tuned checker conservatively MRCs it at robot scale → **HOLD** (fail-closed) |

So today the doer **drives straight down clear corridors and stops before obstacles** —
exactly the right first-hardware behavior (the robot moves and is safe). `GoTo` tracks the
drivable **corridor centerline**, so it does not beeline to an off-axis goal; turning follows
the corridor.

**Per-class checker profile (done).** The slow-loop checker's RSS lateral band is now a
per-class number (`VehicleConfig::courier()` 0.6 m vs robotaxi 4.0 m), proven end to end: for
a side object 0.8 m off the path, the **robotaxi verdict is `MRCFallback` (refused) and the
courier verdict is `Accept` (admitted)** — same scene, same checker logic — while the robotaxi
number stays frozen (`courier_admits_a_side_object_a_robotaxi_refuses`). So the small robot can
now be *judged* as a robot, not a 4.8 m car.

**Doer-side robot tuning (done).** `GeometricPlannerConfig::courier()` is the robot-scale planner
preset (ADR-0028): Occy stops ~1 m short of an in-path object (the Yield standoff, not the car's
5 m) and routes around with the courier's ~0.7 m clearance (not 4.5 m). `planner_service` selects
it for `class:"courier"`, so the courier now *proposes* robot-scale motion the car default never
would, and the per-class checker admits it. (Bend-FOLLOWING still needs Phase-B perception — Taj
Phase A only makes straight corridors.)

## Where Mick and Parko plug in (Phase 2)

- **Mick (the LLM brain):** instead of an RViz goal, Mick publishes the goal/intent — the
  doer is intent-source-agnostic. A richer `/plan` that takes a typed `MickIntent`
  (RouteTo/TurnAt) would let the LLM command turns at junctions.
- **Parko (the ML detector):** its semantic objects feed the same `objects` list the doer
  already passes to the planner — richer and longer-range than Taj's geometric clusters.
  The doer's seam is unchanged; Parko is hardware/model-gated bring-up.
