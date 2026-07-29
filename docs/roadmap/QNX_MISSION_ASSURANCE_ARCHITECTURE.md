# Kirra Mission Assurance Runtime — Architecture and Trust Boundaries

| Field | Value |
|---|---|
| Doc ID | KIRRA-MAR-ARCH-001 |
| Status | **Proposed (planning baseline)** — architectural design for a future direction. **No section of this document describes current deployed capability** unless labeled *Existing foundation*. |
| Date | 2026-07-29 |
| Parent | `QNX_MISSION_ASSURANCE_RUNTIME.md` (KIRRA-MAR-PLAN-001 — objective, phases, gates, namespace declarations) |
| Backlog | `QNX_MISSION_ASSURANCE_BACKLOG.md` (KIRRA-MAR-BACKLOG-001) |

Status labels used throughout: **Existing foundation** · **Adaptation** ·
**Planned** · **Future work** · **Out of scope** (defined in KIRRA-MAR-PLAN-001 §3).

---

## 1. Three planes and trust boundaries

### 1.1 The planes

**Intelligence plane** — autonomous applications and AI: planners, perception,
models, LLM-driven agents. Produces **observations, estimates, uncertainty,
model results, proposals**. It does **not** receive unrestricted actuator
authority. *Existing foundation:* this is the doer discipline generalized —
today's doers (Occy, Mick, learned planners, Autoware per ADR-0036) already
only *propose* typed claims that the checker bounds.

**Mission plane** — owned by Kirra. Responsible for **objectives, mission
workflow, capability selection, policy evaluation, recovery, degradation,
approval requests, authority requests, evidence recording**. *Planned:* the
mission plane is this document's subject; today Kirra owns the checker and the
fleet legitimacy plane, not a mission concept.

**Safety plane** — independent of Kirra mission execution. Responsible for
**actuator permission, emergency shutdown, physical operating limits,
collision envelopes, interlocks, watchdogs, minimum-risk behavior, and final
command acceptance or rejection**. *Existing foundation (pattern):* the
two-tier architecture of ADR-0003 (base downstream Governor + optional D1
add-on as one independent safety channel) and the doer–checker independence
argument (PO-2, `docs/safety/OCCY_DFA.md` §3) are the in-tree precedents. The
mission↔safety protocol in §5/§7 is *Planned*.

### 1.2 The normative invariant

> **Kirra may request an action. The independent safety plane decides whether
> that action is physically permitted. A valid Kirra authority token is
> necessary where configured, but never sufficient to override local safety
> state, hardware constraints, or actuator limits.**

Corollary, equally normative: **mission state and safety state are distinct.**
They must not be represented as one authority or one variable. A mission in
`RUNNING` says nothing about whether an actuator will accept a command; a
safety plane in a permissive state says nothing about whether the mission
intends a command. Every acceptance decision at the actuator boundary is a
*conjunction* (§5.3) — neither plane's state substitutes for the other's.

A mission-state transition (§3) **never by itself guarantees physical
safety**; it only records what the mission plane decided and why.

### 1.3 Mapping onto the existing three-domain model

`docs/ARCHITECTURE_STACK.md` §2 defines the deployment domains: **safety
partition (QNX host) / autonomy guest / the boundary**. The planes are logical
roles; the domains are placement:

| Plane | Default placement | Notes |
|---|---|---|
| Intelligence | Autonomy guest | Unchanged from today (ADR-0036 keeps Autoware isolated in its own container/guest) |
| Mission | QNX host, **outside** the certified checker partition | Mission services (§7) are ordinary QNX processes; they are *upstream* of the verdict path and never inside its WCET budget (ADR-0031 Clause A discipline applies) |
| Safety | The certified checker partition + independent controllers below QNX | The existing governor/actuator stations and the D1-class independent channel; final refusal lives here |

"Control plane" retains its existing P2 fleet meaning and is not one of these
planes.

---

## 2. Mission model

### 2.1 Definition

> A **mission** is a versioned, policy-controlled, bounded execution graph
> with objectives, capability requirements, resource assumptions, deadlines,
> recovery paths, authority requirements and evidence requirements.

("Mission" was verified unused as a technical term in-tree; the one
operational use — UL 4600 per-mission SPI computation — is compatible: a
mission here *is* the sortie those SPIs are computed over.)

### 2.2 First mission model — supported constructs ONLY

- sequence
- parallel execution
- race
- conditional branch
- bounded retry
- timeout
- approval gate
- recovery transition
- abort
- safe-state request
- capability substitution

### 2.3 Explicitly excluded from the mission model

- arbitrary embedded scripts
- unbounded loops
- runtime code download
- implicit retries
- implicit failure swallowing
- unrestricted graph mutation during execution

These exclusions are the mission-level restatement of existing invariants: the
Mick intent vocabulary is a closed set parsed fail-closed
(`MickIntent::from_llm_json`); `OperationalCommand::Unknown` is denied in all
postures; the mission graph gets the same closed-vocabulary treatment.

### 2.4 Mission compiler outputs (Planned)

The compiler transforms a mission source into a deployable **mission package**:

| Output | Purpose |
|---|---|
| Canonical typed graph | The single executable representation; no alternate interpretation paths |
| Deterministic mission package hash | Content-addressed identity; the analogue of the governor artifact digest in the OTA campaign engine |
| Capability dependency lock | Exact capability versions + assurance profiles resolved at compile time (lockfile discipline, as `Cargo.lock` / the Uptane digest-authorization check) |
| Resource admission plan | Declared budgets (§9) summed and checked against the platform declaration |
| Authority plan | Every consequential action's required authority enumerated up front |
| Recovery plan | The resolved recovery ladder (§8) per fault class per node |
| Failure-completeness report | Proof obligation: every fallible node has an explicit failure edge |
| Deployment compatibility report | Platform/QNX-target/safety-gateway requirements vs. the target device's declaration |
| Evidence-recording plan | Which events are recorded, at what tier of trust (§10.2), to which sink |

### 2.5 Compile-time rejection cases (fail-closed; all block packaging)

- missing capabilities
- incompatible versions (per the §4.3 compatibility rule — name+semver alone never suffices)
- missing failure handling (failure-completeness report not clean)
- unbounded retry
- impossible resource assumptions
- invalid transitions (not expressible in the §3 state machine)
- missing approvals (an approval gate with no resolvable approver role)
- policy conflicts
- unsupported platform requirements
- unresolved safety-gateway dependency (a consequential action with no configured safety-plane endpoint)
- unsigned production package (§12; a production profile refuses to emit an unsigned package, in the same spirit as `KIRRA_GOVERNOR_SIGNING_KEY_SOURCE` unset → refuse)

---

## 3. Mission state model (Planned)

```
CREATED
  ↓
VALIDATING
  ↓
READY
  ↓
ARMED
  ↓
RUNNING
  ├─ PAUSED
  ├─ DEGRADED
  ├─ RECOVERING
  ├─ WAITING_FOR_APPROVAL
  └─ ABORTING
        ↓
SAFE
  ↓
COMPLETED / FAILED / CANCELLED
```

Notes:

- `DEGRADED` here is a **mission** state (reduced capability set / reduced
  objectives), deliberately distinct from the fleet-posture `Degraded`
  (decel-to-stop-and-HOLD envelope, issue #70). The mission plane may enter
  mission-`DEGRADED` *because* the fleet posture degraded, but the two are
  separate variables per §1.2 — the record carries both.
- `SAFE` means "the mission plane has requested and observed a safe state";
  it is a claim about the mission record, not a physical guarantee (§1.2).
- Terminal states are `COMPLETED / FAILED / CANCELLED`; there is no implicit
  resurrection — re-running a mission is a new mission instance with a new
  identity.

### 3.1 Transition record

**Every** transition records:

- previous state
- new state
- trigger
- requesting actor
- authorizing actor
- monotonic event sequence (strictly advancing; the existing strictly-advancing
  release-sequence and `(epoch, generation)` disciplines are the precedents —
  ADR-0037)
- wall-clock timestamp (for cross-process comparison and audit only, per §6)
- policy decision
- evidence identity (§10.2 — a typed identity, never a zero/sentinel value)
- mission package hash
- runtime version
- capability versions
- safety-plane response, where applicable

Transition records are appended to the hash-chained evidence stream (§10),
reusing the audit-chain linker discipline (*Existing foundation:*
`src/audit_chain.rs`, EP-17 chained payloads).

---

## 4. Capability model (Planned)

**Disambiguation.** "Capability contract" is a new term. It does **not** refer
to `GovernorContractView` (the frozen `#[repr(C)]` boundary ABI, ADR-0006
Clause 2), to per-class kinematic *contract profiles*
(`docs/CONTRACT_PROFILES.md`), or to the HVCHAN contract channel. Where this
section says "contract" unqualified, it means a capability contract.

Every capability declares **two** contracts:

### 4.1 Functional contract

- name
- version
- input schema
- output schema
- supported actions
- cancellation behavior
- progress behavior
- functional failure modes

### 4.2 Assurance contract

- criticality
- timing assumptions
- maximum command latency
- deadline behavior
- resource requirements
- evidence requirements
- authority requirements
- safe cancellation behavior
- restart semantics
- persistence semantics
- stale-input behavior
- hardware dependencies
- degraded modes
- output validity period
- fault-containment assumptions

The assurance contract is the mission-plane analogue of an AoU entry: it makes
the load-bearing assumptions explicit, per-capability, and machine-checkable
at admission — the same philosophy as `ASSUMPTIONS_OF_USE.md`'s "an OPEN AoU
is a pre-enable gate."

### 4.3 Compatibility rule (normative)

A capability satisfies a mission requirement **only** when ALL hold:

```
functional compatibility
AND platform compatibility
AND assurance-profile compatibility
AND mission-policy compatibility
AND resource compatibility
```

**Capability name and semantic version alone are insufficient.** (Lesson from
the existing per-class contract profiles: a `courier` and a `robotaxi` build
share APIs but must never share envelopes — `KIRRA_VEHICLE_CLASS` has no
default for exactly this reason.)

---

## 5. Authority model

### 5.1 Position relative to the existing chain (read first)

*Existing foundation:* Kirra already has a per-command authority artifact —
the **release token** (Ed25519 over the exact enforced bytes;
`crates/kirra-release-token`, ADR-0031, ADR-0033, HVCHAN §3 steps 5–7,
verify-before-release at the actuator station, strictly-advancing release
sequence, evidence-bound V2 releases in `src/governor_release.rs`).

**The mission authority token does not replace any of that.** It is an
*upstream, mission-scoped grant*: it authorizes the mission plane to *request*
a class of consequential action within an envelope, for a bounded time. The
per-command release token remains the only artifact an actuator station
verifies per command. Designing mission authority as a separate weaker
substitute for verify-before-release is explicitly rejected (one live owner
per safety decision; no incompatible replacement of the existing chain).

### 5.2 Authority request and token (Planned)

An **authority request** names the mission node, the intended action, the
target asset, and the requested envelope, and carries the supporting evidence
identities. The resulting **authority token** should consider binding:

- issuer
- subject
- device
- mission
- mission generation
- boot or session epoch (see §6 — this epoch concept is *Future work* in the
  current protocol and must not be overstated)
- capability instance
- action
- target asset
- command sequence
- evidence identity
- nonce (single-use, volatile, TTL-bounded — the attestation-challenge
  discipline: `pending_challenges`, `CHALLENGE_TTL_MS`)
- permitted operating envelope
- required safety state
- issue time
- lifetime
- signature

### 5.3 Actuator acceptance rule (normative, conceptual)

The actuator-facing service accepts a command **only** when ALL hold:

```
valid authority token
AND valid mission state
AND valid evidence binding
AND valid sequence
AND valid freshness
AND valid command envelope
AND valid local safety state
AND valid hardware constraints
```

**The actuator-facing service must independently reject an otherwise authentic
request.** A perfectly signed, fresh, in-sequence, in-envelope command is
still refused when local safety state or hardware constraints say no — this is
§1.2 restated at the enforcement point, and it is the existing actuator-station
posture (refusals never poison the watermark; a refused command is never
ledger-committed as accepted).

### 5.4 Reuse mandates

- Replay protection: reuse the strictly-advancing sequence rule
  (`sequence <= last_accepted ⇒ reject`, equal = replay — the QNX judge and
  inline-governor rule), and the nonce-burn pattern (federation nonce burn,
  attestation challenge single-use).
- Evidence binding: reuse the evidence-bound release discipline (digest over
  the exact enforced bytes; `inputs_digest_sha256` in EP-17).
- Acknowledgement: reuse the command-acknowledgement design direction already
  present in the actuation-consumer work rather than inventing a parallel one.

---

## 6. Restart and clock model

This section integrates lessons already recorded in the repository; where the
current protocol does not yet provide a guarantee, it is labeled **Future
work** rather than claimed.

### 6.1 Restart

- **Zero or sentinel evidence identities must not be emitted for real
  evidence.** Typed identities over sentinels (ADR-0037 documents the one
  tolerated sentinel, `epoch = 0` = "no claim / legacy source", precisely so
  it is contained; new MAR identity fields get typed "absent" representations
  instead).
- **Restart must be modeled as a new trust epoch unless complete replay and
  freshness state is persisted atomically.** *Existing foundation for the
  pattern:* nonces are volatile by design and die with the process
  (`pending_challenges`); the posture generation persists via
  `posture_engine_state` + `init_generation_from_store` so monotonicity
  survives restart; the HA epoch advances by durable CAS (`try_claim_epoch`)
  so a revived stale primary is fenced. *Future work:* a first-class MAR
  "session/boot epoch" bound into authority tokens (§5.2) is not yet designed
  — the current release-token protocol does not carry one, and this plan must
  not imply it does.
- **Partial persistence must not create the appearance of continuity.** If
  only part of the replay/freshness state survives, the runtime must present
  as a new epoch, not a continued one.
- **Pre-restart authority should not silently regain validity after state
  loss.** Marked **Future work**: restart-scoped invalidation of outstanding
  authority tokens is a required property of the MAR authority kernel
  (MAR-06) and is *not* provided by any existing mechanism today.

### 6.2 Clocks

*Existing foundation:* the two-clock-domain model and the normative non-mixing
rule (HVCHAN §5, R-HV-3; `AOU-TIMESYNC-001`, `AOU-HV-CLOCK-001`): safety/
boundary timing vs. system timing, converted before publish, never mixed.

MAR restates it for mission-plane artifacts:

- **Wall-clock time may be needed for shared cross-process comparisons and
  audit** (transition records, evidence packages).
- **Monotonic time should govern local duration** wherever possible (timeouts,
  deadlines, freshness windows).
- **Backward wall-clock steps can extend apparent token lifetime.** Any
  wall-clock-lifetime check must account for this; clock ambiguity handling
  must consider **both** the token lifetime and the backward-step magnitude.
- **Forward steps shorten validity and are fail-closed** (a token that
  appears expired is expired; no grace).
- **Clock behavior and restart behavior must be explicitly tested** — with
  the deterministic temporal harness (`VirtualClock` + `ScenarioRunner`),
  which exists precisely so these cases are testable without wall time.

---

## 7. QNX integration model (Planned)

### 7.1 Initial process architecture — architectural candidates

Suggested initial services (candidates, **not implementation commitments**):

| Service | Role |
|---|---|
| `kirra-missiond` | Mission executor + state machine (§3) |
| `kirra-capabilityd` | Capability registry, contracts, admission (§4) |
| `kirra-policyd` | Policy evaluation (adaptation of the existing policy layer / RBAC / action-filter decisions to mission scope) |
| `kirra-healthd` | Mission-level health aggregation (§8) |
| `kirra-recorderd` | Structured evidence recorder (§10) |
| `kirra-gatewayd` | The mission-plane endpoint of the safety-gateway protocol (§5.3 conjunction lives on the *safety* side; this daemon only formats/forwards requests and records responses) |

**Naming hazard (open question, decide before Phase 2):** "gateway" already
has two meanings in-tree (`src/gateway/` = the Tower HTTP policy layer;
`CommandGateway` = the checker seam replay records come from). If the
collision proves confusing, rename the candidate (e.g. `kirra-safetylinkd`);
tracked in KIRRA-MAR-PLAN-001 §12.

### 7.2 Division of responsibility (normative direction)

- **QNX processes are the default fault-containment unit.**
- **Kirra integrates with QNX HAM rather than replacing it.** QNX HAM (High
  Availability Manager — a QNX OS facility for process death detection and
  restart; term introduced here, no prior in-tree usage) owns process-level
  detection and resurrection. *Disambiguation:* this is unrelated to Kirra
  "HA" (the epoch-fenced active/passive verifier topology,
  `docs/deployment/HA_TOPOLOGY.md`).
- **QNX owns low-level scheduling and process mechanisms** (`SCHED_FIFO`,
  partitions, adaptive partitioning).
- **Kirra owns mission-level meaning, admission, degradation and recovery
  policy** — HAM restarts the process; Kirra decides what the restart *means*
  for the mission and records it (§8).
- **Kirra owns communication semantics but should not necessarily own every
  transport.** *Existing foundation:* ADR-0006's layering (iceoryx2 inside
  partitions, frozen-layout contract at the boundary) is exactly this
  posture.
- **Request/reply control traffic and high-rate data require different
  transport profiles.** Control traffic: native QNX IPC (message passing),
  bounded and audited. High-rate data: §7.3.

### 7.3 High-rate data profile

For high-rate data paths the profile requires (all *Existing foundation as
discipline* — the HVCHAN §3 seqlock channel and the iceoryx2 spike embody each
item; their generalization to MAR channels is *Planned*):

- shared memory
- bounded descriptor queues
- preallocated pools
- explicit ownership transfer
- freshness metadata
- generation identifiers
- declared overflow behavior

**No implementation support for a MAR-specific transport exists today**; the
QNX artifacts in-tree are the EPIC #270 harness/spike lane (judge, carrier,
WCET measurement), not a mission runtime.

---

## 8. Health and recovery (Planned)

### 8.1 Fault classes

- component unavailable
- process crash
- invalid data
- stale data
- missed deadline
- resource exhaustion
- policy denial
- safety intervention
- communication loss
- hardware fault
- mission infeasibility
- authentication failure
- evidence-binding failure

### 8.2 Staged recovery ladder

```
Level 0 — transient filtering
Level 1 — retry                       (bounded; unbounded retry is a compile error, §2.5)
Level 2 — restart component           (delegated to QNX HAM; Kirra records the consequence)
Level 3 — substitute capability       (per the §4.3 compatibility rule, never name+version alone)
Level 4 — reduce capability
Level 5 — replan mission
Level 6 — pause and request approval
Level 7 — abort mission
Level 8 — enter or request minimum-risk state
```

Escalation is deterministic: each fault class maps, per mission and per the
compiled recovery plan (§2.4), to a starting level and an escalation path.
*Existing foundation for the pattern:* the recovery-hysteresis discipline
(streak thresholds, window expiry resets) and the fail-closed watchdog sweeps;
Level 8's "request" phrasing is deliberate — the safety plane owns
minimum-risk *behavior* (§1.1), the mission plane only requests and records.

**QNX HAM detects and recovers process failures. Kirra determines the mission
consequences and records them.** A HAM-restarted `kirra-capabilityd` might be
a Level-2 non-event for one mission and a Level-7 abort for another — that
judgment is mission-plane policy, not HAM's.

---

## 9. Resource and timing model (Planned)

Kirra manages **declared** timing and resource budgets. It does not claim
that arbitrary AI workloads are deterministic — the intelligence plane is
supervised, not certified, exactly as host timing is INDICATIVE and never WCET
in the existing methodology (`docs/safety/WCET_MEASUREMENT_METHODOLOGY.md`).

Each capability may declare:

- period
- deadline
- execution budget
- priority class
- criticality
- CPU requirement
- memory requirement
- accelerator requirement
- queue bounds
- overrun behavior
- degraded modes

Initial Kirra responsibilities (only these):

1. **Admission validation** — declared budgets vs. the platform declaration,
   at compile time and at mission arm time.
2. **Runtime observation** — measure against declarations.
3. **Deadline-miss escalation** — a missed declared deadline is a §8.1 fault,
   entering the ladder at the compiled level.
4. **Degraded-mode selection** — from *declared* degraded modes only.
5. **Evidence recording** — declarations, observations, and escalations all
   enter the evidence stream.

**Out of scope:** a general-purpose scheduler above QNX. QNX schedules; Kirra
admits, observes, escalates, records.

---

## 10. Evidence and replay

### 10.1 Structured mission evidence package (Planned)

One package per mission instance, containing:

- mission package hash
- device identity
- runtime version
- OS version
- software bill of materials
- capability versions
- model versions
- policy versions
- configuration hashes (*Existing foundation:* the boot `EffectiveConfigDigest` audit event)
- mission transitions (§3.1 records, hash-chained)
- authority requests and decisions
- safety refusals
- commands and acknowledgements
- health events
- recovery actions
- approvals
- clock and epoch information
- final result

*Existing foundation:* the hash-chain linker, the WORM off-box shipper
(`src/audit_shipper.rs`, independently re-verifiable via
`verify_shipped_chain`), and EP-18 safety-case-as-code (a versioned,
hash-chained, self-verifying evidence bundle) are the packaging precedents.

### 10.2 Evidence trust tiers (normative distinctions)

The package format must distinguish, per item:

- **authenticated evidence** — cryptographically verified against a
  registered key (e.g. attested adoption reports)
- **attributed evidence** — tied to an authenticated principal but not
  content-verified
- **caller-supplied identity** — an identity the caller asserted; recorded as
  such
- **recomputed digest** — a digest some component recomputed from bytes it
  actually held (the `inputs_digest_sha256` discipline)
- **independently validated data** — checked against a second channel (the
  True-Redundancy cross-check pattern)
- **untrusted observations** — intelligence-plane output, recorded verbatim,
  trusted for nothing
- **safety-plane decisions** — the safety plane's own accept/refuse records

**Stated plainly: a signature over a supplied evidence identity does not prove
that the identity was derived from genuine sensor data.** It proves who
supplied it. Unless some component *recomputes* the digest from data it held,
or independently attests it, the identity stays at the caller-supplied tier.
(This is the existing repo lesson behind attested-vs-unattested adoption
reports and the recomputed-digest verdict binding.)

### 10.3 Replay

- **Exact event replay** — *initial product.* Feed the recorded mission
  evidence back through the real mission executor and policy engine and
  reproduce every mission decision (state transitions, policy outcomes,
  authority decisions) deterministically. *Existing foundation:* EP-19
  (`kirra-replay`, KIRRA-REPLAY-001) already does exactly this for checker
  verdicts — bit-identical (`f64::to_bits`), real code, nothing
  reimplemented, incomplete records classified rather than guessed. Mission
  replay extends the same doctrine upward and composes with
  `VirtualClock`/`ScenarioRunner` for the time-dependent parts, exactly as
  KIRRA-REPLAY-001 §1 prescribes.
- **Counterfactual replay** — *later work* (MAR Phase 5): re-run with a
  substituted policy/capability/model and report divergence. Not in the
  initial product; not to be conflated with exact replay in any claim.

---

## 11. ROS 2 integration (Planned)

Three modes, all consistent with the ADR-0036 stance (ROS 2 stays; Kirra does
not replace it):

1. **Native Kirra capability** — a capability implemented against the MAR
   contracts directly; ROS 2 not involved.
2. **Wrapped ROS 2 component** — an existing ROS 2 node wrapped in a
   capability adapter that supplies the functional + assurance contracts and
   enforces queue bounds and freshness at the wrap boundary (*Existing
   foundation as pattern:* `kirra-ros2-adapter`'s checker-at-the-boundary
   shape, and ADR-0036's curated hash-verified boundary topics).
3. **Controlled ROS 2 gateway** — a bounded bridge admitting a declared topic
   set under declared budgets.

**In no mode may ROS 2 bypass:** mission authority, policy, freshness
validation, queue bounds, evidence requirements, or safety authority. A ROS 2
topic is intelligence-plane input or a capability transport; it is never a
side door to the actuator (the existing Mick actuation fence —
`ci/check_mick_actuation_fence.py` — is the enforcement precedent: no
dependency route from the conversational/intent side to actuation may
compile).

---

## 12. Security and deployment (Planned; anchors are Existing foundation)

Requirements:

- **Secure-boot integration** — chain starts at boot (the WS-4 measured-boot
  direction: UEFI Secure Boot + dm-verity feeding PCR16 attestation).
- **Signed missions** — a mission package is signed; production devices
  verify before arming (Uptane reuse: `docs/ota/UPTANE_ROLES.md`,
  `kirra-ota-installer` `uptane_trust`, EP-13's rollback-attack and
  downgrade-by-omission refusals).
- **Signed runtime packages** — same chain as the governor artifact (cosign +
  SBOM per WS-0.6).
- **Per-device identity** — the attestation identity registry (AK per node)
  extended to MAR devices.
- **Per-service identity** — each §7.1 service authenticates as itself, not
  as a shared token (the WS-1 per-principal lesson).
- **Least privilege** — per-service QNX security policy; minimized abilities.
- **Mutual authentication across trust boundaries** — the mTLS/cert-principal
  machinery is the fleet-side precedent.
- **Anti-rollback** — Uptane rollback floors persist and are enforced
  (existing installer behavior).
- **Credential rotation** — ordered, fail-closed rotation with documented
  order-of-operations. *Existing foundation and cautionary tale:*
  `docs/safety/GOVERNOR_KEY_PROVISIONING.md` §"Rotation order — this order is
  load-bearing": consumer-first rotation is mandatory; done out of order the
  consumer refuses everything and the system decel-to-stops until re-enrolled.
  MAR rotation procedures inherit this discipline and its alarm pattern.
- **Auditable administration** — every admin mutation audit-chained (existing
  R156-shaped campaign audit is the model).
- **SBOM** and **provenance** — per release, in the evidence package (§10.1).
- **Production authorization** — a device runs a mission only under an
  explicit, signed production authorization.

**A production package must not run merely because it exists on disk.**
Presence is not authorization; the verifier is the digest + signature + trust
metadata chain, never the filesystem.

**Signing-required workflows must fail loudly when no usable private key is
available.** This is already the in-tree rule for the governor signing key
(`KIRRA_GOVERNOR_SIGNING_KEY_SOURCE` unset/empty → refuse; `dev-fixed` needs
an explicit allow flag) — MAR signing inherits it. No GitHub issue number is
cited here because the defect class is documented in
`GOVERNOR_KEY_PROVISIONING.md` rather than an open issue; if an issue exists
at implementation time, the implementing epic (MAR-06/MAR-10) should link it.

---

## 13. Initial reference demonstration (Planned; Gate MA-B/MA-C evidence shape)

One end-to-end demonstration, emphasizing **authority, recovery and evidence —
not AI sophistication**:

1. An inspection mission is compiled and validated (§2.4 outputs produced;
   a deliberately broken variant is shown to be rejected per §2.5).
2. The mission invokes a capability that proposes a consequential action.
3. Kirra evaluates mission state and policy.
4. Kirra requests bounded authority (§5.2).
5. An independent safety gateway accepts — and, in a second run, **refuses** —
   the request; the refusal is shown to be non-bypassable from the mission
   plane.
6. A component failure triggers QNX-level recovery (HAM) and Kirra-level
   mission handling (§8 ladder), each recorded as itself.
7. The complete causal chain is recorded (§10.1 package).
8. The mission is replayed exactly (§10.3) and reproduces every decision.

The negative arms (rejected compile, refused authority, induced failure) are
part of the demonstration by design: safety-relevant tests must demonstrate
that they detect the protected defect.
