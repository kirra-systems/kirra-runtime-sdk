//! EP-10 (MGA G-9) — the **live Postgres backend** for the verifier storage seams.
//!
//! The root crate defines eight backend seams and proves each against SQLite +
//! an in-memory model:
//!
//! - the schema-migration engine (`verifier_store::migrations::run_migrations_generic`)
//!   with its Postgres dialect half (`migrations_postgres::PostgresBackend` over
//!   the injected [`PgExecutor`] seam);
//! - the [`EpochFence`] HA write-ownership contract (durable CAS + fail-closed
//!   actuator fence);
//! - the [`NodeStore`] node-identity registry contract;
//! - the [`PostureEngineStateStore`] contract (the `posture_engine_state` KV store
//!   + the monotonic generation high-water, fail-closed on a corrupt value);
//! - the [`FederationStore`] contract (the trusted-controller key registry + the
//!   durable anti-replay primitives: single-use nonce burn + the per-source
//!   strictly-advancing sequence gate);
//! - the [`OperatorStore`] contract (the per-operator Ed25519 identity registry +
//!   revocation);
//! - the [`PrincipalStore`] contract (the API-principal registry — scoped bearer
//!   tokens stored as their SHA-256, `UNIQUE(token)` one-token-one-principal);
//! - the [`CertPrincipalStore`] contract (the mTLS cert-principal registry — a
//!   fingerprint-pinned client cert + optional X.509 expiry, fail-closed).
//!
//! This crate is the promised integrator binding: [`LivePgExecutor`] is the
//! "~10-line adapter" the seam documentation describes (execute →
//! `batch_execute`, version read → a `SELECT`, transaction →
//! `BEGIN`/`COMMIT`/`ROLLBACK`), and [`PgVerifierStore`] realizes those storage
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
use kirra_verifier::verifier_store::{
    ApiPrincipalRecord, CertPrincipalRecord, CertPrincipalStore, EpochFence, FederationStore,
    FenceError, NodeStore, OperatorRecord, OperatorStore, PostureEngineStateStore, PrincipalStore,
};

/// The Postgres schema version THIS binary supports (mirrors the SQLite
/// `SCHEMA_VERSION` discipline: a newer stamp in the database is refused
/// fail-closed by the shared engine).
pub const PG_SCHEMA_VERSION: i64 = 7;

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
const PG_MIGRATIONS: &[PgMigration] = &[
    PgMigration {
        version: 2,
        sql: "ALTER TABLE nodes ADD COLUMN site TEXT; \
              ALTER TABLE nodes ADD COLUMN firmware_version TEXT",
    },
    // v3 — the `posture_engine_state` KV table (the PostureEngineStateStore seam).
    // A REAL versioned step (not a baseline addition), so the stamp reflects when
    // the table entered the contract and the engine's `apply_and_stamp` path runs.
    PgMigration {
        version: 3,
        sql: "CREATE TABLE IF NOT EXISTS posture_engine_state ( \
                  key   TEXT PRIMARY KEY, \
                  value TEXT NOT NULL \
              )",
    },
    // v4 — the FederationStore seam's tables: the trusted-controller key registry
    // and the two durable anti-replay primitives (the nonce set + the per-source
    // monotonic sequence gate). Mirrors the SQLite shapes.
    PgMigration {
        version: 4,
        sql: "CREATE TABLE IF NOT EXISTS trusted_federation_controllers ( \
                  controller_id    TEXT PRIMARY KEY, \
                  public_key_b64   TEXT NOT NULL, \
                  registered_at_ms BIGINT NOT NULL \
              ); \
              CREATE TABLE IF NOT EXISTS federation_report_nonces ( \
                  nonce_hex            TEXT PRIMARY KEY, \
                  source_controller_id TEXT NOT NULL, \
                  seen_at_ms           BIGINT NOT NULL \
              ); \
              CREATE TABLE IF NOT EXISTS industrial_message_seq ( \
                  source_id     TEXT PRIMARY KEY, \
                  last_sequence BIGINT NOT NULL, \
                  last_seen_ms  BIGINT NOT NULL \
              )",
    },
    // v5 — the operator registry (the OperatorStore seam): per-operator Ed25519
    // identity + revocation. `revoked_at_ms` NULL = active.
    PgMigration {
        version: 5,
        sql: "CREATE TABLE IF NOT EXISTS operators ( \
                  operator_id      TEXT PRIMARY KEY, \
                  pubkey_pem       TEXT NOT NULL, \
                  registered_at_ms BIGINT NOT NULL, \
                  revoked_at_ms    BIGINT \
              )",
    },
    // v6 — the API-principal registry (the PrincipalStore seam): per-principal
    // scoped bearer tokens, stored ONLY as the SHA-256 hex. `UNIQUE(token_sha256)` —
    // one token authorizes at most one principal (registering a hash already held by
    // a DIFFERENT principal errors on the constraint). `revoked_at_ms` NULL = active.
    PgMigration {
        version: 6,
        sql: "CREATE TABLE IF NOT EXISTS api_principals ( \
                  principal_id  TEXT PRIMARY KEY, \
                  token_sha256  TEXT NOT NULL UNIQUE, \
                  role          TEXT NOT NULL, \
                  created_at_ms BIGINT NOT NULL, \
                  revoked_at_ms BIGINT \
              )",
    },
    // v7 — the mTLS cert-principal registry (the CertPrincipalStore seam): a
    // CA-verified client cert pinned by its SHA-256 leaf fingerprint, with an
    // optional X.509 notAfter (`not_after_ms`). `UNIQUE(cert_sha256)` — one cert
    // pins at most one principal. `revoked_at_ms` NULL = active.
    PgMigration {
        version: 7,
        sql: "CREATE TABLE IF NOT EXISTS cert_principals ( \
                  principal_id  TEXT PRIMARY KEY, \
                  cert_sha256   TEXT NOT NULL UNIQUE, \
                  role          TEXT NOT NULL, \
                  created_at_ms BIGINT NOT NULL, \
                  revoked_at_ms BIGINT, \
                  not_after_ms  BIGINT \
              )",
    },
];

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
    /// A stored `last_generation` value is non-numeric or out-of-domain
    /// (`>= i64::MAX`) — CORRUPTION, surfaced fail-closed exactly as the SQLite /
    /// in-memory backends' `load_last_generation` does (never silently read as 0,
    /// which would reintroduce restart generation time-reversal).
    CorruptGeneration(String),
    /// A `u64` input (a timestamp or a replay sequence) exceeds the Postgres
    /// `BIGINT` (i64) domain. Fail-closed: the store REFUSES rather than wrapping
    /// to a negative value that would corrupt ordering or defeat the
    /// strictly-advancing sequence gate (a wrapped-negative high-water would let a
    /// later smaller sequence appear "greater"). Bounded in practice — a ms epoch
    /// timestamp overflows i64 only past year 292M — but refused, never wrapped.
    OutOfDomain { field: &'static str, value: u64 },
}

impl std::fmt::Display for PgStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PgStoreError::Pg(e) => write!(f, "postgres error: {e}"),
            PgStoreError::Encode(e) => write!(f, "status_json encode error: {e}"),
            PgStoreError::CorruptGeneration(v) => {
                write!(f, "corrupt last_generation value: {v:?}")
            }
            PgStoreError::OutOfDomain { field, value } => {
                write!(f, "{field} value {value} exceeds the BIGINT (i64) domain")
            }
        }
    }
}

impl std::error::Error for PgStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PgStoreError::Pg(e) => Some(e),
            PgStoreError::Encode(e) => Some(e),
            PgStoreError::CorruptGeneration(_) => None,
            PgStoreError::OutOfDomain { .. } => None,
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
        let client =
            postgres::Client::connect(url, postgres::NoTls).map_err(PgMigrationError::Executor)?;
        Self::from_client(client)
    }

    /// Wrap an already-connected client (tests use this after pinning a
    /// per-test schema via `SET search_path`), installing / migrating the
    /// schema exactly as [`Self::connect`] does.
    pub fn from_client(
        mut client: postgres::Client,
    ) -> Result<Self, PgMigrationError<postgres::Error>> {
        Self::initialize(&mut client)?;
        Ok(Self {
            client: Mutex::new(client),
        })
    }

    /// Idempotent schema install + versioned migration, fail-closed: a database
    /// stamped NEWER than [`PG_SCHEMA_VERSION`] is refused (the shared engine's
    /// downgrade guard), and a failing step rolls back with its stamp.
    pub fn initialize(
        client: &mut postgres::Client,
    ) -> Result<(), PgMigrationError<postgres::Error>> {
        client
            .batch_execute(BASELINE_DDL)
            .map_err(PgMigrationError::Executor)?;
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
        let row = self
            .lock()
            .query_one("SELECT COUNT(*) FROM nodes WHERE node_id = $1", &[&node_id])?;
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
        let mut tx = guard
            .transaction()
            .map_err(|_| FenceError::EpochUnreadable)?;
        let row = tx
            .query_opt("SELECT epoch FROM ha_state WHERE id = 1 FOR UPDATE", &[])
            .map_err(|_| FenceError::EpochUnreadable)?;
        let durable = match row {
            Some(r) => r.get::<_, i64>(0) as u64,
            // Singleton row absent — never authorize blind.
            None => return Err(FenceError::EpochUnreadable),
        };
        if held_epoch == 0 || durable != held_epoch {
            return Err(FenceError::EpochSuperseded {
                held: held_epoch,
                durable,
            });
        }
        tx.commit().map_err(|_| FenceError::EpochUnreadable)
    }
}

impl PostureEngineStateStore for PgVerifierStore {
    type Error = PgStoreError;

    fn load_last_generation(&self) -> Result<u64, PgStoreError> {
        let row = self.lock().query_opt(
            "SELECT value FROM posture_engine_state WHERE key = 'last_generation'",
            &[],
        )?;
        match row {
            // Genuinely absent row → 0 (fresh store), like SQLite's
            // `QueryReturnedNoRows => Ok(0)`.
            None => Ok(0),
            // Present but non-numeric OR out-of-domain (`>= i64::MAX`) → CORRUPTION,
            // fail closed. Same verdict as `VerifierStore::load_last_generation`.
            Some(r) => {
                let s: String = r.get(0);
                let parsed = s
                    .parse::<u64>()
                    .map_err(|_| PgStoreError::CorruptGeneration(s.clone()))?;
                if parsed >= i64::MAX as u64 {
                    return Err(PgStoreError::CorruptGeneration(s));
                }
                Ok(parsed)
            }
        }
    }

    fn save_last_generation(&self, generation: u64) -> Result<bool, PgStoreError> {
        // Single-statement conditional upsert — ATOMIC, so it is race-safe across
        // connections exactly like the SQLite backend's monotonic upsert: there is no
        // read-then-write window in which two racers could both read the old value and
        // a lower generation clobber a higher one. The `WHERE` gates only the DO UPDATE
        // (conflict) path; a first insert is unconditional. `rows == 1` on insert or an
        // accepted strict advance, `0` when the guard rejects a stale/equal generation.
        //
        // The `CASE` guards the cast of the STORED value: a non-numeric existing value
        // counts as 0, so a positive generation OVERWRITES it — the exact
        // heal-on-corrupt-save parity SQLite gives via `CAST('garbage') = 0` (and the
        // in-memory backend via `parse().unwrap_or(0)`). Only `load_last_generation`
        // fails closed on a corrupt high-water; `save` heals, matching every backend.
        //
        // The comparison is `::numeric` (arbitrary precision), NOT `::bigint`: a stored
        // value that is all-digits but out of the i64/u64 domain (a huge digit string
        // written via a blind `save_engine_state`) would OVERFLOW a `::bigint` cast and
        // re-raise the very crash this heals. `::numeric` never overflows — such a value
        // simply compares greater than any real generation, so the monotonic guard keeps
        // it (never a crash). `EXCLUDED.value` is `$1` (a valid u64 string), likewise
        // overflow-free as `numeric`.
        let n = self.lock().execute(
            "INSERT INTO posture_engine_state (key, value) VALUES ('last_generation', $1) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value \
             WHERE (CASE WHEN posture_engine_state.value ~ '^[0-9]+$' \
                         THEN posture_engine_state.value::numeric ELSE 0 END) \
                   < EXCLUDED.value::numeric",
            &[&generation.to_string()],
        )?;
        Ok(n == 1)
    }

    fn load_engine_state(&self, key: &str) -> Result<Option<String>, PgStoreError> {
        let row = self.lock().query_opt(
            "SELECT value FROM posture_engine_state WHERE key = $1",
            &[&key],
        )?;
        Ok(row.map(|r| r.get::<_, String>(0)))
    }

    fn save_engine_state(&self, key: &str, value: &str) -> Result<(), PgStoreError> {
        // Blind upsert (INSERT-OR-REPLACE) — NOT the monotonic guard.
        self.lock().execute(
            "INSERT INTO posture_engine_state (key, value) VALUES ($1, $2) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            &[&key, &value],
        )?;
        Ok(())
    }
}

impl FederationStore for PgVerifierStore {
    type Error = PgStoreError;

    fn save_trusted_federation_controller(
        &self,
        controller_id: &str,
        public_key_b64: &str,
        registered_at_ms: u64,
    ) -> Result<(), PgStoreError> {
        // Fail-closed on an out-of-domain timestamp rather than wrapping a `u64` to a
        // negative `BIGINT` (bounded in practice — overflows i64 only past year 292M).
        let reg_ms = i64::try_from(registered_at_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "registered_at_ms",
            value: registered_at_ms,
        })?;
        // Upsert by controller_id (SQLite: INSERT OR REPLACE) — re-registering a
        // controller overwrites its key.
        self.lock().execute(
            "INSERT INTO trusted_federation_controllers \
                 (controller_id, public_key_b64, registered_at_ms) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (controller_id) DO UPDATE SET \
                 public_key_b64 = EXCLUDED.public_key_b64, \
                 registered_at_ms = EXCLUDED.registered_at_ms",
            &[&controller_id, &public_key_b64, &reg_ms],
        )?;
        Ok(())
    }

    fn load_trusted_federation_controller_key(
        &self,
        controller_id: &str,
    ) -> Result<Option<String>, PgStoreError> {
        let row = self.lock().query_opt(
            "SELECT public_key_b64 FROM trusted_federation_controllers WHERE controller_id = $1",
            &[&controller_id],
        )?;
        Ok(row.map(|r| r.get::<_, String>(0)))
    }

    fn has_seen_federation_nonce(&self, nonce_hex: &str) -> Result<bool, PgStoreError> {
        let row = self.lock().query_one(
            "SELECT COUNT(*) FROM federation_report_nonces WHERE nonce_hex = $1",
            &[&nonce_hex],
        )?;
        Ok(row.get::<_, i64>(0) > 0)
    }

    fn burn_federation_nonce(&self, nonce_hex: &str) -> Result<bool, PgStoreError> {
        // Atomic single-use claim: `ON CONFLICT DO NOTHING` is the Postgres
        // `INSERT OR IGNORE` — `rows == 1` iff the nonce was newly recorded (first
        // use → proceed), `0` on a replay (already present → reject). No
        // check-then-act window; the PK conflict decides. `seen_at_ms` is diagnostic
        // only (correctness rests on the PK, never the clock), and the source label
        // mirrors the SQLite burn path.
        let seen_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let n = self.lock().execute(
            "INSERT INTO federation_report_nonces (nonce_hex, source_controller_id, seen_at_ms) \
             VALUES ($1, 'fleet-grant-lane', $2) \
             ON CONFLICT (nonce_hex) DO NOTHING",
            &[&nonce_hex, &seen_at_ms],
        )?;
        Ok(n == 1)
    }

    fn industrial_seq_check_and_advance(
        &self,
        source_id: &str,
        sequence: u64,
        now_ms: u64,
    ) -> Result<bool, PgStoreError> {
        // Fail-closed on an out-of-domain sequence/timestamp: a `u64` wrapped to a
        // negative `BIGINT` would DEFEAT the gate (a wrapped-negative high-water lets a
        // later smaller sequence compare "greater"), so refuse rather than wrap.
        let seq = i64::try_from(sequence).map_err(|_| PgStoreError::OutOfDomain {
            field: "sequence",
            value: sequence,
        })?;
        let now = i64::try_from(now_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "now_ms",
            value: now_ms,
        })?;
        // Atomic per-source strictly-advancing gate — the SAME conditional compare-
        // and-set as the SQLite backend: a first message from a new source inserts
        // (baseline accept), an existing source advances ONLY on a strictly greater
        // sequence (the `WHERE` gates the DO UPDATE), and a replay/regress no-ops.
        // `rows == 1` on accept, `0` on reject. Race-safe at the row lock.
        let n = self.lock().execute(
            "INSERT INTO industrial_message_seq (source_id, last_sequence, last_seen_ms) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (source_id) DO UPDATE SET \
                 last_sequence = EXCLUDED.last_sequence, \
                 last_seen_ms = EXCLUDED.last_seen_ms \
             WHERE EXCLUDED.last_sequence > industrial_message_seq.last_sequence",
            &[&source_id, &seq, &now],
        )?;
        Ok(n == 1)
    }
}

impl PgVerifierStore {
    fn row_to_operator(row: &postgres::Row) -> OperatorRecord {
        OperatorRecord {
            operator_id: row.get(0),
            pubkey_pem: row.get(1),
            registered_at_ms: row.get::<_, i64>(2) as u64,
            revoked_at_ms: row.get::<_, Option<i64>>(3).map(|v| v as u64),
        }
    }
}

impl OperatorStore for PgVerifierStore {
    type Error = PgStoreError;

    fn register_operator(
        &mut self,
        operator_id: &str,
        pubkey_pem: &str,
        now_ms: u64,
    ) -> Result<(), PgStoreError> {
        // Fail-closed on an out-of-domain timestamp (see FederationStore).
        let reg_ms = i64::try_from(now_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "now_ms",
            value: now_ms,
        })?;
        // Register or rotate: overwrite the key and CLEAR any prior revocation (a
        // fresh key reactivates), matching the SQLite upsert.
        self.lock().execute(
            "INSERT INTO operators (operator_id, pubkey_pem, registered_at_ms, revoked_at_ms) \
             VALUES ($1, $2, $3, NULL) \
             ON CONFLICT (operator_id) DO UPDATE SET \
                 pubkey_pem = EXCLUDED.pubkey_pem, \
                 registered_at_ms = EXCLUDED.registered_at_ms, \
                 revoked_at_ms = NULL",
            &[&operator_id, &pubkey_pem, &reg_ms],
        )?;
        Ok(())
    }

    fn revoke_operator(&mut self, operator_id: &str, now_ms: u64) -> Result<bool, PgStoreError> {
        let rev_ms = i64::try_from(now_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "now_ms",
            value: now_ms,
        })?;
        // Conditional update — `rows == 1` iff an ACTIVE operator transitioned to
        // revoked; `0` if absent or already revoked.
        let n = self.lock().execute(
            "UPDATE operators SET revoked_at_ms = $2 \
             WHERE operator_id = $1 AND revoked_at_ms IS NULL",
            &[&operator_id, &rev_ms],
        )?;
        Ok(n > 0)
    }

    fn load_operator(&self, operator_id: &str) -> Result<Option<OperatorRecord>, PgStoreError> {
        let row = self.lock().query_opt(
            "SELECT operator_id, pubkey_pem, registered_at_ms, revoked_at_ms \
             FROM operators WHERE operator_id = $1",
            &[&operator_id],
        )?;
        Ok(row.as_ref().map(Self::row_to_operator))
    }

    fn load_operators(&self) -> Result<Vec<OperatorRecord>, PgStoreError> {
        let rows = self.lock().query(
            "SELECT operator_id, pubkey_pem, registered_at_ms, revoked_at_ms \
             FROM operators ORDER BY operator_id",
            &[],
        )?;
        Ok(rows.iter().map(Self::row_to_operator).collect())
    }
}

impl PgVerifierStore {
    fn row_to_api_principal(row: &postgres::Row) -> ApiPrincipalRecord {
        ApiPrincipalRecord {
            principal_id: row.get(0),
            role: row.get(1),
            created_at_ms: row.get::<_, i64>(2) as u64,
            revoked_at_ms: row.get::<_, Option<i64>>(3).map(|v| v as u64),
        }
    }
}

impl PrincipalStore for PgVerifierStore {
    type Error = PgStoreError;

    fn register_api_principal(
        &mut self,
        principal_id: &str,
        token_sha256: &str,
        role: &str,
        now_ms: u64,
    ) -> Result<(), PgStoreError> {
        let created_ms = i64::try_from(now_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "now_ms",
            value: now_ms,
        })?;
        // Register / rotate: overwrite token hash + role and CLEAR revocation. A
        // `token_sha256` already held by a DIFFERENT principal violates the UNIQUE
        // constraint (only the principal_id conflict is handled by ON CONFLICT), so
        // it surfaces as a driver error — the fail-closed "one token, one principal"
        // guarantee, exactly like the SQLite backend.
        self.lock().execute(
            "INSERT INTO api_principals \
                 (principal_id, token_sha256, role, created_at_ms, revoked_at_ms) \
             VALUES ($1, $2, $3, $4, NULL) \
             ON CONFLICT (principal_id) DO UPDATE SET \
                 token_sha256  = EXCLUDED.token_sha256, \
                 role          = EXCLUDED.role, \
                 created_at_ms = EXCLUDED.created_at_ms, \
                 revoked_at_ms = NULL",
            &[&principal_id, &token_sha256, &role, &created_ms],
        )?;
        Ok(())
    }

    fn revoke_api_principal(
        &mut self,
        principal_id: &str,
        now_ms: u64,
    ) -> Result<bool, PgStoreError> {
        let rev_ms = i64::try_from(now_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "now_ms",
            value: now_ms,
        })?;
        let n = self.lock().execute(
            "UPDATE api_principals SET revoked_at_ms = $2 \
             WHERE principal_id = $1 AND revoked_at_ms IS NULL",
            &[&principal_id, &rev_ms],
        )?;
        Ok(n > 0)
    }

    fn load_api_principal_by_token_hash(
        &self,
        token_sha256: &str,
    ) -> Result<Option<ApiPrincipalRecord>, PgStoreError> {
        // Lookup by hash only; the record never carries the token back.
        let row = self.lock().query_opt(
            "SELECT principal_id, role, created_at_ms, revoked_at_ms \
             FROM api_principals WHERE token_sha256 = $1",
            &[&token_sha256],
        )?;
        Ok(row.as_ref().map(Self::row_to_api_principal))
    }

    fn load_api_principals(&self) -> Result<Vec<ApiPrincipalRecord>, PgStoreError> {
        let rows = self.lock().query(
            "SELECT principal_id, role, created_at_ms, revoked_at_ms \
             FROM api_principals ORDER BY principal_id",
            &[],
        )?;
        Ok(rows.iter().map(Self::row_to_api_principal).collect())
    }
}

impl PgVerifierStore {
    fn row_to_cert_principal(row: &postgres::Row) -> CertPrincipalRecord {
        CertPrincipalRecord {
            principal_id: row.get(0),
            role: row.get(1),
            created_at_ms: row.get::<_, i64>(2) as u64,
            revoked_at_ms: row.get::<_, Option<i64>>(3).map(|v| v as u64),
            // FAIL-CLOSED read (matches the SQLite backend, Copilot #857): a NEGATIVE
            // stored `not_after_ms` — only reachable via corruption, since the write
            // path refuses `> i64::MAX` — maps to `Some(0)` ("expired at epoch"), so a
            // tampered expiry can only make a cert MORE restricted, never a huge
            // never-expiring value. `u64::try_from` fails only for negatives → 0.
            not_after_ms: row
                .get::<_, Option<i64>>(4)
                .map(|v| u64::try_from(v).unwrap_or(0)),
        }
    }
}

impl CertPrincipalStore for PgVerifierStore {
    type Error = PgStoreError;

    fn register_cert_principal(
        &mut self,
        principal_id: &str,
        cert_sha256: &str,
        role: &str,
        not_after_ms: Option<u64>,
        now_ms: u64,
    ) -> Result<(), PgStoreError> {
        let created_ms = i64::try_from(now_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "now_ms",
            value: now_ms,
        })?;
        // Refuse a `not_after_ms > i64::MAX` (never truncate to a negative that would
        // read back as a huge never-expiring value — a fail-OPEN expiry). Bounded in
        // practice (~292M years past epoch).
        let not_after_i64 = match not_after_ms {
            Some(v) => Some(i64::try_from(v).map_err(|_| PgStoreError::OutOfDomain {
                field: "not_after_ms",
                value: v,
            })?),
            None => None,
        };
        // A `cert_sha256` already pinned to a DIFFERENT principal violates the UNIQUE
        // constraint (surfaces as a driver error — one cert, one principal).
        self.lock().execute(
            "INSERT INTO cert_principals \
                 (principal_id, cert_sha256, role, created_at_ms, revoked_at_ms, not_after_ms) \
             VALUES ($1, $2, $3, $4, NULL, $5) \
             ON CONFLICT (principal_id) DO UPDATE SET \
                 cert_sha256   = EXCLUDED.cert_sha256, \
                 role          = EXCLUDED.role, \
                 created_at_ms = EXCLUDED.created_at_ms, \
                 revoked_at_ms = NULL, \
                 not_after_ms  = EXCLUDED.not_after_ms",
            &[
                &principal_id,
                &cert_sha256,
                &role,
                &created_ms,
                &not_after_i64,
            ],
        )?;
        Ok(())
    }

    fn revoke_cert_principal(
        &mut self,
        principal_id: &str,
        now_ms: u64,
    ) -> Result<bool, PgStoreError> {
        let rev_ms = i64::try_from(now_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "now_ms",
            value: now_ms,
        })?;
        let n = self.lock().execute(
            "UPDATE cert_principals SET revoked_at_ms = $2 \
             WHERE principal_id = $1 AND revoked_at_ms IS NULL",
            &[&principal_id, &rev_ms],
        )?;
        Ok(n > 0)
    }

    fn load_cert_principal_by_fingerprint(
        &self,
        cert_sha256: &str,
    ) -> Result<Option<CertPrincipalRecord>, PgStoreError> {
        let row = self.lock().query_opt(
            "SELECT principal_id, role, created_at_ms, revoked_at_ms, not_after_ms \
             FROM cert_principals WHERE cert_sha256 = $1",
            &[&cert_sha256],
        )?;
        Ok(row.as_ref().map(Self::row_to_cert_principal))
    }

    fn load_cert_principals(&self) -> Result<Vec<CertPrincipalRecord>, PgStoreError> {
        let rows = self.lock().query(
            "SELECT principal_id, role, created_at_ms, revoked_at_ms, not_after_ms \
             FROM cert_principals ORDER BY principal_id",
            &[],
        )?;
        Ok(rows.iter().map(Self::row_to_cert_principal).collect())
    }
}
