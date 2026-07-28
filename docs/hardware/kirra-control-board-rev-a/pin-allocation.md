# Rev A verified pin allocation

> **Status: worksheet only — NO assignments made.** Every electrical field
> below is `Pending`, `Unverified`, or `Requires measurement`. No STM32
> alternate-function assignment in this repository is presented as fact
> until it has been produced by the process in §2 and frozen at DR-2. This
> follows the evidence discipline of
> `firmware/rosmaster-r2/hal/include/r2/hal/board_manifest.hpp`
> (a claim is sourced or it is marked unverified, and unverified rows never
> enable actuation).

## 1. Allocation worksheet

Column meanings:

- **Function** — the Rev A signal (names match `safety.md` / `interfaces.md`).
- **STM32 pin / Peripheral / Morpho pin** — filled only from the §2 process
  (CubeMX + the exact MB1367 schematic), never from memory or inference.
- **Reset state** — the pin's state from MCU reset until firmware init, from
  the reference manual + board pulls; must be safe per `safety.md` §4.
- **Voltage domain** — measured or schematic-verified, per side.
- **Direction** — from the MCU's point of view (definitional, so filled in).
- **Source document or measurement** — the evidence that closes the row.
- **Protection/interface** — the conditioning stage from `interfaces.md`.
- **External connector** — carrier connector + position (DR-2/DR-3 output).
- **Firmware symbol** — the BSP manifest identifier (definitional).
- **Status** — `Pending` until evidence recorded, then `Frozen (DR-2)`.

| Function | STM32 pin | Peripheral | Morpho pin | Reset state | Voltage domain | Direction | Source document or measurement | Protection/interface | External connector | Firmware symbol | Status |
|---|---|---|---|---|---|---|---|---|---|---|---|
| E_STOP_SENSE | Pending | Pending (GPIO/EXTI) | Pending | Pending | Requires measurement (loop divider) | Input | Pending | Pending | Pending | `e_stop_sense` | Pending |
| MCU_WATCHDOG_KICK | Pending | Pending | Pending | Pending | Pending | Output | Pending | Pending | — (on-board to supervisor) | `mcu_watchdog_kick` | Pending |
| MCU_ENABLE_REQUEST | Pending | Pending (GPIO) | Pending | Must be pulled inactive — Unverified | Pending | Output | Pending | Pending | — (on-board to enable gate) | `mcu_enable_request` | Pending |
| DRIVER_ENABLE_HW | — (gate output, not an MCU pin) | — | — | Disabled — Requires measurement | Requires measurement (driver enable input) | Board output | Pending | Pending | Pending | `driver_enable_hw` (sense-back TBD) | Pending |
| DRIVER_FAULT_L_N | Pending | Pending (GPIO/EXTI) | Pending | Pending | Requires measurement (driver fault output) | Input | Pending | Pending | Pending | `driver_fault_l_n` | Pending |
| DRIVER_FAULT_R_N | Pending | Pending (GPIO/EXTI) | Pending | Pending | Requires measurement | Input | Pending | Pending | Pending | `driver_fault_r_n` | Pending |
| ENCODER_L_A | Pending | Pending (timer encoder mode) | Pending | Pending | Requires measurement (encoder supply/output type) | Input | Pending | Configurable filter — values Requires measurement | Pending | `encoder_l_a` | Pending |
| ENCODER_L_B | Pending | Pending (same timer as L_A) | Pending | Pending | Requires measurement | Input | Pending | Matched to L_A | Pending | `encoder_l_b` | Pending |
| ENCODER_R_A | Pending | Pending (timer encoder mode) | Pending | Pending | Requires measurement | Input | Pending | Configurable filter — values Requires measurement | Pending | `encoder_r_a` | Pending |
| ENCODER_R_B | Pending | Pending (same timer as R_A) | Pending | Pending | Requires measurement | Input | Pending | Matched to R_A | Pending | `encoder_r_b` | Pending |
| PWM_LEFT_IN1 | Pending | Pending (timer PWM) | Pending | Must be inactive — Unverified | Requires measurement (driver input threshold) | Output | Pending | Pending | Pending | `pwm_left_in1` | Pending |
| PWM_LEFT_IN2 | Pending | Pending (timer PWM) | Pending | Must be inactive — Unverified | Requires measurement | Output | Pending | Pending | Pending | `pwm_left_in2` | Pending |
| PWM_RIGHT_IN1 | Pending | Pending (timer PWM) | Pending | Must be inactive — Unverified | Requires measurement | Output | Pending | Pending | Pending | `pwm_right_in1` | Pending |
| PWM_RIGHT_IN2 | Pending | Pending (timer PWM) | Pending | Must be inactive — Unverified | Requires measurement | Output | Pending | Pending | Pending | `pwm_right_in2` | Pending |
| STEERING_PWM | Pending | Pending (timer PWM) | Pending | Must be inactive — Unverified | Requires measurement (servo rail) | Output | Pending | Pending | Pending | `steering_pwm` | Pending |
| STEERING_FB | Pending | Pending (ADC or timer capture — signal type Unverified) | Pending | Pending | Requires measurement | Input (optional) | Pending — whether feedback exists at all is Unverified | Pending | Pending | `steering_fb` | Pending |
| R2CP_RX | Pending | Pending (USART RX, DMA-capable) | Pending | Pending | Requires measurement (Jetson side — NOT assumed 1.8 V or 3.3 V) | Input | Pending | Level translator — selection blocked on voltage verification | Pending | `r2cp_rx` | Pending |
| R2CP_TX | Pending | Pending (USART TX, DMA-capable) | Pending | Pending | Requires measurement | Output | Pending | Level translator — selection blocked on voltage verification | Pending | `r2cp_tx` | Pending |
| LOGIC_5V_IN | — (power input) | — | Pending (Nucleo supply path per MB1367 revision) | — | Nominal 5 V logic — protection per `power-and-grounding.md` | Power in | Pending — MB1367 revision check required | Fuse + reverse polarity + TVS + decoupling (values Pending) | Pending | — | Pending |
| LOGIC_GND | — (power/reference) | — | Pending | — | 0 V reference | Power | Pending | Continuous plane (`power-and-grounding.md`) | Pending | — | Pending |
| SWDIO | Pending — confirm against MB1367 schematic | SWD | Pending | Pending | Pending | Bidirectional (debug) | Pending | Pending | ST-LINK / debug header | `swdio` | Pending |
| SWCLK | Pending — confirm against MB1367 schematic | SWD | Pending | Pending | Pending | Input (debug) | Pending | Pending | ST-LINK / debug header | `swclk` | Pending |
| NRST | Pending — confirm against MB1367 schematic | Reset | Pending | Asserted-safe behavior per `safety.md` §4 — Unverified | Pending | Input (reset) | Pending | Pending | ST-LINK / debug header + test point | `nrst` | Pending |

Rows may be added (e.g. status LEDs, watchdog-supervisor sense-back, board
ID strap) during DR-1/DR-2; rows are never *removed* silently — a dropped
function needs a decision record.

## 2. The pin-selection process (mandatory, in order)

An allocation exists only when all eight steps are complete and their
evidence is recorded in the worksheet's **Source** column:

1. **Identify the exact NUCLEO-G474RE MB1367 revision** physically in hand
   (board marking + photo, recorded in the manufacturing checklist).
2. **Obtain the matching ST schematic** for that MB1367 revision. A
   different revision's schematic does not close any row.
3. **Create a CubeMX project** for the exact target part.
4. **Allocate peripherals in CubeMX**: encoder timers, PWM timers, ADC,
   UART, GPIO, EXTI, and the watchdog-kick output.
5. **Resolve all peripheral conflicts** inside CubeMX (timer channel
   collisions, DMA streams, alternate-function contention) — the tool's
   conflict view, not human recall, is the arbiter.
6. **Map verified STM32 pins to the exact Morpho pins** using the matching
   MB1367 schematic, including solder-bridge/jumper defaults that reroute
   signals on some revisions.
7. **Measure external voltage domains and signal types** (Jetson UART,
   encoder outputs, driver inputs/faults, steering interface, E-stop loop).
8. **Freeze the allocation only after the evidence is recorded** — every
   worksheet field filled with its source, then the table is frozen at
   DR-2. After the freeze, changes require re-review.

## 3. Interaction with the firmware BSP

The frozen worksheet becomes the input to the G474 board-revision manifest,
mirroring the structure of `firmware/rosmaster-r2/hal/include/r2/hal/board_manifest.hpp`: the BSP exports pin selections
from one board-revision manifest and **fails compilation for unresolved
options** — "auto-detection" cannot choose safety wiring
(`firmware/rosmaster-r2/drivers/README.md`). Firmware symbols in the
worksheet are the agreed names for that manifest.
