# Architecture — Kirra OS

A navigable overview of how Kirra OS is put together and where authority
lives. It **decides nothing**: every rule below is owned by an ADR, a
specification, or code, and is linked to that owner.

For the technology-by-technology integration narrative — QNX, iceoryx2, PTP,
Zenoh, tracing, accelerators — the authoritative document is
[`docs/ARCHITECTURE_STACK.md`](docs/ARCHITECTURE_STACK.md). This page does not
duplicate it.

---

## 1. The three-domain architecture

```
Autonomy guest
    Mick / geometric planner / learned planner / other doers
          │ proposes
          ▼
Safety boundary
    typed contracts / authentication / freshness / frame checks
          │ admits
          ▼
Safety partition
    Governor / verifier / checker / release-token enforcement
          │ authorizes
          ▼
Physical actuation
```

**Autonomy guest** — throughput-optimized and *assumed fallible*. Planning,
perception, and the guest-side checker layer live here. The value claim is
precise: even a fully compromised guest cannot actuate, because authority
lives only on the far side of the boundary.

**Safety boundary** — a fixed-size, versioned, pointer-free `#[repr(C)]`
region, not a transport endpoint. The contract is the layout, not the library.
→ [`docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md`](docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md) §2,
[`docs/adr/0006-governor-transport-iceoryx2.md`](docs/adr/0006-governor-transport-iceoryx2.md) Clause 2

**Safety partition** — every dependency must justify itself into the trusted
computing base: frozen contracts, zero-alloc hot paths, minimal TCB.

→ Full narrative: [`docs/ARCHITECTURE_STACK.md`](docs/ARCHITECTURE_STACK.md) §2

> **Maturity.** The three-domain model is decided and specified; the QNX
> partition form is **pending hardware**. `ARCHITECTURE_STACK.md` §5 carries
> the per-technology status table, and nothing on-target is verified until the
> QNX deployment and measurement work lands. The host-side governor, verifier
> and attestation path are built and tested today.

---

## 2. The human-facing path

```
Human
  → Mick
  → typed intent or request
  → Occy or another doer
  → Kirra bounds and verifies
  → verdict
  → release token
  → verifying consumer
  → hardware
```

Each arrow is a narrowing. Language enters at the left; only an authorized,
bounded command reaches the right. Mick's contribution ends at *text*: it does
not construct intents, velocities, or tokens.
→ [`docs/adr/0033-actuation-authority-ros-r2-topology.md`](docs/adr/0033-actuation-authority-ros-r2-topology.md)

---

## 3. Components

| Component | Role | Authority | Where |
|---|---|---|---|
| **Mick** | Conversation, explanation, intent translation | None | [`crates/kirra-sidecars/`](crates/kirra-sidecars/), [`COMPANION.md`](COMPANION.md) |
| **Occy** | Trajectory / plan proposal | Untrusted | [`crates/kirra-planner/`](crates/kirra-planner/) |
| **Taj** | Perception → corridor + objects + health | Untrusted input | [`crates/kirra-taj/`](crates/kirra-taj/) |
| **Kirra Governor / Verifier** | Independent check, bound, authorize | **Trusted checker** | [`crates/kirra-trajectory/`](crates/kirra-trajectory/), [`src/verifier.rs`](src/verifier.rs) |
| **Release token** | Cryptographic authorization of specific bytes | Binding | [`docs/adr/0031-release-token-on-the-actuation-path.md`](docs/adr/0031-release-token-on-the-actuation-path.md) |
| **Verifying consumer** | Verify-before-act at the hardware edge | Enforcement | [`crates/kirra-inline-governor/`](crates/kirra-inline-governor/) |

---

## 4. Trust and authority boundaries

**Trust boundary** — where untrusted input becomes checked input. Everything
upstream is fallible; the checker re-derives what it needs rather than
believing what it was told.

**Authority boundary** — where a checked proposal becomes an authorized
action. The governor digests the validated bytes and signs a release token
over exactly those bytes; the consumer verifies before releasing. "The
governor approved exactly the data represented by this digest."
→ [`docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md`](docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md) §3

The two are deliberately separate. Passing the check is necessary but not
sufficient; the authorization is a distinct, verifiable artifact.

---

## 5. Proposal versus authorization

| | Proposal | Authorization |
|---|---|---|
| Produced by | Any doer | The governor alone |
| Trusted | No | Yes, within its envelope |
| Form | Typed request | Verdict + release token over specific bytes |
| Failure mode | Refused, clamped, or MRC | Consumer refuses to release |

A well-formed proposal is not an authorized one. This is the single
distinction the rest of the architecture exists to preserve.

---

## 6. World model versus conversation

Two separate stores, deliberately not merged:

- **World state** — sourced physical-world facts: perception output, registry
  entries, posture, diagnostics.
- **Conversation** — what was said. Never an input the safety path reads as
  fact.

Language chooses the *type* of a destination; a trusted resolver chooses the
coordinates. A request carrying coordinate-shaped fields is refused rather
than honoured.
→ [`crates/kirra-sidecars/src/destination.rs`](crates/kirra-sidecars/src/destination.rs),
[`docs/hardware/R2_DESTINATIONS.md`](docs/hardware/R2_DESTINATIONS.md)

---

## 7. Typed contracts

Untyped text never crosses a boundary. An LLM reply becomes a typed intent
through one fail-closed parser, and anything that does not parse — wrong
shape, unknown tag, non-finite number — is refused rather than repaired.
→ [`crates/kirra-planner/src/mick.rs`](crates/kirra-planner/src/mick.rs),
[`src/action_policy.rs`](src/action_policy.rs)

---

## 8. Frame, freshness and identity checks

| Check | Question | Owner |
|---|---|---|
| **Frame** | Which coordinate frame, and does the consumer agree? | [`crates/kirra-sidecars/src/destination_service.rs`](crates/kirra-sidecars/src/destination_service.rs) |
| **Freshness** | Is this evidence recent enough to act on? | [`src/telemetry_watchdog.rs`](src/telemetry_watchdog.rs) |
| **Identity** | Did the node it claims to be actually produce this? | [`crates/kirra-safety-authority/src/attestation.rs`](crates/kirra-safety-authority/src/attestation.rs) |
| **Sequence** | Is this a replay of something already accepted? | [`src/verifier.rs`](src/verifier.rs) |

All four fail closed. A frame mismatch is not resolved by guessing; a stale
reading is not extrapolated; an unattested node is not trusted; a replayed
message is refused.

---

## 9. Failure behaviour

The single rule: **absence of evidence is never evidence of safety.**

| Condition | Response |
|---|---|
| Missing / empty credential | Refuse (`503`), never pass through |
| Silent sensor past its budget | Fault → posture degrades → MRC floor |
| Non-finite value | Rejected before any envelope check |
| Stale posture cache | Command gate closes |
| Dependency cycle in the fleet graph | Locked out |
| Unparseable model output | Refused; no repair attempt |
| Rate-limited or unreachable service | Refuse; never assume success |

→ [`docs/safety/SAFE_STATE_SPECIFICATION.md`](docs/safety/SAFE_STATE_SPECIFICATION.md)

Degraded is not a slower version of normal: it is a controlled decel-to-stop
and hold, with re-initiation denied.
→ [`docs/adr/0011-degraded-http-actuator-503-vs-decel-gate.md`](docs/adr/0011-degraded-http-actuator-503-vs-decel-gate.md)

---

## 10. Auditability

State transitions are recorded in a SHA-256 hash-chained, tamper-evident
ledger; a broken link is detectable rather than silent. A denied actuator
command can be rendered as an explainable verdict — deny code, operator
sentence, recorded inputs, and the chain fields.
→ [`src/audit_chain.rs`](src/audit_chain.rs), [`src/verdicts.rs`](src/verdicts.rs)

Captured sessions can be replayed through the *real* checker and compared
bit-identically, so an incident can be reconstructed rather than reasoned
about.
→ [`docs/REPLAY_INCIDENT_RECONSTRUCTION.md`](docs/REPLAY_INCIDENT_RECONSTRUCTION.md)

---

## 11. Hardware abstraction

Kinematic limits are per-platform configuration, selected by vehicle class.
There is no default class — an unset or unknown value aborts startup, because
a wrong envelope is more dangerous than a stopped robot.
→ [`docs/CONTRACT_PROFILES.md`](docs/CONTRACT_PROFILES.md)

Platform kinematics are abstracted so the same checker serves differing
drive geometries.
→ [`docs/adr/0027-platform-kinematics-abstraction.md`](docs/adr/0027-platform-kinematics-abstraction.md)

---

## 12. Where ROS 2 sits

ROS 2 is **middleware in the autonomy guest**. It is not the safety boundary
and carries no authority.

- The checker core is `no_std`-friendly and ROS-agnostic; the ROS 2 adapter is
  a thin wiring layer.
  → [`crates/kirra-ros2-adapter/`](crates/kirra-ros2-adapter/)
- DDS actuator topics are `Volatile`, never `TransientLocal` — a late joiner
  must not receive a stale command.
- On the AV line, Autoware is kept as the doer and isolated, meeting the rest
  of the stack on a small set of hash-verified boundary topics.
  → [`docs/adr/0036-autoware-distro-migration-occy-gap.md`](docs/adr/0036-autoware-distro-migration-occy-gap.md)

---

## 13. The AV checker (Occy line)

The autonomous-driving specialization runs as a two-rate checker: a slow loop
at planning rate validating the candidate trajectory, and a fast loop at
control rate enforcing the verdict. Verdicts are
`TrajectoryVerdict::{Accept, Clamp, MRCFallback, Pending}`.

| Mechanism | Owner |
|---|---|
| SG2 drivable-space containment + lateral margin | [`docs/safety/OCCY_SG2_MARGIN.md`](docs/safety/OCCY_SG2_MARGIN.md) |
| RSS over horizon; longitudinal **and** lateral conjunction | [`docs/safety/KIRRA_RSS_FORMAL_SPECIFICATION.md`](docs/safety/KIRRA_RSS_FORMAL_SPECIFICATION.md) |
| Occlusion-aware junction speed bound | [`docs/adr/0016-occlusion-aware-junction-speed-bound.md`](docs/adr/0016-occlusion-aware-junction-speed-bound.md) |
| Multi-modal predictive RSS (CV / CTRV) | [`docs/adr/0017-multi-modal-predictive-rss.md`](docs/adr/0017-multi-modal-predictive-rss.md) |
| Perception-divergence monitoring | [`docs/adr/0018-perception-divergence-monitor.md`](docs/adr/0018-perception-divergence-monitor.md) |
| Two-tier Governor + independent detection channel | [`docs/safety/OCCY_ARCHITECTURE_TIERS.md`](docs/safety/OCCY_ARCHITECTURE_TIERS.md), [`docs/safety/OCCY_INDEPENDENT_DETECTOR.md`](docs/safety/OCCY_INDEPENDENT_DETECTOR.md) |
| ASIL decomposition | [`docs/safety/ASIL_DECOMPOSITION.md`](docs/safety/ASIL_DECOMPOSITION.md) |
| Dependent failure analysis | [`docs/safety/OCCY_DFA.md`](docs/safety/OCCY_DFA.md) |
| Freedom from interference | [`docs/safety/OCCY_FFI_EVIDENCE.md`](docs/safety/OCCY_FFI_EVIDENCE.md) |
| Minimum-risk-condition behaviour | [`docs/safety/SAFE_STATE_SPECIFICATION.md`](docs/safety/SAFE_STATE_SPECIFICATION.md) |
| Lanelet2 corridor source | [`crates/kirra-map/`](crates/kirra-map/), [`docs/adr/0023-lanelet2-geographic-projection.md`](docs/adr/0023-lanelet2-geographic-projection.md) |
| Subscription-staleness watchdog | [`crates/kirra-ros2-adapter/`](crates/kirra-ros2-adapter/) |
| Learned planner vocabulary, still governed | [`crates/kirra-planner/src/learned.rs`](crates/kirra-planner/src/learned.rs) |
| CARLA scenario coverage | [`docs/testing/CARLA_SCENARIO_SUITE.md`](docs/testing/CARLA_SCENARIO_SUITE.md) |
| Ferrocene-ready traceability conventions | [`docs/safety/TRACEABILITY.md`](docs/safety/TRACEABILITY.md) |

Claims here belong to their owning documents and should not be generalized
beyond them. Occy safety goals and their status:
[`docs/safety/OCCY_SAFETY_GOALS.md`](docs/safety/OCCY_SAFETY_GOALS.md).

---

## 14. Fleet and trust architecture

| Mechanism | Where |
|---|---|
| Per-node and fleet posture; gray/black DAG traversal with cycle detection | [`src/verifier.rs`](src/verifier.rs) |
| Dependency-graph processing | [`src/verifier.rs`](src/verifier.rs) |
| Telemetry timeout thresholds | [`src/telemetry_watchdog.rs`](src/telemetry_watchdog.rs) |
| Hysteresis-based trust recovery | [`src/recovery_hysteresis.rs`](src/recovery_hysteresis.rs) |
| Federation trust reports, Ed25519-signed | [`crates/kirra-fleet-types/`](crates/kirra-fleet-types/) |
| Generation ordering across controllers | [`docs/adr/0037-epoch-fenced-generation-ordering.md`](docs/adr/0037-epoch-fenced-generation-ordering.md) |
| Replay prevention / nonce burning | [`crates/kirra-persistence/`](crates/kirra-persistence/) |
| HA passive-standby promotion; durable epoch fence | [`src/standby_monitor/`](src/standby_monitor/), [`docs/deployment/HA_TOPOLOGY.md`](docs/deployment/HA_TOPOLOGY.md) |
| WAL-mode SQLite persistence | [`crates/kirra-persistence/`](crates/kirra-persistence/) |
| Audit-chain integrity | [`src/audit_chain.rs`](src/audit_chain.rs) |

---

## 15. Where to go next

| Question | Document |
|---|---|
| How do the technologies integrate? | [`docs/ARCHITECTURE_STACK.md`](docs/ARCHITECTURE_STACK.md) |
| What is the safety evidence? | [`SAFETY.md`](SAFETY.md), [`docs/safety/SAFETY_CASE_INDEX.md`](docs/safety/SAFETY_CASE_INDEX.md) |
| What are the non-negotiables? | [`CONSTITUTION.md`](CONSTITUTION.md) |
| What is Mick? | [`COMPANION.md`](COMPANION.md) |
| What must an integrator provide? | [`docs/safety/ASSUMPTIONS_OF_USE.md`](docs/safety/ASSUMPTIONS_OF_USE.md), [`docs/safety/GOVERNOR_SAFETY_MANUAL.md`](docs/safety/GOVERNOR_SAFETY_MANUAL.md) |
| Why was a decision made? | [`docs/adr/`](docs/adr/) |
