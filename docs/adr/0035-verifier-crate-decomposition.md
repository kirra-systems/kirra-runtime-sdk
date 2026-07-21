# ADR-0035 — Decomposing the `kirra-verifier` monolith into layered crates

| | |
|---|---|
| **Status** | Proposed |
| **Date** | 2026-07-13 |
| **Supersedes** | — (extends the ongoing de-monolith program: Stage 1 → `kirra-core`, Stage 7 → perception/kinematics/capture, plus `kirra-fleet-types`, `kirra-trajectory`) |
| **Safety goals touched** | None *directly* — this is a structural refactor. Every stage MUST be behaviour-preserving (byte-identical verdicts, same audit chain, same fail-closed semantics). The checker cores (`kirra-core`, `kirra-trajectory`) and the frozen kinematics talisman are already extracted and are NOT re-cut here. |

## Context

The root `kirra-verifier` crate is an integration monolith: it is simultaneously
the **cargo workspace root**, a **`cdylib`** (the C FFI surface), the home of **6
binaries**, and ~30 library modules spanning HTTP control-plane, SQLite
persistence, HA/epoch fencing, the posture engine + gray/black DAG, attestation,
audit chaining, industrial protocol adapters, fleet/fabric state, and
observability. The runtime engineering review recommends splitting it into
**control-plane / safety-authority / persistence / industrial / observability**
crates.

This ADR fixes the crate boundaries, the dependency DAG, and — critically — the
**sequence and prerequisites**, because the naïve reading ("just move the
industrial adapters out first") does not hold against the actual coupling.

### Evidence (measured against the tree at time of writing)

- **`AppState` is referenced in 39 files.** It is the god-object every layer
  hangs off (store handle, posture cache, federation, audit writer, epoch
  atomics, industrial replay sequence store, HA flags, metrics). The
  *safety-authority / persistence / control-plane* boundaries are **all blocked
  on decomposing `AppState`** — that is the hard core of this work, not a leaf.
- **The industrial layer is not a clean leaf.** `protocol_adapter.rs` +
  `adapters/{canopen,dnp3,ethernet_ip}.rs` depend on:
  - `OperationalCommand` (the command-classification enum, defined in the
    `posture_cache` / `gateway::policy` cluster — `gateway::policy` is 447 LOC and
    itself depends on `posture_cache`);
  - `action_filter` (266 LOC), which depends on `gateway::cmd_vel` and the
    `verifier` posture types;
  - `FleetPosture` (already in `kirra-core`, re-exported — the one clean dep).
  So extracting `kirra-industrial` first requires relocating a shared
  **command/policy-types foundation** — otherwise a new leaf crate would have to
  depend *back* on the root crate, which is a dependency cycle.
- Neither `action_filter` nor `gateway::policy` touches `AppState`/`VerifierStore`
  — they are pure decision logic. That makes a **policy-types foundation crate**
  tractable and the right *first* extraction (Stage 0 below), ahead of industrial.

### Constraints that shape the plan

1. **The root crate stays the workspace root and the `cdylib`.** Moving the
   `[workspace]` table or the FFI surface is out of scope; the root remains
   `kirra-verifier` and becomes the thin **control-plane** layer.
2. **Shim compatibility is mandatory, per the established pattern.** Each
   extraction leaves a `pub use` re-export shim so existing `crate::X` /
   `kirra_verifier::X` paths (including sibling crates and the 6 bins) keep
   resolving unchanged. No stage may force a same-PR edit of an unrelated
   consumer.
3. **Behaviour preservation is the acceptance bar.** Verdicts, audit-chain bytes,
   fail-closed ordering, and WCET-critical paths must be byte-identical. The
   Kani/loom/fuzz/power-loss/replay suites are the regression net.
4. **CI-gate implications** each stage must satisfy:
   - the quality-guardrails ratchet (`ci/quality_guardrails_baseline.json`:
     `max_lines`, `panic_budget`, `ownership_scope`) — moving code between crates
     changes which files are tracked; update the baseline with justification;
   - **MSRV on both lockfiles** (`cargo +1.88.0 check --workspace --locked`) — a
     new crate adds workspace members and must build on 1.88;
   - the orphan-pure-core / wire-or-delete guard (`ci/check_orphan_cores.py`);
   - `cargo-deny` supply-chain (a new crate must add no new advisory surface).

## Decision

### Target dependency DAG (leaf → root; must stay acyclic + strictly layered)

```
            kirra-core / kirra-fleet-types / kirra-trajectory        [DONE — checker cores]
                                   ▲
        kirra-policy-types   (OperationalCommand, action_filter, cmd_vel,     [Stage 0 — NEW foundation]
                              the pure classify/route decision logic)
                                   ▲
        kirra-industrial     (protocol_adapter + adapters/*)                  [Stage 1]
                                   ▲
        kirra-persistence    (verifier_store/*, StoreHandle, EpochFence/       [Stage 2]
                              NodeStore seams — already trait-shaped)
                                   ▲
        kirra-safety-authority  (posture engine + gray/black DAG, actuator     [Stage 3 — the hard core]
                                 gate, attestation, audit chain; OWNS the
                                 AppState decomposition into a SafetyAuthority
                                 handle)
                                   ▲
        kirra-observability  (metrics, health, request-observability,          [Stage 4]
                              audit export/verify surfaces)
                                   ▲
        kirra-verifier  (control-plane: the axum bins, route handlers, auth,   [Stage 5 — residual root]
                         config wiring, FFI cdylib, [workspace] root)
```

Rule: a crate may depend only on crates strictly *below* it. `AppState` is
replaced by a `SafetyAuthority` handle owned by Stage 3; the control-plane holds
it behind the same `ServiceState` the handlers already use.

### Staging (each stage = one shippable, shim-compatible, behaviour-preserving PR)

- **Stage 0 — `kirra-policy-types` (prerequisite, do FIRST).** Extract
  `OperationalCommand`, `action_filter`, `cmd_vel`, and the pure
  `classify_http_command` / `route_command_verdict` decision logic into a lean
  crate depending only on `kirra-core`. Root re-exports them. Unblocks Stage 1
  and de-risks the whole program by proving the shim+guardrail mechanics on
  low-safety-impact code.
- **Stage 1 — `kirra-industrial`.** Move `protocol_adapter` + `adapters/*` onto
  `kirra-policy-types`. Now a clean leaf. The unified/dedicated replay-key
  normalization (PR #918) already lives here conceptually.
- **Stage 2 — `kirra-persistence`.** Move `verifier_store/*` + `StoreHandle`
  behind the existing `EpochFence` / `NodeStore` traits (already defined — this
  is the least-invasive of the big three because the seams exist).
- **Stage 3 — `kirra-safety-authority` (mini-project).** Decompose `AppState`:
  split its fields by owner (persistence handle, posture state, attestation
  state, audit writer, HA epoch), introduce a `SafetyAuthority` aggregate, and
  move the posture engine + DAG + actuator gate + attestation. This is where the
  39-file `AppState` coupling is paid down; expect several sub-PRs.
- **Stage 4 — `kirra-observability`.** Metrics/health/observability middleware +
  audit export/verify read surfaces.
- **Stage 5 — residual `kirra-verifier`.** What remains is the control-plane:
  the bins, handlers, auth, config, FFI, workspace root.

### Non-goals

- Re-cutting the already-extracted checker cores or the frozen talisman.
- Moving the workspace root or the FFI `cdylib` out of `kirra-verifier`.
- Any behaviour change. If a stage tempts a semantic change, that is a *separate*
  ADR/PR.
- Splitting `parko/` (its own sub-workspace) or the doer-side crates.

## Consequences

**Positive:** enforceable layering (the compiler forbids control-plane code from
reaching into persistence internals); independent testability and MSRV surface
per layer; smaller certification units (the safety-authority crate becomes the
reviewable safety boundary, separate from HTTP plumbing); parallel CI.

**Cost / risk:** Stage 3 is genuinely hard (the `AppState` decomposition) and
must be sub-staged. Every stage carries guardrail/MSRV/lockfile bookkeeping.
Mitigation: the shim discipline keeps each PR small and reversible, and the
existing fault-injection suites (Kani, loom, fuzz, power-loss, deterministic
replay) gate behaviour preservation.

## Execution note

Stages land as independent PRs in order (0 → 5); Stage 0 is the gating
prerequisite and the proof-of-mechanics. Do not begin Stage 1 until Stage 0 is
merged, and do not begin Stage 3 until Stages 0–2 are merged.

---

## Addendum A (2026-07-14) — Stage 2 execution finding: `verifier_store` is not a clean leaf, and how the hard tier is actually cut

**Status of this addendum:** Accepted (records what execution discovered; revises
the Stage 2 plan). Stages 0–1 landed as written (#919, #921). Stage 2 is landing
incrementally as documented below (#922, #923).

### What the plan assumed vs. what execution found

The original Stage 2 line ("move `verifier_store/*` + `StoreHandle` behind the
existing `EpochFence`/`NodeStore` traits — the least-invasive of the big three
because the seams exist") **understated the coupling**. Measured against the tree:

- `verifier_store` is ~10.6k LOC across 17 files and is consumed by ~46 files.
- The `EpochFence` + `NodeStore` trait seams cover **2 of ~8 table families**. The
  rest (`api_principals`, `operators`, `cert_principals`, `av_subsystem`, `audit`,
  `federation`, `ota_campaigns`, `posture`, `attestation`, `fabric`) had **no
  seam** — they marshal domain types and/or append signed audit events directly.
- So a clean `kirra-persistence` crate **cannot be one mechanical move**: the
  domain types the store persists (audit-chain records, `FederatedTrustReport`,
  `Campaign`, causal-log entries, `FabricAsset`, verdict ids, `KeyRegistry`, ~4.7k
  LOC) currently sit *beside* the store in the root crate, not *below* it, so a
  persistence crate would not compile independently.

### Decision: trait-seam inversion, family by family (revised Stage 2)

Rather than a big-bang crate move, Stage 2 extends the established storage-trait
seam program (`EpochFence`, `NodeStore`) one table-family at a time. Each seam is
a pure ADDITIVE PR: a backend-agnostic trait sharing the inherent method names
(inherent wins resolution → the SQLite impl delegates without recursion, every
existing caller untouched), a 2nd in-memory reference backend, and a shared
`assert_*_store_contract` conformance suite run against both. Once every family is
behind a trait, consumers depend on traits (not the concrete `VerifierStore`), and
the eventual crate extraction becomes mechanical.

**Clean tier — DONE (6 of ~8 families).** `PrincipalStore` (#922), `OperatorStore`
(#922), `CertPrincipalStore` (#922), `AvSubsystemStore` (#923), on top of the
pre-existing `EpochFence` + `NodeStore`. These are pure CRUD/registry/meta over a
single table with no audit-chaining. Two of them model a real domain failure mode
in the in-memory backend's error type rather than `Infallible`
(`CertPrincipalStore`'s `UNIQUE(cert_sha256)` + `i64::MAX` expiry refusal;
`PrincipalStore`'s `UNIQUE(token_sha256)`; `AvSubsystemStore`'s increment-on-absent).

### The hard tier — two distinct couplings

The remaining families (`audit`, `federation`, `ota_campaigns`, `posture`,
`attestation`, `fabric`) cannot be seamed as pure CRUD. They share two *distinct*
couplings that must be broken first (measured call-site counts in parentheses):

- **C1 — audit-chaining.** The method opens a transaction, writes its table row,
  AND appends a signed, hash-chained `crate::audit_chain::AuditChainLinker` event
  **within the same transaction**, using the store's `signing_key` field. Present
  in `posture` (3), `ota_campaigns` (2), `federation` (2), `operators` clearance
  grants (4), `fabric` (1), `attestation` (1), and the core `audit` module (7).
  The atomicity (row + audit append in ONE tx) is load-bearing — the power-loss
  drill (`audit_chain_prefix_on_kill`) proves the chain never forks — so the audit
  append **cannot simply move up** to the authority layer without losing it.
- **C2 — domain-type marshalling.** The method takes/returns a safety-authority
  domain type: `federation` (`FederatedTrustReport`, `authoritative_posture`),
  `ota_campaigns` (`Campaign`, `NodeArtifactStatus`), `fabric` (`CausalLogEntry`,
  `FabricAsset`), `audit` (verdict-id validation, `authoritative_posture`).
  `posture` and `attestation` are **C1-only** (they marshal primitives/local rows).

The clean-seam recipe doesn't extend here because a portable storage trait for
these would have to *name* `audit_chain` types, the Ed25519 signing key, AND the
domain types — none of which can live below persistence in the target DAG today.

### Proposed sequencing for the hard tier (Stage 2.5, before the crate extraction)

1. **Invert the audit-append dependency (breaks C1).** Introduce an injected
   `AuditAppender` seam: a trait that appends a signed event **into a caller-owned
   transaction** (`append_within(&tx, event_type, payload, at_ms)`). The store
   method keeps owning the transaction (atomicity preserved) but calls the injected
   appender instead of reaching up to `crate::audit_chain` + `self.signing_key`.
   The `AuditChainLinker` impl and the signing key move to the safety-authority
   layer and are injected at construction. This is the crux move — it removes the
   store's dependency on the signing key and the chain-hash logic while keeping the
   one-transaction guarantee the power-loss drill depends on.
2. **Relocate the C2 domain types.** Move `FederatedTrustReport{,V2}`, `Campaign` /
   `NodeArtifactStatus`, `CausalLogEntry` / `FabricAsset`, and the verdict-id
   predicate into a lower shared crate (candidate: extend `kirra-fleet-types`, or a
   new `kirra-domain-types` leaf) so a storage trait can name them. `posture` /
   `attestation` skip this step (C1-only).

   **Status (2026-07-14) — C1 done, C2 in progress.** Step 1 (the `AuditAppender`
   inversion) is COMPLETE across every `verifier_store` C1 family (#924/#925): no
   write path names `crate::audit_chain` — only the read-side `compute_record_hash`
   verify calls remain. Step 2 is proceeding as a per-domain slice, each moving one
   pure domain module to its own lean crate with a `pub use` re-export shim (the
   `kirra-fleet-types` idiom) so every existing `crate::<mod>::*` path resolves
   unchanged. Already relocated when this ADR was written: `FederatedTrustReport{,V2}`
   → `kirra-fleet-types` (the `crate::federation*` shims). **C2 slice 1 (this
   change):** the OTA campaign engine (`Campaign` / `CampaignState` /
   `NodeArtifactStatus` / `summarize_campaigns` / `resolve_node_assignment` /
   `campaign_metrics_prometheus`) → the new lean `kirra-ota-campaign` crate (deps:
   `kirra-core` for `FleetPosture`, `kirra-release-token` for the uptane set), clearing
   the `verifier_store::ota_campaigns` C2 coupling. **C2 slice 2:** the fabric-plane
   domain types (`FabricAsset` + the asset/kinematic-profile enums, `CausalLogEntry`,
   `CAUSAL_EXPORT_MAX_PAGE`) → the new lean `kirra-fabric-types` crate, clearing the
   `verifier_store::fabric` coupling. The store-backed `FabricCausalLog` facade and
   the `KinematicProfileType → VehicleKinematicsContract` mapping (genuine
   verifier-crate behaviour) STAY behind — the latter moves from an inherent impl to
   the `KinematicProfileContracts` extension trait, since the orphan rule forbids an
   inherent impl on the now-external enum. With both `verifier_store` C2 families
   cleared, the remaining work is the seam step (CRUD-trait each hard family), after
   which `kirra-persistence` extracts mechanically.
3. **Seam each hard family** as CRUD, exactly like the clean six, now that C1+C2
   are broken. Then `kirra-persistence` can be extracted mechanically (Stage 2
   proper), depending only on the domain-types leaf + the injected `AuditAppender`
   contract.

   **Status (2026-07-14) — seam step underway. Family 1 (`ota_campaigns`):** the
   `OtaCampaignStore` trait + `InMemoryOtaCampaignStore` reference backend +
   `assert_ota_campaign_store_contract` (run against both), modelling the
   backend-portable STORAGE surface — campaign insert/reads (`insert_campaign`,
   `load_campaign`, `load_campaigns`, `load_active_campaigns`) and the non-audit
   node-adoption CRUD (`upsert_node_artifact_status`, `load_node_artifact_statuses`)
   with its monotonic + attested-per-digest invariants. Matching OperatorStore's
   discipline, the audit-lifecycle `update_campaign` (R156 `event_type` + signed
   audit append) stays INHERENT-ONLY, riding the `AuditAppender` seam — it belongs to
   the authority tier, not the storage contract. `insert_campaign` IS modelled; the
   SQLite backend additionally audit-chains it, a side effect orthogonal to the
   storage contract. **Family 2 (`fabric`):** the `FabricAssetStore` trait +
   `InMemoryFabricAssetStore` + `assert_fabric_asset_store_contract` over the pure
   asset-registry surface (`save_fabric_asset`, `load_fabric_assets`). The forensic
   CAUSAL LEDGER stays inherent — `append_causal_event` is a hash-chained signed
   write (the causal analogue of the audit chain) and `load_causal_entries*` /
   `count_causal_entries` / `verify_causal_chain_integrity` read/verify that signed
   chained data, so it is the authority tier, out of the storage seam. **With both
   hard families seamed, every `verifier_store` domain now has a backend-portable
   storage-trait contract** (the clean six + these two), and the audit-authority
   surface is cleanly the inherent-only residue.

   **Correction (2026-07-14) — the extraction is NOT yet mechanical.** A dependency
   audit of `verifier_store` after the seam step found it still names several ROOT-crate
   items that would cycle if the module moved to its own crate:
   - `crate::audit_chain::*` (~30 refs) — `verifying_key_id`, `AuditChainLinker`,
     `compute_{record,causal_record}_hash`, `canonical_*_payload`, `CausalRecordHashInput`.
     The `AuditAppender` seam broke the WRITE path, but the verify/read path + the
     `ChainedAuditAppender` impl + the pure hash/canonical helpers still live in root.
   - `crate::verifier::RegisteredNode` (root data struct); `crate::audit_shipper::ShippedAuditRecord`;
     `crate::verdicts::is_valid_verdict_id`; `crate::key_registry::KeyRegistry`.

   So `kirra-persistence` needs a few ENABLING slices first, each the familiar
   relocate-to-a-leaf + re-export-shim move, before the wholesale move is truly
   mechanical:
   1. **`RegisteredNode` → `kirra-core`** (DONE — alongside `NodeTrustState`; re-exported
      as `crate::verifier::RegisteredNode`, byte-identical).
   2. **Split `audit_chain`'s PURE core** into a lean crate — DONE for the hash side:
      `kirra-audit-hash` now holds the SHA-256 record-hash computations
      (`compute_record_hash_v1/v2`, `compute_causal_record_hash`), the domain-separated
      canonical signing/anchor-head encoders, `verifying_key_id`, and
      `CausalRecordHashInput` — moved VERBATIM (byte-identical on-disk format; power-loss
      drill + tamper tests green). The stateful `AuditChainLinker` (append-into-a-tx) stays
      in root and delegates its `compute_record_hash*` methods to the crate; root's
      `audit_chain` re-exports the crate so `crate::audit_chain::<fn>` paths are unchanged,
      and `verifier_store`'s verify/read path now names `kirra_audit_hash::*` directly.
      SLICE 2b (DONE — the write seam): rather than injecting the impl up, the append
      MECHANICS moved DOWN. `append_audit_event_tx` writes the persistence-owned
      `audit_log_chain` / `audit_anchor_head` tables using only `kirra_audit_hash` +
      Ed25519, so it was relocated INTO `verifier_store::audit_appender` (as a free fn);
      `ChainedAuditAppender` calls it locally, and root's
      `AuditChainLinker::append_audit_event_tx` now DELEGATES down to it (its typed
      wrappers + external callers unchanged). This needed no store-construction churn and
      no cycle (persistence → `kirra_audit_hash`; root → persistence). Result:
      `verifier_store` has ZERO `crate::audit_chain` code references (production + tests);
      only a prose comment names it. Byte-identical (power-loss drill + tamper tests green).
   3. **Relocate the smaller couplings** — DONE. `ShippedAuditRecord` (the off-box
      shipped-record WIRE type — must stay DB-free for the independent re-verifier) →
      `kirra-core`; `mint_verdict_id` / `is_valid_verdict_id` (content-addressed SHA-256
      audit ids) → `kirra-audit-hash`; `KeyRegistry` was only doc-links (no code coupling).
      `verdicts` / `audit_shipper` re-export from the leaves so root paths are unchanged;
      `audit_shipper`'s `recompute_hash` became a free fn (calls `kirra_audit_hash`) since
      the record type is now external — which also freed `audit_shipper` of `audit_chain`.
   3.5. **Repoint the shim paths — DONE.** `verifier_store` named already-relocated types via
      their ROOT re-export shims; all repointed to the leaves directly: `crate::verifier::{FleetPosture,
      NodeTrustState, RegisteredNode}` → `kirra_core`, `crate::federation{,_reconciliation}::*` →
      `kirra_fleet_types`, `crate::ota_campaign::*` → `kirra_ota_campaign`,
      `crate::fabric::{asset,causal_log}::*` → `kirra_fabric_types`, and the test-only
      `crate::gateway::kinematics_contract::*` → `kirra_core::kinematics_contract` (also a shim).
      Two residual doc-links (`KeyRegistry`, `StoreHandle`) demoted to plain spans. RESULT:
      `verifier_store` now names ZERO root-crate items — only its own `crate::verifier_store::*`
      submodules, the leaf crates, and external crates. Byte-identical (repointing to the same
      types via re-export); 760 lib + power-loss + rollout green.
   4. **Move `verifier_store` wholesale into `kirra-persistence` — DONE.** `git mv`'d the whole
      module dir; `mod.rs`→`lib.rs`; rewrote `crate::verifier_store::` self-refs → `crate::`;
      `src/verifier_store.rs` is now a `pub use kirra_persistence::*;` shim so every existing
      path resolves unchanged. The crate depends ONLY on the leaf crates (`kirra-core` /
      `kirra-audit-hash` / `kirra-fleet-types` / `kirra-ota-campaign` / `kirra-fabric-types`) +
      storage/crypto externals — never back on the verifier tree. Root's `audit_chain` delegates
      to `kirra_persistence::append_audit_event_tx` (now `pub`, a legitimate cross-crate API,
      resolving the #928 review note). Two frictions the enabling audit hadn't reached (they live
      OUTSIDE `verifier_store/`):
      - **`impl FleetTrustStore for VerifierStore`** (`src/fleet_trust_store.rs`) — both types now
        external to root → orphan rule. Relocated into `kirra-persistence` (owns `VerifierStore`);
        its fleet-role key resolution inlined (a base64 decode) so it needs neither the root
        `KeyRegistry` nor `crate::attestation`.
      - **`KeyRegistry`** — its sole in-root consumer was that impl; after the inline it is
        unused-in-root (baselined orphan; follow-up: relocate to persistence or delete).
      - **`#[cfg(test)]` store helpers** used by ROOT tests (a dependency compiles without its own
        test cfg) → a `test-support` feature on `kirra-persistence` gates them + the durable-write
        fault-injection instrumentation; the root crate's dev-dependency enables it.
      Verified byte-identical + test-count-preserving: root lib 607 + persistence 153 = the prior
      760; power-loss drill / two_node_rollout / ha_failover green; full workspace builds.

  - **Slice-4 follow-up — `KeyRegistry` wire-or-delete (WIRE):** the baselined orphan is closed
    by RELOCATION, not deletion — the #329/ADR-0008 unified resolver embodies real, tested
    capability (multi-role resolve + the rotated-out audit-key-ledger fallback) and had a natural
    consumer waiting. `KeyRegistry` moved into `kirra-persistence` (`key_registry.rs`, +8 tests →
    persistence 161, root 599; total 760 unchanged), and `fleet_trust_store::resolve_fleet_pubkey`
    now DELEGATES to it — removing the slice-4 inlined base64 decode (a duplication) and giving
    `KeyRegistry` a genuine runtime consumer via the `FleetTrustStore` seam. The enabling move:
    the pure SPKI parser `parse_ed25519_public_pem` (no store dep) relocated from root
    `attestation.rs` into the `kirra-audit-hash` leaf (which already holds `verifying_key_id`), so
    persistence's `KeyRegistry` and the root attestation / TPM-quote paths share ONE vetted parser
    (root re-exports it → `crate::attestation::parse_ed25519_public_pem` unchanged). The root
    `key_registry` module is deleted outright (nothing in root imported it — a shim would just be a
    new consumer-less orphan); the `kirra_verifier::key_registry` baseline entry is dropped. The
    `.trim()` whitespace tolerance the fleet path needs is now single-sourced in `KeyRegistry`
    (resolving the merged-PR review note in one place).

**Alternative considered — domain-types-first only (no audit inversion):** relocate
C2 types and move `verifier_store` wholesale into a persistence crate that *keeps*
depending on `audit_chain`. Rejected as the end state: it drags the signed-audit
machinery (a safety-authority concern) into the persistence layer, inverting the
intended DAG (persistence must not know audit semantics). It may still be a useful
*intermediate* if the `AuditAppender` inversion proves too large for one step.

### Interaction with later stages

The signing key lives on `VerifierStore`/`AppState` today, so step 1 above
partially overlaps Stage 3 (the `AppState` decomposition) — the `AuditAppender`
injection is the natural place to relocate signing-key ownership to the
safety-authority layer. Sequence step 1 as the leading edge of Stage 3 rather than
duplicating the work.

### Non-goals for the hard tier (unchanged)

No behaviour change: byte-identical verdicts, the SAME audit chain bytes, the same
fail-closed semantics and one-transaction atomicity. The power-loss, loom, and
deterministic-replay suites gate this; any tempting semantic change is a separate
ADR/PR.

## Shim deprecation (#1029 — the shim-deprecation front)

The `pub use` re-export shims (Constraint 2) are a DELIBERATE transition aid, but
they are **deprecated indirection, not a permanent layer**. The #1029 review
flagged them as "pure indirection cost" — every relocated type keeps two
names/paths, which muddies the safety-case boundary. The policy:

- **Freeze the set.** `ci/reexport_shims_baseline.json` is the tracked inventory
  (each shim → its canonical crate). `ci/check_reexport_shims.py` (guardrails CI
  job) discovers every pure `pub use` re-export module under `src/` and FAILS if a
  NEW untracked shim appears or the count exceeds the ceiling. New indirection
  cannot accrete silently — a new shim is a conscious, recorded decision (or,
  preferably, avoided). The ceiling only moves DOWN.
- **Removal milestone: the next MAJOR (v2.0.0).** Deleting a shim is a
  path-breaking change for `crate::<mod>::*` / `kirra_verifier::<mod>::*`
  consumers, so it is gated to a MAJOR bump per `docs/VERSIONING_POLICY.md`. Until
  then the shims stay (back-compat) but cannot grow.
- **How to remove one** (at the MAJOR, or opportunistically for an internal-only
  shim): migrate its callers to the canonical crate path, delete the module + its
  `pub mod` line in `lib.rs`, drop the entry from the baseline, and lower
  `max_shims` to lock the gain. The ratchet's "removal win" note flags a
  baseline entry whose file has already vanished.
- **Dead vs live (the pre-2.0 removal test):** a shim with **zero** consumers
  anywhere in the workspace — no `crate::<mod>` internal use, no
  `kirra_verifier::<mod>` cross-crate/bin use — is dead indirection with no
  reachable dependent, and removing it pre-2.0 is safe (nothing in-tree breaks, and
  a transitional re-export nobody imports is not a committed contract). A shim with
  ≥1 consumer stays v2.0.0-gated: its `kirra_verifier::<mod>` path is live published
  API and removal breaks a real import.
- **Removals so far:**
  - *(17 → 16)* `src/gateway/interceptor.rs` — an internal import-surface shim
    (re-exported `crate::posture_cache` + `kirra_core` for `policy_layer`), zero
    consumers.
  - *(16 → 14)* `src/gateway/cmd_vel.rs` + `src/posture_tracker.rs` — dead **leaf**
    re-export shims, zero consumers (every in-tree user reaches
    `kirra_policy_types::cmd_vel` / `kirra_core::posture_tracker` directly). The
    remaining 14 all have live consumers, so their removal stays gated to v2.0.0.

This converts the shims from an open-ended layer into a tracked, shrinking one —
the "deprecation with a removal milestone" the review asked for.
