//! #1030 stage 3 — the two-node HA failover drill RE-RUN over Postgres shared state.
//!
//! `tests/ha_failover.rs` proves the shared-file (SQLite) HA topology
//! deterministically at the store level: a standby promotes by claiming the next
//! durable epoch, and the revived old primary is FENCED out of writing. ADR-0038
//! moves the SHARED control-plane tiers (the epoch fence above all) to Postgres
//! when `KIRRA_DB_URL` selects it; stage 3 proves the SAME exactly-one-writer
//! guarantee holds on the PG backend — the shared state is now a Postgres row, not
//! a shared file, and the takeover authority is the transactional `SELECT … FOR
//! UPDATE` epoch CAS instead of a SQLite `BEGIN IMMEDIATE`.
//!
//! The topology is realized honestly: TWO independent `PgVerifierStore`
//! connections (two OS-level sessions) share ONE Postgres database (one schema),
//! exactly as two `kirra_verifier_service` processes would share one managed PG.
//! No async monitors, no wall-clock timers — the real `try_claim_epoch` CAS +
//! `assert_actuator_epoch_held` fence + the durable `renew_lease` / `read_ha_lease`
//! lease, driven by the SAME `kirra_verifier::lease` timing model the SQLite drill
//! uses (so the two topologies are held to one contract).
//!
//! Skip-loudly discipline (matching the persistence live suites): without
//! `KIRRA_PG_URL` every test passes vacuously (loudly on stderr); the
//! `postgres-conformance` CI lane provides the `services: postgres` container.
//! Compiles to nothing without the root `postgres` feature.
#![cfg(feature = "postgres")]

use std::sync::atomic::{AtomicU32, Ordering};

use kirra_persistence::postgres::driver as postgres;
use kirra_persistence::postgres::PgVerifierStore;
use kirra_persistence::{EpochFence, FenceError, PostureEngineStateStore};

use kirra_verifier::lease::{holder_must_self_demote, promotion_due_since_renew, LeaseParams};
use kirra_verifier::standby_monitor::{HEARTBEAT_KEY, PROMOTION_TIMEOUT_MS};

static SCHEMA_SEQ: AtomicU32 = AtomicU32::new(0);

fn pg_url() -> Option<String> {
    match std::env::var("KIRRA_PG_URL") {
        Ok(u) if !u.trim().is_empty() => Some(u),
        _ => {
            eprintln!("SKIPPED: KIRRA_PG_URL unset — the PG two-node HA drill needs a server");
            None
        }
    }
}

/// A fresh, test-private schema (the shared "database" the two nodes share). The
/// caller opens TWO connections into it — the shared-DB HA topology.
fn shared_schema() -> Option<(String, String)> {
    let url = pg_url()?;
    let schema = format!(
        "kirra_ha_{}_{}",
        std::process::id(),
        SCHEMA_SEQ.fetch_add(1, Ordering::Relaxed),
    );
    let mut c = postgres::Client::connect(&url, postgres::NoTls).expect("connect");
    c.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}"
    ))
    .expect("create fresh shared schema");
    Some((url, schema))
}

/// Open one node's connection into the shared schema. The FIRST call installs the
/// schema (migrations); subsequent calls are idempotent — exactly what a second
/// service process attaching to the same managed PG sees.
fn node_store(url: &str, schema: &str) -> PgVerifierStore {
    let mut c = postgres::Client::connect(url, postgres::NoTls).expect("connect");
    c.batch_execute(&format!("SET search_path TO {schema}"))
        .expect("pin schema");
    PgVerifierStore::from_client(c).expect("initialize schema on live PG")
}

/// Stage 3 core drill: standby promotes by claiming the next durable epoch on PG;
/// the revived old primary is fenced (write refused AND stale-epoch re-claim
/// refused). Mirrors `ha_failover_promotes_standby_and_fences_the_old_primary`
/// but over two PG connections to one database.
#[test]
fn pg_ha_failover_promotes_standby_and_fences_the_old_primary() {
    let Some((url, schema)) = shared_schema() else {
        return;
    };
    let mut a = node_store(&url, &schema); // primary
    let mut b = node_store(&url, &schema); // standby (same shared PG)

    // --- Primary A claims the epoch → it owns writes ---
    let e0 = a.current_epoch().unwrap();
    let e1 = a
        .try_claim_epoch(e0, "A", 1_000)
        .unwrap()
        .expect("A claims the epoch at startup");
    assert_eq!(e1, e0 + 1);
    a.assert_actuator_epoch_held(e1)
        .expect("A holds the epoch → its writes are admitted");

    // A heartbeats into the SHARED (PG) engine-state store.
    a.save_engine_state(HEARTBEAT_KEY, &1_000u64.to_string())
        .unwrap();

    // --- Standby B reads a FRESH heartbeat from PG → it must NOT promote ---
    let hb: u64 = b
        .load_engine_state(HEARTBEAT_KEY)
        .unwrap()
        .expect("heartbeat present")
        .parse()
        .unwrap();
    let now_fresh = 1_000 + PROMOTION_TIMEOUT_MS - 1;
    assert!(
        now_fresh.saturating_sub(hb) < PROMOTION_TIMEOUT_MS,
        "a fresh heartbeat keeps B in standby"
    );

    // --- A dies (stops heartbeating); time advances past the promotion timeout ---
    let now_stale = 1_000 + PROMOTION_TIMEOUT_MS + 1;
    assert!(
        now_stale.saturating_sub(hb) >= PROMOTION_TIMEOUT_MS,
        "a stale heartbeat is B's promotion trigger"
    );

    // --- B promotes by claiming the NEXT durable epoch (the real transactional CAS) ---
    let observed = b.current_epoch().unwrap();
    assert_eq!(observed, e1, "B observes A's epoch before promoting");
    let e2 = b
        .try_claim_epoch(observed, "B", now_stale)
        .unwrap()
        .expect("B wins the epoch claim");
    assert_eq!(e2, e1 + 1);
    b.assert_actuator_epoch_held(e2)
        .expect("B now holds the epoch → its writes are admitted");

    // --- SPLIT-BRAIN FENCE: the old primary A revives and tries to act at its STALE
    // epoch. It still believes it holds e1, but the durable epoch is now e2. ---
    assert_eq!(
        a.assert_actuator_epoch_held(e1),
        Err(FenceError::EpochSuperseded {
            held: e1,
            durable: e2,
        }),
        "the fenced old primary CANNOT write (epoch superseded)"
    );
    assert!(
        a.try_claim_epoch(e1, "A", now_stale + 1).unwrap().is_none(),
        "A's stale-epoch re-claim is refused by the durable CAS"
    );

    // Exactly ONE writer (B) at a time — split brain prevented, observed from A's
    // connection (the shared PG row, not a per-process view).
    let (cur, holder) = a.current_active_holder().unwrap();
    assert_eq!(cur, e2);
    assert_eq!(holder.as_deref(), Some("B"), "B is the sole active holder");
}

/// Stage 3, lease variant: the DURABLE lease drives failover on PG end to end (the
/// real `renew_lease` / `read_ha_lease` over the shared `ha_state` row + the real
/// CAS + fence over two connections), driven by the SAME `LeaseParams` timing the
/// SQLite drill uses. Proves promotion-on-lease-expiry composes with the
/// split-brain fence on Postgres: a live holder keeps its lease fresh (challenger
/// stays down); a dead holder's lease goes stale (challenger promotes + takes the
/// lease); a REVIVED old holder's renewal is refused by the same epoch+holder
/// guard, forcing its self-demote — and the durable lease still names the winner.
#[test]
fn pg_lease_driven_failover_promotes_and_fences_using_the_durable_lease() {
    let Some((url, schema)) = shared_schema() else {
        return;
    };
    let p = LeaseParams::default_params();
    let mut a = node_store(&url, &schema);
    let mut b = node_store(&url, &schema);

    // --- A claims the epoch and establishes its lease (renew at claim time) ---
    let e0 = a.current_epoch().unwrap();
    let e1 = a
        .try_claim_epoch(e0, "A", 1_000)
        .unwrap()
        .expect("A claims the epoch");
    assert!(
        a.renew_lease("A", e1, 1_000).unwrap(),
        "A holds the epoch → its renewal lands"
    );

    // --- A live holder keeps its lease fresh; the standby must NOT promote ---
    let renew_at = 1_000 + p.renew_interval_ms; // A renews at half-life
    assert!(
        a.renew_lease("A", e1, renew_at).unwrap(),
        "A renews at half-life"
    );
    let lease = b.read_ha_lease().unwrap();
    assert_eq!(lease.epoch, e1);
    assert_eq!(lease.holder.as_deref(), Some("A"));
    assert_eq!(
        lease.last_renew_ms, renew_at,
        "B observes A's fresh renewal"
    );
    assert!(!promotion_due_since_renew(
        renew_at + 10,
        lease.last_renew_ms,
        &p
    ));
    assert!(!holder_must_self_demote(
        renew_at + 10,
        lease.last_renew_ms,
        &p
    ));

    // --- A dies (stops renewing). Time advances past the promote deadline ---
    let now = renew_at + p.promote_after_ms;
    let lease = b.read_ha_lease().unwrap();
    assert!(
        holder_must_self_demote(now, lease.last_renew_ms, &p),
        "A's own lease has long expired → A must have self-demoted"
    );
    assert!(
        promotion_due_since_renew(now, lease.last_renew_ms, &p),
        "the stale durable lease is B's promotion trigger"
    );

    // --- B promotes by claiming the NEXT epoch (real CAS) and takes the lease ---
    let observed = b.current_epoch().unwrap();
    assert_eq!(observed, e1);
    let e2 = b
        .try_claim_epoch(observed, "B", now)
        .unwrap()
        .expect("B wins the claim");
    assert!(
        b.renew_lease("B", e2, now).unwrap(),
        "B now holds and renews the lease"
    );
    b.assert_actuator_epoch_held(e2)
        .expect("B holds the epoch → its writes are admitted");

    // --- SPLIT-BRAIN FENCE: the revived old holder A cannot renew NOR write ---
    assert!(
        !a.renew_lease("A", e1, now + 1).unwrap(),
        "A's renewal at its STALE epoch is refused by the epoch+holder guard → A self-demotes"
    );
    assert!(
        a.assert_actuator_epoch_held(e1).is_err(),
        "the fenced old holder A cannot write (epoch superseded)"
    );
    // The lease still names B — A's refused renewal did not touch it.
    let lease = a.read_ha_lease().unwrap();
    assert_eq!(lease.epoch, e2);
    assert_eq!(
        lease.holder.as_deref(),
        Some("B"),
        "B is the sole holder — no split brain"
    );
    assert_eq!(
        lease.last_renew_ms, now,
        "the lease carries B's renewal, not A's refused write"
    );
}
