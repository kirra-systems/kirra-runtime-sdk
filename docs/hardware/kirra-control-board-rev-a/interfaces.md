# Rev A signal interfaces

> **Status: DR-1 input.** Interface *philosophy* freezes at DR-1; connector
> pinouts, protection parts, and translator selection freeze at DR-2, after
> the measurements this document requires. No Yahboom connector pinout is
> asserted here — the vendor harness is characterized on the unit, never
> assumed (`firmware/rosmaster-r2/docs/HARDWARE_REFERENCE.md`).

## 1. Interface philosophy

1. **No naked MCU pins leave the board.** Every signal crossing a board
   connector passes through an appropriate protection/conditioning stage
   (series impedance, clamp, filter, buffer, or translator as the interface
   requires). ESD/protection footprints are placed on every exposed
   interface even where the initial build may not populate them.
2. **Every safety-critical and communication signal receives a test point.**
   At minimum: all eight `safety.md` signals, both UART lines, all encoder
   channels, all PWM outputs, logic supply, and ground references.
3. **Connectors are keyed and locking where practical**, and distinct enough
   that harnesses cannot be cross-plugged (the manufacturing checklist
   verifies drawings against physical parts).
4. **Read-only means read-only.** Sense and fault inputs (`E_STOP_SENSE`,
   `DRIVER_FAULT_*_N`, optional `STEERING_FB`) are conditioned as inputs
   only; no board path allows the MCU to drive them.

## 2. R2CP transport (Jetson ↔ MCU)

- The logical interface is R2CP v1 exactly as specified in
  `firmware/rosmaster-r2/docs/PROTOCOL.md` (normative; `firmware/rosmaster-r2/protocol/src/wire.cpp` is the
  reference implementation, `crates/kirra-r2cp` the host codec). **Rev A
  documentation invents no R2CP wire constants** — framing, rates, and
  message semantics belong to the protocol spec.
- Physical carrier: UART (`R2CP_RX` / `R2CP_TX`). The protocol spec's
  latency analysis already rules out the vendor's 115200-baud path for the
  sub-2 ms target and names a high-rate UART candidate; carrier validation
  (BER, latency under load) is a bring-up/HIL activity, not a Rev A paper
  claim.
- **Voltage-domain verification is mandatory before translator selection**
  (§4). The Jetson attach point (direct TTL UART header vs. USB bridge) is
  an open architecture question (`architecture.md` §6).

## 3. Actuator-side interfaces (to retained Yahboom hardware)

| Interface | Rev A provision | Unknowns gating DR-2 |
|---|---|---|
| Motor-driver control | `PWM_LEFT_IN1/IN2`, `PWM_RIGHT_IN1/IN2` outputs, buffered/protected | Retained drivers' input thresholds, polarity, coast/brake truth table — bench characterization required |
| Driver enable | `DRIVER_ENABLE_HW` (from the `safety.md` gate, not a bare GPIO) | Enable-input polarity and drive requirements of the retained drivers |
| Driver faults | `DRIVER_FAULT_L_N`, `DRIVER_FAULT_R_N` inputs, conditioned | Whether the retained drivers expose fault outputs at all, and their polarity/latching — verify on the unit |
| Encoders | `ENCODER_L_A/B`, `ENCODER_R_A/B` inputs with **configurable filter footprints**; A/B channels get **matched components and matched routing** per side | Encoder supply voltage, output type (open-collector vs. push-pull), maximum edge rate — **filter values stay configurable until frequency and voltage are measured** |
| Steering | `STEERING_PWM` output; optional `STEERING_FB` input footprint | The R2 steering connector, pulse range, and whether any feedback signal exists are unverified (`firmware/rosmaster-r2/docs/HARDWARE_REFERENCE.md` §Steering) |
| E-stop loop | NC loop termination + `E_STOP_SENSE` divider/conditioning | Loop voltage/current per the `../R2_ESTOP_SPEC.md` §5 unknowns |

## 4. UART level translation — verify before selecting

- **The Jetson UART voltage is not assumed.** It must be **measured, or
  verified from the actual Jetson carrier-board schematic for the exact
  carrier in use, before a level translator is chosen.** This document
  deliberately does not state 1.8 V or 3.3 V.
- The Nucleo side is likewise confirmed against the exact MB1367 revision
  (`power-and-grounding.md` §5).
- **Generic auto-direction level translators are not assumed suitable for
  UART.** Auto-direction parts rely on drive-strength sensing that can
  misbehave with UART idle states, pull-ups, and asymmetric edges; a
  direction-fixed or purpose-rated translator is preferred. The final part
  is a DR-2 selection made **only after** both domains are verified.
- Both UART lines receive test points (§1.2) so the domains can be verified
  on the assembled board, not just on paper.

## 5. Debug and service

- SWD access (`SWDIO`, `SWCLK`, `NRST`): the Nucleo's on-board ST-LINK is
  the primary path; the carrier keeps the signals accessible (header or
  test points) so a detached probe can be used and so `NRST` behavior is
  observable during safety testing.
- USB/ST-LINK physical access with the Nucleo mounted on the carrier is a
  mechanical requirement checked at DR-3 and in the manufacturing
  checklist.
- Status LEDs: minimum set = logic power present, `DRIVER_ENABLE_HW` state,
  fault indication. LEDs indicate; they never gate.
