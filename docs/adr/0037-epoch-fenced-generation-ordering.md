# ADR-0037 — Epoch-Fenced Posture Ordering: the `(epoch, generation)` Lexicographic Tuple

**Status:** Accepted
**Date:** 2026-07-22
**Owner:** Kirra Systems, LLC
**Scope:** How posture snapshots and cross-controller federation reports are ORDERED once an HA failover can reset the emitting instance's generation stream (#791 I1). Extends epoch protection from the write path (the #79 `assert_epoch_held` fence) to the ordering/consume paths: the local posture-cache CAS, the federation wire + reconciliation ladder, the inbound high-water gate, and the SSE surface. Ratified on issue #791 (design comment + the recorded owner decision on omission-downgrade).

---

## Decision

Posture ordering is the **lexicographic tuple `(epoch, generation)`**, where `epoch` is the emitting instance's durable HA epoch (`try_claim_epoch` — the `epoch = epoch + 1` CAS on the `synchronous=FULL` connection, #74) and `generation` is the existing per-process recalc counter. `epoch = 0` means "no claim / legacy source" and sorts below every claimed epoch.

A freshly-promoted controller (higher epoch, reset-adjacent generation) is therefore **newer by construction**: no consumer ever needs the new primary's counter to catch up to the old primary's. This retires the counter-catch-up problem class the #G10 seeding + #782 re-seed stopgap addressed, structurally.

### The five surfaces

1. **Local stamp + cache CAS** (`src/posture_engine.rs`). `CachedFleetPosture` carries `epoch` (stamped from `app.ha_fence.held_epoch` at recalc time); `replace_cache_if_newer` compares tuples. Two #688-preserving rules, both proven in the loom models (`crates/kirra-loom-models`):
   - `force_lockout` loads `held_epoch` **after** its caller set the sticky flag, and a sticky LockedOut candidate **inherits the cached epoch** under the CAS lock — so the epoch rung can never reject the forced lockout;
   - the CAS **re-reads the sticky escalation flags under the cache write lock** (the authoritative read; the caller's loom-proven late pre-read remains as the fast path). This closes the promotion-concurrent-with-trip window by lock ordering (release→acquire happens-before), not by cross-variable memory-ordering subtlety — loom finds the downgrade interleaving without it (`sticky_lockout_never_downgraded_under_promotion_race`).

2. **Federation wire** (`kirra-fleet-types::federation_reconciliation`). `FederatedTrustReportV2.source_epoch: Option<u64>`, **inside the signed canonical payload** — a third arm of the payload ladder (none / `source_generation` / `source_generation`+`source_epoch`), byte-stability-pinned. Trust model unchanged: like `source_generation`, a signed self-claim by a registry-trusted controller. **Structural rule:** `source_epoch` without `source_generation` is malformed (`epoch_field_well_formed`) — the gateway rejects it (`MALFORMED_EPOCH_WITHOUT_GENERATION`), and the canonical payload deliberately canonicalizes that shape WITHOUT the epoch so its signature can never be laundered into an epoch claim.

3. **Reconciliation ladder** (`reconcile_reports`). One new rung ABOVE the generation rung, mirroring the existing `Some > None` protocol-version preference: both-epoch → lexicographic tuple; one-epoch → prefer the carrier; neither → the legacy ladder verbatim. An exhaustive grid test pins agreement between the ladder and the raw tuple order.

4. **Inbound high-water gate** (`kirra-persistence::federation`, schema **v3**: `federation_generation_highwater.last_epoch INTEGER NOT NULL DEFAULT 0` + `federated_trust_reports.source_epoch INTEGER`). The per-(controller, asset) gate is the tuple compare, inside the same IMMEDIATE transaction as the #79 fence and the nonce burn. Semantics:
   - **epoch bump with generation reset = the failover signature** — accepted, NOT a regress, NOT a gap; recorded in-chain as `FEDERATION_EPOCH_ADVANCE` (so auditors can explain the generation discontinuity);
   - **epoch regress** (`offered < high-water`) → `EpochRegress`, atomic rollback (no row, no nonce burn, no advance) → `FEDERATED_EPOCH_REGRESS` at the handler;
   - **omission-downgrade = HARD REJECT** (owner decision, #791): once a peer's high-water carries epoch ≥ 1, ANY report without `source_epoch` — generation-only v2 or full v1 — is rejected (`EpochRegress { found: 0 }`). This is the EP-13 downgrade-by-omission principle applied to ordering fields. **Nothing latches:** recovery is simply the peer resuming epoch-carrying reports whose tuple exceeds the high-water. A never-epoch peer (high-water epoch 0) keeps the legacy semantics verbatim.

5. **SSE** (`PostureStreamEvent`). Additive optional `epoch`/`generation` fields on the engine-transition events (elided when absent — legacy consumers byte-identical), so a stream consumer can order explicitly instead of inferring from arrival.

### Local persistence (§5 of the design)

`posture_engine_state` gains a **`last_epoch` KV cell**, written in the SAME transaction as the generation high-water (monotonic-max, same CAST-guarded discipline). It is **diagnostic/forward-compat only**: the epoch's durable authority remains the `ha_state` row (single authority — this copy never seeds the fence). The generation high-water guard deliberately **stays generation-monotonic**: a per-epoch floor reset is dormant while boot-seeding (`init_generation_from_store`) keeps the counter restart-monotonic, so there is no live path where a legitimate write needs the floor lowered. Revisit only if boot-seeding is ever removed.

---

## Rollout — the one hard constraint

Because the epoch enters the **signed** canonical payload, an old receiver canonicalizes without it and **fails signature verification** on an epoch-carrying report — rejected, not misread (fail-closed, but an availability cliff on a mixed fleet). Therefore:

- **Reception is always-on from day one** (this change): accepting the new arm is backward-compatible, and the high-water gate only hard-rejects omission AFTER a peer has proven an epoch.
- **Emission is the integrator's rollout gate.** No production emitter of V2 reports exists in this repo (the sidecars/carrier only relay; the tests sign their own). An emitter MUST NOT populate `source_epoch` until every receiver in its fleet understands the third canonical arm. The env knob named in the design (`KIRRA_FEDERATION_EPOCH_EMIT`, default off) is accordingly an **emitter-side obligation documented here**, not a verifier `KIRRA_ENV_KEYS` row — the registry covers only vars the verifier service itself reads. When an in-repo emitter appears, it must add the knob + registry row with it.

Note the corollary the omission rule creates: once a peer emits ONE epoch-carrying report to a receiver, it cannot roll back to epoch-less emission against that receiver (hard reject, by design — a rollback is exactly the event the fence exists to surface). Flip emission per-fleet, deliberately.

---

## What this deliberately does NOT do

- **No change to the write-path fence** — `assert_epoch_held` remains the sole split-brain write authority; this ADR extends epoch protection to ordering/consume paths.
- **No PG parity work** — `federation_generation_highwater` and `federated_trust_reports` are SQLite-inherent (verified not in `schema_spec::SHARED_TABLES`); the `FederationStore` portable seam (keys/nonces/sequence) is untouched.
- **No INTEGER storage migration** for the `posture_engine_state` cells (#791 F8) — deferred per the cross-backend parity rationale recorded on the issue.
- **No multi-active semantics** — the tuple merely stops precluding them.

---

## Evidence

- Loom: `cache_holds_highest_tuple_under_cross_epoch_replace`, `sticky_lockout_never_downgraded_under_recalc_race` (lifted to tuples), `sticky_lockout_never_downgraded_under_promotion_race` (3-thread promotion×trip×recalc race; verified non-vacuous — removing the under-lock re-read reproduces the downgrade counterexample within the preemption bound).
- Byte-stability pins for all three canonical-payload arms; signature-coverage tests (strip/rewrite the epoch → verification fails).
- Storage-gate tests: failover signature (accept, `FEDERATION_EPOCH_ADVANCE`, no gap marker), epoch regress rollback, omission hard-reject + no-latch recovery, never-epoch peer unchanged, ill-formed-shape normalization, tuple round-trip through both loaders driving `authoritative_posture`.
- Handler e2e: accept/regress/omission/structural-reject over genuinely signed reports.
- Schema v3 migration: framework tests + fresh-store column assertions.
