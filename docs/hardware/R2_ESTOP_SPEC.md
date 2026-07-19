# R2 hardware e-stop — specification

> **Status: design spec (buildable). This is the ONE prerequisite for driving the
> R2 untethered** (`R2_UNTETHERED_BRINGUP.md` §3). Your SSH Ctrl-C soft-stop
> vanishes off-network; the software safe-state (SS-002 decel, ADR-0013 request)
> is necessary but **not sufficient** — a wedged process, a hung SBC, or a lost
> link must STILL be stoppable by a human. That requires a **software-independent
> hardware kill in the motor-power line.** The build is electrical; this spec is
> the engineering, parameterized where the R2's exact power topology must be
> confirmed on the unit.

## 1. Requirements (what "correct" means)

| # | Requirement | Rationale |
|---|---|---|
| R1 | **Software-independent.** The kill removes motor power through hardware; it works with the Jetson hung, the consumer crashed, or the code in any state. | The whole point — software can't stop software that's wedged. |
| R2 | **Fail-safe / energize-to-run.** De-energized = stopped. Any fault (button pressed, RF lost, coil power lost, wire cut) opens the circuit → motors dead. | Loss of anything defaults to STOP, never to run. |
| R3 | **Latching.** A hit stays stopped until a human deliberately resets (twist-release), never auto-clears. | Mirrors LockedOut's human-reset semantics. |
| R4 | **Cuts DRIVE power, keeps COMPUTE alive** (if the topology allows). | The Jetson stays up to log the event + hold the software safe-state + let Rabbit announce it. A blunt whole-robot kill is the acceptable fallback if the rails can't be split. |
| R5 | **Deliberate two-part re-arm.** Hardware twist-release restores power, but motion does NOT resume until a software clearance (ADR-0013 grant). | Prevents a post-reset lurch; defense in depth. |
| R6 | **Sense line (advisory).** The e-stop state is readable by software (a GPIO input) so the consumer latches a hold + Rabbit says "emergency stop engaged." | Observability only — NEVER the stopping mechanism (R1 stands alone). |
| R7 | **Reachable.** A mushroom head on the robot AND an RF fob for range. | You must be able to hit it from where you watch. |

## 2. Where it cuts (relative to the existing safety spine)

Three nested layers, outermost wins:

```
  planner (doer) ─► KIRRA checker ─► verifier mint ─► consumer FFI verify (ADR-0033)
                                                            │  set_motor over /dev/myserial
       (1) software MRC: SS-002 decel-to-stop, ADR-0013     ▼
       (2) sense line → consumer latches hold          [ Rosmaster motor driver ]
                                                            │  motor + steering power
       (3) HARDWARE E-STOP  ── the SAFETY RELAY ──────────►╳  ← opens here (R1, software-independent)
                                                            ▼
                                                          wheels
```

- **(1)** is the graceful, software stop (already built).
- **(2)** is advisory — the consumer reads the sense line and behaves consistently.
- **(3)** is this spec: a relay in the **motor-drive power feed** that opens on any
  fault, independent of (1)/(2). **Cut the motor-driver supply, not the Jetson
  rail** (R4) — confirm on the unit which conductor that is (see §5 unknowns).

## 3. The circuit — an energize-to-run safety relay (primary design)

The classic e-stop pattern. One **safety relay** sits in series with the motor
supply; its coil is held energized (relay closed → motors powered) ONLY while the
whole safety chain is intact:

```
   motor supply (+) ──►[ SAFETY RELAY contacts (NO, held closed) ]──► Rosmaster motor-driver (+)

   coil drive (+V) ──►[ NC mushroom E-stop ]──►[ RF-receiver run contact ]──► RELAY COIL ──► GND
                        (open when pressed)      (open on RF loss/low batt)
```

- **Mushroom pressed** → coil circuit open → relay drops out → motors dead. Latched
  (mushroom stays in until twisted).
- **RF fob kill** (or RF signal lost, or fob battery dead) → receiver run-contact
  opens → relay drops out → motors dead. **Fail-safe on link loss** (R2).
- **Coil power lost / wire cut** → relay drops out → dead (R2).
- Relay **energizes to run**, so the *default* everywhere is STOP.

**Minimal version (no RF, bench first):** the NC mushroom **directly in series**
with the motor supply (no relay) if the switch is rated for the motor current.
Simpler, but no remote kill and the switch carries full motor current — the relay
version is preferred (the switch then only carries coil current) and is required
once you add RF.

## 4. The sense line + two-part re-arm (R5/R6)

- **Sense (advisory):** a spare relay contact (or a voltage divider off the motor
  feed) into a Jetson **GPIO input**. A small watcher (mirror of `ptt_button.py`)
  reads it; on "engaged" the **consumer latches a hold** like LockedOut and Rabbit
  (`rabbit_watch.py`) announces *"Emergency stop engaged."* This is observability —
  the hardware already cut power; software just stays consistent.
- **Re-arm (deliberate, two-part):**
  1. **Hardware:** twist-release the mushroom (and/or RF re-arm) → motor power
     restored.
  2. **Software:** the consumer STAYS held (no releases minted) until an explicit
     **ADR-0013 clearance grant** (`ClearanceLoop::try_clear`, `OperatorClearance
     Grant`) — the same operator-authenticated path that clears a post-collision
     latch. Only then may motion resume. **No lurch on reset.**

## 5. What to confirm on the unit (the honest unknowns)

Parameterize these before buying parts — they are R2-specific and not yet measured:

- **Motor supply voltage + stall current** — sizes the relay/contactor contacts and
  the mushroom rating. (Small robot: likely ~7–12 V, a few A per motor; **measure
  the stall current**, don't assume.)
- **Power topology** — which conductor feeds the Rosmaster **motor driver** vs. the
  Jetson/compute rail, so the kill cuts drive-only (R4). If they can't be split on
  this board, fall back to a **master kill of the whole robot** (blunter — you lose
  logging, but wheels definitely stop; R4 becomes best-effort).
- **Steering servo behavior on cut** — cutting drive power leaves the Ackermann
  servo either unpowered (wheels hold last angle, limp) or also cut. Decide whether
  to also cut/center the servo; for a low-speed stop, drive-cut alone is acceptable
  (the robot decelerates roughly straight).
- **The battery main switch is separate** — the e-stop is NOT the on/off switch; it
  is an always-in-circuit safety element.

## 6. Test procedure (before the first untethered meter)

Wheels-up first, then tethered floor:

1. **Kill under drive:** command slow motion; hit the mushroom → wheels stop
   **immediately** (verify < ~250 ms by eye/scope). Latched: stays stopped.
2. **RF loss = stop:** with the RF kill armed, power off the fob (or walk out of
   range) → motors drop out (fail-safe, R2).
3. **Compute survives (R4):** during a kill, confirm the Jetson stays up — `curl
   localhost:8090/health` still answers, and `rabbit_watch` announced the sense event.
4. **No-lurch re-arm (R5):** twist-release → confirm the robot does NOT move until
   you issue the software clearance; then it resumes.
5. **Software-independent (R1):** kill the consumer process (`pkill`), then hit the
   e-stop mid-coast → wheels still cut (proves the hardware path doesn't need code).

Record results in the bring-up log; only after all five pass is untethered driving
in scope.

## 7. Bill of materials (indicative — size per §5)

- 1× **latching mushroom E-stop**, NC contacts, rated ≥ motor supply V and (if
  used in-series) ≥ stall current.
- 1× **safety relay / contactor**, coil at the drive voltage, contacts rated ≥
  stall current, **energize-to-run** wiring (NO contacts held closed).
- 1× **RF relay kit** (receiver + fob) with an **energize-to-run** run-contact
  (signal present = closed), for the remote kill (R7). Confirm its fail-open-on-
  signal-loss behavior.
- Wiring, fuse on the motor feed, a spare relay contact (or divider) + a resistor
  for the Jetson GPIO **sense** input (level-shift to 3.3 V — do NOT feed motor
  voltage into the Orin).

## 8. Scope + honesty

- This is a **functional safety element added by the integrator**; it is not
  certified and its rating must match the measured R2 electricals (§5).
- It composes with, and does not replace, the software safe-state — SS-002 decel
  and the ADR-0013 clearance path stay live; the e-stop is the outermost,
  software-independent layer and the re-arm gate.
- The GPIO sense line is **advisory only** — the hardware kill (R1) never depends
  on the Jetson reading it.

## References
- `docs/hardware/R2_UNTETHERED_BRINGUP.md` §3 — why the e-stop is the prerequisite
- ADR-0013 / SS-002 — the software e-stop request + decel-to-stop safe state
- ADR-0033 — the on-device verify chokepoint (the software actuation gate the
  e-stop sits OUTSIDE of)
- `parko/crates/parko-core/src/impact.rs` — `OperatorClearanceGrant` /
  `ClearanceLoop` (the authenticated clearance the two-part re-arm reuses)
