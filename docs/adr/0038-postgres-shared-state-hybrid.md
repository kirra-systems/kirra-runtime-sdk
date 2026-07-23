# ADR-0038 — Postgres for Shared Control-Plane State, Local SQLite for the Audit Ledger (the Hybrid)

**Status:** Accepted (stages 1–3 landed on #1030)
**Date:** 2026-07-23
**Owner:** Kirra Systems, LLC
**Scope:** How #1030 ("shared-file SQLite control plane is an availability SPOF; promote Postgres") is realized. Decides WHICH storage tiers move to Postgres when `KIRRA_DB_URL` selects it, which deliberately stay local, and the staged path from today's single-backend service to the selectable one. Ratified by the owner on the #1030 scoping question (2026-07-23), superseding the earlier assumption that the whole store could switch backends behind one flag.

---

## Context

The A2 review finding (#1030) names the shared-file SQLite store as the control
plane's availability SPOF in the HA topology: primary and standby share one
database file, so the file (and its host) is a single point of failure even
though the verifier process itself fails over.

EP-10/G-9 built the exit ramp: ten storage-seam traits (`EpochFence`,
`NodeStore`, `PostureEngineStateStore`, `FederationStore`, `OperatorStore`,
`PrincipalStore`, `CertPrincipalStore`, `FabricAssetStore`, `OtaCampaignStore`,
`AvSubsystemStore`) with shared conformance suites, and
`crates/kirra-verifier-pg` binding all ten to a live Postgres server in the
`postgres-conformance` CI lane.

A full call-surface audit for this ADR established what the conformance story
does NOT cover — the service methods with **no Postgres implementation, by
explicit design** (`kirra-verifier-pg/src/lib.rs` records the audit-chaining
exclusion):

- the **hash-chained audit tier** — every `save_*_chained*` write (posture
  events, denials, federation reports, clearance grants), chain
  verification/pagination, the signing-key ledger, verdict-id lookup, the
  WORM ship cursor. These ride nearly every request;
- the **posture-event history** (`load_node_history`, flap counting, backup
  export);
- the **fabric causal log** (its own hash chain);
- the **attestation identity registry**;
- assorted epoch-fenced write variants, the HA lease rows, and
  SQLite-lifecycle methods (`is_wal_mode` — the SG-008 startup invariant —
  and `durable_checkpoint`).

Reimplementing the chained-audit tier on Postgres is a from-scratch,
safety-critical build (tamper-evidence semantics, fsync equivalence, key
ledger) — not a wiring exercise.

## Decision

**Hybrid.** When `KIRRA_DB_URL=postgres://…` is configured (stage 2), the
verifier routes the **shared control-plane state** — the ten trait-seam tiers:
node registry + trust, epoch fence, engine state/generation, federation
registry + anti-replay, operators, API/cert principals, fabric assets, OTA
campaigns + adoption, AV subsystem meta — to Postgres. The
**tamper-evident audit ledger** (audit chain, posture-event history, causal
log, key ledger) **stays on per-instance local SQLite**, always.

Rationale:

1. **The SPOF is the SHARED state.** What makes the shared file a single
   point of failure is exactly the state both instances must agree on — the
   epoch fence above all. Moving that tier to a Postgres deployment (which
   brings its own replication/HA story) removes the shared-file coupling.
2. **The audit ledger is inherently per-writer.** Each chain entry links to
   the previous entry written by THAT instance; the chain is an
   append-only, per-writer artifact whose durability story is local fsync
   (`synchronous=FULL`, the #1046 power-loss drill) and whose off-box story
   already exists — the WS-4 WORM audit shipper re-verifies the stream
   independently of the source DB. Centralizing it in Postgres would weaken
   the tamper-evidence argument (a compromised central DB rewrites every
   instance's history at once) while buying no availability (a verifier that
   cannot write its local ledger must fail closed anyway).
3. **The WCET/latency story survives.** The chained audit write rides the
   deny/verdict path; keeping it a local fsync avoids adding a network RTT
   to the safety-critical path. Shared-state writes (registration, campaign
   lifecycle) are control-plane-slow already.
4. **SG-008 stays intact.** The WAL-mode startup invariant and
   `durable_checkpoint` remain meaningful because local SQLite remains —
   they now guard the audit ledger specifically.

Consequences accepted with the hybrid: a fleet's shared state can be current
in Postgres while an instance's local ledger lags (crash between the two
writes). This is the SAME class of gap the WORM shipper's at-least-once
cursor already handles.

**Fused-op decomposition order (stage-2 amendment).** The SQLite backend's
fused write+audit methods (federated-report accept, campaign lifecycle,
clearance grants) are single transactions — accept and ledger are atomic. On
the Pg backend they decompose, and the order is **shared write FIRST, local
ledger append second** for every REFUSABLE operation: the shared transaction
carries the gates (nonce burn, generation/epoch high-water, epoch fence,
phantom checks), and a mutation those gates REFUSE must never appear in the
ledger as accepted — ledger-first would record events that then fail to
commit, which is worse than a lagging ledger (the chain is supposed to be the
pessimistic record of what HAPPENED, not of what was attempted and refused).
The crash window (shared committed, ledger append missed) is LOUD
(`tracing::error` in `SharedOps::ledger_append`) and reconcilable by
cross-checking the shared rows against the chain. INVARIANT #12's
disk-before-memory discipline is about durable-before-volatile within one
node's write path and is unchanged (the shared backend IS the durable record
for shared tiers; the in-memory fleet map still updates only after it).

## Rejected alternatives

- **Full Postgres including the audit chain.** Largest scope; weakens the
  per-writer tamper-evidence story; adds a network RTT to the deny path;
  duplicates durability machinery the local ledger + WORM shipper already
  prove. Revisitable later as an *additional* central mirror (never the
  authoritative chain).
- **Single-flag whole-store swap (the original #1030 framing).** Impossible
  as scoped: the audit tier has no PG implementation and should not get one
  (above), and `StoreHandle` passes concrete `&mut VerifierStore` closures at
  ~150 call sites — a whole-store trait object is a big-bang refactor with
  no safety payoff.

## Staging

- **Stage 1 (this ADR's PR): de-cycle.** `kirra-verifier-pg` depended on the
  ROOT crate for two type imports whose canonical home is `kirra-core`
  (`NodeTrustState`, `RegisteredNode`) — an inverted edge that made root →
  pg consumption impossible (dependency cycle). Fixed: the crate now depends
  on the leaf. No behavior change; the `postgres-conformance` lane is the
  regression evidence.
- **Stage 2 (landed): the backend seam + flag.** First movements, merged:
  the shared-tier inherent-method GAP-FILL (`postgres/shared_ext.rs`: the
  dependency graph, epoch-fenced node upserts, attestation policy, WP-19 HA
  lease, unchained campaign/clearance-grant row primitives, WP-15 cert
  census — with the v11 PG migration and `schema_spec::SHARED_TABLES` grown
  13→16), and the FOLD-IN of the PG backend into
  `kirra_persistence::postgres` behind a `postgres` feature. The fold-in
  supersedes stage 1's assumption that the de-cycled crate could stay
  workspace-detached: a path dependency whose own workspace root
  back-references parent-workspace members is rejected by cargo ("multiple
  workspace roots"), so the backend lives IN the persistence crate — the
  driver tree still compiles only under the feature, preserving the
  detachment's actual goals (default build, MSRV lane byte-identical).
  Then, completing stage 2: `StoreHandle` grew a
  `shared`-tier accessor dispatching to either the local `VerifierStore` or
  a `PgVerifierStore` (enum, not trait object — two variants, exhaustive
  match, no vtable on the hot path); `KIRRA_DB_URL` (registered in
  `KIRRA_ENV_KEYS`, boot-validated per EP-12) selects it. Unset →
  byte-identical SQLite path. Set-but-unreachable → **fail-closed startup
  abort**; connection loss at runtime → the affected operation errors (5xx /
  posture-recalc skip), never a silent SQLite fallback (a split brain
  between backends is worse than an outage). The rusqlite leaks in handlers
  (`is_unique_violation` matching, `rusqlite::Error` closure annotations,
  the audit-shipper cursor error type) are abstracted behind
  `kirra-persistence` as part of this stage. Root gains an optional
  `postgres` cargo feature; the workspace-detachment policy for the driver
  tree is revisited THERE (the feature stays off in the MSRV lane and the
  default build).
- **Stage 3 (landed): proof.** The two-node HA drill re-run with PG shared
  state, in the `postgres-conformance` lane: `tests/ha_failover_pg.rs` drives
  the SQLite drill's exact narrative (`tests/ha_failover.rs`) over TWO
  `PgVerifierStore` connections sharing one database — the standby promotes by
  claiming the next durable epoch (transactional `SELECT … FOR UPDATE` CAS),
  and the revived old primary is FENCED (write refused + stale-epoch re-claim
  refused), plus the lease-driven variant, all on the SAME
  `kirra_verifier::lease` timing model the shared-file drill uses (so both
  topologies meet ONE exactly-one-writer contract). The persistence-crate
  primitives (a genuine two-connection concurrent CAS race
  `two_connections_racing_the_cas_produce_exactly_one_winner`, the
  `SELECT … FOR UPDATE` fence, the wedge fail-closed) back it. Topology docs
  updated (`docs/deployment/HA_TOPOLOGY.md`). The shared-file topology remains
  the DEFAULT and the documented small-deployment path; Postgres is the
  recommended multi-host HA topology.

## Invariants preserved

INVARIANT #12 (disk-before-memory) extends across backends: the durable
shared-tier write precedes the in-memory effect it records (see the fused-op
decomposition order above for how the two durable halves of a decomposed
operation are sequenced). The epoch fence contract
(`assert_actuator_epoch_held`, at-most-one-writer) is already
conformance-proven on both backends and does not change shape. `KIRRA_DB_PATH`
semantics are untouched — it names the LOCAL ledger DB in both modes.
