# Control-plane HA & storage topology

**Document ID:** KIRRA-DEPLOY-HA-TOPOLOGY-001
**Status:** Active
**Addresses:** A2 (#1030) · **Cross-refs:** `docs/adr/0005-availability-model.md`,
`docs/v1_active_passive_runbook.md`, `docs/v1_dr_drill_transcript.md`, EP-02
(`tests/ha_two_process_drill.rs`), EP-03 (`src/lease.rs`), EP-10
(`crates/kirra-verifier-pg/`)

> **Safety-vs-availability framing (read first).** A control-plane outage is an
> **availability** event, never a safety one. Actuation is gated *locally* by the
> in-line governor / checker and **fails closed** the instant the plane is
> unreachable or stale (`503 -> 0.0` consumer safe-stop; posture cache TTL;
> LockedOut). Nothing in this document is on the actuation safety path — it is
> about how long the *fleet-management* plane (trust registry, OTA campaigns,
> audit ledger) stays writable and how much it can lose on a failure.

Kirra ships **two** control-plane storage topologies. This document states what
each guarantees, its blast radius, and **which to deploy at what scale** — so an
integrator picks deliberately rather than inheriting the single-box default.

---

## 1. Topology A — shared-file SQLite (the default; single-box / small fleet)

Two `kirra_verifier_service` processes (Active + PassiveStandby) over **one
SQLite file** on shared storage, arbitrated by the durable **epoch CAS**
(`try_claim_epoch`, `src/verifier_store/epoch.rs`). Exactly-one-writer is
*proven* (EP-02 two-process drill + `tests/ha_failover.rs`: the revived old
primary is fenced, its stale-epoch re-claim refused); EP-03 adds an optional
lease trigger (`KIRRA_HA_LEASE_ENABLED`) for <=5 s failover.

**What it guarantees**
- At most one writer at any instant (epoch CAS is the takeover authority).
- The audit chain is crash-consistent (never torn/forked) across a process
  kill — see `tests/audit_chain_prefix_on_kill.rs`.

**Blast radius — state it explicitly (the A2 finding):**
- **The shared file / its filesystem IS the single point.** Loss or corruption
  of the volume, or a split of the storage fabric between the two nodes, takes
  the whole control plane down (both processes see the same file). The epoch CAS
  protects against *two writers*, not against *the disk*.
- **Durability is checkpoint-bounded on the throughput path.** The audit tail is
  `synchronous=NORMAL` (no per-row fsync); a hard power loss can drop the last
  un-checkpointed audit rows (a forensic gap, never a safety-state gap — see
  `VerifierStore::durable_checkpoint` §74 note and the P-Drill WAL-drop test).
  Incident-class rows (posture transitions, post-incident sequence) ride the
  `synchronous=FULL` durable connection and survive a power cut at write time.
- **Write ceiling.** A single process-wide `Mutex` + a single SQLite writer
  connection with globally-serialized `BEGIN IMMEDIATE` audit appends caps
  sustained write throughput at single-connection commit latency (worse on the
  FULL durable path). Adequate for a small/medium fleet at posture-event rates;
  **not** a thousands-of-high-rate-nodes plane. See §3.

**Deploy it for:** a single box, a lab/pilot, a small fleet, or any site where
the control plane and its storage share a failure domain anyway.

---

## 2. Topology B — Postgres backend (first-class; fleet scale)

`crates/kirra-verifier-pg` binds every storage seam (`PgExecutor` / `EpochFence`
/ `NodeStore` / `FederationStore` / `PostureEngineStateStore` / `OperatorStore`
/ `PrincipalStore` / `CertPrincipalStore` / `FabricAssetStore` /
`OtaCampaignStore` / `AvSubsystemStore`) to a real PostgreSQL server. It is **not
an experiment** — it runs the *same* conformance suites the SQLite backend runs
(`assert_*_store_contract`) against a live server in the `postgres-conformance`
CI lane, plus PG-only drills (a genuine two-connection CAS race; the migration
engine's future-schema refusal; the `SELECT … FOR UPDATE` actuator fence). The
cross-backend column schema is pinned by ONE spec both backends assert against
(`kirra_persistence::schema_spec`, #1033).

**Why it is the fleet-scale configuration**
- **Storage durability + failover move to Postgres**, whose replication /
  point-in-time recovery / managed-service HA are mature and operable — the
  shared-file single point is gone. The actuator fence is realized
  transactionally (`SELECT … FOR UPDATE`), so exactly-one-writer holds across
  real connections, not one file.
- **The write ceiling lifts** — PostgreSQL admits concurrent writers and its own
  commit pipelining, rather than one serialized SQLite connection.

**Deploy it for:** a production fleet, multi-thousand-node scale, any site that
needs the control plane's durability/failover decoupled from a single volume.

**Status / remaining (honest):** the backend + conformance are **landed and
CI-gated**; promoting it to the *documented default* for a given deployment is a
provisioning choice (connection string, managed-PG topology, backup policy) — an
integrator obligation, not a code gap. The driver binding is the promised
~10-line `LivePgExecutor` adapter.

---

## 3. The write-throughput ceiling (shared with both, worst on Topology A)

The posture engine already **coalesces** recalculation bursts
(`start_posture_engine_worker` drains all buffered triggers then recalculates
once) — but that coalescing stops at the in-memory posture computation; the
*persistence* of high-rate posture events is not yet batched. At fleet scale the
next lever is to **extend coalescing into the write path** (batch/merge high-rate
posture-event and adoption writes into fewer transactions). This is a tracked
follow-up of A2 (#1030), separable from the topology choice above and lower-risk
on Topology B (Postgres absorbs the write rate far better than one SQLite
connection). It is **not** on the actuation path and does not change any
fail-closed semantics.

---

## 4. Choosing — quick reference

| Question | Topology A (shared-file SQLite) | Topology B (Postgres) |
|---|---|---|
| Single point of failure | the shared file / volume | Postgres HA (managed) |
| Exactly-one-writer | epoch CAS over one file (proven) | epoch CAS via `SELECT … FOR UPDATE` (proven) |
| Failover | Active/Standby, EP-02 drill; EP-03 lease <=5 s | Postgres failover + standby promotion |
| Write throughput | single SQLite writer (ceiling) | concurrent writers |
| Hard-power-loss durability | checkpoint-bounded tail; incident rows FULL | Postgres WAL / PITR |
| When | single box · pilot · small fleet | production fleet · scale |

Actuation safety is **identical** in both — it never depends on the plane being
up. The choice is purely about control-plane availability, durability, and write
scale.
