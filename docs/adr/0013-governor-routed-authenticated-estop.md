# ADR-0013: Software emergency-stop is an authenticated REQUEST to the governor, never a console→actuator command

| Field | Value |
|---|---|
| Status | **Proposed (design note — precedes code)** — for owner sign-off; ratified on merge. |
| Date | 2026-06-21 |
| Deciders | Project / safety-case owner |
| Issues | #412 (this), #314 (operator identity), #410 / #405 (MRC / decel-to-stop), ADR-0006 (the QM↔safety boundary), #411 (console safety-theater fix) |
| Code (when built) | `src/verifier.rs` / `src/bin/kirra_verifier_service.rs` (governor-request endpoint, mirrors the clearance loop), the audit chain, the MRC vocabulary (`TRAJECTORY_MRC_FALLBACK`) |

## Principle

An emergency stop must **NOT** be a console→actuator command. The console is QM-domain and
holds **no** actuator authority — that boundary is the core of KIRRA's ASIL-D safety case
(ADR-0006). A software E-stop enters as an **authenticated REQUEST to the fail-closed
governor**, which then commands the MRC under **its own** authority. The console asks; the
governor acts. The console never touches the actuator.

This is the clearance loop, **inverted** — same channel, different verb:

- **Clearance:** authenticated operator requests RELEASE → governor validates → governor acts
  (resume motion).
- **E-stop:** authenticated operator requests STOP → governor commands MRC → governor acts
  (controlled stop).

Both route operator intent through the governor as the **sole authority**, over the
authenticated-request channel that already exists (operator identity, Ed25519-signed path,
verify-then-consume, chain logging).

## Decision

Adopt a **governor-routed authenticated emergency-stop REQUEST**, bound by these hard
constraints (which **are** the safety argument):

1. **REQUEST, not command.** The operator's stop is an input the governor *judges*, not a
   command that reaches the actuator. The governor executes the MRC
   (`TRAJECTORY_MRC_FALLBACK` / decel-to-stop·hold) under its own authority.
2. **Authenticated + non-repudiable.** Same Ed25519 operator-identity path as clearance grants
   — signed, verify-then-consume; the operator who requested the stop is provable in the chain.
3. **Chain-logged.** A distinct audit event pair (`OperatorStopRequested` →
   `GovernorMRCCommanded`) written to the tamper-evident chain — reconstructable after the
   fact: who requested the stop, when, and what the governor did.
4. **Supplementary to a PHYSICAL E-stop.** The certifiable emergency stop is a hardwired
   circuit independent of compute (mushroom button → cut power / engage brakes). This software
   request is a supervision/convenience layer **on top of** the physical E-stop, **never** the
   primary safety mechanism. Stated explicitly so an assessor sees we understand the
   certifiable stop is hardware.
5. **Fail-closed.** If the request channel is down, the physical E-stop and the governor's
   autonomous detection (SG1–SG6) still hold. The software request **adds** capability; its
   absence removes nothing.

## Rejected alternative (the anti-pattern)

A **direct console→actuator stop command.** It would give the QM-domain console actuator
authority, punch a hole in the hypervisor boundary (ADR-0006), and force the safety case to
account for a compromised / laggy browser, session, or network commanding the vehicle. This is
the easy version and it is **forbidden** — it destroys the property that makes KIRRA
certifiable. (The console correctly has **no** E-stop button today; the dead safety-theater
button was removed and only the E-stop *readiness indicator* remains — #411.)

## Console framing (when built)

The console gains an authenticated **"Request Emergency Stop"** action that goes through the
governor-request channel — framed in the UI as a **REQUEST** ("Request Stop · routed to
governor"), never as a direct command, and gated behind operator authentication exactly like
clearance.

## Reuse (not a new mechanism)

- **Mirrors:** the clearance loop (`verifier.rs`, `kirra_verifier_service.rs`).
- **Reuses:** operator identity (#314), the Ed25519 signed path, verify-then-consume, the
  audit chain, the MRC vocabulary (`TRAJECTORY_MRC_FALLBACK`).
- **Posture:** an operator stop request drives the vehicle to a governor-commanded safe state;
  relate to `FleetPosture`. Ties to the teleop-supervision cluster — operator intent is always
  *governed*, never a bypass.

## Phase scope / sequencing

This **design note first** (this ADR). Then implementation (laptop; end-to-end
**hardware-gated** — needs a running governor + actuator path):

1. the governor-request endpoint (mirror the clearance endpoint),
2. the signed-request path (reuse the operator-identity verify-then-consume),
3. the chain events (`OperatorStopRequested` → `GovernorMRCCommanded`),
4. the console "Request Emergency Stop" action.

Cross-ref: #314, the clearance-loop PRs, ADR-0006 (the boundary), #411 (console
safety-theater fix).

## Status

**Proposed — for owner sign-off** (merge ratifies, as with ADR-0011 / ADR-0012). This records
the **request-not-command** architecture *before* code so the implementation cannot drift into
the console→actuator anti-pattern.
