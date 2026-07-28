# System interfaces — the one-page wiring architecture

> **Status: architectural, frozen at the signal-role level with this
> document (ratified at DR-1).** No pin numbers, no connector numbers, no
> voltages beyond declared domains — those live in `pin-allocation.md` and
> `connector-map.md` and stay `Pending` until DR-2. This page is the
> stable contract: Rev B, Rev C, or an MCU migration should change it
> little or not at all. Signal names are the `safety.md` /
> `pin-allocation.md` worksheet symbols.

## 1. The robot, wired (architecture only)

```
  Jetson Orin NX  (verifier / governor / consumer — authorization authority)
        │
        │  UART — R2CP v1 (Interface 1)
        │
  STM32 (NUCLEO-G474RE on the Rev A carrier)
        ├── ENCODER_L_A/B, ENCODER_R_A/B      ← wheel encoders
        ├── STEERING_PWM →, STEERING_FB ←     ↔ steering gear (FB optional)
        ├── PWM_LEFT_*, PWM_RIGHT_* →         → motor drivers (Interface 2)
        ├── DRIVER_FAULT_L_N, _R_N ←          ← motor drivers (read-only)
        ├── MCU_WATCHDOG_KICK →               → external watchdog/supervisor
        ├── MCU_ENABLE_REQUEST →              → hardware enable gate
        ├── E_STOP_SENSE ←                    ← E-stop loop (read-only)
        ├── SWD (SWDIO/SWCLK/NRST)            ↔ debug (ST-LINK)
        └── status LEDs →                     (indicate, never gate)

  Hardware enable gate (on the carrier — Interface 3, safety.md)
        E_STOP_OK_HW ──┐
        WATCHDOG_OK_HW ┼─ AND ──► DRIVER_ENABLE_HW ──► motor drivers ENABLE
        MCU_ENABLE_REQUEST ┘

  External motor drivers (retained — HDR-0006)
        drive inputs ← STM32     ENABLE ← gate     FAULT → STM32
        motor power ← battery via E-stop relay chain (never via carrier)
```

## 2. Power tree (architecture only)

```
  Battery (external — RA-X2/X7)
     ├──► E-stop relay chain ──► motor drivers ──► motors     (never crosses
     │        (../R2_ESTOP_SPEC.md)                            the carrier)
     ├──► Jetson supply (Jetson's own regulation — not Rev A's concern)
     └──► external buck/regulator ──► LOGIC_5V_IN (protected input:
              fuse, reverse-polarity, TVS, decoupling —
              power-and-grounding.md §3)
                 └──► Nucleo 5 V path (per verified MB1367 config)
                        └──► G474 3.3 V logic/IO domain
```

The buck sits **off-board** for Rev A (final production power distribution
is excluded, RA-X7); the carrier begins at the protected `LOGIC_5V_IN`.

The E-stop relay chain is **off-board wiring in the external motor-supply
path** (`../R2_ESTOP_SPEC.md`) — the switching of motor battery current
happens entirely outside the carrier. Rev A is never part of the raw
motor-current route (RA-X3): its only contact with the E-stop system is
logic-level — the NC loop feeding `E_STOP_OK_HW` and the read-only
`E_STOP_SENSE`.

## 3. Interface 1 — Jetson ↔ MCU (frozen)

| Signal | Role |
|---|---|
| `R2CP_TX` / `R2CP_RX` | R2CP v1, UART carrier — protocol normative in `firmware/rosmaster-r2/docs/PROTOCOL.md`; no wire constants bound here |
| GND | Common logic reference |
| *(optional)* MCU reset | Jetson-driven `NRST` — **DR-2 decision, default absent** |
| *(optional)* boot select | Jetson-driven `BOOT0` for ROM-loader recovery — **DR-2 decision, default absent** |

The optional lines are deliberately undecided: a Jetson-controlled
reset/boot path eases recovery (the firmware tree requires preserving a
recoverable ROM-loader path) but hands a rich-OS process the ability to
put the MCU in bootloader mode, so admitting them is an explicit DR-2
decision with the `safety.md` §4 rows (reset and bootloader both converge
to disabled actuation) re-verified for the chosen wiring. Level
translation per `interfaces.md` §4 — domains measured, never assumed.

## 4. Interface 2 — MCU ↔ Motor Driver (frozen as roles)

Per driver side (left/right):

| Role | Signal(s) | Note |
|---|---|---|
| Drive command | `PWM_LEFT_IN1`/`IN2`, `PWM_RIGHT_IN1`/`IN2` | Two lines per side. Whether the retained drivers interpret them as an IN1/IN2 pair or as PWM+DIR is **measured at bring-up stage 11**, not assumed — the *count and direction* are frozen, the encoding is bench evidence |
| Enable | `DRIVER_ENABLE_HW` | From the Interface 3 gate only — never a bare MCU GPIO |
| Fault | `DRIVER_FAULT_L_N` / `DRIVER_FAULT_R_N` | Read-only into the MCU; polarity/latching measured |

A future driver choice (Rev B integrated drivers, different modules) may
re-bind the drive-command encoding; the role set — drive command, gated
enable, read-only fault — is the stable contract.

## 5. Interface 3 — MCU ↔ Safety (frozen)

The five authority signals are the **hardware API of the safety
architecture** (`safety.md` is normative):

| Signal | Direction | Role |
|---|---|---|
| `E_STOP_OK_HW` | loop → gate | Hardware permission from the NC E-stop loop |
| `MCU_WATCHDOG_KICK` | MCU → supervisor | Health pulse from firmware |
| `WATCHDOG_OK_HW` | supervisor → gate | Independent watchdog permission |
| `MCU_ENABLE_REQUEST` | MCU → gate | Firmware's request; default inactive |
| `DRIVER_ENABLE_HW` | gate → drivers | The conjunction; sole energize path |

Companions (observability, never authority): `E_STOP_SENSE`,
`DRIVER_FAULT_L_N`, `DRIVER_FAULT_R_N`.

These names and roles do not change between board revisions without a
decision record; firmware, tests, and the FDIT matrices reference them as
an API.

## 6. What this page deliberately omits

Pin numbers, Morpho mapping, connector designators/pinouts, voltage
values, translator parts, timing windows, and the drive-command encoding
— all DR-2 outputs owned by `pin-allocation.md`, `connector-map.md`, and
the bring-up evidence. If a detail here ever conflicts with those
documents after DR-2, the measured/frozen worksheet wins and this page is
corrected.

## 7. Rev A success criteria (scope guard)

Rev A answers one question: **"Can Kirra completely replace the Yahboom
MCU?"** — not "can Kirra become the perfect robotics controller." Rev A
is a success when it:

1. boots to the latched-safe state;
2. handshakes over R2CP;
3. drives the motors (wheels up);
4. reads the encoders;
5. enforces the watchdog term;
6. enforces the E-stop term;
7. survives HIL testing (`bringup-plan.md` stage 14).

Everything else — CAN-FD, EtherCAT, isolated power, richer diagnostics,
integrated drivers — is Rev B+ material (HDR-0002), and adding it to
Rev A is scope creep to be rejected at review.
