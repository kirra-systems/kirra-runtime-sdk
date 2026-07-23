//! EP-10 (MGA G-9) — the **live Postgres backend** for the verifier storage seams.
//!
//! The root crate defines eleven backend seams and proves each against SQLite +
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
//!   tokens stored ONLY as their SHA-256, `UNIQUE(token_sha256)` one-token-one-principal);
//! - the [`CertPrincipalStore`] contract (the mTLS cert-principal registry — a
//!   fingerprint-pinned client cert + optional X.509 expiry, fail-closed);
//! - the [`FabricAssetStore`] contract (the fabric asset registry — id, type,
//!   kinematic profile, metadata);
//! - the [`OtaCampaignStore`] contract (the OTA governor-artifact campaign
//!   persistence + reads — insert/load/active-filter with fail-closed row decode —
//!   and the per-node adoption reports, monotonic on `reported_at_ms` with
//!   attested-per-digest carry; the audit-chaining of lifecycle mutations stays
//!   inherent on the SQLite backend, OUTSIDE this storage contract);
//! - the [`AvSubsystemStore`] contract (the AV diagnostic-meta store — per-node
//!   confidence floor + last-telemetry stamp + the recovery-streak counters that
//!   drive the AV recovery hysteresis and the telemetry watchdog; the 0→1-edge
//!   streak-start rule and the increment-on-absent fail-closed error are held
//!   identical across backends).
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

use kirra_fabric_types::asset::{AssetType, FabricAsset, KinematicProfileType};
use kirra_ota_campaign::{Campaign, CampaignState, HaltReason, NodeArtifactStatus};
use kirra_persistence::migrations_postgres::{
    PgExecutor, PgMigration, PgMigrationError, PostgresBackend,
};
use kirra_persistence::{
    ApiPrincipalRecord, AvSubsystemRecord, AvSubsystemStore, CertPrincipalRecord,
    CertPrincipalStore, EpochFence, FabricAssetStore, FederationStore, FenceError, NodeStore,
    OperatorRecord, OperatorStore, OtaCampaignStore, PostureEngineStateStore, PrincipalStore,
};
use kirra_core::{NodeTrustState, RegisteredNode};

/// The Postgres schema version THIS binary supports (mirrors the SQLite
/// `SCHEMA_VERSION` discipline: a newer stamp in the database is refused
/// fail-closed by the shared engine).
pub const PG_SCHEMA_VERSION: i64 = 10;

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
    // v8 — the fabric asset registry (the FabricAssetStore seam): the enum fields
    // (`asset_type`, `kinematic_profile`) and the `metadata` map are JSON-serialized
    // into TEXT columns, exactly as the SQLite backend.
    PgMigration {
        version: 8,
        sql: "CREATE TABLE IF NOT EXISTS fabric_assets ( \
                  asset_id          TEXT PRIMARY KEY, \
                  asset_type        TEXT NOT NULL, \
                  display_name      TEXT NOT NULL, \
                  kinematic_profile TEXT NOT NULL, \
                  registered_at_ms  BIGINT NOT NULL, \
                  last_seen_ms      BIGINT NOT NULL, \
                  metadata_json     TEXT NOT NULL DEFAULT '{}' \
              )",
    },
    // v9 — the OTA governor-artifact campaign tables (the OtaCampaignStore seam).
    // `ota_campaigns` carries the FULL post-v2 SQLite shape in one step (the live-PG
    // schema is authored current, no legacy `uptane_metadata_json`-less era to
    // migrate through): the immutable identity/schedule columns + the mutable
    // lifecycle fields. `cohorts`/`stages` are JSON-serialized into TEXT exactly as
    // SQLite. `node_artifact_status` is the per-node adoption report (upsert by
    // `node_id`); `attested` is a native BOOLEAN here (SQLite stores 0/1) — the
    // Rust `bool` round-trips through both. The audit-chaining of lifecycle
    // mutations is NOT part of this storage contract (it stays inherent on the
    // SQLite backend via the AuditAppender seam), so no audit table is needed here.
    PgMigration {
        version: 9,
        sql: "CREATE TABLE IF NOT EXISTS ota_campaigns ( \
                  campaign_id            TEXT PRIMARY KEY, \
                  artifact_digest        TEXT NOT NULL, \
                  artifact_version       TEXT NOT NULL, \
                  cohorts_json           TEXT NOT NULL, \
                  stages_json            TEXT NOT NULL, \
                  stage_index            BIGINT NOT NULL DEFAULT 0, \
                  rollout_percent        BIGINT NOT NULL DEFAULT 0, \
                  state                  TEXT NOT NULL, \
                  halt_reason            TEXT, \
                  created_at_ms          BIGINT NOT NULL, \
                  updated_at_ms          BIGINT NOT NULL, \
                  artifact_signature_b64 TEXT, \
                  uptane_metadata_json   TEXT \
              ); \
              CREATE TABLE IF NOT EXISTS node_artifact_status ( \
                  node_id          TEXT PRIMARY KEY, \
                  applied_digest   TEXT NOT NULL, \
                  campaign_id      TEXT, \
                  artifact_version TEXT, \
                  reported_at_ms   BIGINT NOT NULL, \
                  attested         BOOLEAN NOT NULL DEFAULT FALSE \
              )",
    },
    // v10 — the AV-subsystem diagnostic-meta table (the AvSubsystemStore seam): the
    // per-node confidence floor + last-telemetry stamp + the recovery-streak
    // counters that drive the AV recovery hysteresis and the telemetry watchdog. No
    // secrets. `confidence_floor` is `DOUBLE PRECISION` (SQLite REAL); the streak
    // columns DEFAULT 0 so a `register` that omits them (INSERT OR REPLACE) resets
    // them, exactly as SQLite.
    PgMigration {
        version: 10,
        sql: "CREATE TABLE IF NOT EXISTS av_subsystem_meta ( \
                  node_id                  TEXT PRIMARY KEY, \
                  subsystem_type           TEXT NOT NULL, \
                  hardware_id              TEXT NOT NULL, \
                  confidence_floor         DOUBLE PRECISION NOT NULL DEFAULT 0.70, \
                  last_telemetry_ms        BIGINT NOT NULL DEFAULT 0, \
                  recovery_streak_count    BIGINT NOT NULL DEFAULT 0, \
                  recovery_streak_start_ms BIGINT NOT NULL DEFAULT 0 \
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
    /// A value could not be ENCODED to its JSON column on the WRITE path — the
    /// node `status_json`, or a campaign's `cohorts`/`stages`. (The READ-path
    /// inverse — a stored blob that no longer DECODES — is stored-row corruption,
    /// surfaced as [`PgStoreError::CorruptGeneration`]/[`PgStoreError::CorruptCampaignRow`],
    /// never this variant.)
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
    /// A stored `ota_campaigns` row no longer decodes to a valid [`Campaign`]:
    /// an unknown `state`/`halt_reason` token, an out-of-range `stage_index`
    /// (`>= stages.len()`), or a malformed `cohorts`/`stages` JSON blob. Surfaced
    /// fail-closed exactly as the SQLite backend's `map_campaign_row` — a corrupt
    /// row is an error, NEVER handed back as a `Campaign` the engine could later
    /// index out of bounds (`Campaign::advance` → `stages[stage_index]`).
    CorruptCampaignRow(String),
    /// `increment_recovery_streak` was called on a node with no `av_subsystem_meta`
    /// row (nothing to increment). The SQLite backend surfaces the same condition as
    /// `rusqlite::Error::QueryReturnedNoRows`, the in-memory backend as
    /// `InMemAvError::NodeNotRegistered` — the portable contract holds all three to a
    /// fail-closed error, never a silently-fabricated streak.
    AvNodeNotRegistered(String),
}

impl std::fmt::Display for PgStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PgStoreError::Pg(e) => write!(f, "postgres error: {e}"),
            PgStoreError::Encode(e) => write!(f, "JSON column encode error: {e}"),
            PgStoreError::CorruptGeneration(v) => {
                write!(f, "corrupt last_generation value: {v:?}")
            }
            PgStoreError::OutOfDomain { field, value } => {
                write!(f, "{field} value {value} exceeds the BIGINT (i64) domain")
            }
            PgStoreError::CorruptCampaignRow(detail) => {
                write!(f, "corrupt ota_campaigns row: {detail}")
            }
            PgStoreError::AvNodeNotRegistered(node) => {
                write!(f, "av_subsystem_meta: node {node:?} is not registered")
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
            PgStoreError::CorruptCampaignRow(_) => None,
            PgStoreError::AvNodeNotRegistered(_) => None,
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

    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, postgres::Client> {
        self.client.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn row_to_node(row: &postgres::Row) -> RegisteredNode {
        let status_json: String = row.get(1);
        // Same corrupt-status fallback as the SQLite backend: an undecodable
        // status is Unknown (fail toward "not trusted"), never a panic.
        let status: NodeTrustState =
            serde_json::from_str(&status_json).unwrap_or(NodeTrustState::Unknown);
        RegisteredNode {
            node_id: row.get(0),
            status,
            registered_at_ms: row.get::<_, i64>(2).max(0) as u64,
            last_trust_update_ms: row.get::<_, i64>(3).max(0) as u64,
            ak_public_pem: row.get(4),
            expected_pcr16_digest_hex: row.get(5),
            site: row.get(6),
            firmware_version: row.get(7),
        }
    }
}

mod av_subsystem;
mod cert_principals;
mod epoch;
mod fabric;
mod federation;
mod nodes;
mod operators;
mod ota_campaigns;
mod posture;
mod principals;
