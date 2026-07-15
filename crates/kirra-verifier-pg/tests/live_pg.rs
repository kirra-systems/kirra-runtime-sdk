// crates/kirra-verifier-pg/tests/live_pg.rs
//
// EP-10 — the live-Postgres conformance run. Each test connects to the server
// named by `KIRRA_PG_URL` (the CI lane's `services: postgres` container),
// isolates itself in a fresh Postgres SCHEMA (namespace), and drives the SAME
// conformance suites the root crate runs against SQLite + the in-memory model —
// `assert_fence_contract` / `assert_node_store_contract` are imported from
// `kirra_verifier`, not re-implemented, so all three backends are held to the
// byte-identical contract. PG-only drills (a genuine two-connection CAS race,
// the migration engine's future-schema refusal against a live stamp) follow.
//
// With `KIRRA_PG_URL` unset every test SKIPS (passes vacuously, loudly on
// stderr) so a local `cargo test` without a server stays green; only the CI
// lane, which provides the server, exercises the live paths. The lane fails if
// the URL is set but unreachable — a misconfigured lane cannot skip-to-green.

use std::sync::atomic::{AtomicU32, Ordering};

use kirra_verifier::verifier::{NodeTrustState, RegisteredNode};
use kirra_verifier::verifier_store::migrations_postgres::PgMigrationError;
use kirra_verifier::verifier_store::{
    assert_cert_principal_store_contract, assert_fabric_asset_store_contract,
    assert_federation_store_contract, assert_fence_contract, assert_node_store_contract,
    assert_operator_store_contract, assert_ota_campaign_store_contract,
    assert_posture_engine_state_store_contract, assert_principal_store_contract, EpochFence,
    FenceError, NodeStore,
};
use kirra_verifier_pg::{PgVerifierStore, PG_SCHEMA_VERSION};

/// Per-test schema counter (combined with the process id so parallel test
/// threads and successive runs never collide on a schema name).
static SCHEMA_SEQ: AtomicU32 = AtomicU32::new(0);

fn pg_url() -> Option<String> {
    match std::env::var("KIRRA_PG_URL") {
        Ok(u) if !u.trim().is_empty() => Some(u),
        _ => {
            eprintln!("SKIPPED: KIRRA_PG_URL unset — live-Postgres conformance needs a server");
            None
        }
    }
}

/// Connect a raw client pinned to a fresh, test-private schema. Every
/// connection for the same `schema` sees the same tables (the two-racer test
/// opens two).
fn raw_client_in_schema(url: &str, schema: &str) -> postgres::Client {
    let mut c = postgres::Client::connect(url, postgres::NoTls).expect("connect to KIRRA_PG_URL");
    c.batch_execute(&format!(
        "CREATE SCHEMA IF NOT EXISTS {schema}; SET search_path TO {schema}"
    ))
    .expect("create + pin test schema");
    c
}

/// A fresh store in its own schema; returns `None` (test skips) without a server.
fn isolated_store(test: &str) -> Option<(String, String, PgVerifierStore)> {
    let url = pg_url()?;
    let schema = format!(
        "kirra_ep10_{}_{}_{}",
        std::process::id(),
        SCHEMA_SEQ.fetch_add(1, Ordering::Relaxed),
        test
    );
    // Drop any leftover from a crashed prior run of the same pid+seq (unlikely
    // but cheap), then build fresh.
    let mut c = postgres::Client::connect(&url, postgres::NoTls).expect("connect");
    c.batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .expect("drop stale test schema");
    drop(c);
    let client = raw_client_in_schema(&url, &schema);
    let store = PgVerifierStore::from_client(client).expect("initialize schema on live PG");
    Some((url, schema, store))
}

#[test]
fn migrations_install_stamp_and_are_idempotent_on_live_pg() {
    let Some((url, schema, store)) = isolated_store("migrate") else {
        return;
    };
    // Fresh install stamped to the binary's target (the v2 step genuinely ran:
    // the console columns exist — proven by a full-field save below).
    assert_eq!(store.schema_version().unwrap(), PG_SCHEMA_VERSION);

    // Reopen on a second connection: idempotent (no step re-runs, stamp holds).
    let reopened = PgVerifierStore::from_client(raw_client_in_schema(&url, &schema))
        .expect("idempotent reopen");
    assert_eq!(reopened.schema_version().unwrap(), PG_SCHEMA_VERSION);

    // The v2 columns are live: a node with every optional field round-trips.
    let full = RegisteredNode {
        node_id: "orin-07".to_string(),
        status: NodeTrustState::Trusted,
        registered_at_ms: 1111,
        last_trust_update_ms: 2222,
        ak_public_pem: Some(
            "-----BEGIN PUBLIC KEY-----\nMCow...\n-----END PUBLIC KEY-----".to_string(),
        ),
        expected_pcr16_digest_hex: Some("ab".repeat(32)),
        site: Some("plant-3".to_string()),
        firmware_version: Some("2.4.1".to_string()),
    };
    reopened.save_node(&full).unwrap();
    let got = reopened
        .load_node("orin-07")
        .unwrap()
        .expect("saved node present");
    assert_eq!(got.node_id, full.node_id);
    assert_eq!(got.status, full.status);
    assert_eq!(got.registered_at_ms, full.registered_at_ms);
    assert_eq!(got.last_trust_update_ms, full.last_trust_update_ms);
    assert_eq!(got.ak_public_pem, full.ak_public_pem);
    assert_eq!(
        got.expected_pcr16_digest_hex,
        full.expected_pcr16_digest_hex
    );
    assert_eq!(got.site, full.site);
    assert_eq!(got.firmware_version, full.firmware_version);
}

#[test]
fn a_future_schema_stamp_is_refused_fail_closed_on_live_pg() {
    let Some((url, schema, store)) = isolated_store("future") else {
        return;
    };
    // Stamp the database NEWER than this binary supports…
    store.schema_version().unwrap();
    let mut c = raw_client_in_schema(&url, &schema);
    c.execute(
        "UPDATE kirra_schema_version SET version = $1 WHERE id = 1",
        &[&(PG_SCHEMA_VERSION + 7)],
    )
    .unwrap();
    // …and a reopen must REFUSE (the shared engine's downgrade guard), leaving
    // the stamp untouched.
    let err = PgVerifierStore::from_client(raw_client_in_schema(&url, &schema))
        .err()
        .expect("a future stamp must refuse to open");
    assert!(
        matches!(
            err,
            PgMigrationError::FutureSchema { db_version, target }
                if db_version == PG_SCHEMA_VERSION + 7 && target == PG_SCHEMA_VERSION
        ),
        "expected FutureSchema, got: {err:?}"
    );
    let row = c
        .query_one("SELECT version FROM kirra_schema_version WHERE id = 1", &[])
        .unwrap();
    assert_eq!(
        row.get::<_, i64>(0),
        PG_SCHEMA_VERSION + 7,
        "refusal must not downgrade the stamp"
    );
}

#[test]
fn live_pg_satisfies_the_epoch_fence_contract() {
    let Some((_, _, mut store)) = isolated_store("fence") else {
        return;
    };
    // The SAME suite SQLite and the in-memory model pass in the root crate.
    assert_fence_contract(&mut store);
}

#[test]
fn live_pg_satisfies_the_node_store_contract() {
    let Some((_, _, store)) = isolated_store("nodes") else {
        return;
    };
    // The SAME suite SQLite and the in-memory model pass in the root crate.
    assert_node_store_contract(&store);
}

#[test]
fn live_pg_satisfies_the_posture_engine_state_store_contract() {
    let Some((_, _, store)) = isolated_store("posturestate") else {
        return;
    };
    // The SAME suite SQLite and the in-memory model pass in the root crate —
    // including the fail-closed-on-corrupt-generation invariant (the Postgres
    // backend surfaces it as `PgStoreError::CorruptGeneration`).
    assert_posture_engine_state_store_contract(&store);
}

#[test]
fn live_pg_satisfies_the_federation_store_contract() {
    let Some((_, _, store)) = isolated_store("federation") else {
        return;
    };
    // The SAME suite SQLite and the in-memory model pass in the root crate: the
    // controller key registry, the single-use nonce burn / has-seen, and the
    // per-source strictly-advancing sequence gate — realized here with atomic
    // Postgres upserts (ON CONFLICT DO NOTHING / conditional DO UPDATE).
    assert_federation_store_contract(&store);
}

#[test]
fn live_pg_satisfies_the_operator_store_contract() {
    let Some((_, _, mut store)) = isolated_store("operators") else {
        return;
    };
    // The SAME suite SQLite + the in-memory model pass: register/rotate (clears
    // revocation), the conditional revoke (true only on an active→revoked
    // transition), and load/list. Takes `&mut` (register/revoke mutate).
    assert_operator_store_contract(&mut store);
}

#[test]
fn live_pg_satisfies_the_principal_store_contract() {
    let Some((_, _, mut store)) = isolated_store("principals") else {
        return;
    };
    // Register/rotate (clears revocation), conditional revoke, lookup by token hash,
    // and the `UNIQUE(token_sha256)` guarantee (a hash already held by a DIFFERENT
    // principal errors on the constraint) — realized here by the live PG UNIQUE index.
    assert_principal_store_contract(&mut store);
}

#[test]
fn live_pg_satisfies_the_cert_principal_store_contract() {
    let Some((_, _, mut store)) = isolated_store("certprincipals") else {
        return;
    };
    // Register (with optional X.509 expiry) / rotate / revoke, lookup by fingerprint,
    // the `UNIQUE(cert_sha256)` one-cert-one-principal guarantee, and the fail-closed
    // expiry (a `not_after_ms > i64::MAX` is refused, a corrupt negative reads as
    // expired-at-epoch) — held identical across all three backends.
    assert_cert_principal_store_contract(&mut store);
}

#[test]
fn live_pg_satisfies_the_fabric_asset_store_contract() {
    let Some((_, _, store)) = isolated_store("fabricassets") else {
        return;
    };
    // Upsert-by-id + ordered load, with the enum fields + metadata map JSON-round-
    // tripped through TEXT columns (same encoding + lenient decode as SQLite).
    assert_fabric_asset_store_contract(&store);
}

#[test]
fn live_pg_satisfies_the_ota_campaign_store_contract() {
    let Some((_, _, mut store)) = isolated_store("otacampaigns") else {
        return;
    };
    // Insert→load roundtrip, duplicate-id conflict, newest-first listing, the
    // active-only (Staged/Rolling) filter, and the node-adoption upsert's monotonic
    // + attested-per-digest invariants — the SAME contract SQLite + the in-memory
    // model run. Campaign cohorts/stages JSON-round-trip through TEXT; `attested`
    // round-trips through a native BOOLEAN; the v9 migration installs both tables.
    assert_ota_campaign_store_contract(&mut store);
}

#[test]
fn a_corrupt_status_json_loads_as_unknown_not_a_panic() {
    let Some((url, schema, store)) = isolated_store("corrupt") else {
        return;
    };
    let mut c = raw_client_in_schema(&url, &schema);
    c.execute(
        "INSERT INTO nodes (node_id, status_json, registered_at_ms, last_trust_update_ms) \
         VALUES ('mangled', 'not json at all', 1, 0)",
        &[],
    )
    .unwrap();
    // Identical fallback to the SQLite backend: undecodable → Unknown (fail
    // toward "not trusted"), never a panic or a skipped row.
    assert_eq!(
        store.load_node("mangled").unwrap().unwrap().status,
        NodeTrustState::Unknown
    );
    assert_eq!(store.load_nodes().unwrap().len(), 1);
}

#[test]
fn two_connections_racing_the_cas_produce_exactly_one_winner() {
    // The drill only a LIVE server can run honestly: two independent
    // connections (two OS-level sessions) observe epoch 0 and claim
    // concurrently — the row lock serializes them and exactly one sees
    // `rows_affected == 1`. The loser is then fenced by the FOR-UPDATE
    // assertion while the winner passes.
    let Some((url, schema, _store)) = isolated_store("race") else {
        return;
    };
    let mk = || {
        PgVerifierStore::from_client(raw_client_in_schema(&url, &schema))
            .expect("open racer connection")
    };
    let mut a = mk();
    let mut b = mk();

    let (ra, rb) = std::thread::scope(|s| {
        let ta = s.spawn(|| a.try_claim_epoch(0, "racer-A", 10).unwrap());
        let tb = s.spawn(|| b.try_claim_epoch(0, "racer-B", 11).unwrap());
        (ta.join().unwrap(), tb.join().unwrap())
    });
    assert!(
        (ra == Some(1)) ^ (rb == Some(1)),
        "exactly one racer must win the CAS: A={ra:?} B={rb:?}"
    );
    assert!(
        ra.is_none() || rb.is_none(),
        "the other must lose: A={ra:?} B={rb:?}"
    );

    // The winner holds the fence; the loser (never claimed → held 0) is fenced.
    let (mut winner, mut loser) = if ra == Some(1) { (a, b) } else { (b, a) };
    assert_eq!(winner.assert_actuator_epoch_held(1), Ok(()));
    assert_eq!(
        loser.assert_actuator_epoch_held(0),
        Err(FenceError::EpochSuperseded {
            held: 0,
            durable: 1
        })
    );

    // The loser now legitimately claims the NEXT epoch — and the old winner is
    // fenced in turn (the supersession chain the HA promotion path relies on).
    assert_eq!(
        loser
            .try_claim_epoch(1, "racer-loser-promotes", 20)
            .unwrap(),
        Some(2)
    );
    assert_eq!(
        winner.assert_actuator_epoch_held(1),
        Err(FenceError::EpochSuperseded {
            held: 1,
            durable: 2
        })
    );
}

#[test]
fn an_absent_ha_row_denies_the_fence_fail_closed() {
    let Some((url, schema, mut store)) = isolated_store("wedge") else {
        return;
    };
    store.try_claim_epoch(0, "A", 1).unwrap();
    // Wedge the singleton (the live analogue of the SQLite / in-memory drills).
    let mut c = raw_client_in_schema(&url, &schema);
    c.execute("DELETE FROM ha_state WHERE id = 1", &[]).unwrap();
    assert_eq!(
        store.assert_actuator_epoch_held(1),
        Err(FenceError::EpochUnreadable)
    );
    assert!(
        store.current_epoch().is_err(),
        "the read path is fail-closed too"
    );
}
