# Kirra Mission Assurance Runtime — Initial Backlog (MAR-01 … MAR-12)

| Field | Value |
|---|---|
| Doc ID | KIRRA-MAR-BACKLOG-001 |
| Status | **Proposed (issue-ready backlog)** — no GitHub issues have been created from this document. Epics are filed only after Gate MA-A review, per the repository workflow. |
| Date | 2026-07-29 |
| Parent | `QNX_MISSION_ASSURANCE_RUNTIME.md` (KIRRA-MAR-PLAN-001) · Architecture: `QNX_MISSION_ASSURANCE_ARCHITECTURE.md` (KIRRA-MAR-ARCH-001) |
| Namespace | `MAR-*`, declared in KIRRA-MAR-PLAN-001 §0 (avoids the existing `WS-*` / `EP-*` / `WP-*` / `PARK-NNN` namespaces) |
| Extension | **MAR-13…MAR-20** (QNX assurance observability in Kirra Mission Console — health-event normalization, liveness/recovery cards, APS telemetry, containment evidence, consequence correlation, stream contract, card semantics, console replay) live in `QNX_MISSION_ASSURANCE_CONSOLE_OBSERVABILITY.md` (KIRRA-MAR-CONSOLE-001) — same namespace, same roadmap, not a parallel one |

Conventions: each epic follows the house closure rule — **an epic closes
against verified capability, not merged intent**. Acceptance criteria are
written to be testable; "required test evidence" names the artifact that must
exist, in the spirit of `project-board/templates.md` (Safety Task template:
verification methods + traceability) and the AoU register's non-vacuousness
discipline (a protective test must be shown to detect the defect it protects
against).

Section references (§n) are into KIRRA-MAR-ARCH-001 unless prefixed.

---

## MAR-01 — Freeze mission and safety ownership

- **Objective:** Ratify the three-plane boundary: what the mission plane owns,
  what the safety plane owns, and the normative invariant (§1.2) that a Kirra
  authority token is necessary-where-configured but never sufficient.
- **In-scope work:** An ADR (next free number — nominally 0039; verify against
  the known 0035 double-booking) recording: plane definitions, the
  plane↔domain mapping (§1.3), mission-state/safety-state separation, the
  Gate MA-A positioning decision (P3 relationship, KIRRA-MAR-PLAN-001 §1),
  the reference vertical and platform, and the threat-model outline.
- **Explicit non-goals:** No code. No protocol design. No service naming
  commitments (the `kirra-gatewayd` question may be resolved here or deferred
  to MAR-10).
- **Dependencies:** Owner review of the KIRRA-MAR-PLAN-001 baseline.
- **Major risks:** Scope creep into protocol design; ambiguity with the
  existing P1/P2/P3 ladder if the positioning question is dodged.
- **Acceptance criteria:** ADR merged with the repo's ADR status convention
  (`Proposed (design note) — ratified on merge` or explicit `Accepted`);
  every §1.2 sentence appears verbatim or strengthened, never weakened.
- **Required test evidence:** N/A (decision record). The ADR must include its
  "Conditions that reopen this decision" section per house style.
- **Required documentation:** The ADR; KIRRA-MAR-PLAN-001 §1/§7 updated to
  cite it instead of carrying the open question.
- **Closure condition:** Gate MA-A items all decided and recorded; the two
  index files (roadmap README, root README) point at the ADR.

## MAR-02 — Define the bounded mission schema

- **Objective:** A versioned schema for the §2 mission model: exactly the
  eleven supported constructs, none of the excluded ones expressible.
- **In-scope work:** Schema definition (typed, versioned); the mission-package
  identity rule (deterministic hash over canonical form); worked example
  missions for the industrial-inspection vertical, including at least one
  deliberately invalid mission per §2.5 rejection class.
- **Explicit non-goals:** No compiler implementation (MAR-03); no executor
  (MAR-04); no runtime schema mutation support (excluded permanently, §2.3).
- **Dependencies:** MAR-01 (vertical + ownership frozen).
- **Major risks:** Expressiveness pressure ("just one embedded script…") —
  the schema's value is what it refuses; hash canonicalization subtleties
  (field ordering, float encoding — reuse the repo's bit-exact discipline).
- **Acceptance criteria:** Every §2.2 construct representable; every §2.3
  exclusion demonstrably non-representable (there is no encoding that means
  "unbounded loop"); package hash is stable across platforms and
  serialization runs.
- **Required test evidence:** Schema-level tests: round-trip determinism of
  the package hash; a corpus of invalid missions, one per rejection class,
  each failing for the *stated* reason (not a generic parse error).
- **Required documentation:** Schema reference doc + the example corpus,
  cross-linked from KIRRA-MAR-ARCH-001 §2.
- **Closure condition:** Schema versioned and frozen for Phase 1; invalid-
  mission corpus green in CI.

## MAR-03 — Build the mission compiler

- **Objective:** Compile mission source to the §2.4 outputs; reject per §2.5.
- **In-scope work:** Canonical typed graph; deterministic package hash;
  capability dependency lock; resource admission plan; authority plan;
  recovery plan; failure-completeness report; deployment compatibility
  report; evidence-recording plan. All eleven §2.5 rejection cases
  implemented fail-closed.
- **Explicit non-goals:** No execution; no QNX awareness beyond the platform
  fields of the compatibility report; no network access at compile time.
- **Dependencies:** MAR-02 (schema), MAR-05 (capability contracts, for the
  dependency lock and compatibility checks — may proceed in parallel against
  the contract *schema*).
- **Major risks:** Failure-completeness analysis correctness (the compiler's
  central proof obligation); silent acceptance of a mission that should have
  been rejected.
- **Acceptance criteria:** Each §2.4 output produced and deterministic; each
  §2.5 case rejected with a typed, specific diagnostic; an unsigned package
  is refused under the production profile.
- **Required test evidence:** Per-rejection-case negative tests that
  demonstrate detection of the protected defect (mutate a valid mission into
  each invalid class; assert the specific rejection); determinism test
  (compile twice, byte-identical package).
- **Required documentation:** Compiler reference (inputs, outputs,
  diagnostics catalogue).
- **Closure condition:** Gate MA-B item "a mission graph outside the bounded
  schema cannot compile" demonstrated by the negative corpus.

## MAR-04 — Build the mission executor

- **Objective:** Execute compiled missions per the §3 state machine, emitting
  a complete §3.1 transition record for every transition.
- **In-scope work:** State machine implementation; the eleven graph
  constructs' runtime semantics (bounded retry counters, timeout via
  monotonic clock, race resolution with deterministic tie-breaking, approval
  gates, capability substitution per the §4.3 rule); hooks to the policy
  interface (MAR-05), authority kernel (MAR-06), recorder (MAR-08).
- **Explicit non-goals:** No physical actuation of any kind — Phase 1 runs
  against the simulated safety gateway (MAR-07) only; no QNX-specific code;
  no scheduler.
- **Dependencies:** MAR-02, MAR-03; interfaces of MAR-05/06/07/08.
- **Major risks:** Hidden nondeterminism (map iteration order, time reads,
  race resolution) breaking exact replay — design for MAR-09 from day one
  (accept `now_ms`/clock injection per the existing `Clock` trait
  discipline, never `SystemTime::now()` inline).
- **Acceptance criteria:** All transitions produce complete §3.1 records;
  mission-`DEGRADED` and fleet-posture `Degraded` are separate recorded
  variables; terminal states are terminal; a `SAFE` entry records the
  safety-plane response where applicable.
- **Required test evidence:** `VirtualClock`/`ScenarioRunner`-style
  deterministic scenario tests for every construct, including timeout,
  bounded-retry exhaustion, race ties, and abort-during-recovery.
- **Required documentation:** Executor semantics reference (per-construct
  runtime behavior).
- **Closure condition:** The MAR Phase 1 CLI can run the reference-vertical
  example missions end-to-end against the simulated gateway,
  deterministically.

## MAR-05 — Define functional and assurance capability contracts

- **Objective:** The two-contract capability model (§4.1/§4.2) and the
  five-way compatibility rule (§4.3), machine-checkable at admission.
- **In-scope work:** Contract schemas; registry semantics; compatibility
  evaluation; the policy interface the executor consults (adaptation of the
  existing policy-decision shape — pure, fail-closed, injectable — per
  `authz::authorize_request` precedent).
- **Explicit non-goals:** No live capability implementations beyond test
  fixtures; no ROS 2 wrapping (Phase 4); not a replacement for
  `GovernorContractView` or the per-class contract profiles (§4
  disambiguation is normative).
- **Dependencies:** MAR-02 (schema hosting the requirements side).
- **Major risks:** The assurance contract degenerating into freeform prose —
  every field must be typed and comparable, or the §4.3 conjunction cannot
  be evaluated.
- **Acceptance criteria:** A capability differing only in assurance profile
  is rejected for a mission requiring the stronger profile; name+semver
  match alone demonstrably insufficient (test exists).
- **Required test evidence:** Compatibility-matrix tests covering each of
  the five conjuncts failing independently.
- **Required documentation:** Capability-contract reference; a filled-in
  contract pair for each demo capability.
- **Closure condition:** MAR-03's dependency lock and MAR-04's substitution
  path both consume the rule from one shared implementation (one live owner).

## MAR-06 — Build the authority kernel

- **Objective:** Mission-scoped authority requests and tokens (§5.2) and the
  mission-plane half of the §5.3 acceptance conjunction — as an upstream
  extension of the existing release-token chain, never a replacement (§5.1).
- **In-scope work:** Request/token formats; binding fields incl. nonce
  single-use + TTL, sequence, evidence identity, envelope, lifetime;
  freshness rules per §6.2 (backward-step accounting, forward-step
  fail-closed); restart invalidation of outstanding tokens (§6.1 —
  currently Future work, this epic makes it real for MAR tokens).
- **Explicit non-goals:** No changes to `kirra-release-token` semantics or
  the actuator stations' verify-before-release; no weakening of per-command
  tokens into windowed/epoch tokens (rejected in ADR-0031 Clause C — that
  rejection binds here too).
- **Dependencies:** MAR-01 (ownership), MAR-04 (executor hooks), MAR-08
  (evidence identities to bind).
- **Major risks:** Clock ambiguity handling (§6.2); accidental creation of a
  second authority-of-record for the same decision (violates one-live-owner);
  sentinel identity leakage (must use typed identities).
- **Acceptance criteria:** No consequential action proceeds without a grant;
  replayed token rejected (sequence rule `<= last_accepted ⇒ reject`);
  expired/future-dated tokens rejected; post-restart, pre-restart tokens are
  invalid without any silent regain.
- **Required test evidence:** Replay, staleness, backward/forward clock-step,
  and restart-invalidation tests under `VirtualClock`; a
  bypass-attempt test proving the executor cannot reach a consequential
  action around the kernel.
- **Required documentation:** Authority model reference, cross-linked to
  ADR-0031/0033 and HVCHAN §3 to show the chain composition.
- **Closure condition:** Gate MA-B item "authority required for consequential
  action" demonstrated, including the negative arms.

## MAR-07 — Build a simulated independent safety gateway

- **Objective:** A simulation-only stand-in for the safety plane that
  evaluates the full §5.3 conjunction independently and can refuse
  authentic requests.
- **In-scope work:** The gateway side of the request protocol; configurable
  safety state, hardware constraints, and envelopes; scripted refusal
  scenarios; response records suitable for §3.1 `safety-plane response`
  fields.
- **Explicit non-goals:** No claim of safety integrity — it is a test
  double for protocol and mission-plane behavior, not a safety mechanism;
  no physical I/O; it must be *removed from the trust story*, not promoted,
  when MAR-12 lands (an inactive duplicate safety implementation is removed,
  not retained).
- **Dependencies:** MAR-06 (protocol shape).
- **Major risks:** The simulation drifting from the eventual physical
  protocol (MAR-12); teams treating simulator acceptance as safety evidence.
- **Acceptance criteria:** Refuses an otherwise fully-authentic request on
  local safety state alone; the mission plane demonstrably cannot bypass or
  override the refusal (Gate MA-B item).
- **Required test evidence:** The bypass-attempt suite: every mission-plane
  code path that could reach actuation is exercised against a refusing
  gateway and shown blocked.
- **Required documentation:** Gateway protocol draft (explicitly marked as
  the Phase 3 protocol's input, not its definition).
- **Closure condition:** The KIRRA-MAR-ARCH-001 §13 demonstration's refusal
  arm runs against it in CI.

## MAR-08 — Build the structured evidence recorder

- **Objective:** The §10.1 mission evidence package with §10.2 trust tiers,
  hash-chained, reusing the audit-chain and shipper disciplines.
- **In-scope work:** Package format; per-item trust-tier typing; chained
  transition records; recomputed-digest points (the recorder recomputes
  digests over bytes it holds rather than trusting caller-supplied
  identities); export; independent re-verification of a shipped package
  (the `verify_shipped_chain` pattern).
- **Explicit non-goals:** No analytics/UI; no counterfactual machinery; no
  replacement of the existing verifier audit ledger — the mission recorder
  is a new stream composed with, not forked from, `src/audit_chain.rs`
  conventions.
- **Dependencies:** MAR-04 (transition records), MAR-06 (authority records),
  MAR-07 (safety responses).
- **Major risks:** Sentinel evidence identities (§6.1 — typed absent
  representations required); silently recording caller-supplied identities
  at a higher trust tier than earned.
- **Acceptance criteria:** Every §10.1 item present for a demo mission; every
  item carries its tier; a tampered package fails independent
  re-verification (and the tamper test demonstrably detects the tamper).
- **Required test evidence:** Chain-tamper detection test; trust-tier
  mislabeling test (a caller-supplied identity can never appear as
  recomputed); crash-consistency posture consistent with the existing
  audit-chain drill discipline (`tests/audit_chain_prefix_on_kill.rs` as the
  model, adapted at Phase 2 when durability becomes real).
- **Required documentation:** Evidence package format reference incl. the
  §10.2 tier definitions verbatim.
- **Closure condition:** The §13 demonstration's causal chain is a single
  exportable, independently re-verifiable package.

## MAR-09 — Build exact replay

- **Objective:** Exact event replay of a mission (§10.3): the recorded
  package fed back through the *real* executor and policy engine reproduces
  every mission decision.
- **In-scope work:** Replay driver; divergence classification (identical /
  DIVERGENT / not-replayable with a named missing input — adopt
  KIRRA-REPLAY-001's classification doctrine verbatim); CLI (exit 1 on
  divergence, matching `kirra-replay`).
- **Explicit non-goals:** No counterfactual replay (Phase 5); no simulator;
  no reimplementation of decision logic (the comparison must run the same
  code the deployment ran — the EP-19 anti-drift rule).
- **Dependencies:** MAR-04 (deterministic executor), MAR-08 (complete
  records).
- **Major risks:** Determinism leaks found late (mitigated by MAR-04's
  clock-injection requirement); version skew between recording and replaying
  builds — the package's runtime-version field must gate replay claims.
- **Acceptance criteria:** Gate MA-B item: replay of the demo missions
  reproduces every transition, policy decision, and authority decision;
  an artificially corrupted record is classified DIVERGENT loudly.
- **Required test evidence:** Record→replay round-trip in CI; a mutation
  test proving divergence detection detects (change one recorded input,
  observe DIVERGENT — non-vacuousness).
- **Required documentation:** Replay reference, positioned relative to
  KIRRA-REPLAY-001 (verdict replay) as the mission-level extension.
- **Closure condition:** §13 demonstration step 8 green in CI.

## MAR-10 — Port the service model to QNX

- **Objective:** The §7.1 candidate services as isolated QNX processes with
  native IPC, bounded channels, per-service identities, and minimized
  privileges.
- **In-scope work:** Process decomposition (resolving the `kirra-gatewayd`
  naming question if MAR-01 deferred it); native message-passing for
  request/reply control traffic; the §7.3 bounded shared-memory profile for
  any high-rate path; QNX security policy per service; explicit restart
  behavior per service; resource declarations wired to §9 admission.
- **Explicit non-goals:** No HAM integration (MAR-11); no physical safety
  boundary (MAR-12); no certified-toolchain claims (the P3/Ferrocene track
  owns those); no scheduler above QNX.
- **Dependencies:** MAR-04…MAR-09 (the kernel being ported); EPIC #270
  toolchain groundwork (`docs/adr/KIRRA_QNX_CROSSCOMPILE.md`,
  `KIRRA_QNX_RUNBOOK.md`) as Existing foundation.
- **Major risks:** QNX `std`/toolchain constraints for full services (the
  known #189-class blockers that the no_std judge deliberately sidestepped —
  MAR services are *not* no_std, so this must be spiked early); unbounded
  queue defaults sneaking in via library defaults.
- **Acceptance criteria:** Gate MA-C items: isolated services, every IPC
  channel's depth + overflow behavior declared and enforced, privileges
  minimized (each service's abilities enumerated), restart behavior explicit
  and tested per service.
- **Required test evidence:** On-target (or QNX-VM, honestly labeled per the
  house INDICATIVE discipline) tests: queue-overflow behavior, kill/restart
  of each service, privilege-denial probes.
- **Required documentation:** QNX process architecture doc; per-service
  identity + privilege matrix.
- **Closure condition:** The Phase 1 demonstration runs on a QNX target with
  the decomposed services, minus physical actuation.

## MAR-11 — Integrate QNX HAM

- **Objective:** QNX HAM detects and recovers MAR process failures; Kirra
  determines and records the mission consequence (§8) — integration, not
  replacement.
- **In-scope work:** HAM entity registration for each MAR service; restart
  policies; the healthd mapping from HAM events to §8.1 fault classes and
  ladder levels; evidence records for every HAM action; disambiguation doc
  note vs. Kirra "HA" (per §7.2).
- **Explicit non-goals:** No reimplementation of process supervision inside
  Kirra; no mission-level logic pushed down into HAM scripts.
- **Dependencies:** MAR-10.
- **Major risks:** Restart semantics interacting with §6.1 (a HAM-restarted
  authority kernel must not resurrect pre-restart tokens); double-recovery
  races between HAM (Level 2) and mission-level substitution (Level 3) —
  the ladder ordering must be enforced, deterministically.
- **Acceptance criteria:** Gate MA-C item: kill any MAR service → HAM
  restarts it → the affected missions each record the correct
  per-mission consequence (which may differ per mission) → no
  pre-restart authority remains valid.
- **Required test evidence:** Kill-matrix test (each service × representative
  mission states) on target/VM, with evidence-package assertions.
- **Required documentation:** HAM integration guide incl. the HA-vs-HAM
  disambiguation.
- **Closure condition:** Kill-matrix green; §13 demonstration step 6 runs
  with real HAM recovery.

## MAR-12 — Integrate a physical independent controller

- **Objective:** Replace the simulated safety gateway with a physical
  independent controller for the reference vertical: the live §5.3
  conjunction at real hardware, with the mission plane demonstrably unable
  to override it.
- **In-scope work:** The Phase 3 safety-gateway protocol (command envelopes,
  evidence binding, session epochs, replay protection, acknowledgements,
  watchdogs) hardened from the MAR-07 draft; hardware-in-the-loop rig;
  deployment verification that confirms authority ownership (the device's
  actuator path only accepts through the independent controller);
  removal of the MAR-07 simulator from any production configuration
  (inactive duplicate safety implementations are removed, not retained).
- **Explicit non-goals:** No claim of certified integrity for the controller
  itself (procurement/qualification is its own decision, recorded at Gate
  MA-D); no hazardous-motion authority in Kirra (permanent non-goal).
- **Dependencies:** MAR-06…MAR-11; hardware procurement (a named external
  dependency with a date, per the house rule of chasing licenses/hardware as
  named dependencies).
- **Major risks:** Protocol drift from the simulator (mitigated by reusing
  the MAR-07 test suite against the physical controller); clock-domain
  mistakes at the boundary (AOU-TIMESYNC-001 discipline applies); the
  acknowledgement-binding design diverging from the existing
  actuation-consumer direction instead of reusing it.
- **Acceptance criteria:** Gate MA-D items: independent actuator gate live;
  replayed and stale evidence rejected on hardware; acknowledgement binding
  proven; deployment verification confirms authority ownership end-to-end;
  every MAR-07 bypass-attempt test passes against the physical controller.
- **Required test evidence:** HIL runs of the full §13 demonstration incl.
  refusal and recovery arms; fault-injection at the boundary (replay, stale,
  malformed, out-of-envelope, out-of-sequence) — each refused and each
  refusal evidenced.
- **Required documentation:** Safety-gateway protocol spec (normative);
  HIL rig runbook; new/updated AoU entries for integrator obligations.
- **Closure condition:** Gate MA-D passed with the evidence pack exported by
  MAR-08 and replayable by MAR-09.
