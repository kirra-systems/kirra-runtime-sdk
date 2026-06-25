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
the corridor. **Turning to follow a bend / routing around an obstacle needs robot-scaled
RSS/containment constants** in the checker (currently car-tuned) — a tracked follow-up — plus,
for bends, Phase-B perception (Taj Phase A only makes straight corridors).

## Where Mick and Parko plug in (Phase 2)

- **Mick (the LLM brain):** instead of an RViz goal, Mick publishes the goal/intent — the
  doer is intent-source-agnostic. A richer `/plan` that takes a typed `MickIntent`
  (RouteTo/TurnAt) would let the LLM command turns at junctions.
- **Parko (the ML detector):** its semantic objects feed the same `objects` list the doer
  already passes to the planner — richer and longer-range than Taj's geometric clusters.
  The doer's seam is unchanged; Parko is hardware/model-gated bring-up.
