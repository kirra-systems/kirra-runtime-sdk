# Kirra Mission Assurance Runtime — QNX Assurance Observability in Kirra Mission Console (MAR-13 … MAR-20)

| Field | Value |
|---|---|
| Doc ID | KIRRA-MAR-CONSOLE-001 |
| Status | **Proposed (issue-ready backlog extension)** — planning only. **No QNX integration, console component, backend service, collector, schema, or transport described here exists** unless a line is labeled *Existing foundation*. No GitHub issues have been created. |
| Date | 2026-07-30 |
| Parent | `QNX_MISSION_ASSURANCE_RUNTIME.md` (KIRRA-MAR-PLAN-001) · `QNX_MISSION_ASSURANCE_ARCHITECTURE.md` (KIRRA-MAR-ARCH-001) · extends `QNX_MISSION_ASSURANCE_BACKLOG.md` (KIRRA-MAR-BACKLOG-001, epics MAR-01…MAR-12) |
| Namespace | Same `MAR-*` namespace declared in KIRRA-MAR-PLAN-001 §0 — this is an extension of the one MAR roadmap, not a parallel one |

Status labels: **Existing foundation** · **Adaptation** · **Planned** ·
**Future work** · **Out of scope** (KIRRA-MAR-PLAN-001 §3). Section references
(§n) are into KIRRA-MAR-ARCH-001 unless prefixed.

---

## 0. Scope, ownership, and the core rule

### 0.1 Intended flow (Planned)

```
QNX HAM / APS / procfs / trace and process events
                        │
                        ▼
                 kirra-healthd
       normalize, classify, correlate, sequence
                        │
                        ▼
             Kirra typed evidence events
                   │             │
                   ▼             ▼
             kirra-recorderd   Console event stream
                                      │
                                      ▼
                        Kirra Mission Console cards
```

`kirra-healthd` and `kirra-recorderd` are the §7.1 architectural candidates —
still candidates, not implementation commitments.

### 0.2 Ownership boundary (normative)

| Owner | Owns |
|---|---|
| QNX + HAM | Process observation and process-level recovery |
| QNX Adaptive Partitioning (APS) | Scheduler budgets and partition accounting |
| Kirra (mission plane) | Capability health, mission consequence, degradation policy, authority consequence, event correlation, evidence identity |
| Kirra Mission Console | **Presentation only** |
| Independent safety plane | Final hazardous-motion authority (§1.2 — unchanged, always) |

The console must not become an independent safety or health authority. **A
console card displays an already-classified Kirra state or event; frontend
JavaScript must never be the sole owner of a `safe`/`warn`/`crit`
(green/amber/red) safety-relevant classification.** *Existing foundation for
this posture:* the console's own error-boundary copy states "the UI is not the
fleet"; its proxy is GET-only with a path allow-list (a console client already
cannot inject verifier state); demo data is loudly fenced (`KIRRA-DEMO-*`,
"DEMO DATA — NOT EVIDENCE").

### 0.3 The core invariant (carried through every epic)

> **QNX reports operating-system facts. Kirra assigns mission meaning.
> Kirra Mission Console renders the result.**

### 0.4 False inferences this plan forbids (normative)

None of the following may be implied by any MAR document, event type, card, or
claim:

| Forbidden inference | Why it is false |
|---|---|
| A process restart proves capability recovery | Restart is stage 3 of 7 in the §MAR-13 recovery distinction; capability health has its own evidence |
| A green process proves mission readiness | Mission readiness is a mission-plane classification over capabilities, policy, and authority — not process presence |
| An APS graph proves deadlines were met | CPU share ≠ deadline success; deadline evidence comes from §9 runtime observation, never from utilization alone |
| Neighboring green nodes prove fault isolation | "Unaffected" requires positive evidence (§MAR-16 tiers); absence of observed failure is *unknown* |
| A process spawn is equivalent to service recovery | Spawn precedes endpoint availability, responsiveness, and capability restoration — separately evidenced stages |
| A slog2 line is authoritative because it appeared in slog2 | slog2 text enters at the *untrusted diagnostic* tier (§10.2); it never drives a state transition by itself |
| Console presentation creates or changes authority | Presentation-only ownership (§0.2); the observability path is read-only by construction |
| HAM replaces Kirra mission-level recovery policy | §7.2 / §8: HAM restarts processes; Kirra decides and records what that means per mission |
| Kirra replaces HAM process-level recovery | Same boundary, other direction — MAR-11 is integration, not reimplementation |
| QNX telemetry alone proves physical safety | Physical safety claims belong to the safety plane and its own evidence (§1.2) |

### 0.5 Terminology decisions (conflicts reported, not silently resolved)

| Term | Finding | Resolution here |
|---|---|---|
| "Kirra Console" | The in-tree product is **"Kirra Mission Console"** (`console/README.md`), also called the Operator Console (`docs/CONSOLE_RUNBOOK.md`) | These docs use *Kirra Mission Console*; "console" unqualified means it |
| WebSocket | Not used anywhere in-tree; the established live transport is **SSE snapshot-plus-delta** (`GET /system/posture/stream` + coalesced snapshot refetch, bounded broadcast ring `POSTURE_BROADCAST_CAPACITY`) | The MAR-18 stream contract is transport-agnostic; SSE snapshot-plus-delta is the reference realization; WebSocket only if a later decision replaces it — framed as transport, never event authority |
| Red/amber/green | Console tokens are `crit`/`warn`/`safe` | Card specs use the token names |
| `Degraded` | Three senses now exist: fleet-posture `Degraded` (decel-to-stop envelope, #70), mission-`DEGRADED` (§3), and a proposed card state | Every card naming "Degraded" must declare **which variable** it renders; the two runtime senses are never merged into one indicator (§1.2 separation) |
| Heartbeat / staleness | Established: telemetry watchdog thresholds (`AV_TELEMETRY_WARN_MS` 1 s / `AV_TELEMETRY_TIMEOUT_MS` 2 s), posture-cache TTL staleness fail-closed, subscription staleness budgets | Card states "Suspect"/"Missed heartbeat" bind to *declared* thresholds in this style — never ad-hoc frontend timers |
| Recovery streaks | Established: recovery hysteresis (streak threshold + window, resets on fault) | "Restored" and "Recovery exhausted" states reuse the hysteresis shape rather than inventing a second streak model |
| HAM / APS / slog2 / procfs | **No prior in-tree usage** (HAM only in MAR docs). APS = QNX Adaptive Partitioning (scheduler partitions with budgets); slog2 = QNX system logger | Introduced here as *candidate* QNX-side mechanisms; every named QNX API/facility is an implementation decision to verify against the target QNX version (the ADR-0006/EPIC-#270 discipline: exact carriers are decided, and reopened, by ADR) |
| Event taxonomy | KIRRA-MAR-ARCH-001 already defines the §3.1 transition record, §10.1 evidence package, and §10.2 trust tiers | **No second taxonomy.** The §1 envelope below is a *presentation view* over those records; `source_classification` IS the §10.2 tier enum, not a new scale |
| `boot_epoch` | §6.1 marks the MAR session/boot epoch as **Future work** (MAR-06 makes it real) | Every epoch field below inherits that dependency; nothing here claims the epoch exists today |

---

## 1. Cross-epic event model (Planned)

### 1.1 Envelope

One common envelope for everything the console receives — a serialized view of
recorder-owned events (the recorder stays authoritative, §MAR-18):

```
ConsoleAssuranceEvent
  schema_version            — stream schema version; mismatch fails visibly (MAR-18)
  event_id                  — typed identity (never zero/sentinel, §6.1)
  event_seq                 — monotonic sequence, the ordering authority (timestamps never order events)
  boot_epoch                — session/boot epoch (Future work dependency: MAR-06)
  source_service            — which MAR service observed/classified
  source_instance           — process-instance identity of the source (not PID)
  event_kind                — closed enum, fail-closed on unknown (the `OperationalCommand::Unknown` discipline)
  mission_id?               — where applicable
  capability_id?
  process_instance_id?      — instance identity; PID is auxiliary, never identity across restart
  authority_id?
  evidence_id               — §10.2-typed evidence identity
  observed_monotonic_time   — local-duration authority (§6.2)
  observed_wall_time        — cross-process comparison + audit only (§6.2)
  freshness_deadline?       — after which the event's *liveness implication* (not the record) is stale
  payload                   — event-kind-typed
  source_classification     — the §10.2 trust tier of the payload's source
```

### 1.2 Source classifications (reuses §10.2 — no new scale)

Every event/field is tagged as exactly one of: **authoritative QNX
observation** (an OS fact, at its OS-facility trust level) · **authoritative
Kirra mission state** (§3 machine output) · **authoritative safety-plane
response** (the safety plane's own record) · **normalized event**
(healthd-translated, source identity preserved) · **inferred correlation**
(Kirra-computed, marked inferred) · **operator annotation** ·
**untrusted diagnostic text** (slog2 lines and similar).

**A slog2 line must not silently become an authoritative state transition.**
It may *accompany* a transition as supplementary evidence at the untrusted
tier; the transition itself requires a typed observation.

---

## 2. Epic set

Template per KIRRA-MAR-BACKLOG-001, extended with the fields this set
requires: *current Kirra foundations*, *authoritative data owner*, *event or
data model*, *status classification*.

---

## MAR-13 — QNX health event normalization (Epic 1)

- **Status classification:** Proposed (adapter: Planned; QNX inputs: Future
  work until verified on the target QNX version).
- **Objective:** The planned boundary through which QNX process and recovery
  observations become typed Kirra health events — the `kirra-healthd`
  normalize/classify/correlate/sequence stage of §0.1.
- **Current Kirra foundations:** §8 fault classes + recovery ladder; the
  telemetry watchdog's declared-threshold liveness model; recovery hysteresis;
  the audit-chain event discipline; §3.1 transition records; the closed-enum
  fail-closed parsing posture (`MickIntent::from_llm_json`,
  `OperationalCommand::Unknown`).
- **In-scope work:** The healthd QNX adapter *plan*: candidate inputs — HAM
  entity/recovery events, process exit and restart observations, service
  registration state, explicit heartbeat state, selected slog2 diagnostics
  (supplementary tier only), process-instance/PID changes, boot/session
  epoch, process-manager fault information, service responsiveness probes.
  Exact QNX APIs are implementation decisions to verify at MAR-10/11 time.
  **Log scraping is never the sole authoritative recovery mechanism.**
- **Explicit non-goals:** No collector implementation; no HAM subscription
  code; no console work (MAR-14); no second event taxonomy (§0.5); no claim
  that any listed input is available on the target QNX version until
  verified.
- **Dependencies:** MAR-08 (recorder), MAR-10/11 (QNX services + HAM — the
  observations only exist once those land); MAR-06 (epoch binding).
- **Authoritative data owner:** QNX/HAM own the *observations*; healthd owns
  the *normalization and classification*; the recorder owns the durable
  record.
- **Major risks:** Collapsing the seven-stage distinction below into
  "Recovered"; PID-as-identity bugs; slog2 promotion to authoritative;
  epoch field designed before MAR-06 defines the epoch.
- **Event or data model:** §1.1 envelope; normalized fields (adapted to house
  conventions): monotonic event sequence, boot/session epoch, process
  instance identity, service identity, PID (auxiliary, where applicable),
  HAM entity identity (where applicable), event kind, previous health state,
  new health state, observation monotonic time, observation wall-clock time,
  recovery action, recovery attempt, triggering condition, mission identity
  (where applicable), mission consequence, capability consequence, authority
  consequence, evidence identity, source classification (§1.2).
  **Required distinction — seven separate event kinds, never one:**
  1. process failure observed
  2. HAM recovery initiated
  3. replacement process created
  4. service endpoint available
  5. service health restored
  6. capability health restored
  7. mission recovery completed
- **Acceptance criteria (properties):**
  - Every process restart is represented as a **new process instance**, even
    when the operating system reuses the same PID.
  - A process becoming runnable or present does **not** by itself transition
    a Kirra capability to healthy.
  - Mission recovery is recorded **separately** from process recovery.
  - QNX-originated observations retain their source identity through
    normalization and recording (§1.2 tags survive end-to-end).
  - An unknown observation kind is refused/quarantined, never coerced into a
    known kind.
- **Required test evidence:** Normalization tests over a synthetic
  observation corpus covering all seven stages, PID-reuse across restart,
  out-of-order delivery, and unknown kinds; a non-vacuousness test proving a
  collapsed "Recovered" encoding is unrepresentable.
- **Required documentation:** Healthd adapter boundary spec; the event-kind
  catalogue cross-linked to §8 fault classes.
- **Closure condition:** The seven-stage corpus normalizes deterministically
  and lands in recorder records reproducible by MAR-20 replay.

---

## MAR-14 — Process liveness and recovery feed (Epic 2)

- **Status classification:** Proposed (cards: Planned; depends on MAR-13
  events existing).
- **Objective:** The Kirra Mission Console cards that display process
  liveness, recovery progress, and mission consequences from normalized
  MAR-13 events.
- **Current Kirra foundations:** The console's fleet tiles + SSE
  snapshot-plus-delta hook + live/demo labeling + UTC-pinned rendering;
  `safe`/`warn`/`crit` tokens; watchdog thresholds; hysteresis states.
- **In-scope work (console concepts, minimum):** daemon heartbeat grid;
  service health grid; active recovery card; historical recovery timeline;
  recovery-attempt counter; and the four duration metrics —
  time-to-process-restart, time-to-service-responsive,
  time-to-capability-restored, time-to-mission-restored — each computed from
  a **defined event pair** of the MAR-13 seven stages; unresolved recovery
  indicator. Candidate card states (bound to declared thresholds/hysteresis
  per §0.5, reusing existing vocabulary where present): Healthy, Suspect,
  Missed heartbeat, Failed, Restart requested, Restarting, Process
  available, Service unavailable, Recovering, Degraded (declared variable),
  Restored, Recovery exhausted.
- **Explicit non-goals:** No UI implementation; no frontend-computed
  classification (cards render Kirra-classified states, §0.2); no merging of
  the three "Degraded" senses.
- **Dependencies:** MAR-13, MAR-18 (stream), MAR-19 (card contracts).
- **Authoritative data owner:** healthd classifications via the recorder;
  the console renders.
- **Major risks:** Duration metrics silently falling back to wall-clock
  arithmetic across epochs; frontend timers reinventing staleness.
- **Event or data model:** Consumes §1.1 events only; no card-local state
  machine beyond presentation.
- **Acceptance criteria (properties):**
  - **The console must not report a service or capability as recovered
    merely because HAM created a replacement process** (stages 3 vs 5/6/7).
  - Event ordering is deterministic (by `event_seq`, never timestamps).
  - Duplicate or delayed observations do not silently rewrite history
    (idempotent by `event_id`; late events are marked late).
  - A missing event of a duration pair yields an explicit
    **incomplete-duration** state, never a guessed number.
  - Boot-epoch boundaries prevent false correlation across restart.
  - The UI can explain why a component is `crit`/`warn`/`safe` (reason code
    surfaced, MAR-19).
  - Every status card links to the evidence events supporting it.
  - Exact replay (MAR-20) reproduces the same recovery timeline and
    classifications.
- **Required test evidence:** Card-classification snapshot tests over
  recorded corpora (incl. duplicate/late/missing-event cases); an
  epoch-boundary corpus proving no cross-epoch duration is computed.
- **Required documentation:** Card contract entries (MAR-19 format) for
  every card above.
- **Closure condition:** All cards render from recorded events alone, with
  the replacement-process-≠-recovered invariant demonstrated by a test that
  tries to violate it.

---

## MAR-15 — APS partition and budget telemetry (Epic 3)

- **Status classification:** Proposed (Future work until APS interfaces are
  verified against the target QNX version — no APS terminology exists
  in-tree today).
- **Objective:** Plan collection and presentation of QNX Adaptive
  Partitioning configuration and runtime telemetry.
- **Current Kirra foundations:** §9 declared-budget model (admission,
  observation, deadline-miss escalation); the INDICATIVE-vs-certified timing
  honesty doctrine (`docs/safety/WCET_MEASUREMENT_METHODOLOGY.md`);
  stale-data fail-closed patterns (posture-cache TTL).
- **In-scope work:** A backend adapter *plan* using **supported APS control
  and query interfaces** (never undocumented filesystem assumptions; no API
  name is anchored until verified against the target QNX version).
  Candidate data: partition identity; configured minimum budget; configured
  maximum budget where supported; critical budget; current usage;
  averaging-window usage; critical-time usage; throttling state; bankruptcy
  state; scheduler configuration generation; process/thread membership;
  observation timestamp; stale-data state. Console concepts: partition
  budget overview; configured min/max gauges; current usage bars;
  averaging-window graph; critical-time consumption; throttle state;
  bankruptcy event timeline; configured-versus-observed membership; stale
  telemetry indicator.
- **Explicit non-goals:** No scheduler above QNX (§9); no deadline claims
  from CPU data; no implementation.
- **Dependencies:** MAR-10 (QNX services), MAR-13 (envelope), MAR-19
  (cards); correlation targets from §9 deadline evidence.
- **Authoritative data owner:** QNX APS owns budgets and accounting; Kirra
  owns the *observation record* and any correlation; the console renders.
- **Major risks:** Treating an APS budget as a hard cap (see terminology
  rule); membership assumptions surviving a scheduler-generation change;
  stale observations rendering as current.
- **Event or data model:** APS observations as §1.1 events at the
  *authoritative QNX observation* tier; scheduler configuration generation
  is a first-class field — a generation change invalidates prior membership
  assumptions.
  **Terminology rule (normative):** an APS budget is never casually a "hard
  cap". The model distinguishes: guaranteed/configured share **under
  contention**; opportunistic use of free CPU; configured maximum-budget
  **enforcement** (where supported); observed usage; throttling; bankruptcy.
- **Auditor-facing claim discipline:** Never «this graph proves the safety
  partition was never starved». The bounded form (final wording to follow
  the assurance-evidence house style at implementation time): «During the
  recorded interval, the safety partition retained its configured APS
  budget, reported no bankruptcy condition, and Kirra observed no protected
  deadline miss while the inference partition was saturated.»
- **Acceptance criteria (properties):**
  - Budget configuration and observed usage are displayed separately.
  - Missing maximum-budget enforcement is visible (never silently assumed).
  - Stale APS observations cannot appear current.
  - Scheduler generation changes invalidate old membership assumptions.
  - APS evidence can be correlated with Kirra deadline and health events —
    and only *correlated*, never substituted.
  - Replay reproduces the displayed APS timeline from recorded
    observations.
  - The console does not infer application-level deadline success from CPU
    percentage alone.
- **Required test evidence:** Stale-data and generation-change corpora with
  card-state assertions; a negative test showing a saturated-CPU corpus with
  a recorded deadline miss renders the miss, not a green partition.
- **Required documentation:** APS observation model + the budget-semantics
  glossary (the six distinctions above).
- **Closure condition:** APS cards render exclusively from recorded
  observations with staleness and generation semantics demonstrated.

---

## MAR-16 — Spatial and temporal fault-containment evidence (Epic 4)

- **Status classification:** Proposed (model: Planned; QNX trace inputs:
  Future work pending target-platform verification).
- **Objective:** A Kirra evidence model showing where a failure occurred,
  what boundary contained it, and what consequences propagated.
- **Current Kirra foundations:** The dependency-graph posture DAG (gray/
  black traversal, `blocked_by` lists) as the propagation precedent; §8
  fault classes; §3 mission states; the §10.2 tiers; QNX-processes-as-
  fault-containment-unit (§7.2).
- **In-scope work:** The containment evidence model over candidate inputs:
  HAM recovery events, process-manager failure information, service
  heartbeat loss, explicit IPC timeout events, channel disconnects,
  connection-generation changes, process instance changes, selected QNX
  trace instrumentation, capability health transitions, mission state
  transitions, authority revocation/restriction, safety-plane responses.
  QNX channel flags, pulses, tracing, and process-manager facilities are
  **candidate implementation mechanisms to verify against the target
  platform** — in particular, `ChannelCreate()` / `ConnectAttach()` are
  never described as universal system-wide fault-monitoring hooks. Console
  concepts: component boundary map; process-instance view; dependency
  graph; fault containment indicators; IPC generation changes; capability
  impact; mission impact; authority impact; safety-plane response; recovery
  progression; incomplete-evidence indicator.
- **Explicit non-goals:** No visual graph design before the typed evidence
  model is stable (Slice 3 rule, §3); no inference of safety-plane state
  from mission-plane state; no implementation.
- **Dependencies:** MAR-13 (events + instance identity), MAR-17
  (consequence correlation), MAR-06 (authority records).
- **Authoritative data owner:** QNX owns the OS-boundary facts; Kirra owns
  impact classification and the dependency model; the safety plane owns its
  own responses; the console renders.
- **Major risks:** Equating "no observed failure" with "proved unaffected";
  cross-instance confusion when generations are missing; dependency
  *inference* presented at a confirmed tier.
- **Event or data model:** Containment records over §1.1 events with
  **ten distinguished classifications** — observed process failure;
  observed IPC failure; observed capability loss; inferred dependency
  impact; confirmed mission consequence; confirmed authority consequence;
  confirmed safety-plane response; unknown consequence;
  unaffected-by-positive-evidence; merely not observed to fail. The last
  two are distinct by construction: **"unaffected" requires positive
  evidence; absence of evidence renders as unknown, never healthy.**
  A candidate card (names to follow house conventions at implementation):
  component; process instance (PID + generation); failure kind; containment
  boundary; HAM action; IPC generation transition; capability impact;
  mission consequence (§3 transition); authority consequence; safety-plane
  response; recovery state; evidence-chain status.
- **Acceptance criteria (properties):**
  - Process and IPC generations prevent cross-instance event confusion.
  - A failed process can remain `crit` while unrelated mission functions
    stay healthy — and the *unrelated* claim is positive-evidence-backed.
  - "Mission unaffected" requires positive mission evidence.
  - Safety-plane state is never inferred from mission-plane state.
  - Dependency propagation is explicit and explainable (which edge, which
    evidence).
  - Replay reconstructs the same containment graph and fault timeline.
  - Absence of evidence is represented as unknown, not healthy.
- **Required test evidence:** Corpus tests distinguishing all ten
  classifications; a mutation test proving the unknown-vs-unaffected
  distinction detects its defect (drop the positive evidence, observe the
  tier drop to unknown).
- **Required documentation:** Containment evidence model spec incl. the
  ten-way classification and the candidate-mechanism verification list.
- **Closure condition:** The §13 reference demonstration's failure arm
  produces a complete containment record replayable by MAR-20.

---

## MAR-17 — Mission and authority consequence correlation (Epic 5)

- **Status classification:** Proposed (Planned; **the essential epic** —
  without it this set is only a QNX operations dashboard).
- **Objective:** Correlate QNX operational events with capability, mission,
  authority, and safety-plane consequences so OS facts acquire mission
  meaning.
- **Current Kirra foundations:** §3.1 transition records already carry
  trigger + policy decision + safety-plane response; MAR-06 authority
  records; the EP-17 deny-verdict → explanation pattern (a machine code
  bound to an operator-readable reason); ADR-0037 typed ordering.
- **In-scope work:** Correlation *plan* across: process failure; process
  restart; capability health; mission transition; degradation mode;
  recovery action; authority issuance; authority refusal; authority
  revocation; safety-gateway response; minimum-risk request; final recovery
  result. Candidate console card — "Authority Consequence" (name to follow
  house conventions): source failure; affected capability; mission state
  transition; authority action; safety-plane action; recovery action;
  evidence-chain status; exact-replay identity.
- **Explicit non-goals:** No automatic authority restoration logic (policy
  belongs to MAR-06 and mission policy); no correlation by timestamp alone;
  no implementation.
- **Dependencies:** MAR-04 (mission transitions), MAR-06 (authority),
  MAR-07/12 (safety-plane responses), MAR-13 (normalized events).
- **Authoritative data owner:** Kirra mission plane owns the correlation
  and disposition; each correlated input retains its own owner and tier.
- **Major risks:** Silent authority inheritance across process instances;
  timestamp-based correlation producing false causality; dispositions
  defaulting to "no consequence" instead of "unknown".
- **Event or data model:** Correlation records as §1.1 events at the
  *inferred correlation* tier unless every input is confirmed; a closed
  **disposition enum**: no authority consequence · authority restricted ·
  authority revoked · safety action requested · **disposition unknown**
  (the fail-closed default).
- **Acceptance criteria (properties):**
  - Every safety-relevant process or capability failure has an explicit
    disposition from the enum above — never an absent field.
  - Process recovery does **not** restore authority automatically unless
    mission and authority policies explicitly permit it.
  - A new process instance cannot silently inherit authority, evidence
    freshness, or sequence continuity from the failed instance (§6.1
    restart discipline applied to instances).
  - Console correlation uses typed identities and event sequences, **not
    timestamps alone**.
- **Required test evidence:** A correlation corpus where each disposition
  arises; an inheritance-attempt test (new instance presents the old
  instance's tokens/sequence → refused and recorded); a
  timestamp-collision corpus proving sequence-based ordering.
- **Required documentation:** Correlation + disposition model spec; the
  Authority Consequence card contract (MAR-19 format).
- **Closure condition:** The §13 demonstration step 6 yields a complete
  failure→disposition chain, replayable, with the no-silent-inheritance
  test green.

---

## MAR-18 — Console assurance event stream (Epic 6)

- **Status classification:** Proposed (contract: Planned; SSE
  snapshot-plus-delta: Existing foundation as the reference realization).
- **Objective:** The transport *contract* between MAR runtime services and
  Kirra Mission Console: bounded, typed, replayable.
- **Current Kirra foundations:** SSE snapshot-plus-delta (`GET
  /system/posture/stream` + coalesced snapshot refetch; stream-error →
  polling fallback with backoff); bounded broadcast
  (`POSTURE_BROADCAST_CAPACITY` ring); backpressure/shed pools (429 +
  `Retry-After`, never unbounded queues); RBAC scopes for stream access
  (identity-gated SSE tier); the GET-only allow-list proxy.
- **In-scope work:** Contract requirements: typed event envelopes (§1.1);
  schema version; event sequence; boot/session epoch; mission identity;
  process-instance identity; capability identity; evidence identity;
  source service; event generation time; transmission time; freshness;
  dropped-event reporting; stream resynchronization;
  bounded queues; declared backpressure policy; snapshot-plus-delta
  recovery; authorization; offline evidence replay feed.
  **Transport is not committed:** the repo's established abstraction is SSE
  snapshot-plus-delta and is the reference; any transport (including
  WebSocket, if ever adopted) is *transport, not event authority*.
- **Explicit non-goals:** No protocol implementation; no new authority
  surface (the stream is read-only; writes stay on their existing
  authenticated routes); no unbounded buffering anywhere.
- **Dependencies:** MAR-08 (recorder is the authority the stream mirrors),
  MAR-13 (event types), MAR-06 (epoch).
- **Authoritative data owner:** `kirra-recorderd` (the durable record);
  the stream is a lossy-but-honest mirror; the console is a subscriber.
- **Major risks:** The stream becoming a de-facto second source of truth;
  overflow hidden instead of reported; old-epoch merge on reconnect.
- **Event or data model:** §1.1 envelope on the wire; drop reporting is an
  in-band typed event (count + first/last dropped `event_seq`), mirroring
  the honest-truncation house rule ("no silent caps").
- **Required invariant:** **Loss of the console connection must not affect
  mission execution, safety decisions, recovery, or evidence recording.**
  (*Existing foundation as pattern:* the SSE stream rides the observability
  side; posture/verdict paths never depend on subscribers.)
- **Acceptance criteria (properties):**
  - Console transport cannot block control paths (bounded ring +
    drop-and-report, never backpressure into the producer's decision path).
  - Recorder evidence remains authoritative when the console is
    disconnected.
  - Queue overflow is explicit (typed drop event).
  - Reconnect performs deterministic resynchronization
    (snapshot-plus-delta; the delta anchored by `event_seq`).
  - Old-epoch events cannot be merged into the current live view.
  - Schema incompatibility fails visibly (no best-effort parse).
  - Console clients cannot inject health or authority state through the
    observability stream (read-only contract; the existing proxy
    allow-list is the precedent).
  - Replay can feed the same presentation contract **without impersonating
    a live system** (MAR-20 mode flag is part of the envelope contract).
- **Required test evidence:** Overflow, reconnect-resync, epoch-boundary,
  and schema-mismatch corpora against a contract test double; an
  injection-attempt test on the stream surface.
- **Required documentation:** Stream contract spec (transport-agnostic +
  the SSE reference realization notes).
- **Closure condition:** The contract test suite passes against the
  reference realization and against the MAR-20 replay feeder.

---

## MAR-19 — Console assurance card semantics (Epic 7)

- **Status classification:** Proposed (Planned).
- **Objective:** Reusable card *contracts* so presentation stays consistent
  and never invents safety meaning.
- **Current Kirra foundations:** Console primitives (`Panel`/`Pill`/
  `StatusDot`, `safe`/`warn`/`crit` tokens); live-vs-demo labeling; pinned
  UTC; the EP-17 reason-code → operator-sentence pattern; the DEMO-fence
  discipline.
- **In-scope work:** A card-contract template defining, per card:
  authoritative source; required fields; state classification rules (which
  Kirra-owned classification the card renders); stale-data behavior;
  unknown-data behavior; evidence links; replay behavior; accessibility
  requirements; operator-facing explanation; prohibited claims. Applied to
  at least: Process Liveness; Recovery Timeline; APS Budget; Deadline
  Health; Fault Containment; Mission Consequence; Authority Consequence;
  Safety-Plane Response; Evidence Completeness.
- **Explicit non-goals:** No component implementation; no card-local
  classification logic; no new color semantics beyond the tokens.
- **Dependencies:** MAR-14/15/16/17 (the cards' data), MAR-18 (delivery),
  MAR-20 (replay behavior column).
- **Authoritative data owner:** Each card names exactly one authoritative
  source; the card never merges owners.
- **Major risks:** Contract drift between cards; "green by default" leaking
  in through component defaults.
- **Event or data model:** Cards consume §1.1 events and recorder
  snapshots only.
- **Acceptance criteria (required rules, all normative):**
  - No card shows `safe` solely because data is absent (absent → unknown
    rendering, §MAR-16).
  - No card hides staleness (freshness deadline exceeded → visible stale
    state).
  - No card collapses mission state and safety state (§1.2 separation).
  - No card calls a restart successful before the defined recovery
    condition (the MAR-13 stage the card contract names) is met.
  - No card claims physical safety from operating-system telemetry alone.
  - Every safety-relevant classification exposes a reason code (EP-17
    pattern).
  - Every historical card identifies its boot/session epoch.
- **Required test evidence:** Contract-conformance tests runnable per card
  (absent-data, stale-data, unknown-data, epoch-labeling cases) — written
  so a violating card fails (non-vacuousness).
- **Required documentation:** The card-contract template + one filled
  contract per listed card.
- **Closure condition:** Every shipped MAR card has a merged contract and a
  green conformance suite; a deliberately violating fixture card is shown
  to fail.

---

## MAR-20 — QNX console evidence replay (Epic 8)

- **Status classification:** Proposed (Planned; extends MAR-09 / EP-19
  doctrine — Existing foundation).
- **Objective:** The QNX assurance views are reconstructable from recorded
  evidence: exact replay of the observability surface.
- **Current Kirra foundations:** EP-19 `kirra-replay` (bit-identical,
  real-code, classify-don't-guess; KIRRA-REPLAY-001); MAR-09 mission
  replay; the console's seeded-deterministic demo mode + DEMO-fence
  labeling as the visual-distinguishability precedent.
- **In-scope work:** Exact replay support *plan* for: process failures; HAM
  actions; service-health changes; capability-health changes; APS
  observations; partition bankruptcy; IPC generation changes; mission
  transitions; authority consequences; safety-plane responses; recovery
  completion. Replay feeds the MAR-18 presentation contract.
- **Explicit non-goals:** No counterfactual replay (MAR Phase 5); no replay
  of authority *effects* (replay never emits live authority); no simulator.
- **Dependencies:** MAR-09 (replay core), MAR-13…MAR-17 (recorded event
  types), MAR-18 (contract), MAR-19 (replay-behavior columns).
- **Authoritative data owner:** The recorded evidence package (MAR-08);
  replay adds nothing and guesses nothing.
- **Major risks:** Wall-clock leakage into duration math; replay windows
  mistaken for live views; incomplete evidence "healed" during replay.
- **Event or data model:** Recorded §1.1 events; a mandatory
  live-vs-replay mode marker in the presentation contract.
- **Acceptance criteria (properties):**
  - Exact replay produces the same event ordering (by recorded
    `event_seq`).
  - Exact replay produces the same card classifications.
  - Duration calculations use recorded event identities, never current
    wall-clock time.
  - Incomplete evidence remains incomplete in replay (the EP-19
    not-replayable classification doctrine, verbatim).
  - Live and replay modes are visibly distinguishable (the DEMO-fence
    discipline applied to replay).
  - Replay does not emit live authority.
  - Replay cannot be mistaken for current QNX state.
  - Console views remain deterministic under equal timestamps through
    monotonic sequence ordering.
- **Required test evidence:** Record→replay→render round-trips with
  classification snapshots; a mutation test (alter one recorded event →
  visible divergence); a mode-marker test (replay feed without the marker
  is refused by the contract).
- **Required documentation:** Replay-mode addendum to the MAR-18 contract
  and the MAR-19 replay-behavior columns.
- **Closure condition:** Every card listed in MAR-19 renders identically
  from live-recorded and replayed corpora, with divergence detection shown
  to detect.

---

## 3. Recommended implementation order

- **Slice 1 — Process recovery feed** (MAR-13 core + MAR-14 + recorder
  integration + mission-consequence correlation subset of MAR-17): QNX
  health adapter boundary, normalized process/HAM events, process-instance
  identity, boot-epoch binding, recorder integration, console health grid,
  recovery timeline. **First because ownership is clearest and semantic
  ambiguity smallest.**
- **Slice 2 — APS telemetry** (MAR-15): supported APS query mechanism,
  partition configuration, usage observations, stale-data behavior,
  bankruptcy events, correlation with Kirra deadline evidence, budget
  cards.
- **Slice 3 — Containment and dependency graph** (MAR-16 + the rest of
  MAR-17): **only after** the event taxonomy for process failure, IPC
  failure, process generation, capability impact, mission impact, and
  authority impact is stable. The visual graph is not designed before the
  underlying typed evidence model exists.
- **Slice 4 — Exact replay** (MAR-20, with MAR-18/19 contracts hardened):
  recorded QNX assurance events fed through the same presentation contract,
  visibly marked as replay, never impersonating a live system.

MAR-18 and MAR-19 are cross-cutting: their *contracts* are drafted in Slice
1 and versioned forward, so later slices extend rather than fork them.
