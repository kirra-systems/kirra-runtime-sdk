# HDR-0003: R2CP host link

| Field | Value |
|---|---|
| Status | Proposed — ratified at DR-1 |
| Date | 2026-07-28 |
| Cross-refs | `firmware/rosmaster-r2/docs/PROTOCOL.md` (normative); `crates/kirra-r2cp`; `docs/adr/0033-actuation-authority-ros-r2-topology.md`; `firmware/rosmaster-r2/docs/ROADMAP.md` §Near-term bridge state |

## Context

The Jetson↔MCU protocol is already decided and specified: R2CP v1, with
`firmware/rosmaster-r2/protocol/src/wire.cpp` as the normative implementation, `crates/kirra-r2cp` as the host
codec, a PTY simulated MCU for stage-1 bridge testing, and an explicit
link-vs-authorization distinction (`AUTH_TAG` authenticates the link; the
verifier/consumer chain authorizes motion). None of that is re-decided
here.

## Decision

Rev A's Jetson link **is** R2CP, carried over UART, with the carrier
providing only the physical layer: connector, protection, level
translation (selected after voltage-domain verification —
`../interfaces.md` §4), and test points. Rev A documentation binds **no**
R2CP wire constants, rates, or message semantics — those remain the
protocol spec's alone. No second protocol, and no vendor-legacy byte
compatibility, exists on this link (legacy compatibility belongs in an
isolated Linux adapter per `firmware/rosmaster-r2/docs/PROTOCOL.md`, never on the MCU).

## Consequences

- Bring-up stage 5 (`../bringup-plan.md`) exercises the same codec and
  bridge already proven against the PTY simulated MCU — the carrier link
  becomes a swap, not a leap, exactly as the roadmap's stage discipline
  intends.
- Carrier validation obligations (BER, latency under load, the high-rate
  UART candidate) transfer unchanged from the protocol spec and roadmap to
  Rev A HIL (stage 14).
- CAN-FD remains a possible later carrier for the same logical messages;
  Rev A keeps the option open without committing a transceiver.
