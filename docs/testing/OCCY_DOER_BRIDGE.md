# Occy doer bridge — drive the robot to a goal, governed by KIRRA

This is the **doer**: the piece that makes real Occy drive the robot to a goal, with Taj
perceiving and KIRRA governing. It closes the loop "Mick/Occy proposes → KIRRA disposes" on
hardware. The five pieces working together (Mick · Taj · Occy · KIRRA, with Parko as the
Phase-2 perception upgrade):

```
  goal (RViz / Mick) ─/goal_pose─┐
  /odom (pose, speed) ───────────┤
  /scan (lidar) ─────────────────┤
                                 ▼
                            occy_doer
                    ┌────────────┴────────────┐
                    │                         │
        TIMER (ROS executor thread)    WORKER (one bounded job)
        take result → publish/hold     Taj :8101 ─▶ Occy :8100 /plan
        then issue the next job        (proposes + KIRRA slow-loop)
                    ▲                         │
                    └──── JobResult slot ◀────┘
                                 │
                     pure-pursuit → bound proposal envelope
                                 ▼
            /cmd_vel_raw ─▶ cmd_vel_interceptor [Taj speed cap + KIRRA fast-loop] ─/cmd_vel─▶ wheels
             (std_msgs/String: velocities + release_binding, one atomic message)
```

**The sidecar calls do not run in the timer callback.** rclpy's executor is
single-threaded, so two sequential blocking POSTs stalled every other callback on the node —
including the scan subscription, so a slow sidecar froze perception ingest as well as
planning. They run in one bounded background job instead: the **worker computes, the timer
publishes**. The worker never publishes, never touches goal/pose/scan/frame-health state, and
deposits only an immutable result into a lock-protected slot. Every actuation-relevant
decision stays on the executor thread.

Each tick (`occy_doer`, `plan_hz` — 10 Hz in the shipped `kirra_params.yaml`; the node's own
default is 5):
1. read the robot pose + speed (`/odom`) and the current goal (`/goal_pose`),
2. **take** the last completed job's result (taken, not peeked — one verdict per result, so
   reuse is structurally impossible) and decide whether it is usable,
3. if it is, turn Occy's **KIRRA-validated** trajectory into velocities (pure pursuit) and
   publish them on `/cmd_vel_raw` as an **atomic evidence-bound proposal envelope** —
   canonical JSON in a `std_msgs/String` carrying the velocities *and* the `release_binding`
   (Taj scan/camera/tracker identity, platform profile digest, Occy proposal digest) they were
   authored against, so the verifier can bind that evidence into the signed V2 motor release.
   A bare `Twist` names no evidence and is refused by the interceptor. If it is not usable,
   **hold**,
4. then start the next job if a new scan has arrived: snapshot the scan as plain values, POST
   it to **Taj** → the geometric corridor + objects, extend the corridor behind the robot
   (footprint containment), and POST `{ego, goal, corridor, objects, vehicle}` to
   **Occy** `/plan`.

Exactly one publish per tick — a proposal or a hold. Consume-then-issue makes this a
**pipeline**: the tick acts on the previous tick's perception while the next round trip is
already in flight, so perception is one tick period plus one round trip old when used. That is
why `plan_hz` is a safety-adjacent number: it must fit inside `scan_stale_s` (the node WARNs at
startup when it does not — see
[`R2_FIELD_DIAGNOSTICS.md`](../hardware/R2_FIELD_DIAGNOSTICS.md) §4c).

A result is used only if it carries no fault, Taj's `frame_id.scan_sequence` matches the
planner's echoed `perception_frame_id.scan_sequence` (both sidecars describing the *same*
perception), its scan strictly advances a local watermark, and that scan is still inside
`scan_stale_s`. Anything else holds. ROS 2 removed `header.seq`, so scan identity is a
node-local arrival counter; Taj's sequence is used only for the cross-sidecar consistency
check.

**Occy only PROPOSES; KIRRA DISPOSES — twice:** the planner runs the slow-loop checker
(`validate_trajectory_slow`) and returns a verdict; the `cmd_vel_interceptor` then re-checks
every command with the fast-loop kinematic governor + the Taj speed cap. The doer is
**fail-soft**: no goal, a stale scan, an unhealthy scan stream, a service error, a refused
plan, or a plan whose evidence cannot be bound all **hold** — and even if they didn't, the
interceptor + governor are the safety authority.

A hold publishes an **empty** envelope, not a zero Twist. Empty data is not a parseable
envelope, so the interceptor refuses it fail-closed into a stop — which is what a hold means.
It is deliberately *not* a zero-velocity envelope: most holds happen before the tick has any
evidence frame, and fabricating a binding to describe "no motion" would invent perception
identity the doer never observed.

**Holds are audible.** Safe is not the same as visible: a wedged sidecar holds forever and
looks exactly like a robot standing at a delivered goal. A sustained hold warns once naming its
cause, and again if the cause changes; a job stuck far past its budget warns too (an HTTP
timeout bounds each socket read, not the whole request). Idle holds — waiting for a goal,
arrived — never warn. Operator recipes per cause: `R2_FIELD_DIAGNOSTICS.md` §4.

## Run it

### Typed text via Mick (no speech)

With the Mick sidecar running (`kirra-sidecars mick_service`, `:8102`) and the doer
launched with `mick_url:=http://localhost:8102`, a typed request becomes the goal:

```bash
curl -s localhost:8102/intent -X POST -H 'content-type: application/json'      -d '{"text":"head to the loading dock, about 8 meters ahead"}'
# → {"ok":true,"seq":1,"intent":{"intent":"go_to","x_m":8.0,"y_m":0.0}}
```

The doer polls `GET /intent/last` — on its own background fetch, for the same reason the
sidecar calls moved off the timer — and grounds a NEW positional intent as the goal
(ego frame at receipt → odom), clearing it on `hold`. The fetch worker only deposits the raw
wire dict; **all goal mutation happens on the executor thread**, and a Mick outage can neither
stall nor fault the plan cycle. Fail-closed end-to-end: an
unparseable model reply never latches an intent, an unreachable Mick keeps the
current goal, and KIRRA still bounds every proposal — Mick publishes INTENTS,
never commands (the actuation fence is CI-enforced:
`ci/check_mick_actuation_fence.py`).

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

### Timing parameters (not chassis tuning)

Kept out of the table above on purpose — these are not knobs to fit to a robot, they are the
pipeline's timing contract.

- **`scan_stale_s`** — REQUIRED, no default; the node refuses to start without it. How old the
  perception behind a proposal may be. This is the safety bound on how blind the doer can be
  while still proposing motion, set per deployment from the lidar rate (0.25 s for the
  bench-verified ~10 Hz TG30). **Do not widen it to silence a warning.**
- **`plan_hz`** — the decision-loop rate (10.0 shipped). Because the cycle is a pipeline, one
  tick period plus one round trip has to fit inside `scan_stale_s`, or plans age out and the
  doer holds intermittently on a *healthy* lidar. Raising it above the lidar rate is free: a
  job is only issued when a new scan has actually arrived.
- **`http_timeout_ms`** — per-request budget for each sidecar call. Note it bounds each socket
  operation, not the whole request, which is why there is a separate overdue-job watchdog.

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
