# Kirra Control Board Rev A

> **Status: documentation foundation only.** Nothing in this directory is a
> schematic, a PCB layout, a verified pin assignment, or a manufacturing
> release. Every electrical claim below is either sourced, or marked
> **Pending / Unverified / Requires measurement**. This documentation does
> **not** authorize floor motion (see `bringup-plan.md` §15).

## Mission

Kirra Control Board Rev A is a safety-focused STM32 NUCLEO-G474RE carrier
used first on the Yahboom ROSMASTER R2 bring-up platform. It replaces the
Yahboom MCU while retaining the current external motor drivers, steering
hardware, motors, and encoders; it communicates with the Jetson over the
Kirra R2 Control Protocol (R2CP) and enforces a hardware actuation gate
independently of Linux. Its interfaces and mechanical documentation are
structured so the controller can migrate to a selected Traxxas 1/10-based
Kirra platform (`mechanical-reference.md`, HDR-0007) without changing R2CP
or the actuation authorization boundary. No Traxxas fit is claimed until
verified — Rev A's mechanical compatibility class is **Unclassified**.

Rev A is the hardware realization of two decisions already on the record:

- the firmware architecture's planned **STM32G4/H7 control-board revision**
  (`firmware/rosmaster-r2/docs/ARCHITECTURE.md` §Decision summary);
- the firmware roadmap's **stage 2 "bench board"** in the staged migration
  from the vendor MCU to Kirra-owned firmware
  (`firmware/rosmaster-r2/docs/ROADMAP.md` §Staged migration).

## The three states — do not conflate them

| State | MCU / board | Firmware | Actuation gate |
|---|---|---|---|
| **Current** (bring-up robot today) | Stock Yahboom/Rosmaster expansion board (STM32F103RCT6) | Vendor firmware, unverified | Software-only: ADR-0033 consumer chokepoint + single-writer serial; hardware kill per `../R2_ESTOP_SPEC.md` where fitted |
| **Near-term: Rev A carrier** (this directory) | Kirra carrier hosting a NUCLEO-G474RE; external motor drivers retained | Kirra clean-room firmware (`firmware/rosmaster-r2/`), retargeted to G474 (BSP is future work) | Hardware-combined `DRIVER_ENABLE_HW` (see `safety.md`) + all existing software gates |
| **Future: custom Kirra controller** | Fully integrated Kirra MCU PCB (integrated drivers, production power distribution) | Kirra firmware, hardware root of trust | Superset of Rev A; out of scope here |

Rev A deliberately sits between the two ends: it moves the *actuation-gate
hardware* into Kirra's hands without taking on motor-power electronics,
battery distribution, or a custom MCU package. See
`decisions/HDR-0002-carrier-before-custom-pcb.md`.

The **mechanical** dimension is tracked separately from these electrical
states (`mechanical-reference.md`): the Yahboom R2 is Mechanical
Reference B (the bring-up adapter platform, retained through the
migration), and the long-term mechanical reference is a Traxxas
1/10-based Kirra platform (Reference A — platform class adopted, exact
chassis model pending the MR-1 selection review; HDR-0007).

## What Kirra owns today vs. what Rev A adds

Kirra currently owns (all software, all on the Jetson):

- motion authorization (verifier/governor policy, the checker);
- consumer-side release verification (ADR-0033, `robot/kirra_motor_consumer.py`);
- the single-writer serial boundary (`TIOCEXCL`, udev rule, boot sentinel
  `robot/serial_exclusivity.py`);
- device ownership checks (`robot/motor_authority.py` serial-authority
  detection);
- the R2CP host codec (`crates/kirra-r2cp`);
- the PTY simulated MCU (`kirra-r2cp-sim`);
- the R2CP consumer drive mode (`KIRRA_DRIVE_MODE=r2cp`, landed with the
  governed consumer drive mode work — the roadmap's bridge item 4).

Kirra does **not** yet own:

- the firmware running on the physical motor-control MCU;
- the physical MCU board;
- the motor-driver electronics;
- the complete authenticated physical link;
- hardware-in-loop validation;
- final floor-driving odometry and closed-loop wheel control.

**Honest limit of the current safeguards:** serial exclusivity and sole
device ownership are *operational* safeguards. They stop a second process
from holding the port; they do not stop a privileged (root) process from
taking it, and they offer no protection against physical access to the
wire. That is why they are described in the firmware roadmap as a
sole-writer guard, not an authenticated link — and why they must remain in
place even after Rev A lands (replacing the board does not remove the
Jetson verifier, governor, or consumer; see
`firmware/rosmaster-r2/docs/ROADMAP.md` §Final Kirra-owned state).

## Document map

| Document | Contents |
|---|---|
| `architecture.md` | System context, current state, Rev A block-level architecture, boundaries |
| `system-interfaces.md` | One-page wiring architecture; the three frozen interfaces (Jetson↔MCU, MCU↔driver, MCU↔safety) and Rev A success criteria |
| `requirements.md` | Numbered Rev A requirements: includes, excludes, constraints |
| `safety.md` | The hardware safety boundary: `DRIVER_ENABLE_HW` logic, signal definitions, safe-behavior matrix |
| `interfaces.md` | Signal-interface and connector philosophy; R2CP transport interface |
| `connector-map.md` | Mechanical connector inventory, keying/labeling conventions, mating-part worksheet (families/MPNs pending) |
| `mechanical-reference.md` | Mechanical References A (Kirra/Traxxas 1/10) and B (Yahboom R2 adapter), compatibility classes, evidence worksheet, MR-1 gate |
| `pin-allocation.md` | The verified pin-allocation **process** and the worksheet (all rows pending) |
| `power-and-grounding.md` | Grounding and logic-power philosophy |
| `design-reviews.md` | DR-1…DR-4 review gates; no fabrication before DR-4 |
| `bringup-plan.md` | Wheels-up staged bring-up, 15 stages |
| `manufacturing-checklist.md` | Pre-fabrication release checklist |
| `decisions/` | Hardware decision records (HDR-0001…HDR-0006) |

## Project status

| Work item | Status |
|---|---|
| Rev A documentation foundation | **Started** (this directory) |
| Pin allocation | **Blocked** on exact Nucleo (MB1367) board revision + measurements |
| Schematic | Not started |
| PCB layout | Not started |
| Manufacturing release | **Not approved** |
| Traxxas 1/10 mechanical reference | **Adopted as a platform direction** (HDR-0007) |
| Exact chassis model | **Pending MR-1** |
| Yahboom R2 adapter reference | Retained for Rev A bring-up |
| Class A mounting definition | Not frozen |
| Adapter plate | Not designed |
| 3D fit verification | Not started |

Hardware phases are never marked complete because documentation exists;
each phase closes only at its design-review gate (`design-reviews.md`).

## Cross-references

- `firmware/rosmaster-r2/docs/ROADMAP.md` — staged migration this board serves
- `firmware/rosmaster-r2/docs/PROTOCOL.md` — R2CP v1 (normative; this
  directory binds **no** wire constants)
- `firmware/rosmaster-r2/docs/HARDWARE_REFERENCE.md` — evidence-ranked vendor
  hardware manifest (what Rev A replaces / retains)
- `docs/hardware/R2_ESTOP_SPEC.md` — the existing hardware E-stop spec Rev A's
  E-stop loop composes with
- `docs/adr/0033-actuation-authority-ros-r2-topology.md` — the authorization
  chokepoint that remains the trust boundary
- `docs/adr/0014-rosmaster-r2-orin-nx-kirra-integration.md` — the R2 + Orin
  dual-system stack
