# R2 — Residual-risk register

**Doc ID:** KIRRA-R2-RESIDUAL-001
**Platform:** Yahboom Rosmaster R2 (Ackermann, ~1/10 RC scale) on Jetson Orin NX
**Status:** Working draft for review. **No row has been formally accepted** — see §1.3.
**Issue:** #1220 (child of #1209).

---

## 1. What this register is, and what it is not

### 1.1 The three registers

This project now has three registers, and they answer different questions. A
row belongs in exactly one of them.

| Register | Answers | Keyed by |
|---|---|---|
| `ASSUMPTIONS_OF_USE.md` | What must an operator, integrator, site or other system **do**? | `AOU-<AREA>-NNN` |
| Assumption register (#1219) | Where did this authority-relevant **number** come from, and is it validated? | `class.field` |
| **This register** | What hazardous outcome is **still possible** after credited mitigations and AOUs are applied? | HARA hazard **+ residual scenario** |

A residual row may **cite** an AOU. It must not **restate** the obligation. One
record says what another party must do; the other says what can still happen
and why the software cannot prevent it.

### 1.2 Keyed below the hazard

Rows are keyed `H-NNN / R2-RS-NNN`, not by hazard alone. A single HARA hazard
can carry several mitigations that leave materially different residuals —
H-006 is the clearest case here, where the ADR-0033 token chokepoint eliminates
the hazard's original causal path entirely while leaving a different one
untouched. One row per hazard would have to either summarize both vaguely or
hide one inside the other.

### 1.3 This register is not an issue tracker

A residual risk is not automatically a defect to eliminate, and an
unimplemented required control is not comfortably "accepted residual risk".
Every row carries one of four dispositions:

| Disposition | Meaning |
|---|---|
| **ACCEPTED** | The acceptance authority has explicitly accepted this residual. |
| **PENDING ACCEPTANCE** | Characterized, not yet ruled on. |
| **REQUIRES FURTHER MITIGATION** | A control is specified or identified as needed and is not in place. Not acceptable as-is. |
| **BLOCKED ON VERIFICATION** | The mitigation exists; the evidence that it works in this deployment does not. |

**Every row in this register is currently PENDING ACCEPTANCE, REQUIRES FURTHER
MITIGATION, or BLOCKED ON VERIFICATION. None is ACCEPTED.** `HARA.md` records
its review status as "Pending TUV pre-assessment" and names no acceptance
authority, so there is nobody who has signed anything. Marking rows ACCEPTED
would be recording an approval that has not happened.

### 1.4 Credited-mitigation status

A mitigation is credited with an explicit status, so that a specified-but-unbuilt
control cannot be counted as though it exists:

| Status | Meaning |
|---|---|
| **IMPLEMENTED + VERIFIED** | In the deployed configuration, with evidence named. |
| **IMPLEMENTED, NOT VERIFIED** | In the code and in the deployed path; no deployment evidence. |
| **IMPLEMENTED, NOT ARMED** | In the code; disabled in the deployed configuration. |
| **SPECIFIED, NOT IMPLEMENTED** | A design exists. Nothing runs. Credited at zero. |
| **EXTERNAL / AOU** | Discharged by another party; cites the AOU id. |

### 1.5 Detection / ODD-exit behaviour

`R2_ODD.md` §4 establishes that most environmental ODD exits on this platform
are not detected. This register must not let "mitigated by ODD" masquerade as
"ODD exit is enforced", so every row states detection explicitly:

| Value | Meaning |
|---|---|
| **DETECTED → POSTURE DROPS** | A runtime check fires and the posture or cap changes. |
| **DETECTED BY CONSEQUENCE** | The effect is observed and derated; the condition itself is not checked. |
| **NOT DETECTED** | Nothing fires. The system continues believing it is nominal. |
| **OUTSIDE CLAIMED ARCHITECTURE** | The failure is below or beside the software item boundary. |

**`BY CONSEQUENCE` is not a residual-risk status.** It describes how a condition
affects authority, not whether what remains is acceptable. A row detected by
consequence still needs its own residual statement — see R2-RS-008.

---

## 2. HARA coverage — every hazard has exactly one disposition

The walk runs from the hazard, not from the ODD. Starting at `R2_ODD.md` and
asking "what risks should I list" produces a list shaped by what was already
noticed.

**Assertion A (hazard → disposition):** every one of `HARA.md`'s 17 hazards
appears exactly once below.
**Assertion B (residual → hazard):** every residual row in §3 resolves to an
applicable hazard. **This assertion currently FAILS — see §4.**

| Hazard | Disposition | Basis |
|---|---|---|
| H-001 speed exceeds contract max | **RESIDUAL** → R2-RS-001 | Enforced; the limit enforced against is unvalidated |
| H-002 lateral accel / rollover | **RESIDUAL** → R2-RS-002 | Enforced; steering-rate term explicitly unmeasured |
| H-003 sensor fault not detected in time | **RESIDUAL** → R2-RS-003 | Timeout path covers silence, not identity freeze |
| H-004 stale posture cache → MRC not applied | **MITIGATED, no material residual** | `POSTURE_CACHE_TTL_MS`; `should_route_command` refuses on a stale cache |
| H-005 NaN/Inf passes enforcement | **MITIGATED, no material residual** | Talisman NaN gates (Kani K1–K5, machine-checked); `ros2_adapter` rejects before publish |
| H-006 process crash → commands pass unfiltered | **ELIMINATED (original path) + RESIDUAL** → R2-RS-004 | ADR-0033 inverts it; the wedge case remains |
| H-007 standby fails to promote | **INAPPLICABLE** | Single-instance deployment; no `PassiveStandby` peer on the R2 topology |
| H-008 audit chain tampered | **RESIDUAL** → R2-RS-005 | Chain verified in place; no off-box copy on this deployment |
| H-009 `Unknown` command not denied | **MITIGATED, no material residual** | INVARIANT #9 early return, before any posture check |
| H-010 CANOpen NMT not repostured | **INAPPLICABLE** | No industrial protocol adapter in the R2 deployment |
| H-011 DNP3 broadcast unaudited | **INAPPLICABLE** | As H-010 |
| H-012 fabric cross-asset propagation failure | **INAPPLICABLE** | Single asset; no fabric peers |
| H-013 recovery hysteresis bypassed by replay | **RESIDUAL** → R2-RS-006 | `sensor_monitor` posts reports; the R2's are unauthenticated on-box |
| H-014 forged federation report | **INAPPLICABLE** | No federation peers registered |
| H-015 rate limiter applied before clamp | **MITIGATED, no material residual** | INVARIANT #8 — envelope clamp always precedes rate limiting |
| H-016 admin token absent → fail-open | **MITIGATED, no material residual** | INVARIANTs #1/#6 — absent or empty → 503, never fail-open |
| H-017 stale DDS command after reconnect | **ELIMINATED by architecture** → note below | The release watermark, not QoS, is the control |

**H-017 note.** The HARA's mitigation is `DurabilityPolicy::Volatile` (INVARIANT
#10). On the R2 the release path is a ROS 2 topic, so the durability argument
would have to be re-made per-QoS. It does not need to be: a replayed or
history-cached token is refused by the **strictly-advancing release sequence**
in `ActuatorStation`/`kirra_motor_consumer`, independent of transport
durability. The stale-delivery hazard is eliminated by a property of the
payload rather than a property of the carrier, which is the stronger form.

**Inapplicability is a claim with evidence, not an omission.** Five hazards are
inapplicable, each because a subsystem is absent from this deployment rather
than because it is believed safe.

`AOU-LOCALIZATION-001` is treated the same way and belongs here as an
illustration rather than a row: it is **inapplicable to the R2** with
architectural evidence — `crates/kirra-sidecars/src/planner.rs` runs
`lane_graph: None`, there is no AMCL or SLAM in the R2 stack, and the doer plans
corridor-relative from the live scan. It is not copied into this register merely
because it exists elsewhere.

---

## 3. Residual rows

### R2-RS-001 — H-001 — Envelope enforced against an unvalidated limit

| Field | Value |
|---|---|
| **Hazardous event** | The R2 moves faster than its true safe speed for the situation, because the enforced ceiling was chosen rather than measured. |
| **Credited mitigations** | `validate_vehicle_command` against the `r2` contract, effective ceiling `min(1.5, 1.0) = 1.0` m/s — **IMPLEMENTED + VERIFIED** (MC/DC evidence, Kani K1–K5, proptest). `KIRRA_DEMO_VX_MAX = 0.15` m/s at the motor consumer — **IMPLEMENTED + VERIFIED**. |
| **Credited AOUs** | AOU-R2-ENVIRONMENT-001, AOU-R2-CROWD-001 (the low-consequence operating context). |
| **Why risk remains** | The enforcement is sound; the number is not. `r2.max_speed`, `r2.accel`, `r2.brake`, `r2.lat_accel`, `r2.follow` are all **VALIDATION-PENDING** (#1219). No bench measurement establishes that 1.0 m/s is stoppable within the corridor the checker admits. |
| **Detection / ODD exit** | **DETECTED → POSTURE DROPS** for an over-envelope *command*. **NOT DETECTED** for the envelope itself being wrong. |
| **S / E / C after mitigation** | S1 (0.2 m, ~1 kg platform at ≤1 m/s in a supervised area) / E2 / C1. The HARA's S3-E4-C3 reflects a road vehicle; the R2 does not inherit it. |
| **Residual classification** | Low, bounded by platform mass and the demo backstop. |
| **Disposition** | **BLOCKED ON VERIFICATION** — the mechanism is proven; the numbers are the Rev A bench items. |
| **Evidence** | `src/gateway/contract_profiles.rs`; `docs/safety/OCCY_MCDC_EVIDENCE.md`; `verification/kani/`. |
| **Open verification** | Bench max-speed and braking measurement (#1216). Provenance rows tracked by #1219. |

### R2-RS-002 — H-002 — Lateral bound rests on an unmeasured steering rate

| Field | Value |
|---|---|
| **Hazardous event** | A steering command is admitted that the servo cannot track, or tracks faster than the lateral-accel clamp assumed, producing a path the checker believed was bounded. |
| **Credited mitigations** | Bicycle-model lateral clamp `a_lat = v²·|tan δ| / L ≤ max_lateral_accel_mps2` — **IMPLEMENTED + VERIFIED**. `max_steering_deg = 39.0` — **MEASURED** (bench Phase C, 2026-07-17). Wheelbase `0.229` — **MEASURED**. |
| **Credited AOUs** | None. |
| **Why risk remains** | `max_steering_rate_deg_s = 30.0` is **VALIDATION-PENDING with the measurement named as owed** — "servo slew NOT yet measured (time a full −45→+45 sweep)". The rate limit is the one term in this bound taken on faith, and it is the term that governs how fast the geometry can change between checks. |
| **Detection / ODD exit** | **DETECTED → POSTURE DROPS** for an out-of-envelope command. **NOT DETECTED** if the servo simply does not track the commanded rate. |
| **S / E / C after mitigation** | S1 / E2 / C1. Rollover of a 0.2 m wheelbase platform at ≤1 m/s is not a credible harm; the real consequence is path divergence from the checked trajectory. |
| **Residual classification** | Low. |
| **Disposition** | **BLOCKED ON VERIFICATION** — one bench measurement discharges it. |
| **Evidence** | `crates/kirra-core/src/kinematics_contract.rs`; `robot/r2_drive_calibration_results.txt` Phase C. |
| **Open verification** | Servo slew-rate bench capture (#1216). Also gates #1213's proposed swept-envelope extension. |

### R2-RS-003 — H-003 — A wedged lidar is arrival-fresh and content-frozen

| Field | Value |
|---|---|
| **Hazardous event** | The doer plans and the checker admits motion against a world that stopped updating, because a wedged driver republishes one buffer at the full rate. The robot drives into whatever entered the scene after the freeze. |
| **Credited mitigations** | `scan_stale_s = 0.25` arrival-freshness hold — **IMPLEMENTED + VERIFIED**, and *structurally incapable* of covering this scenario. Confidence floor 0.5 → 0.0 cap — **IMPLEMENTED + VERIFIED**, covers a degraded scan, not a repeated valid one. Frame-identity detector (`sensor_freshness.py`, `STAMP_NOT_ADVANCING`, #1211) — **IMPLEMENTED, NOT ARMED** (`frame_health_enabled: false`). |
| **Credited AOUs** | AOU-R2-LIDAR-HEALTH-001. |
| **Why risk remains** | Every deployed freshness check measures **arrival**. A frozen stream satisfies all of them. The detector that measures **identity** exists and is disarmed, for a stated and defensible reason: a driver that never populates `header.stamp` would trip it immediately and hold the robot, so it must not be armed before the deployed driver's stamp behaviour is observed. |
| **Detection / ODD exit** | **NOT DETECTED.** The stack reports healthy perception throughout. |
| **S / E / C after mitigation** | S1 / E2 / C2. Higher C than the rows above: the supervisor sees a robot driving confidently and has no cue that its world model is frozen. |
| **Residual classification** | Moderate — the highest-consequence *undetected* row, because the system actively reports health. |
| **Disposition** | **BLOCKED ON VERIFICATION** — arming is a config change gated on one bench observation. |
| **Evidence** | `ros2_ws/src/kirra_safety/kirra_safety/sensor_freshness.py`; `config/kirra_params.yaml`. |
| **Open verification** | Confirm TG30 `header.stamp` monotonicity on the bench, then set `frame_health_enabled: true` with a measured `frame_stall_budget` (#1216). |

### R2-RS-004 — H-006 — No software-independent stop for a wedged process

| Field | Value |
|---|---|
| **Hazardous event** | The consumer process wedges, or the Jetson hangs, with the motors at a non-zero PWM. The robot continues at its last commanded velocity until a human removes power by hand. |
| **Credited mitigations** | ADR-0033 verify-before-release chokepoint — **IMPLEMENTED + VERIFIED**; a crashed *verifier* mints no tokens, so the consumer starves. SS-002 liveness clock (`make_tick_handler` → `consumer.on_tick`) — **IMPLEMENTED + VERIFIED**; token silence, and equally a refusal flood, ramps the commanded velocity to zero on the consumer's own timer. `safe_stop()` on SIGTERM/exit — **IMPLEMENTED + VERIFIED**. Hardware e-stop (`R2_ESTOP_SPEC.md` R1–R7) — **SPECIFIED, NOT IMPLEMENTED**, credited at zero. |
| **Credited AOUs** | AOU-R2-SUPERVISION-001. |
| **Why risk remains** | The credited software mitigations are stronger than they first appear and cover more than expected: the deployed `r2_ackermann` path has a **timer-driven** liveness ramp, so "commands stopped arriving" is a handled case, not a residual. What remains is narrower and irreducible in software: **the process is alive but its timers are not firing**, or the SBC has hung. Nothing that requires software to be executing can cover that, which is precisely requirement R1 of the unbuilt e-stop. Note also that the `r2cp` drive mode delegates a watchdog to the MCU — **that mode is not the deployed one** (`KIRRA_DRIVE_MODE=r2_ackermann`), so that watchdog is not credited here. |
| **Detection / ODD exit** | **OUTSIDE CLAIMED ARCHITECTURE.** By construction no software mechanism observes it. |
| **S / E / C after mitigation** | S1 / E1 / **C3**. Severity and exposure are low; controllability is the worst on the platform — the operator's only recourse is physical. |
| **Residual classification** | **The platform's largest residual**, on controllability rather than severity. |
| **Disposition** | **REQUIRES FURTHER MITIGATION.** Not eligible for acceptance as a bare residual unless the acceptance authority explicitly accepts "a human within SSH and physical reach" as the operational control — which no one has yet been asked to do. |
| **Evidence** | `robot/kirra_motor_consumer.py` (`make_tick_handler`, `safe_stop`); ADR-0033; `docs/hardware/R2_ESTOP_SPEC.md`. |
| **Open verification** | Build R1–R7. Until then the operating envelope is bounded by supervisor reach, and that bound should be stated in the operating procedure, not inferred. |

### R2-RS-005 — H-008 — The audit ledger has no off-box copy

| Field | Value |
|---|---|
| **Hazardous event** | An incident cannot be reconstructed because the only ledger lived on the robot that was involved in it. |
| **Credited mitigations** | SHA-256 hash-chained ledger with tamper detection — **IMPLEMENTED + VERIFIED** (crash-consistency and power-loss drills, `tests/audit_chain_prefix_on_kill.rs`). WORM off-box shipping (`audit_shipper.rs`) — **IMPLEMENTED, NOT ARMED** (`KIRRA_AUDIT_SHIP_PATH` unset on this deployment). |
| **Credited AOUs** | None. |
| **Why risk remains** | The chain detects tampering; it does not survive destruction. On-box storage loss, host compromise, or simple SD-card failure removes the record. |
| **Detection / ODD exit** | **DETECTED → POSTURE DROPS** for tampering (the chain verifies). **NOT DETECTED** for loss. |
| **S / E / C after mitigation** | S1 / E2 / C3, matching the HARA's own S1 — this is an investigability harm, not a physical one. |
| **Residual classification** | Low, and cheap to close (one env var plus a sink). |
| **Disposition** | **PENDING ACCEPTANCE** — reasonable to accept for bench operation; should not silently carry into any deployment that matters. |
| **Evidence** | `src/audit_shipper.rs`; `src/audit_chain.rs`. |
| **Open verification** | None. Arming is a configuration decision. |

### R2-RS-006 — H-013 — On-box health reports are unauthenticated

| Field | Value |
|---|---|
| **Hazardous event** | A faulted sensor is promoted back to Trusted early because health reports were manipulated, restoring Nominal posture over a degraded input. |
| **Credited mitigations** | Recovery hysteresis — 5 consecutive healthy reports within a 10 s window, fault resets the streak — **IMPLEMENTED + VERIFIED**. Attestation-signed adoption reports exist for the OTA path — **not applicable** to sensor health reports. |
| **Credited AOUs** | AOU-ACTUATION-SERIAL-001 (partial — bounds who can actuate, not who can report). |
| **Why risk remains** | `sensor_monitor` posts to the verifier over localhost with a client-id header, not a per-node signature. Any process on the Orin can post health reports. The hysteresis bounds how *fast* a false recovery can occur; it does not bound *who* can claim one. |
| **Detection / ODD exit** | **NOT DETECTED.** A well-formed forged report is indistinguishable from a real one at this interface. |
| **S / E / C after mitigation** | S1 / E1 / C2. Requires code execution on the Orin, at which point stronger attacks are available. |
| **Residual classification** | Low — dominated by the host-compromise precondition. |
| **Disposition** | **PENDING ACCEPTANCE** — the honest framing is that on-box process isolation, not report authentication, is the control. |
| **Evidence** | `src/recovery_hysteresis.rs`; `ros2_ws/src/kirra_safety/config/kirra_params.yaml` (`sensor_monitor`). |
| **Open verification** | None proposed. Signing on-box reports would be defence against an attacker who has already won. |

---

## 4. Where Assertion B fails — three residuals resolve to no HARA hazard

Assertion B ("every residual row resolves to an applicable hazard") was meant as
the non-duplication guard. Running it produces a finding instead.

**`HARA.md`'s 17 hazards are all malfunction hazards.** Every one has the shape
*Kirra passes X*, *Kirra fails to detect Y*, or *Kirra crashes*. That is correct
for an ISO 26262 Part 3 analysis of a software item — its subject is what
happens when the item malfunctions.

The R2's three largest environmental risks are not of that shape. In each, the
software **functions exactly as designed** against inputs that are complete,
internally consistent and wrong:

| Scenario | Why no hazard covers it |
|---|---|
| Driving off an unfenced stair | The corridor is genuinely clear in the plane the sensor measures. No check is bypassed, no limit exceeded, no fault missed. |
| Contact with an uninstructed person | The VRU bound is disarmed by configuration; nothing malfunctions when it does not fire. |
| Operation outdoors | No weather-derate path exists to fail. |

These are SOTIF hazards (ISO 21448 — hazardous behaviour absent system failure),
and the HARA is a 26262 document. Forcing them into a hazard row would require
fabricating a malfunction that does not occur; dropping them would remove the
three residuals a supervisor most needs to know about.

**Resolution taken here:** they are recorded as ODD-derived residuals in §5, in
the same schema, keyed to their ODD boundary rather than to a hazard, and
Assertion B is restated as:

> Every residual row resolves to **either** an applicable HARA hazard **or** a
> stated ODD boundary in KIRRA-R2-ODD-001 — and rows of the second kind are
> flagged as evidence that the HARA does not yet cover this platform's SOTIF
> hazard class.

**Recommended follow-up (not taken in this issue):** extend `HARA.md`, or add an
R2 SOTIF analysis in the shape of `OCCY_SOTIF.md` §3's triggering-condition
catalog, so this class has a home. Filed as a note on #1220 rather than done
here, because amending the HARA is a safety-review action and not a
documentation one.

---

## 5. ODD-derived residuals (no HARA hazard)

### R2-RS-007 — ODD S2 — Negative obstacles are invisible to a planar lidar

| Field | Value |
|---|---|
| **Hazardous event** | The R2 drives over a stair edge or drop-off. |
| **Credited mitigations** | None in software. The corridor check, envelope clamp and RSS bounds all operate correctly and all pass. |
| **Credited AOUs** | AOU-R2-SURFACE-001. |
| **Why risk remains** | A horizontally-scanning 2-D lidar provides **no evidence of a ground-plane discontinuity**. This is not insufficient conservatism — the measurement does not exist, so no downstream margin reconstructs it. Operator fencing is the control, and it is the entire control. |
| **Detection / ODD exit** | **NOT DETECTED.** No posture transition is expected or possible. |
| **S / E / C** | S1 / E1 / C3 — uncontrollable once initiated; the fall is over before a supervisor can react. |
| **Residual classification** | Acceptable **only** for fenced operation under AOU-R2-SURFACE-001. |
| **Disposition** | **PENDING ACCEPTANCE** as a fenced-operation residual. Becomes **REQUIRES FURTHER MITIGATION** the moment unfenced operation is contemplated. |
| **Evidence** | `docs/safety/R2_ODD.md` §2.2; `sensor_msgs/LaserScan` on `/scan` — a single horizontal plane. |
| **Open verification** | None. Closing it needs a sensor (downward ToF, cliff sensor, or depth-camera floor extraction), which is #1217's territory. |

### R2-RS-008 — ODD E3 — Camera-dependent tightening can be unavailable without an exit indication

| Field | Value |
|---|---|
| **Hazardous event** | A hazard the camera would have clipped from the corridor is not clipped, so the ego proceeds into a region a fully-lit run would have refused. |
| **Credited mitigations** | Lidar-only free-space authority — **IMPLEMENTED + VERIFIED**; camera input is structurally **tighten-only** (`clip_corridor_to_hazards` truncates; `SemanticClass::is_drivable()` is true for `Road` alone; the KPI gate's `ForbiddenLoosen` row is a hard zero). |
| **Credited AOUs** | None. |
| **Why risk remains** | Stated carefully, because the easy phrasing overclaims in both directions. **Darkness does not independently grant motion authority** — free-space evidence comes from the lidar and the camera cannot extend a corridor. What it can do is fall silent: camera-dependent *tightening* becomes unavailable, and there is **no direct environmental-exit indication** that it has. The corridor reverts to the conservative Phase-A geometry, which is correct behaviour and is also indistinguishable, from outside, from a run where the camera had nothing to report. |
| **Detection / ODD exit** | **DETECTED BY CONSEQUENCE** for the free-space path — the architecture degrades correctly without checking illumination. **NOT DETECTED** as an ODD exit: nothing announces that the tightening input has gone. |
| **S / E / C** | S1 / E2 / C2. |
| **Residual classification** | Low. The failure direction is loss of conservatism relative to a lit run, never a false permission. |
| **Disposition** | **PENDING ACCEPTANCE.** Adding an illumination check would be a *worse* design — a light sensor asserting adequate illumination while the camera is fouled, misaimed or lens-capped grants confidence the perception channel has not earned. The defensible improvement is a camera-liveness indication, not an environment sensor. |
| **Evidence** | `docs/hardware/CAMERA_PERCEPTION_INTEGRATION.md`; `docs/safety/R2_ODD.md` §2.9; the `camera_can_never_extend_the_corridor` sweep. |
| **Open verification** | None required for the safety claim. A camera-contribution health signal would close the observability half. |

### R2-RS-009 — ODD P1/P2 — Persons are an operator obligation, not a checker input

| Field | Value |
|---|---|
| **Hazardous event** | Contact with a person who entered the operating area unbriefed. |
| **Credited mitigations** | Generic object avoidance via the corridor and RSS bounds — **IMPLEMENTED + VERIFIED**; a person is an obstacle like any other and is avoided as one. Omnidirectional pedestrian bound (`vru_channel::resolve_vru_channel`) — **IMPLEMENTED, NOT ARMED** (`KIRRA_VRU_CHANNEL_ENABLED` off). |
| **Credited AOUs** | AOU-R2-CROWD-001. |
| **Why risk remains** | Persons receive no treatment distinct from furniture. The VRU-specific bound cannot be armed without a pedestrian producer publishing at rate — armed-and-silent resolves to an MRC floor, which is the correct fail-closed behaviour and the reason it cannot be enabled speculatively. |
| **Detection / ODD exit** | **NOT DETECTED** as a person; **DETECTED → POSTURE DROPS** as an obstacle, if within the scan plane. Note the interaction with R2-RS-007: a small child below the scan plane is not detected as either. |
| **S / E / C** | S1 / E2 / C1. A ~1 kg platform at ≤1 m/s, and at the deployed 0.15 m/s backstop the contact energy is negligible. |
| **Residual classification** | Low by physics rather than by control. |
| **Disposition** | **PENDING ACCEPTANCE** for supervised operation. |
| **Evidence** | `docs/safety/R2_ODD.md` §2.4; AOU-VRU-RATE-001. |
| **Open verification** | A pedestrian producer at rate would allow arming the bound (#1217). |

---

## 6. Summary

| Disposition | Rows |
|---|---|
| ACCEPTED | **none** |
| PENDING ACCEPTANCE | R2-RS-005, R2-RS-006, R2-RS-007, R2-RS-008, R2-RS-009 |
| REQUIRES FURTHER MITIGATION | R2-RS-004 |
| BLOCKED ON VERIFICATION | R2-RS-001, R2-RS-002, R2-RS-003 |

Two observations worth carrying out of the walk.

**The BLOCKED rows are all one hardware session.** R2-RS-001, -002 and -003
close on bench measurements and one config change, all of which belong to the
Rev A session (#1216). They are not open-ended.

**The severity profile is not the HARA's.** Every row lands at S1. The HARA's
S3 ratings describe a road vehicle; a 0.2 m, ~1 kg platform at ≤1 m/s in a
supervised indoor area does not inherit them. What distinguishes the rows here
is **controllability** — R2-RS-004 at C3 and R2-RS-007 at C3 are the platform's
real exposure, and both are cases where a supervisor who notices has no
effective action left.

---

## 7. Cross-references

- `docs/safety/HARA.md` (AEGIS-HARA-001) — the hazard source
- `docs/safety/R2_ODD.md` (KIRRA-R2-ODD-001) — boundaries and enforcement status
- `docs/safety/ASSUMPTIONS_OF_USE.md` — the five R2 AOUs cited above
- `docs/safety/SAFE_STATE_SPECIFICATION.md` SS-002 — the liveness ramp credited in R2-RS-004
- `docs/hardware/R2_ESTOP_SPEC.md` — specified, not implemented
- `docs/adr/0033-actuation-authority-ros-r2-topology.md` — the release chokepoint
- Issues #1220 (this register), #1216 (Rev A measurements), #1219 (numeric provenance), #1217 (perception depth)
