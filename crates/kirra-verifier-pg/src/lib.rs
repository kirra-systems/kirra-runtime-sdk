//! EP-10 (MGA G-9) — the **live Postgres backend** for the verifier storage seams.
//!
//! The root crate defines three backend seams and proves each against SQLite +
//! an in-memory model:
//!
//! - the schema-migration engine (`verifier_store::migrations::run_migrations_generic`)
//!   with its Postgres dialect half (`migrations_postgres::PostgresBackend` over
//!   the injected [`PgExecutor`] seam);
//! - the [`EpochFence`] HA write-ownership contract (durable CAS + fail-closed
//!   actuator fence);
//! - the [`NodeStore`] node-identity registry contract.
//!
//! This crate is the promised integrator binding: [`LivePgExecutor`] is the
//! "~10-line adapter" the seam documentation describes (execute →
//! `batch_execute`, version read → a `SELECT`, transaction →
//! `BEGIN`/`COMMIT`/`ROLLBACK`), and [`PgVerifierStore`] realizes BOTH storage
//! contracts over a real server:
//!
//! - the epoch CAS is the same `UPDATE … WHERE id = 1 AND epoch = $1`
//!   rows-affected compare-and-set as SQLite;
//! - the actuator fence is realized **transactionally with
//!   `SELECT … FOR UPDATE`** (the cross-backend constraint recorded on the
//!   trait): while the assertion transaction holds the row lock, a competing
//!   claim serializes behind it, so a superseded holder can never observe a
//!   stale epoch and slip a command out.
//!
//! The tests in `tests/live_pg.rs` run the **same conformance suites** the root
//! crate runs against SQLite (`assert_fence_contract`,
//! `assert_node_store_contract`) against a live server (CI lane
//! `postgres-conformance`, `services: postgres`), plus PG-only drills the other
//! backends cannot express (two-connection CAS race, `FOR UPDATE`
//! serialization). Fail-closed semantics are identical by construction and
//! proven identical by the shared suites.

use std::sync::Mutex;

use kirra_verifier::verifier::{NodeTrustState, RegisteredNode};
use kirra_verifier::verifier_store::migrations_postgres::{
    PgExecutor, PgMigration, PgMigrationError, PostgresBackend,
};
use kirra_verifier::verifier_store::{EpochFence, FenceError, NodeStore};

/// The Postgres schema version THIS binary supports (mirrors the SQLite
/// `SCHEMA_VERSION` discipline: a newer stamp in the database is refused
/// fail-closed by the shared engine).
pub const PG_SCHEMA_VERSION: i64 = 2;

/// Baseline (v1) DDL — idempotent, applied on every open BEFORE the versioned
/// steps run (v1 is the engine's baseline; steps are v ≥ 2). The two tables
/// mirror the SQLite shapes exactly: the `ha_state` singleton (the epoch fence)
/// and the `nodes` identity registry at its pre-console (v1) shape.
const BASELINE_DDL: &str = "
CREATE TABLE IF NOT EXISTS ha_state (
    id                 INTEGER PRIMARY KEY,
    epoch              BIGINT NOT NULL,
    active_instance_id TEXT,
    updated_at_ms      BIGINT NOT NULL
);
CREATE TABLE IF NOT EXISTS nodes (
    node_id                   TEXT PRIMARY KEY,
    status_json               TEXT NOT NULL,
    registered_at_ms          BIGINT NOT NULL,
    last_trust_update_ms      BIGINT NOT NULL,
    ak_public_pem             TEXT,
    expected_pcr16_digest_hex TEXT
);
INSERT INTO ha_state (id, epoch, active_instance_id, updated_at_ms)
    VALUES (1, 0, NULL, 0) ON CONFLICT (id) DO NOTHING;
";

/// The versioned migration registry. v2 mirrors the SQLite history where the
/// console rollup columns were added after the initial registry shape — a REAL
/// transactional step (DDL + version stamp commit together), so the live-PG
/// conformance run exercises the engine's `apply_and_stamp` path, not just the
/// baseline stamp.
const PG_MIGRATIONS: &[PgMigration] = &[PgMigration {
    version: 2,
    sql: "ALTER TABLE nodes ADD COLUMN site TEXT; \
          ALTER TABLE nodes ADD COLUMN firmware_version TEXT",
}];

/// The "~10-line adapter" binding the root crate's [`PgExecutor`] seam to the
/// sync `postgres` client, exactly as the seam documentation promises:
/// `execute` → [`postgres::Client::batch_execute`], `query_version` → a
/// `SELECT` (0 when the row is absent), `transaction` →
/// `BEGIN`/`COMMIT`/`ROLLBACK` on the same connection (which is what makes a
/// migration step's DDL + version stamp atomic).
pub struct LivePgExecutor<'a> {
    client: &'a mut postgres::Client,
}

impl<'a> LivePgExecutor<'a> {
    /// Wrap a connected client.
    pub fn new(client: &'a mut postgres::Client) -> Self {
        Self { client }
    }
}

impl PgExecutor for LivePgExecutor<'_> {
    type Error = postgres::Error;

    fn execute(&mut self, sql: &str) -> Result<(), postgres::Error> {
        self.client.batch_execute(sql)
    }

    fn query_version(&mut self) -> Result<i64, postgres::Error> {
        // `read_version` ensures the table exists before calling this; the ROW
        // may still be absent on a fresh store → version 0.
        let rows = self
            .client
            .query("SELECT version FROM kirra_schema_version WHERE id = 1", &[])?;
        Ok(rows.first().map(|r| r.get::<_, i64>(0)).unwrap_or(0))
    }

    fn transaction<R>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<R, postgres::Error>,
    ) -> Result<R, postgres::Error> {
        self.client.batch_execute("BEGIN")?;
        match f(self) {
            Ok(r) => {
                self.client.batch_execute("COMMIT")?;
                Ok(r)
            }
            Err(e) => {
                // Best-effort ROLLBACK; the original error is the verdict. A
                // failed ROLLBACK leaves the connection in an aborted
                // transaction that the next statement surfaces loudly.
                let _ = self.client.batch_execute("ROLLBACK");
                Err(e)
            }
        }
    }
}

/// Why a [`PgVerifierStore`] operation failed. Wraps the driver error and the
/// one non-driver failure mode ([`NodeStore::save_node`]'s status encoding —
/// the SQLite backend maps the same case to `rusqlite::Error::InvalidQuery`).
#[derive(Debug)]
pub enum PgStoreError {
    /// The underlying `postgres` driver failed.
    Pg(postgres::Error),
    /// `NodeTrustState` could not be encoded to `status_json`.
    Encode(serde_json::Error),
}

impl std::fmt::Display for PgStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PgStoreError::Pg(e) => write!(f, "postgres error: {e}"),
            PgStoreError::Encode(e) => write!(f, "status_json encode error: {e}"),
        }
    }
}

impl std::error::Error for PgStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PgStoreError::Pg(e) => Some(e),
            PgStoreError::Encode(e) => Some(e),
        }
    }
}

impl From<postgres::Error> for PgStoreError {
    fn from(e: postgres::Error) -> Self {
        PgStoreError::Pg(e)
    }
}

/// The live Postgres verifier store: one connection realizing BOTH storage
/// contracts ([`EpochFence`], [`NodeStore`]) over the schema installed by
/// [`Self::connect`] / [`Self::initialize`].
///
/// Interior mutability mirrors the in-memory reference backend: the traits'
/// `&self` methods lock a `Mutex<postgres::Client>` and RECOVER from poisoning
/// (`unwrap_or_else(PoisonError::into_inner)`) — a panic elsewhere while
/// holding the lock can never make a store op panic, and the connection carries
/// no cross-call invariant a torn statement could break (every mutation here is
/// a single statement or an explicit transaction).
pub struct PgVerifierStore {
    client: Mutex<postgres::Client>,
}

impl PgVerifierStore {
    /// Connect to `url` (e.g. `postgres://user:pass@host:5432/db`), install /
    /// migrate the schema fail-closed, and seed the `ha_state` genesis row.
    pub fn connect(url: &str) -> Result<Self, PgMigrationError<postgres::Error>> {
        let client = postgres::Client::connect(url, postgres::NoTls)
            .map_err(PgMigrationError::Executor)?;
        Self::from_client(client)
    }

    /// Wrap an already-connected client (tests use this after pinning a
    /// per-test schema via `SET search_path`), installing / migrating the
    /// schema exactly as [`Self::connect`] does.
    pub fn from_client(
        mut client: postgres::Client,
    ) -> Result<Self, PgMigrationError<postgres::Error>> {
        Self::initialize(&mut client)?;
        Ok(Self { client: Mutex::new(client) })
    }

    /// Idempotent schema install + versioned migration, fail-closed: a database
    /// stamped NEWER than [`PG_SCHEMA_VERSION`] is refused (the shared engine's
    /// downgrade guard), and a failing step rolls back with its stamp.
    pub fn initialize(
        client: &mut postgres::Client,
    ) -> Result<(), PgMigrationError<postgres::Error>> {
        client.batch_execute(BASELINE_DDL).map_err(PgMigrationError::Executor)?;
        let mut backend = PostgresBackend::new(LivePgExecutor::new(client));
        backend.migrate(PG_MIGRATIONS, PG_SCHEMA_VERSION)?;
        Ok(())
    }

    /// The stamped schema version (the `kirra_schema_version` singleton).
    pub fn schema_version(&self) -> Result<i64, PgStoreError> {
        let mut c = self.lock();
        let rows = c.query("SELECT version FROM kirra_schema_version WHERE id = 1", &[])?;
        Ok(rows.first().map(|r| r.get::<_, i64>(0)).unwrap_or(0))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, postgres::Client> {
        self.client.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn row_to_node(row: &postgres::Row) -> RegisteredNode {
        let status_json: String = row.get(1);
        // Same corrupt-status fallback as the SQLite backend: an undecodable
        // status is Unknown (fail toward "not trusted"), never a panic.
        let status: NodeTrustState =
            serde_json::from_str(&status_json).unwrap_or(NodeTrustState::Unknown);
        RegisteredNode {
            node_id: row.get(0),
            status,
            registered_at_ms: row.get::<_, i64>(2) as u64,
            last_trust_update_ms: row.get::<_, i64>(3) as u64,
            ak_public_pem: row.get(4),
            expected_pcr16_digest_hex: row.get(5),
            site: row.get(6),
            firmware_version: row.get(7),
        }
    }
}

const NODE_COLUMNS: &str = "node_id, status_json, registered_at_ms, last_trust_update_ms, \
                            ak_public_pem, expected_pcr16_digest_hex, site, firmware_version";

impl NodeStore for PgVerifierStore {
    type Error = PgStoreError;

    fn save_node(&self, node: &RegisteredNode) -> Result<(), PgStoreError> {
        let status_json = serde_json::to_string(&node.status).map_err(PgStoreError::Encode)?;
        self.lock().execute(
            "INSERT INTO nodes (node_id, status_json, registered_at_ms, last_trust_update_ms, \
                                ak_public_pem, expected_pcr16_digest_hex, site, firmware_version) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (node_id) DO UPDATE SET \
                 status_json = EXCLUDED.status_json, \
                 registered_at_ms = EXCLUDED.registered_at_ms, \
                 last_trust_update_ms = EXCLUDED.last_trust_update_ms, \
                 ak_public_pem = EXCLUDED.ak_public_pem, \
                 expected_pcr16_digest_hex = EXCLUDED.expected_pcr16_digest_hex, \
                 site = EXCLUDED.site, \
                 firmware_version = EXCLUDED.firmware_version",
            &[
                &node.node_id,
                &status_json,
                &(node.registered_at_ms as i64),
                &(node.last_trust_update_ms as i64),
                &node.ak_public_pem,
                &node.expected_pcr16_digest_hex,
                &node.site,
                &node.firmware_version,
            ],
        )?;
        Ok(())
    }

    fn load_node(&self, node_id: &str) -> Result<Option<RegisteredNode>, PgStoreError> {
        let row = self.lock().query_opt(
            &format!("SELECT {NODE_COLUMNS} FROM nodes WHERE node_id = $1"),
            &[&node_id],
        )?;
        Ok(row.as_ref().map(Self::row_to_node))
    }

    fn load_nodes(&self) -> Result<Vec<RegisteredNode>, PgStoreError> {
        let rows = self
            .lock()
            .query(&format!("SELECT {NODE_COLUMNS} FROM nodes"), &[])?;
        Ok(rows.iter().map(Self::row_to_node).collect())
    }

    fn node_exists(&self, node_id: &str) -> Result<bool, PgStoreError> {
        let row = self.lock().query_one(
            "SELECT COUNT(*) FROM nodes WHERE node_id = $1",
            &[&node_id],
        )?;
        Ok(row.get::<_, i64>(0) > 0)
    }

    fn count_nodes(&self) -> Result<i64, PgStoreError> {
        let row = self.lock().query_one("SELECT COUNT(*) FROM nodes", &[])?;
        Ok(row.get::<_, i64>(0))
    }
}

impl EpochFence for PgVerifierStore {
    type Error = PgStoreError;

    fn current_epoch(&self) -> Result<u64, PgStoreError> {
        // `query_one` errors when the singleton row is absent — the read path
        // is fail-closed exactly like the SQLite backend's `query_row`.
        let row = self
            .lock()
            .query_one("SELECT epoch FROM ha_state WHERE id = 1", &[])?;
        Ok(row.get::<_, i64>(0) as u64)
    }

    fn current_active_holder(&self) -> Result<(u64, Option<String>), PgStoreError> {
        let row = self.lock().query_one(
            "SELECT epoch, active_instance_id FROM ha_state WHERE id = 1",
            &[],
        )?;
        Ok((row.get::<_, i64>(0) as u64, row.get(1)))
    }

    fn try_claim_epoch(
        &mut self,
        observed: u64,
        instance_id: &str,
        now_ms: u64,
    ) -> Result<Option<u64>, PgStoreError> {
        // The SAME rows-affected compare-and-set as the SQLite backend: two
        // racers reading the same `observed` serialize at the row lock and
        // exactly one sees `rows == 1`.
        let n = self.lock().execute(
            "UPDATE ha_state SET epoch = epoch + 1, active_instance_id = $2, updated_at_ms = $3 \
             WHERE id = 1 AND epoch = $1",
            &[&(observed as i64), &instance_id, &(now_ms as i64)],
        )?;
        Ok(if n == 1 { Some(observed + 1) } else { None })
    }

    fn assert_actuator_epoch_held(&mut self, held_epoch: u64) -> Result<(), FenceError> {
        // The cross-backend constraint recorded on the trait, realized the
        // Postgres way: a transaction whose `SELECT … FOR UPDATE` takes the
        // `ha_state` row lock. While this assertion transaction is open a
        // competing `try_claim_epoch` serializes behind it; if a competing
        // claim already landed, the locked read observes the newer epoch and
        // the fence rejects before any actuator response is issued. Fail
        // closed on EVERY failure: transaction/read errors → `EpochUnreadable`;
        // `held == 0` or any mismatch → `EpochSuperseded`. The transaction
        // rolls back on drop, so the reject path never commits anything.
        let mut guard = self.lock();
        let mut tx = guard.transaction().map_err(|_| FenceError::EpochUnreadable)?;
        let row = tx
            .query_opt("SELECT epoch FROM ha_state WHERE id = 1 FOR UPDATE", &[])
            .map_err(|_| FenceError::EpochUnreadable)?;
        let durable = match row {
            Some(r) => r.get::<_, i64>(0) as u64,
            // Singleton row absent — never authorize blind.
            None => return Err(FenceError::EpochUnreadable),
        };
        if held_epoch == 0 || durable != held_epoch {
            return Err(FenceError::EpochSuperseded { held: held_epoch, durable });
        }
        tx.commit().map_err(|_| FenceError::EpochUnreadable)
    }
}
