# R2 — Operational Design Domain specification

**Doc ID:** KIRRA-R2-ODD-001
**Platform:** Yahboom Rosmaster R2 (Ackermann, ~1/10 RC scale) on Jetson Orin NX
**Vehicle class:** `r2` (`KIRRA_VEHICLE_CLASS=r2`; `src/gateway/contract_profiles.rs`)
**Status:** Working draft for review. Not a certified analysis.
**Issue:** #1220 (child of #1209).

---

## 0. Why this document exists, and what it is not

Before this document, the R2's operational design domain consisted of one
sentence, in a parenthetical, inside a constant's doc comment:

```rust
/// Basis: a small indoor/tethered Ackermann robot operated well below walking
/// pace; …
pub const R2_ODD_SPEED_CAP_MPS: f64 = 1.0;
```

"Indoor" and "tethered" are operating-context boundaries — arguably the most
restrictive this platform has. They had never been written where they could be
reviewed, cited by a component, or diffed when they change.

**This document is authored, not extracted.** Two ODDs already exist in the
tree and neither is this platform's: `OCCY_SOTIF.md` §1 holds the urban /
robotaxi ODD (ADR-0001, ADR-0002), and ADR-0028 holds the sidewalk-courier ODD.
They are the structural template here and are cited where a boundary is
genuinely shared, but the R2's boundaries are stated fresh because they have
never been stated at all.

**What this document is not.** It is not a claim that the boundaries below are
enforced. §2 classifies every one of them, and most are not. Recording an
unenforced boundary as an assumption is the point of the exercise; recording it
as a control would be worse than saying nothing.

---

## 1. The two ODDs, kept apart

The same distinction `OCCY_SOTIF.md` §1 draws applies here and is easy to lose
on a bench robot, where the test environment and the operating environment are
the same room:

- **V&V / bench ODD** — what the R2 is *tested* against, including deliberately
  injected faults (wedged lidar, silent sidecar, stale scan, clock step). High
  frequency here is not exposure.
- **Deployment ODD** — what the R2 is *operated* in. This is what the sections
  below specify.

For a supervised bench robot these coincide more than they would on a road
vehicle. That is a reason to be explicit about which one a given statement is
about, not a reason to merge them.

---

## 2. Deployment ODD — boundaries and enforcement status

Each boundary is stated as a checkable condition. The **Enforcement** column
takes one of three values, and the distinction is load-bearing:

| Value | Meaning |
|---|---|
| **ENFORCED** | A runtime check refuses or derates when the condition fails. Named. |
| **BY CONSEQUENCE** | Not checked as a condition; the runtime observes the *effect* of the condition failing and derates on that. Often stronger than a condition check — it cannot be fooled by a sensor asserting the condition holds. |
| **ASSUMED** | Nothing checks it. It is an assumption of use and carries an AOU id. |

### 2.1 Environment

| # | Boundary | Condition | Enforcement |
|---|---|---|---|
| E1 | **Indoor only** | Operating area is enclosed; no exposure to weather | **ASSUMED** — AOU-R2-ENVIRONMENT-001 |
| E2 | **No precipitation** | No rain, snow, standing water on the operating surface | **ASSUMED** — follows from E1; same AOU |
| E3 | **Lighting: any** | No constraint on ambient illumination for the safety path | **BY CONSEQUENCE** — see §2.6 |
| E4 | **Ambient temperature** | Within Jetson Orin NX and TG30 operating range | **ASSUMED** — vendor-specified, not monitored |

E1 and E2 are the boundaries the `R2_ODD_SPEED_CAP_MPS` prose asserted. They
remain assumptions. Nothing in the stack detects that the robot has been
carried outdoors.

### 2.2 Surface and terrain

| # | Boundary | Condition | Enforcement |
|---|---|---|---|
| S1 | **Flat hard floor** | Slope within an untested bound; hard surface (tile / sealed concrete / low-pile carpet) | **ASSUMED** — AOU-R2-SURFACE-001 |
| S2 | **No drop-offs** | No stairs, edges, or level changes in the operating area | **ASSUMED** — the 2-D lidar scans a horizontal plane and is structurally blind to negative obstacles; same AOU |
| S3 | **Traversable clearance** | Obstacles taller than the lidar plane, or shorter than the chassis, are absent or fenced by the operator | **ASSUMED** — same structural blindness; same AOU |

S2 is the sharpest limitation on this platform and deserves to be stated
plainly rather than buried: **a horizontally-scanning 2-D lidar cannot see a
stair.** The corridor will read as clear right up to the edge. No amount of
checker conservatism recovers this — the evidence does not exist in the sensor.
It is an operator-fenced boundary, and the operator has to know that.

### 2.3 Speed

| # | Boundary | Condition | Enforcement |
|---|---|---|---|
| V1 | **Class ceiling ≤ 1.0 m/s** | `effective_max_speed_mps() = min(1.5, 1.0) = 1.0` | **ENFORCED** — `R2_ODD_SPEED_CAP_MPS`, applied by `validate_vehicle_command` via the `r2` contract |
| V2 | **Demo backstop ≤ 0.15 m/s** | `KIRRA_DEMO_VX_MAX = 0.15` at the motor consumer | **ENFORCED** — `robot/kirra_motor_consumer.py`; a second, lower ceiling below the class contract |
| V3 | **MRC crawl ≤ 0.5 m/s** | Degraded posture; decel-to-stop-and-hold per SS-002 | **ENFORCED** — `r2_mrc` profile |

V1 and V2 are two regimes, not one. The deployed demo configuration operates an
order of magnitude below the class ceiling. **Which regime is active is a
configuration fact, not an ODD fact** — the ODD boundary is V1; V2 is an
additional operational restriction the deployment currently imposes on itself.

### 2.4 Persons in the operating area

| # | Boundary | Condition | Enforcement |
|---|---|---|---|
| P1 | **No uninstructed persons** | Only the supervisor and briefed participants are present | **ASSUMED** — AOU-R2-CROWD-001 |
| P2 | **No VRU-classified agents** | No children, no pets, no persons unable to anticipate the robot | **ASSUMED** — same AOU |
| P3 | **Pedestrian channel disarmed** | `KIRRA_VRU_CHANNEL_ENABLED` default off → the omnidirectional pedestrian bound is a checker no-op | **ASSUMED** — the mechanism exists (`vru_channel::resolve_vru_channel`) and is not armed on this platform |

P3 is worth reading carefully. The VRU bound is implemented and tested; it is
*not enabled here*, because arming it without a pedestrian producer publishing
at rate would MRC-floor the robot permanently. So persons in the operating area
are handled by P1/P2 — an operator obligation — and not by the checker.

### 2.5 Network and connectivity

| # | Boundary | Condition | Enforcement |
|---|---|---|---|
| N1 | **No network in the control loop** | Every element of the governed loop is a localhost process on the Orin | **ENFORCED BY ARCHITECTURE** — `R2_UNTETHERED_BRINGUP.md` §0; wifi loss does not affect actuation |
| N2 | **Supervisor within stop range** | A human can stop the robot at all times | **ASSUMED** — AOU-R2-SUPERVISION-001; see below |

N1 is a genuine strength and is architecturally enforced: cutting wifi mid-drive
does not enter the control path.

N2 is the counterweight and must not be read as covered by N1. **The stop of
last resort today is a human at an SSH session pressing Ctrl-C.** The
software-independent hardware e-stop specified in `R2_ESTOP_SPEC.md` (R1:
"works with the Jetson hung, the consumer crashed") **is a design spec and is
not built.** Until it exists, the R2's operating range is bounded by SSH reach
and line of sight, and a wedged process is stoppable only by removing power by
hand. This is the single largest residual risk on the platform and belongs in
the register (§5).

### 2.6 Required sensor health

| # | Boundary | Condition | Enforcement |
|---|---|---|---|
| H1 | **Lidar present and fresh** | Newest scan age ≤ `scan_stale_s` = 0.25 s (2.5 periods of the bench-verified ~10 Hz TG30) | **ENFORCED** — `occy_doer` holds instead of proposing motion |
| H2 | **Perception cap fresh** | Cap age ≤ `perception_cap_stale_ms` = 300 ms | **ENFORCED** — fail-closed stop at the interceptor |
| H3 | **Perception request answered within budget** | In-flight request ≤ `deadline_ms` = 250 ms on the node's own clock | **ENFORCED** — #1218; floors the cap independently of transport progress |
| H4 | **Lidar frame identity advances** | Producer header stamp progresses; a wedged driver republishing its last buffer is detected | **ASSUMED** — AOU-R2-LIDAR-HEALTH-001; `frame_health_enabled: false` by default |
| H5 | **Confidence floor** | Scan confidence ≥ 0.5 | **ENFORCED** — below the floor → 0.0 cap |
| H6 | **Perception redundancy** | Two independent perception channels agree | **NOT APPLICABLE** — `KIRRA_PERCEPTION_REDUNDANCY_ENABLED` off; the R2 has one lidar. Single-channel perception is an ODD condition, not a fault |

H4 is the gap that #1211 built the mechanism for and this platform has not
armed. The parameter file states the reason honestly: a driver that never
populates `header.stamp` would trip `STAMP_NOT_ADVANCING` immediately and hold
the robot, so it stays off until the deployment lidar's stamp behaviour is
confirmed on the bench. That is a defensible sequencing decision and an open
assumption at the same time.

H6 deserves the same care. One lidar is not a degraded two-lidar configuration;
it is the platform's design. The correct treatment is an ODD boundary ("this
platform operates on single-channel perception") rather than a permanent fault
indication.

### 2.7 Localization

| # | Boundary | Condition | Enforcement |
|---|---|---|---|
| L1 | **No global localization required** | No AMCL, no SLAM, no HD map in the R2 stack | **N/A by construction** — the doer plans corridor-relative from live lidar |
| L2 | **No map-anchored behaviour** | No commit zones, no route following beyond the visible corridor | **N/A by construction** — follows from L1 |

This is a real difference from the Occy ODD and should not be papered over by
reusing its localization requirement. `AOU-LOCALIZATION-001` (≤ 0.10 m 95th-pct
lateral error) **does not apply to the R2**, because the R2 makes no
map-anchored claim for it to bound. The R2's equivalent constraint is that it
can only act on what is currently in the corridor — which is L1/L2 stated as a
capability limit rather than an accuracy requirement.

### 2.8 Drive path

| # | Boundary | Condition | Enforcement |
|---|---|---|---|
| D1 | **Path-B Ackermann drive** | `KIRRA_DRIVE_MODE=r2_ackermann`; rear wheels via `set_motor`, steering via `set_akm_steering_angle` | **ENFORCED** — `robot/r2_drive.py`; bypasses the broken X3 firmware mixer |
| D2 | **Exclusive serial ownership** | The verifying motor consumer exclusively owns the motor port | **ENFORCED** — AOU-ACTUATION-SERIAL-001, #887 Tier-3 boot sentinel |
| D3 | **Governed actuation only** | Every actuation carries a verified Ed25519 release token | **ENFORCED** — ADR-0033 chokepoint, `kirra_motor_consumer.py` + `libkirra_consumer_ffi.so` |

### 2.9 Lighting, restated precisely (E3)

Lighting gets its own note because the naive entry — "lidar doesn't care about
light, so lighting is unconstrained" — is right about free space and wrong
about hazards.

- **Free space** is decided by the TG30 lidar alone, which is active-ranging and
  genuinely light-independent. `CAMERA_PERCEPTION_INTEGRATION.md` makes this
  structural: the camera channel is **TIGHTEN-ONLY** and
  `clip_corridor_to_hazards` truncates boundaries, so a camera blinded by
  darkness cannot make unseen ground drivable.
- **Hazards** are where light matters. Semantic hazard clipping is a camera
  input. In darkness the camera contributes nothing, so a hazard it would have
  clipped is *not* clipped.

So the failure direction under poor lighting is **loss of a tightening input**,
not gain of a false permission. The corridor reverts to the lidar-only Phase-A
geometry, which is the conservative baseline the KPI gate's `ForbiddenLoosen`
row already forbids Phase-B from exceeding. That is why E3 is classified BY
CONSEQUENCE and not ASSUMED: the architecture degrades correctly without
checking the condition.

This is also why an explicit "is it dark" check would be a **worse** design.
A light sensor asserting adequate illumination while the camera is fouled,
misaimed or lens-capped would grant confidence the perception channel has not
earned. Observing the consequence cannot be fooled that way.

---

## 3. Sub-ODDs

**The R2 has no sub-ODD partition, and adding one is not currently justified.**

ADR-0002 partitions the Occy ODD because a future controlled-access highway
deployment would otherwise require re-architecture. The R2 has no analogous
expansion in view: one indoor environment, one speed regime bounded by V1, one
sensor configuration.

Recorded explicitly so that a future reader does not read the absence as an
oversight, and so that a proposal to add one is a deliberate change with this
paragraph to argue against.

---

## 4. ODD exit — what happens at the boundary

The acceptance criterion for this document asks whether posture drops to
constrained or locked out outside the validated ODD. **Stated plainly: for most
boundaries above, it does not, because most boundaries are not detected.**

What is true:

| Situation | Response | Enforced? |
|---|---|---|
| Scan stale beyond 0.25 s | Doer holds; no motion proposed | Yes |
| Perception cap stale / absent / faulted | Cap floors to 0.0 | Yes |
| Perception request past its deadline | Cap floors to 0.0 | Yes |
| Confidence below floor | Cap floors to 0.0 | Yes |
| Command exceeds the `r2` envelope | Refused by `validate_vehicle_command` | Yes |
| Fleet posture Degraded | Decel-to-stop-and-hold (SS-002) | Yes |
| Robot carried outdoors | *Nothing* | No — E1/E2 assumed |
| Robot driven toward a stair | *Nothing* | No — S2 structurally undetectable |
| Uninstructed person enters the area | *Nothing* | No — P1/P2 assumed; VRU channel disarmed |
| Lidar wedged, republishing a stale buffer at full rate | *Nothing* | No — H4 detector disarmed |
| Process wedged, no software stop available | *Nothing automatic* | No — hardware e-stop not built |

The enforced rows share a shape worth naming: **they are all detected as
evidence problems, not as environment problems.** The stack is well
instrumented to notice that it cannot see, and not instrumented at all to notice
where it is. For a supervised bench robot that is a coherent position — the
supervisor is the environment sensor — but it only holds while N2 holds, and N2
is the assumption with no hardware behind it.

---

## 5. Assumptions of use raised by this document

Five boundaries above are assumptions rather than controls. Each is registered
in `ASSUMPTIONS_OF_USE.md` with an id; they are summarized here and **authored
there**, not in both places.

| AOU ID | Covers | Boundary |
|---|---|---|
| AOU-R2-ENVIRONMENT-001 | Indoor operation; no precipitation | E1, E2 |
| AOU-R2-SURFACE-001 | Flat hard floor; no drop-offs; no lidar-plane-invisible obstacles | S1, S2, S3 |
| AOU-R2-CROWD-001 | No uninstructed persons or VRU-class agents | P1, P2, P3 |
| AOU-R2-SUPERVISION-001 | A supervisor with a working stop of last resort, within range | N2 |
| AOU-R2-LIDAR-HEALTH-001 | Lidar frame identity actually advances | H4 |

---

## 6. Cross-references

- `docs/safety/OCCY_SOTIF.md` §1 — the urban / robotaxi ODD; structural template
- `docs/adr/0028-sidewalk-courier-odd.md` — the courier ODD
- `docs/adr/0001-occy-odd-speed-cap.md`, `docs/adr/0002-condition-dependent-cap-subodds.md` — ODD speed cap + sub-ODD partition (Occy)
- `docs/adr/0014-rosmaster-r2-orin-nx-kirra-integration.md`, `docs/adr/0015-rosmaster-r2-perception-layer.md`, `docs/adr/0033-actuation-authority-ros-r2-topology.md`
- `docs/CONTRACT_PROFILES.md` — the `r2` class column
- `docs/safety/SAFE_STATE_SPECIFICATION.md` SS-002 — Degraded decel-to-stop-and-hold
- `docs/hardware/R2_ESTOP_SPEC.md` — the unbuilt hardware e-stop (N2)
- `docs/hardware/R2_UNTETHERED_BRINGUP.md` §0 — the localhost control loop (N1)
- `docs/hardware/CAMERA_PERCEPTION_INTEGRATION.md` — tighten-only camera authority (E3)
- `docs/hardware/HARDWARE_FINDINGS_R2X3.md`, `robot/install/PLATFORM_R2_PENDING.md` — bench provenance
- `ros2_ws/src/kirra_safety/config/kirra_params.yaml` — the deployed freshness / health parameters
- `src/gateway/contract_profiles.rs` — the `r2` contract and `R2_ODD_SPEED_CAP_MPS`
