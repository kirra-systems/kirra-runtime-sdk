# Kirra Mission Assurance Runtime for QNX — Product Plan

| Field | Value |
|---|---|
| Doc ID | KIRRA-MAR-PLAN-001 |
| Status | **Proposed (planning baseline)** — a future product direction. **Nothing in this document describes current deployed capability** unless a row is explicitly labeled *Existing foundation*. |
| Date | 2026-07-29 |
| Companions | `QNX_MISSION_ASSURANCE_ARCHITECTURE.md` (KIRRA-MAR-ARCH-001 — planes, mission/capability/authority models, QNX integration), `QNX_MISSION_ASSURANCE_BACKLOG.md` (KIRRA-MAR-BACKLOG-001 — issue-ready epics MAR-01…MAR-12) |
| Existing foundation anchors | `PRODUCT_EXECUTION_PLAN.md` (P1/P2/P3 ladder, GATE A–E), `docs/analysis/ENGINEERING_EXECUTION_PROGRAM.md` (EP-xx), EPIC #270 (QNX lane), `docs/safety/HYPERVISOR_CONTRACT_CHANNEL.md` (HVCHAN), ADR-0031/0033 (release token), `docs/REPLAY_INCIDENT_RECONSTRUCTION.md` (EP-19), `docs/safety/ASSUMPTIONS_OF_USE.md` (AoU register) |

> **Governing rule (borrowed from `docs/ARCHITECTURE_STACK.md`): where a decision
> already lives in an ADR, spec, AoU, or code, this plan cites it and does not
> restate it as a second source of truth.** Everything else here is a proposal
> awaiting owner review; merge of this document ratifies the *plan shape*, not
> any implementation commitment.

---

## 0. Namespace and naming declarations (read first)

This plan deliberately mints new identifiers to avoid colliding with the four
work-ID namespaces already in use (`WS-*` product execution, `EP-*` engineering
execution program, `WP-*` historical gap closure, `PARK-NNN` task backlog),
following the precedent set by `docs/analysis/ENGINEERING_EXECUTION_PROGRAM.md`:

- **`MAR-*`** — Mission Assurance Runtime epics/workstreams (MAR-01…MAR-12, backlog doc).
- **`Gate MA-A` … `Gate MA-F`** — this plan's decision gates. The unprefixed
  `GATE A–E` remain the property of `PRODUCT_EXECUTION_PLAN.md` §2 and are
  never reused here.
- **`MAR Phase 0–6`** — this plan's phases, distinct from the QNX lane's
  Phase I/II (WCET) and the execution plan's Stage 0–5.

Terminology conflicts found during preparation, and how this plan resolves them:

| Term | Existing meaning | Resolution here |
|---|---|---|
| "plane" | The fleet/QM **control plane** (P2); safety architecture uses **safety partition / autonomy guest / boundary** (`docs/ARCHITECTURE_STACK.md` §2) | The three *product* planes (intelligence / mission / safety) are defined in KIRRA-MAR-ARCH-001 §1 **with an explicit mapping onto the existing three-domain model**; "control plane" keeps its P2 meaning |
| "gateway" | `src/gateway/` (Tower policy layer) and `CommandGateway` (the replay seam) | The candidate QNX service is named **`kirra-gatewayd` (safety-gateway daemon)** with a disambiguation note; renaming it before Phase 2 is an open question (KIRRA-MAR-ARCH-001 §7) |
| "contract" | `GovernorContractView` (frozen boundary ABI, ADR-0006 Clause 2), per-class contract profiles (`docs/CONTRACT_PROFILES.md`), HVCHAN | **Capability contracts** (functional + assurance) are a new, disambiguated concept (KIRRA-MAR-ARCH-001 §4) and never refer to the frozen ABI |
| "authority" | `crates/kirra-safety-authority` (posture DAG aggregate); *actuation authority* (ADR-0033, HVCHAN §3 steps 6–7) | **Mission authority** is defined strictly as an *upstream extension of the existing release-token chain* — it narrows what may be requested; it never replaces verify-before-release (KIRRA-MAR-ARCH-001 §5) |
| "mission" | Effectively unused (one operational-sortie use in `docs/safety/UL4600_SAFETY_CASE.md` §per-shift SPIs) | Free to mint; the UL 4600 operational sense is compatible (a mission *is* the sortie the SPIs are computed over) |
| "HAM" | No existing mentions; repo "HA" = the epoch-fenced active/passive verifier topology (`docs/deployment/HA_TOPOLOGY.md`) | **QNX HAM** (High Availability Manager, a QNX OS facility) is introduced in KIRRA-MAR-ARCH-001 §7 and explicitly disambiguated from Kirra "HA" |

---

## 1. Product objective

Evolve Kirra into the **mission-assurance runtime between autonomous
applications and QNX**: the layer that owns mission definition and execution,
capability lifecycle and compatibility, mission-level health and recovery,
policy evaluation, evidence-bound action authority, integration with an
independent safety plane, structured operational evidence, deterministic
mission replay, and signed deployment/configuration provenance.

The intended product boundary:

```
Autonomous applications and AI
            │
            ▼
Kirra Mission Assurance Runtime
            │
            ▼
QNX services and middleware
            │
            ▼
QNX OS / QNX OS for Safety / QNX Hypervisor
            │
            ▼
Independent safety controllers, sensors and actuators
```

**Relationship to the existing product ladder** (`PRODUCT_EXECUTION_PLAN.md` §1):
this is **not** a rename of P3 "Kirra Certified". P3 is the certified *checker
partition* (governor + contract channel + release tokens on QNX/Ferrocene) —
the bottom of the mission plane's trust chain. The Mission Assurance Runtime is
a proposed **fourth packaging** that layers a *mission plane* above the
P1 checker spine and beside the P2 fleet plane, reusing both. It consumes the
P3 track's QNX artifacts; it does not compete with them. Whether it ships as
"P4" in the ladder or absorbs P3's positioning is a **Gate MA-A** decision.

## 2. Product promise

> **Define a mission, prove its constraints, deploy it to a QNX-powered
> machine, supervise every consequential action, recover predictably, and
> reconstruct exactly why the system behaved as it did.**

Every phrase maps to an existing Kirra discipline: *prove its constraints* →
the compile-time fail-closed rejection posture (KIRRA-MAR-ARCH-001 §3);
*supervise every consequential action* → the release-token verify-before-release
chain (ADR-0031/0033, HVCHAN §3); *recover predictably* → the staged recovery
ladder (KIRRA-MAR-ARCH-001 §8); *reconstruct exactly why* → deterministic
replay extended from EP-19's bit-identical verdict re-derivation.

## 3. Initial scope

Status labels: **Existing foundation** (present in Kirra today, reusable) ·
**Adaptation** (present, requires rework) · **Planned** (designed here, not
built) · **Future work** (acknowledged, not yet designed) · **Out of scope**.

| Kirra owns | Status | Existing anchor |
|---|---|---|
| Mission definition and execution (bounded mission graph, compiler, executor) | Planned | New — no "mission" concept exists in-tree today |
| Capability lifecycle and compatibility (functional + assurance contracts) | Planned | Adjacent: per-class contract profiles (`docs/CONTRACT_PROFILES.md`), typed-intent seam (`MickIntent`) |
| Mission-level health and recovery | Planned / Adaptation | Recovery hysteresis (`src/recovery_hysteresis.rs`), telemetry watchdog, posture engine — node-level today, mission-level is new |
| Policy evaluation | Adaptation | `KirraPolicyLayer`, `src/authz.rs` RBAC scopes, `src/action_filter.rs` |
| Evidence-bound action authority | Existing foundation / Adaptation | Release token + evidence-bound V2 release (`src/governor_release.rs`, ADR-0031/0033); extension to mission-scoped authority is Planned |
| Integration with an independent safety plane | Existing foundation (pattern) / Planned (protocol) | Doer–checker independence (PO-2, `docs/safety/OCCY_DFA.md` §3), ADR-0003 two-tier + D1 channel; the mission↔safety-gateway *protocol* is new |
| Structured operational evidence | Existing foundation / Adaptation | Hash-chained audit ledger (`src/audit_chain.rs`), EP-17 explainable verdicts, EP-18 safety-case-as-code; the mission evidence *package* is Planned |
| Deterministic mission replay | Existing foundation (verdicts) / Planned (missions) | EP-19 `kirra-replay` (KIRRA-REPLAY-001) replays checker verdicts bit-identically; mission-transition replay is new |
| Signed deployment and configuration provenance | Existing foundation | Uptane trust (`docs/ota/UPTANE_ROLES.md`, `kirra-ota-installer`), OTA campaigns (R156-shaped audit), `GOVERNOR_KEY_PROVISIONING.md`, `EffectiveConfigDigest` |

## 4. Explicit non-goals (initially)

All **Out of scope** for the initial product; none are judgments that the
capability is unimportant — each names who owns it instead.

| Not owned by Kirra | Owner instead |
|---|---|
| Hard real-time actuator control | The existing checker/actuator stations and the platform's control loops (ADR-0031 budget split: the verdict path stays crypto-free and bounded; Kirra's mission plane sits *above* it) |
| Final hazardous-motion authority | The independent safety plane (ADR-0003 two-tier; KIRRA-MAR-ARCH-001 §1 invariant) |
| A general-purpose robotics data bus | ROS 2 / DDS / iceoryx2 remain the carriers (ADR-0006, ADR-0007) |
| Low-level CPU scheduling | QNX (`SCHED_FIFO` etc.); Kirra validates *declared* budgets only (KIRRA-MAR-ARCH-001 §9) |
| A complete AI model-operations platform | Model integrity stays the WS-2 allow-list scope; training/serving platforms are external |
| A fleet-cloud platform | P2 Kirra Fleet remains the fleet plane; MAR integrates with it, not replaces it |
| A universal world-model database | WS-6 world-model work remains its own track |
| Replacement of QNX HAM | QNX HAM is *integrated with* (KIRRA-MAR-ARCH-001 §7) |
| Replacement of ROS 2 | Three integration modes instead (KIRRA-MAR-ARCH-001 §11; consistent with ADR-0036) |
| Replacement of an independent safety controller | Never — this is the load-bearing invariant of the whole plan |

## 5. Three-plane architecture (summary)

Full definition, trust boundaries, and the plane↔existing-domain mapping:
**KIRRA-MAR-ARCH-001 §1**. In one paragraph:

- **Intelligence plane** — autonomous applications, models, planners. Produces
  observations, estimates, uncertainty, model results, proposals. Never holds
  unrestricted actuator authority (today's doer discipline, generalized).
- **Mission plane** — Kirra. Objectives, mission workflow, capability
  selection, policy evaluation, recovery, degradation, approval requests,
  authority requests, evidence recording.
- **Safety plane** — independent of Kirra mission execution. Actuator
  permission, e-stop, physical limits, collision envelopes, interlocks,
  watchdogs, minimum-risk behavior, final command acceptance/rejection.

**The invariant, stated once here and normatively in the architecture doc:**

> Kirra may *request* an action. The independent safety plane decides whether
> that action is physically permitted. A valid Kirra authority token is
> necessary where configured, but **never sufficient** to override local
> safety state, hardware constraints, or actuator limits.

Mission state and safety state are distinct and are never represented as one
authority or one variable.

## 6. Product principles

Carried through every MAR document and epic (these are the repo's existing
engineering ethics, restated as product principles):

1. **Fail closed.** Absent input, unknown state, unparseable config → refuse,
   never default open (the posture of every existing gate).
2. **One live owner per safety decision.** No duplicated authority; an
   inactive duplicate safety implementation is removed, not retained.
3. **Bounded queues and execution.** Every channel has a declared capacity and
   overflow behavior (`POSTURE_BROADCAST_CAPACITY`, ingress token buckets,
   backpressure pools are the precedents).
4. **Explicit clock semantics.** Two-clock-domain non-mixing rule (HVCHAN §5,
   R-HV-3, AOU-TIMESYNC-001) governs; monotonic time for local durations,
   wall time only where cross-process comparison/audit requires it.
5. **Explicit restart semantics.** Restart is a new trust epoch unless
   complete replay/freshness state is persisted atomically (KIRRA-MAR-ARCH-001 §6).
6. **Deterministic tie-breaking.** Worst-case-over-hypotheses, as in the
   multi-modal predictive RSS producer (one dangerous hypothesis refuses).
7. **Typed identities instead of sentinel conventions** where possible
   (lesson: ADR-0037's `epoch = 0` sentinel is documented, contained, and not
   to be imitated for new identity fields).
8. **No silent compatibility fallback that restores weaker behavior**
   (precedents: `KIRRA_DB_URL` never falls back to SQLite; half-configured TLS
   aborts startup).
9. **Documentation describes deployed behavior.** This plan is labeled
   Proposed for exactly that reason.
10. **Issues close against verified capability, not merged intent** (the
    existing "DoD is a gate row, not code merged" rule).
11. **Safety-relevant tests must demonstrate that they detect the protected
    defect** (the non-vacuousness discipline of the tamper-detection anchors).

## 7. Initial reference vertical

**Industrial inspection** (mobile or fixed-route inspection of industrial
assets). Rationale:

- Aligns with P1's industrial lane (Modbus/DNP3/CANopen/CIP adapters already
  in-tree) and its buyer set; no new protocol surface required for a first
  demonstration.
- Consequential actions exist (approach, actuate a probe, open/close a valve
  path) but the hazard envelope is narrower than robotaxi/delivery-AV, so the
  independent safety plane can be a real but modest controller.
- The mission shape (route → inspect → report, with recovery and approval
  gates) exercises every mission-plane feature without demanding the WS-6
  world model.

No existing repository product decision names a first MAR vertical; the
existing verticals (courier/sidewalk P1 flagship, robotaxi P3) remain owned by
their plans. Confirming this vertical is a **Gate MA-A** item.

## 8. Major engineering workstreams

Each workstream maps 1:1 onto backlog epics (KIRRA-MAR-BACKLOG-001):

| Workstream | Epics | One-line scope |
|---|---|---|
| Mission kernel | MAR-02, MAR-03, MAR-04 | Bounded mission schema, compiler, executor + state machine |
| Capability & policy | MAR-05 | Functional + assurance capability contracts, compatibility rule, policy interface |
| Authority | MAR-06, MAR-07 | Mission-scoped authority kernel; simulated independent safety gateway |
| Evidence & replay | MAR-08, MAR-09 | Structured mission evidence recorder; exact mission replay |
| QNX runtime | MAR-10, MAR-11 | Service model port to QNX; QNX HAM integration |
| Physical safety boundary | MAR-12 | Physical independent-controller integration |
| Boundary freeze | MAR-01 | Mission/safety ownership freeze (the Gate MA-A input) |

## 9. Phased implementation plan

| Phase | Name | Builds | Exit |
|---|---|---|---|
| **MAR Phase 0** | Product boundary and reference architecture | Frozen product requirements, three-plane architecture, trust boundaries, threat model, reference vertical + platform, QNX target choice (reuses EPIC #270's `qnx800` targets and the Phase I/II hardware doctrine of `docs/safety/WCET_QNX_BRINGUP.md`), independent safety boundary, non-goals | Gate MA-A |
| **MAR Phase 1** | Mission and authority kernel (simulation only) | Bounded mission graph, compiler, mission state machine, capability contracts, policy interface, authority model, *simulated* independent safety gateway, structured recorder, exact replay, CLI validation/simulation | Gate MA-B |
| **MAR Phase 2** | QNX-native runtime | QNX process architecture, native IPC, bounded shared-memory profile (reusing the HVCHAN/iceoryx2 carrier discipline), service identities, QNX security policy, HAM integration, health aggregation, capability lifecycle, resource declarations | Gate MA-C |
| **MAR Phase 3** | Physical safety boundary | Safety-gateway protocol, command envelopes, evidence binding, session epochs, replay protection, acknowledgements, watchdogs, HIL, independent controller integration, deployment verification | Gate MA-D |
| **MAR Phase 4** | Production supervision | Signed packages, device identity, anti-rollback (Uptane reuse), update/rollback, deadline monitoring, declared degraded modes, operator approvals, ROS 2 integration, evidence export, offline operation | Gate MA-E |
| **MAR Phase 5** | Replay and assurance productization | Replay inspection, divergence reports, counterfactual prototype, policy comparison, capability/model substitution, incident reports, assurance exports | (feeds Gate MA-F) |
| **MAR Phase 6** | First vertical | Industrial-inspection configuration frozen, operational limits documented, field-failure reproduction, multi-unit deployment | Gate MA-F |

Phase 1 is deliberately simulation-only: the mission kernel must be provably
correct against a *simulated* safety gateway before any QNX or hardware work,
mirroring how the checker matured against `VirtualClock`/`ScenarioRunner`
before hardware bring-up.

## 10. Decision gates

Hard, checkable, in the house style of `PRODUCT_EXECUTION_PLAN.md` §2:

- **Gate MA-A — Product boundary.** Exactly one vertical and one reference
  platform chosen; the independent safety authority is named (who/what refuses
  commands); the non-goals list in §4 is owner-approved; the P3/P4 positioning
  question (§1) is decided.
- **Gate MA-B — Mission kernel.** A mission graph outside the bounded schema
  cannot compile; no consequential action executes without an authority grant;
  exact replay reproduces every mission decision from the evidence package;
  a safety-gateway refusal cannot be bypassed by any mission-plane path
  (demonstrated by a test that *tries*).
- **Gate MA-C — QNX integration.** Services isolated as separate QNX
  processes; all IPC bounded (declared queue depths + overflow behavior); HAM
  restarts a killed service and Kirra records the mission consequence;
  privileges minimized per service; restart behavior explicit and tested.
- **Gate MA-D — Hardware authority.** Independent actuator gate live on real
  hardware; replayed and stale evidence rejected (demonstrated, not asserted);
  acknowledgement binding proven; deployment verification confirms authority
  ownership end-to-end.
- **Gate MA-E — Production supervision.** Signed packages enforced (an
  unsigned package on disk does not run); offline operation demonstrated;
  degraded modes measured, not just declared; rollback proven; complete
  evidence export for a full mission.
- **Gate MA-F — Vertical readiness.** Supported configuration frozen;
  operational limits documented; field failures reproducible via replay;
  evidence demonstrably useful to operators and assurance reviewers;
  deployment repeatable across multiple units.

## 11. Definition of done

For this plan as a whole: **every gate MA-A…MA-F passed, with each gate's
evidence recorded in the audit/evidence conventions this plan defines** —
per the existing rule, DoD is always a gate row, never "code merged". For
individual epics, per-epic closure conditions are in KIRRA-MAR-BACKLOG-001.

For *this document set* (the only deliverable that exists today): the three
MAR docs merged with Proposed status, indexed from `docs/roadmap/README.md`
and the root README, with no claim of current capability.

## 12. Immediate next steps

1. Owner review of this baseline; resolve the Gate MA-A positioning question
   (§1) and confirm the industrial-inspection vertical (§7).
2. File the MAR-01 boundary-freeze epic (backlog doc has the issue-ready
   text); its output is an ADR (next free number, currently 0039 — verify at
   filing time; the 0035 double-booking is a known hazard).
3. Decide the `kirra-gatewayd` naming question (KIRRA-MAR-ARCH-001 §7) before
   any Phase 2 scaffolding.
4. Fold the restart/epoch future-requirements list (KIRRA-MAR-ARCH-001 §6)
   into the AoU register (`KIRRA-OCCY-AOU-001`) as new `AOU-MAR-*` entries
   when MAR-01 lands — not before, to keep the register describing real
   obligations.

## 13. What exists today vs. what this plan adds

| Bucket | Contents |
|---|---|
| **Existing foundation (reuse as-is)** | Release-token chain + evidence-bound V2 release (ADR-0031/0033, HVCHAN §3); hash-chained audit ledger + EP-17 verdicts; EP-19 replay of checker verdicts; Uptane metadata verification + OTA campaign engine; signing-key provisioning discipline (`GOVERNOR_KEY_PROVISIONING.md` — including its fail-closed "unset → refuse" rule and the documented rotation-order defect class); two-clock-domain rule (HVCHAN §5); epoch-fence CAS + `(epoch, generation)` ordering (ADR-0037); QNX cross-compile recipes + RTM harness (EPIC #270); posture model + policy layer + RBAC scopes |
| **Adaptation** | Policy evaluation (route-level → mission-level); health/recovery (node posture → mission consequence); evidence (audit events → structured mission evidence package); replay (per-command verdicts → mission transitions) |
| **Not yet designed (Planned here)** | Mission schema/compiler/executor/state machine; capability functional+assurance contracts; mission-scoped authority tokens; safety-gateway protocol; QNX service decomposition + HAM integration; counterfactual replay |
| **Explicitly excluded** | §4 in full |
