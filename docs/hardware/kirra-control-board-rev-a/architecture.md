# Rev A architecture

> **Status: DR-1 input.** Block-level only. No pin, part, or connector value
> in this document is verified unless it cites a source; see
> `pin-allocation.md` for the evidence discipline.

## 1. System context — where Rev A sits

The bring-up robot today (the *current state*, recorded in
`firmware/rosmaster-r2/docs/ROADMAP.md` §Deployment reality):

- **Jetson Orin NX**, Ubuntu 22.04 / JetPack 6.2, ROS 2 Humble;
- **Kirra verifier/governor** — the authorization authority (the checker);
- **Kirra motor consumer** (`robot/kirra_motor_consumer.py`) — verifies the
  Ed25519 release token (ADR-0033) before any bytes reach the wire;
- **`/dev/myserial`** — the single-writer serial boundary (CH340 USB bridge,
  `TIOCEXCL`, 0600, boot sentinel);
- **Yahboom/Rosmaster MCU firmware** on the vendor expansion board
  (STM32F103RCT6), unverified;
- **external Yahboom motor hardware** — H-bridge driver modules, two 520
  propulsion motors with quadrature encoders, one steering gear.

The *authorization* boundary is already Kirra's, in Linux userspace. The
*actuation* boundary is still the vendor's. Rev A moves the actuation
boundary — the MCU, its firmware, and the hardware enable gate — into
Kirra's hands, while the motor-power electronics stay external and
unchanged.

```
                         current                       Rev A
                         ───────                       ─────
 Rabbit/Mick → planner → verifier/governor   (unchanged — still required)
             → kirra_motor_consumer          (unchanged — still required)
             → serial (single writer)      → R2CP over UART (level-shifted,
             → Yahboom MCU (vendor fw)       see interfaces.md)
             → Yahboom drivers → motors    → NUCLEO-G474RE on Kirra carrier
                                             (Kirra firmware)
                                           → DRIVER_ENABLE_HW gate (safety.md)
                                           → EXISTING external motor drivers
                                           → motors / steering / encoders
```

**Replacing the board does not remove the Jetson verifier or governor.**
R2CP's `AUTH_TAG` authenticates the *link*; it says nothing about whether
the checker authorized the motion. ADR-0033's token-verifying consumer, the
serial ACL, and the startup sentinel remain the trust boundary
(`firmware/rosmaster-r2/docs/ROADMAP.md` §Final Kirra-owned state).

## 2. What Rev A is

An **interface and hardware-safety carrier** around a NUCLEO-G474RE:

- **Mechanical + electrical host** for the Nucleo: mounting, Morpho-header
  connection (`decisions/HDR-0001-nucleo-g474re-for-rev-a.md`).
- **Jetson-to-MCU R2CP transport**: the UART link, level translation
  (selected only after voltage-domain verification — `interfaces.md` §4),
  protection, and test points. The protocol itself is normatively defined in
  `firmware/rosmaster-r2/docs/PROTOCOL.md`; Rev A binds no wire constants.
- **Hardware actuation gate**: E-stop input, independent external watchdog,
  and the hardware-combined driver enable
  (`DRIVER_ENABLE_HW = E_STOP_OK_HW AND WATCHDOG_OK_HW AND
  MCU_ENABLE_REQUEST` — `safety.md`). Firmware *requests* actuation; it
  never possesses sole authority to energize the drivers.
- **Signal conditioning** for the retained vendor actuator set: encoder
  inputs, steering PWM output (+ optional feedback input), motor-driver
  control outputs, driver-fault inputs.
- **Observability and service**: status LEDs, SWD/debug access, test points
  on every safety-critical and communication signal.
- **Protected logic power input**: low-current only; fuse, reverse-polarity,
  transient suppression, decoupling (`power-and-grounding.md`).

## 3. What Rev A is not

Explicitly excluded (see `requirements.md` §3 for the normative list):
integrated high-current motor drivers, battery charging, raw motor-current
routing, wireless networking, camera interfaces, a final integrated MCU
package, final production power distribution, and any automatic approval to
drive on the floor. The existing external motor drivers remain in use.

Motor current **never flows through the carrier**. The carrier carries
logic-level control, sense, and enable signals only.

## 4. Relationship to the firmware tree

`firmware/rosmaster-r2/` is a clean-room, portable firmware platform whose
core is device-agnostic (abstract HAL seams; only the BSP layer may include
STM32 headers — `firmware/rosmaster-r2/docs/ARCHITECTURE.md` §Layering). Its current BSP target is
the vendor board's STM32F103RCT6.

Rev A supplies the *hardware* for the architecture's already-recorded plan:
"plan an STM32G4/H7 control-board revision for CAN-FD, hardware crypto and
stronger diagnostics." A NUCLEO-G474RE BSP is therefore **future firmware
work**, gated exactly like the F103 BSP is: no register implementation until
the board revision is physically verified
(`firmware/rosmaster-r2/drivers/README.md`). Nothing in this directory
claims that BSP exists.

In the roadmap's staged migration, Rev A is the **stage 2 bench board**
("R2CP to Kirra firmware on a bench board") and, after HIL acceptance, the
stage 3 swap target. Stage 1 (the consumer's R2CP drive mode against the
PTY simulated MCU) remains a prerequisite and is host-side work independent
of this board.

## 5. Ownership boundary after Rev A

| Element | Owner after Rev A |
|---|---|
| Motion authorization (verifier/governor, release tokens) | Kirra (unchanged) |
| Consumer-side release verification, serial single-writer | Kirra (unchanged, still required) |
| R2CP host codec + bridge | Kirra (unchanged) |
| MCU board (carrier + Nucleo) | **Kirra (new)** |
| MCU firmware | **Kirra (new — clean-room tree, G474 BSP pending)** |
| Hardware actuation gate (E-stop loop, watchdog, combined enable) | **Kirra (new)** |
| Motor drivers, motors, encoders, steering gear | Yahboom hardware, retained (`decisions/HDR-0006-retain-external-motor-drivers.md`) |
| Battery + motor power distribution | External / vendor, retained |
| Authenticated physical link (R2CP `AUTH_TAG` in production) | Kirra — design requirement, not yet shipped (`firmware/rosmaster-r2/docs/PROTOCOL.md`) |

## 6. Open architecture questions (tracked, not resolved here)

- Jetson-side UART attach point (direct TTL header vs. USB bridge) and its
  voltage domain — **requires measurement** (`interfaces.md` §4).
- Steering feedback: whether the retained steering gear exposes a usable
  position signal at all — **unknown**
  (`firmware/rosmaster-r2/docs/HARDWARE_REFERENCE.md` §Steering).
- Driver-fault semantics of the retained external drivers (polarity,
  latching) — **requires bench characterization**.
- CAN(-FD) as a later carrier: the G474 makes it possible; Rev A only keeps
  the option open (no CAN transceiver committed at DR-1).
