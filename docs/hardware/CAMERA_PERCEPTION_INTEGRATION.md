# Camera integration — lidar + camera into Taj, and "drive to the red cup"

How the Orbbec Astra camera joins the R2 perception stack, what it is allowed to
decide, and how an object-named drive command ("drive to the red cup") is grounded
**without giving the camera any safety authority.**

## The one rule everything follows

A camera contributes **two different things**, and conflating them would be a
safety bug. The code keeps them in separate modules on purpose:

| | Channel | Type | Authority |
|---|---|---|---|
| 1 | **Drivability** — "may the ego drive here?" | `SemanticClass` / `SemanticDetection` → `clip_corridor_to_hazards` | **TIGHTEN-ONLY.** May shorten the drivable corridor, never extend it. |
| 2 | **Where a named thing is** — "the red cup is 2.4 m ahead" | `kirra_taj::object_goal::LabeledTarget` → `ObjectGoal` | **None.** A destination, not a claim about the ground. |

🔴 **Lidar (Phase A) remains the sole authority on free space.** The camera can
never make unseen ground drivable. This is enforced three ways:

1. `clip_corridor_to_hazards` *truncates* boundaries — structurally incapable of
   extending them.
2. `SemanticClass::is_drivable()` is `true` for `Road` only; every unrecognized
   class token decodes to `Unknown`, which is **non-drivable** (fail-closed —
   never assume drivable).
3. The KPI gate classifies a Phase-B corridor reaching past Phase-A's as
   **`ForbiddenLoosen`**, a hard zero. The property is also swept in
   `camera_can_never_extend_the_corridor` (classes × ranges).

## Why "drive to the red cup" is safe by construction

The command decomposes across the two channels, and *neither* lets the camera
authorize motion:

```
"drive to the red cup"
        │
        ├─ camera → object_goal::resolve_object_goal("red cup", targets…)
        │      → ObjectGoal{ x: 2.4, y: 0.3 }        ← a DESTINATION
        │      → caller emits MickIntent::GoTo{x,y}  ← an ordinary intent
        │
        ├─ lidar  → Taj Phase-A corridor              ← the free space
        ├─ camera → Phase-B fusion (tighten-only)      ← may only shorten it
        │
        └─ Occy plans inside that corridor → KIRRA checker BOUNDS the trajectory
```

So if the cup is **visible but the path is blocked**, the robot drives as far as
is safe and **stops short** — it never drives at the cup because the camera "saw"
it. `MickIntent::GoTo` already documents exactly this ("it never drives *to* the
point if getting there is unsafe"), which is why the resolver emits a **plain
`GoTo`**:

- no new `MickIntent` variant,
- no change to the fail-closed intent parse (`MickIntent::from_llm_json`),
- no new actuation path — the single-door invariant (text → mick `/intent` →
  checker) is untouched.

## Channel arming (the house rule)

The camera channel makes the same three-way, fail-closed decision as the
occlusion / VRU channels (`resolve_camera_channel`):

| State | Condition | Behaviour |
|---|---|---|
| **Disarmed** | `camera_armed: false` (default) | Phase-B never runs → byte-identical to the lidar-only endpoint. |
| **Fresh** | armed + `camera_stamp_ms` within `camera_max_age_ms` | Fuse the detections (tighten-only). |
| **Faulted** | armed + **no frame**, stale, implausibly future-stamped, or non-finite geometry | `camera_healthy: false` → **MRC floor** (`speed_cap_mps: 0.0`). |

A fresh frame with **zero detections** is a legitimate "I looked, it's clear" and
does **not** fault — distinct from silence. *"The detector did not look" is never
"clear."*

## Wire API (`POST /perception`, taj_service)

New optional fields — all default to the disarmed/no-op state:

```jsonc
{
  "angle_min_rad": -3.14, "angle_increment_rad": 0.0031,
  "range_min_m": 0.01, "range_max_m": 50.0, "ranges": [ ... ],
  "stamp_ms": 1234567,

  "camera_armed": true,              // opt-in; default false
  "camera_stamp_ms": 1234500,        // the frame the detections came from
  "camera_max_age_ms": 500,          // freshness budget
  "detections": [                    // SAFETY classes only (not goal labels)
    { "class": "water", "near_x_m": 2.0,
      "lateral_min_m": -0.5, "lateral_max_m": 0.5 }
  ]
}
```

`class` ∈ `road` | `water` | `static_obstacle` | anything-else→`unknown`.
Response gains `camera_healthy` and `camera_clip_x_m` (where the fusion clipped,
`null` if nothing bound the corridor).

## Bring-up on the Orin

1. **Driver** — the R2 ships an Orbbec Astra (`my_camera: astraplus`). Bring up the
   vendor ROS 2 camera node so `/camera/color/image_raw` and
   `/camera/depth/image_raw` publish. Verify:
   ```bash
   ros2 topic hz /camera/color/image_raw   # steady frame rate
   ros2 topic hz /camera/depth/image_raw
   robot/kirra_doctor.py --module devices --verbose   # camera device row
   ```
   `ros2_ws/src/kirra_safety/kirra_safety/sensor_monitor.py` already subscribes
   `/camera/depth/image_raw` and scores a `depth_camera` confidence, so the health
   plumbing exists once the topics are live.
2. **Extrinsics** — detections must arrive in the **ego frame (+X forward, +Y
   left)**, the same frame as `/scan` (verify the lidar first with
   `robot/lidar_orient_check.py`). A camera-vs-lidar frame mismatch would clip the
   corridor in the wrong place, so validate with a single object at a known offset
   before arming.
3. **Arm it** — send `camera_armed: true` with a stamped frame. Start with the
   channel **disarmed** and confirm parity with lidar-only, then arm.

## The detector (shipped: classical CV)

`camera_detect` (`ros2_ws/src/kirra_safety/kirra_safety/camera_detect.py`) is the
producer. Split the house way: every decision that could affect motion lives in
the **pure core** `camera_detect_core.py` (stdlib only — no rclpy, cv2, numpy or
camera, host-tested in `test/test_camera_detect_core.py`), and the node does only
the pixel work (HSV threshold → morphology → connected components, plus a coarse
depth grid).

```
camera_detect ──/camera_detect/detections (JSON)──▶ occy_doer
                                                      ├─ POST /perception  detections  (tighten-only)
                                                      └─ POST /plan        object_goal + targets
```

Run it:

```bash
ros2 run kirra_safety camera_detect --ros-args \
  -p colours:="['red','blue']" \
  -p mount_x_m:=0.10 -p mount_z_m:=0.20 -p mount_pitch_deg:=15.0
```

🔴 **HONEST SCOPE — the classical detector grounds colour, not nouns.** It can
confirm "there is a red thing at 2.4 m"; it cannot confirm the thing is a *cup*.
So `colour_term("red cup") -> "red"` and the emitted target label is the colour
term. Labelling a red blob `"red cup"` would be a fabrication, so the code
refuses to; verifying the noun is what the ML tier is for.

Its own fail-closed rules (each tested):

| Situation | Behaviour |
|---|---|
| Blob with no valid depth / non-finite maths | dropped — never a guessed range |
| Blob that projects behind the ego (`x <= 0`) | dropped — not a destination |
| Unknown colour token | refused |
| Depth not registered to colour (different sizes) | frame refused — a wrong depth would put the target metres off |
| Depth frame mostly INVALID (`frame_valid_fraction` below floor) | **stamp withheld** → the consumer's channel faults → MRC floor |
| No known colour configured at startup | node refuses to start (an empty `targets` forever would read as "the object is not there") |

Other detector tiers, when the classical one is not enough:

- **ML detector (ADR-0015's plan).** An RGB detector through
  `parko-onnx --features cuda` / `parko-tensorrt`. General labels — this is the
  tier that can verify the noun; needs the CUDA path (see
  `docs/hardware/JETSON_CUDA_SETUP.md`) and a model.
- **VLM (`gemma3:4b` is multimodal).** Good for *conversation* ("what do you
  see?") — Channel A. **Not** for the control loop: seconds-scale latency, not
  per-tick safe, and never a drivability authority.

## The consumer hop (`occy_doer`)

Both channels are **opt-in**; with the defaults the endpoint is byte-identical to
the lidar-only path.

| Parameter | Default | Effect |
|---|---|---|
| `camera_armed` | `false` | Arms the Taj Phase-B fusion (tighten-only). |
| `camera_detections_topic` | `/camera_detect/detections` | Where the detector frame arrives. |
| `camera_max_age_ms` | 500 | Relayed freshness budget. |
| `object_goal_topic` | `/object_goal` | A `std_msgs/String` phrase; empty clears. |

Relay discipline: the doer is a **faithful relay, not a second opinion.**
Freshness is Taj's call, so the doer sends the scan's own ROS header stamp as
`stamp_ms` and the depth frame's as `camera_stamp_ms` — **one clock domain**, per
`AOU-TIMESYNC-001`. Armed with no usable frame ⇒ armed but *no stamp* ⇒ Taj
resolves Faulted ⇒ speed cap floored.

Object-goal flow, all fail-closed:

- The phrase is reduced to its colour term; ungroundable (`"cup"`) or ambiguous
  (`"red blue thing"`) ⇒ **hold**, logged once, never a fall-back to another goal.
- No usable camera frame ⇒ hold.
- Otherwise `object_goal` + `targets` + `targets_stamp_ms` + `now_ms` go to
  `/plan`, which resolves and refuses on its own terms (not seen / low confidence
  / ambiguous / stale / behind ego).
- **Arrival** is latched by `object_goal_reached` on the same tolerance the
  positional goal uses. Without it the loop oscillates at close range: the target
  leaves the detector's usable band, the planner refuses, the doer holds, the
  target reappears, the robot nudges. Arrival is a *positive* claim — not-seen
  falls through to the planner's refusal rather than reporting "we are there".

## What is NOT wired yet

- Camera frames into rabbit/mick for conversation (Channel A; `gemma3:4b` accepts
  images via Ollama, so this is a doer-side UX addition with no safety surface).
- A launch file wiring `camera_detect` into the R2 stack (run it standalone for
  bring-up; the vendor camera node comes up separately either way).

## The object-goal call site (`POST /plan`, planner_service)

The resolver is **wired live** on the planner sidecar. Send a label plus the
ego-frame targets the detector produced; the sidecar grounds it to a plain
`MickIntent::GoTo` in world coordinates and runs the ordinary checker path:

```jsonc
{
  "ego": { "x": 0.0, "y": 0.0, "heading": 0.0, "speed": 0.0 },
  "object_goal": "red cup",
  "targets": [ { "label": "red cup", "x": 2.4, "y": 0.3, "confidence": 0.9 } ],
  "targets_stamp_ms": 1234500,
  "now_ms": 1234600
}
```

Fail-closed properties (each has a test):

- `object_goal` **and** a typed `intent` in the same request → refused
  (`PLAN_AMBIGUOUS_GOAL_SOURCE`); there is exactly one goal source per plan.
- Every `GoalRefusal` (not seen / low confidence / ambiguous / stale /
  non-finite / behind ego) → refusal, i.e. **no motion** — never a guessed goal.
  `GoalRefusal::code()` gives the machine token, `refusal_sentence` the operator
  sentence Rabbit can speak.
- Absent `object_goal` → byte-identical prior behaviour.
- The ego→world hop is `ObjectGoal::to_world(ego_x, ego_y, ego_heading)`; the
  detector never has to know the world frame.
