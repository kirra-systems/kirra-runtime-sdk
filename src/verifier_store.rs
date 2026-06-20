// src/verifier_store.rs

use std::collections::HashMap;
use rusqlite::{params, Connection, Result};
use crate::verifier::{NodeTrustState, RegisteredNode};
use crate::federation::FederatedTrustReport;
use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};

pub struct AuditChainVerifyResult {
    pub chain_intact: bool,
    pub total_entries: u64,
    pub latest_hash: String,
    pub signing_enabled: bool,
    pub signed_entries: u64,
    pub unsigned_entries: u64,
    pub signature_valid: bool,
    pub first_invalid_signature_index: Option<u64>,
    pub first_signed_at_ms: Option<u64>,
    pub public_key_b64: Option<String>,
    /// #77 anchor-HEAD high-water check. `true` when the signed head matches the
    /// chain tail (or the chain is empty); `false` is fail-closed — the tail is
    /// behind the head (truncation/deletion), the head signature is invalid
    /// (tamper), or a non-empty chain has no head. Independent of `chain_intact`
    /// (which catches in-place row edits); together they cover edit + truncation.
    pub head_verified: bool,
    /// Machine-readable reason for `head_verified` (e.g. `OK`, `EMPTY_CHAIN`,
    /// `HEAD_ABSENT`, `TRUNCATION_DETECTED`, `HEAD_SIGNATURE_INVALID`,
    /// `HEAD_TAIL_MISMATCH`, `HEAD_KEY_UNKNOWN`, `HEAD_UNSIGNED`, `OK_UNSIGNED`).
    pub head_status: String,
}

/// Verification verdict for the fabric causal-log forensic chain (#87).
/// Same shape as [`AuditChainVerifyResult`]: `chain_intact` covers in-place
/// row edits (recomputed record hash mismatch, broken prev-linkage, sequence
/// gaps); `head_verified` covers tail truncation/deletion via the signed
/// anchor-head high-water mark. Together they cover edit + truncation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CausalChainVerifyResult {
    pub chain_intact: bool,
    pub total_entries: u64,
    pub latest_hash: String,
    pub signing_enabled: bool,
    pub signed_entries: u64,
    pub unsigned_entries: u64,
    pub signature_valid: bool,
    pub first_invalid_signature_index: Option<u64>,
    pub first_signed_at_ms: Option<u64>,
    pub public_key_b64: Option<String>,
    pub head_verified: bool,
    pub head_status: String,
}

#[derive(serde::Serialize)]
pub struct AuditExportEntry {
    pub id: i64,
    pub timestamp_ms: u64,
    pub event_type: String,
    pub source: String,
    pub payload: String,
    pub prev_hash: String,
    pub entry_hash: String,
    pub signature_b64: Option<String>,
    pub signature_status: String,
}

#[derive(serde::Serialize)]
pub struct AuditExportPage {
    pub entries: Vec<AuditExportEntry>,
    pub total: u64,
    pub public_key_b64: Option<String>,
    pub chain_intact: bool,
}

/// A pending clearance grant taken for delivery (operator-console Phase B, #304).
#[derive(Debug, Clone)]
pub struct PendingClearanceGrant {
    pub rowid: i64,
    pub node_id: String,
    pub operator_id: String,
    /// The verifier's RECORD time (Phase A), NOT the pickup time. The
    /// `ClearanceLoop` ages the grant against this at delivery (checkpoint 2).
    pub granted_at_ms: u64,
}

/// Diagnostic meta for a registered AV subsystem. Read-only projection of the
/// `av_subsystem_meta` table — no secrets (confidence floor, recovery streak,
/// last telemetry timestamp).
#[derive(Debug, Clone)]
pub struct AvSubsystemRecord {
    pub node_id: String,
    pub subsystem_type: String,
    pub hardware_id: String,
    pub confidence_floor: f64,
    pub last_telemetry_ms: u64,
    pub recovery_streak_count: u32,
    pub recovery_streak_start_ms: u64,
}

/// A registered operator (#314 Phase 1). `pubkey_pem` is the PUBLIC key only.
#[derive(Debug, Clone)]
pub struct OperatorRecord {
    pub operator_id: String,
    pub pubkey_pem: String,
    pub registered_at_ms: u64,
    /// `None` = active; `Some(ms)` = revoked at that time (cannot clear grants).
    pub revoked_at_ms: Option<u64>,
}

impl OperatorRecord {
    /// True iff the operator is registered and not revoked.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.revoked_at_ms.is_none()
    }
}

/// The latest clearance grant's delivery state for a node (console read surface).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClearanceGrantState {
    pub granted_at_ms: u64,
    /// Set once the grant has been taken for delivery (the one-shot consume).
    pub consumed_at_ms: Option<u64>,
    /// `"Cleared"` on success, else the loop's rejection reason code. `None`
    /// while still pending delivery.
    pub outcome: Option<String>,
    pub outcome_detail: Option<String>,
}

// --- HA epoch fence — fail-closed outcomes (issue #79) ----------------------

/// Why the in-transaction HA epoch fence rejected a top-tier durable write.
///
/// Returned by [`VerifierStore::assert_epoch_held`], which re-reads the durable
/// `ha_state.epoch` INSIDE the write transaction and compares it to the
/// instance's in-memory `held_epoch`. Every variant is fail-closed: the
/// enclosing transaction is dropped WITHOUT commit, so no partial write lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceError {
    /// The instance's `held_epoch` no longer matches the durable epoch — it has
    /// been superseded by another instance's `try_claim_epoch` (any divergence,
    /// including `durable < held`, fences), OR `held == 0` (the node never made
    /// a legitimate claim). Active nodes always claim an epoch at startup before
    /// serving, so `held == 0` at a top-tier write is anomalous → reject.
    EpochSuperseded { held: u64, durable: u64 },
    /// The durable epoch could not be read (SELECT failed / `ha_state` row
    /// absent). A top-tier write never proceeds blind → reject.
    EpochUnreadable,
}

/// Error from a top-tier (durable, `synchronous=FULL`) state mutation: either
/// the HA epoch fence fired (`Fenced`) or the underlying SQLite write failed
/// (`Db`). Callers self-demote on `Fenced`; both are denials (no partial write).
#[derive(Debug)]
pub enum DurableWriteError {
    Fenced(FenceError),
    Db(rusqlite::Error),
}

impl From<FenceError> for DurableWriteError {
    fn from(e: FenceError) -> Self {
        DurableWriteError::Fenced(e)
    }
}

impl From<rusqlite::Error> for DurableWriteError {
    fn from(e: rusqlite::Error) -> Self {
        DurableWriteError::Db(e)
    }
}

impl std::fmt::Display for DurableWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DurableWriteError::Fenced(FenceError::EpochSuperseded { held, durable }) => write!(
                f,
                "durable write fenced: held epoch {held} != durable epoch {durable} (superseded)"
            ),
            DurableWriteError::Fenced(FenceError::EpochUnreadable) => {
                write!(f, "durable write fenced: HA epoch unreadable (fail-closed)")
            }
            DurableWriteError::Db(e) => write!(f, "durable write failed: {e}"),
        }
    }
}

impl std::error::Error for DurableWriteError {}

pub struct VerifierStore {
    /// Hot/read connection — `synchronous=NORMAL`. Carries the verdict-adjacent
    /// per-command audit (no fsync; throughput-safe at 20 Hz+).
    conn: Connection,
    /// Durable connection — `synchronous=FULL` (fsync per commit). Carries
    /// durability-critical writes whose loss is a CORRECTNESS or anti-replay
    /// bug (#74): the HA epoch CAS and the federation nonce burn. `synchronous`
    /// is per-connection, so this second handle to the SAME WAL DB force-syncs
    /// while the hot path stays NORMAL. `None` for in-memory stores (no
    /// power-loss semantics, and a 2nd `:memory:` open is a DISTINCT db) — there
    /// durability-critical writes fall back to `conn`. This is the reusable
    /// durable-write seam #165 (active-key + genesis persistence) extends.
    durable_conn: Option<Connection>,
    pub signing_key: Option<ed25519_dalek::SigningKey>,
}

// --- audit key-rotation helpers (#76) --------------------------------------

/// Decode a base64 32-byte Ed25519 verifying key.
fn audit_decode_vk(b64: &str) -> Option<ed25519_dalek::VerifyingKey> {
    use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
    let bytes = b64e.decode(b64).ok()?;
    let arr: [u8; 32] = bytes.as_slice().try_into().ok()?;
    ed25519_dalek::VerifyingKey::from_bytes(&arr).ok()
}

/// Build the canonical signing payload for a row, dispatched by hash version.
fn audit_signing_payload(
    hash_version: i64,
    prev: &str,
    rec: &str,
    event_type: &str,
    created_at_ms: i64,
    sequence: Option<i64>,
) -> String {
    use crate::audit_chain::{canonical_signing_payload, canonical_signing_payload_v2};
    match hash_version {
        2 => canonical_signing_payload_v2(
            prev, rec, event_type, created_at_ms,
            sequence.unwrap_or(0).max(0) as u64,
        ),
        _ => canonical_signing_payload(prev, rec, event_type, created_at_ms),
    }
}

/// Verify a base64 Ed25519 signature over `payload` under `vk`.
fn audit_verify_sig(vk: &ed25519_dalek::VerifyingKey, payload: &str, sig_b64: &str) -> bool {
    use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
    use ed25519_dalek::{Signature, Verifier};
    b64e.decode(sig_b64)
        .ok()
        .and_then(|b| <[u8; 64]>::try_from(b.as_slice()).ok())
        .map(|arr| vk.verify(payload.as_bytes(), &Signature::from_bytes(&arr)).is_ok())
        .unwrap_or(false)
}

/// If a (already-signature-verified) `KEY_ROTATION` event's payload announces a
/// new pubkey + key_id whose fingerprint matches, add it to the keyring.
/// Content-addressed: a rotation cannot smuggle in a key under a wrong id.
fn extend_keyring_from_rotation(
    keyring: &mut std::collections::HashMap<String, ed25519_dalek::VerifyingKey>,
    event_json: &str,
) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(event_json) else { return };
    let (Some(npk), Some(nkid)) =
        (v["new_public_key_b64"].as_str(), v["new_key_id"].as_str()) else { return };
    let Some(nvk) = audit_decode_vk(npk) else { return };
    if crate::audit_chain::verifying_key_id(&nvk) == nkid {
        keyring.insert(nkid.to_string(), nvk);
    }
}

// --- #165 durable audit-key trust map helpers ------------------------------

/// Outcome of admitting the env-loaded signing key against the durable ledger
/// at boot. The bin maps the `*Rejected`/`Mismatch` variants to a fatal,
/// fail-closed startup error (refuse to sign); the others proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAdmission {
    /// Env key matches the durable active key — normal resume.
    Resumed,
    /// No durable anchor existed; first-boot backfill wrote the anchor + a
    /// genesis ledger row (and reconciled any pre-existing in-chain rotations
    /// into forensic `backfill` ledger rows). Env key adopted as genesis/active.
    BackfilledGenesis,
    /// Anchor existed; env key was a NEW id and an explicit adopt signal was
    /// present, so a durable `reanchor` ledger row was recorded and the env key
    /// adopted as active. (Gap-2 operator env-rotation, consented.)
    AdoptedReanchor,
    /// FAIL-CLOSED: env key is present in the ledger but is NOT the active key
    /// (a restart reverted to a retired key). The store does NOT adopt it.
    RetiredKeyRejected,
    /// FAIL-CLOSED: env key is a NEW id not in the ledger and no explicit adopt
    /// signal was present (anti-silent-re-root). The store does NOT adopt it.
    UnadoptedNewKeyRejected,
    /// FAIL-CLOSED: a config-pinned genesis key-id did not match the durable
    /// anchor's genesis.
    GenesisPinMismatch,
    /// FAIL-CLOSED at the UPGRADE moment (#165 migration hardening): a pre-#165
    /// chain records a KEY_ROTATION whose latest resulting key (`chain_latest_key_id`)
    /// does NOT match the env key (`env_key_id`). The env key has reverted to a
    /// pre-rotation key (or is foreign to the chain), so anchoring genesis on it
    /// would silently re-root trust away from what the chain asserts is active.
    /// Refused unless the operator explicitly consents via
    /// `KIRRA_LOG_SIGNING_KEY_ADOPT` (which records a consented reanchor). Fires
    /// ONLY when the chain has ≥1 rotation and its latest key != env; clean and
    /// correctly-rotated upgrades are unaffected.
    MigrationReversionRejected {
        chain_latest_key_id: String,
        env_key_id: String,
    },
}

/// Canonical, versioned signing payload for an `audit_key_ledger` row. The NEW
/// key signs this to bind `key_id ↔ pubkey` (and its place in the chain). `seq`
/// is deliberately excluded — it is a local ordering PK, not security-relevant;
/// the binding is over the content-addressed identity, predecessor, role and ts.
fn ledger_signing_payload(
    key_id: &str,
    prev_key_id: Option<&str>,
    role: &str,
    pubkey_b64: &str,
    created_at_ms: i64,
) -> String {
    format!(
        "KIRRA_KEY_LEDGER_V1|{key_id}|{prev}|{role}|{pubkey_b64}|{created_at_ms}",
        prev = prev_key_id.unwrap_or("")
    )
}

/// A decoded `audit_key_ledger` row.
struct LedgerRow {
    key_id: String,
    role: String,
    pubkey_b64: String,
    signature_b64: String,
    prev_key_id: Option<String>,
    created_at_ms: i64,
}

/// True iff a ledger row is content-addressed AND carries a valid self-signature
/// by its own key — the condition for trusting it as a verification key. The
/// forensic `backfill` rows (empty signature) are intentionally NOT trusted here
/// (their keys are reachable via the in-chain KEY_ROTATION replay instead).
fn ledger_row_is_self_attested(r: &LedgerRow) -> bool {
    if r.signature_b64.is_empty() {
        return false;
    }
    let Some(vk) = audit_decode_vk(&r.pubkey_b64) else { return false };
    if crate::audit_chain::verifying_key_id(&vk) != r.key_id {
        return false; // content-addressing violated
    }
    let payload = ledger_signing_payload(
        &r.key_id,
        r.prev_key_id.as_deref(),
        &r.role,
        &r.pubkey_b64,
        r.created_at_ms,
    );
    audit_verify_sig(&vk, &payload, &r.signature_b64)
}

/// Bundled event-data inputs for [`VerifierStore::append_causal_event`].
///
/// Groups the causal-event payload fields (everything that describes the event
/// being appended). The `signing_key` is intentionally kept as a separate
/// parameter on the method, since it is a distinct concern (signing identity,
/// not event data).
#[derive(Debug, Clone)]
pub struct CausalEventInput<'a> {
    pub entry_id: &'a str,
    pub asset_id: &'a str,
    pub event_type: &'a str,
    pub payload: &'a str,
    pub caused_by: &'a [String],
    pub affects_assets: &'a [String],
    pub fabric_generation: u64,
    pub timestamp_ms: u64,
}

impl VerifierStore {
    pub fn new(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS nodes (
                node_id                    TEXT PRIMARY KEY,
                status_json                TEXT NOT NULL,
                registered_at_ms           INTEGER NOT NULL DEFAULT 0,
                last_trust_update_ms       INTEGER NOT NULL DEFAULT 0,
                ak_public_pem              TEXT,
                expected_pcr16_digest_hex  TEXT
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS dependencies (
                node_id  TEXT NOT NULL,
                dep_id   TEXT NOT NULL,
                PRIMARY KEY (node_id, dep_id)
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS posture_events (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id        TEXT    NOT NULL,
                event_type     TEXT    NOT NULL,
                posture_json   TEXT    NOT NULL,
                reason         TEXT,
                created_at_ms  INTEGER NOT NULL
            )",
            [],
        )?;

        // Operator clearance grants (#103 SG6 / operator-console Phase A).
        // RECORD-ONLY: a row here is a recorded + audit-chained supervisor grant;
        // it does NOT release any node. Delivery to the node's ClearanceLoop is
        // Phase B (node transport) — `delivery` stays 'PENDING-NODE-TRANSPORT'.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS clearance_grants (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id        TEXT    NOT NULL,
                operator_id    TEXT    NOT NULL,
                granted_at_ms  INTEGER NOT NULL,
                delivery       TEXT    NOT NULL DEFAULT 'PENDING-NODE-TRANSPORT',
                created_at_ms  INTEGER NOT NULL,
                consumed_at_ms INTEGER,
                outcome        TEXT,
                outcome_detail TEXT
            )",
            [],
        )?;
        // Phase-B delivery columns (additive, idempotent — upgrade a Phase-A
        // clearance_grants table that predates these). `consumed_at_ms` is the
        // one-shot consume marker; `outcome`/`outcome_detail` are the
        // ClearanceLoop verdict at delivery. Mirrors the audit_log_chain
        // ADD-COLUMN migration convention below.
        for col_sql in [
            "ALTER TABLE clearance_grants ADD COLUMN consumed_at_ms INTEGER",
            "ALTER TABLE clearance_grants ADD COLUMN outcome TEXT",
            "ALTER TABLE clearance_grants ADD COLUMN outcome_detail TEXT",
            // #314 Phase 1 — operator-proven identity. ADDITIVE: how the grant was
            // authorized ("operator-signed" / "supervisor-break-glass" /
            // "unspecified") and WHICH operator key signed it (fingerprint). Phase-B
            // `take_pending_clearance_grant` does not read these — delivery unchanged.
            "ALTER TABLE clearance_grants ADD COLUMN auth_method TEXT",
            "ALTER TABLE clearance_grants ADD COLUMN operator_key_fingerprint TEXT",
        ] {
            if let Err(e) = conn.execute(col_sql, []) {
                if !e.to_string().contains("duplicate column name") {
                    return Err(e);
                }
            }
        }

        // #314 Phase 1 — registered operators (per-operator Ed25519 identity).
        // `pubkey_pem` is the operator's PUBLIC key only (no private material ever
        // touches the server). `revoked_at_ms` NULL = active; a revoked operator
        // can never clear a grant. Mirrors the `nodes` registry shape.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS operators (
                operator_id       TEXT    PRIMARY KEY,
                pubkey_pem        TEXT    NOT NULL,
                registered_at_ms  INTEGER NOT NULL,
                revoked_at_ms     INTEGER
            )",
            [],
        )?;

        // AV subsystem metadata: confidence floors, telemetry timestamps, recovery streak.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS av_subsystem_meta (
                node_id                  TEXT    PRIMARY KEY,
                subsystem_type           TEXT    NOT NULL,
                hardware_id              TEXT    NOT NULL,
                confidence_floor         REAL    NOT NULL DEFAULT 0.70,
                last_telemetry_ms        INTEGER NOT NULL DEFAULT 0,
                recovery_streak_count    INTEGER NOT NULL DEFAULT 0,
                recovery_streak_start_ms INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )?;

        // Posture engine persistent state (generation counter, heartbeat, etc.).
        conn.execute(
            "CREATE TABLE IF NOT EXISTS posture_engine_state (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
            [],
        )?;

        // HA fencing token (durable epoch). Singleton row (CHECK id = 1).
        // The `epoch` column is the source of truth for "which generation of
        // Active currently owns writes." Promotion bumps it via a conditional
        // UPDATE (rows_affected == 1 → durable compare-and-set). Active
        // instances cache their claimed epoch in `AppState::held_epoch`; the
        // mutation gate fails closed when held != current.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS ha_state (
                id                 INTEGER PRIMARY KEY CHECK (id = 1),
                epoch              INTEGER NOT NULL DEFAULT 0,
                active_instance_id TEXT,
                updated_at_ms      INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO ha_state (id, epoch, active_instance_id, updated_at_ms)
             VALUES (1, 0, NULL, 0)",
            [],
        )?;

        // --- Durable audit-key trust map (#165) --------------------------------
        // A write-once trust ANCHOR (durable genesis fingerprint) + an append-only
        // signed key LEDGER. Together they make signing-key rotation durable across
        // restart and pin the verification root to a DURABLE anchor (not the
        // mutable env key). All writes ride the `synchronous=FULL` durable_conn
        // (see record_key_rotation / admit_signing_key) so they inherit #74's
        // hard-power-loss durability.
        //
        // GENERIC SHAPE (reused by #164's hmac_salt_ledger): the pair is a
        // "versioned-secret" pattern — a write-once anchor singleton naming the
        // root version, plus an append-only ledger of {version-id, prev-id, role,
        // material, self-attestation, ts}. The audit-key specialization fills
        // `pubkey_b64` + `signature_b64` (Ed25519 self-signature); a symmetric
        // secret (HMAC salt) would carry a salt fingerprint instead of a pubkey
        // and an HMAC tag instead of a signature. The skeleton is identical.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS audit_trust_anchor (
                id              INTEGER PRIMARY KEY CHECK (id = 1),
                genesis_key_id  TEXT    NOT NULL,
                created_at_ms   INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS audit_key_ledger (
                seq            INTEGER PRIMARY KEY AUTOINCREMENT,
                key_id         TEXT    NOT NULL,
                prev_key_id    TEXT,
                role           TEXT    NOT NULL,   -- 'genesis' | 'rotation' | 'reanchor' | 'backfill'
                pubkey_b64     TEXT    NOT NULL,
                signature_b64  TEXT    NOT NULL,   -- self-signature by this key ('' for forensic 'backfill')
                created_at_ms  INTEGER NOT NULL
            )",
            [],
        )?;
        // #77: signed anchor-HEAD high-water mark. A singleton (id = 1) row
        // recording the highest committed chain position (sequence, record_hash),
        // signed over `canonical_anchor_head_payload`. It is advanced in the SAME
        // transaction as each audit append (see `append_audit_event_tx`), so it
        // shares the chain's NORMAL durability exactly — never more durable (#74).
        // Verification compares the chain tail to this head: tail behind the head
        // ⇒ tail rows were truncated/deleted; bad head signature ⇒ tamper.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS audit_anchor_head (
                id              INTEGER PRIMARY KEY CHECK (id = 1),
                sequence        INTEGER NOT NULL,
                record_hash_hex TEXT    NOT NULL,
                signature_b64   TEXT,
                key_id          TEXT
            )",
            [],
        )?;

        Self::init_audit_chain_schema(&conn)?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS fabric_assets (
                asset_id          TEXT PRIMARY KEY,
                asset_type        TEXT NOT NULL,
                display_name      TEXT NOT NULL,
                kinematic_profile TEXT NOT NULL,
                registered_at_ms  INTEGER NOT NULL,
                last_seen_ms      INTEGER NOT NULL,
                metadata_json     TEXT NOT NULL DEFAULT '{}'
            );

            -- #87: forensic, tamper-evident, hash-chained, signed causal ledger.
            -- Mirrors audit_log_chain: `previous_hash_hex`/`record_hash_hex`
            -- chain the rows; `sequence` is the monotone position; the record
            -- hash BINDS the causality edges (caused_by, affects_assets,
            -- fabric_generation) so tampering an edge is detected. `entry_id`
            -- remains the causal reference id (caused_by references entry_ids).
            CREATE TABLE IF NOT EXISTS fabric_causal_log (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                entry_id          TEXT NOT NULL,
                sequence          INTEGER NOT NULL,
                timestamp_ms      INTEGER NOT NULL,
                asset_id          TEXT NOT NULL,
                event_type        TEXT NOT NULL,
                payload           TEXT NOT NULL,
                caused_by         TEXT NOT NULL,        -- JSON array of entry_id strings
                affects_assets    TEXT NOT NULL,        -- JSON array of strings
                fabric_generation INTEGER NOT NULL,
                previous_hash_hex TEXT NOT NULL,
                record_hash_hex   TEXT NOT NULL,
                signature_b64     TEXT,
                key_id            TEXT
            );

            -- #87: signed anchor-HEAD high-water mark for the causal chain.
            -- Singleton (id = 1); advanced in the SAME transaction as each
            -- append so it shares the chain tail's durability exactly. A tail
            -- behind the head ⇒ truncation; bad head signature ⇒ tamper.
            CREATE TABLE IF NOT EXISTS fabric_causal_anchor_head (
                id              INTEGER PRIMARY KEY CHECK (id = 1),
                sequence        INTEGER NOT NULL,
                record_hash_hex TEXT NOT NULL,
                signature_b64   TEXT,
                key_id          TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_causal_log_asset
                ON fabric_causal_log(asset_id, timestamp_ms);
            CREATE INDEX IF NOT EXISTS idx_causal_log_time
                ON fabric_causal_log(timestamp_ms);"
        )?;

        // Durable (force-synced) connection for the fence-correctness + anti-
        // replay writes (#74). Same WAL DB file; `synchronous=FULL` fsyncs every
        // commit. In-memory stores have no power-loss semantics and a second
        // `:memory:` open would be a separate database, so we skip it there and
        // fall back to `conn` for those writes (a no-op durability-wise).
        let durable_conn = if path == ":memory:" {
            None
        } else {
            let dc = Connection::open(path)?;
            dc.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
            Some(dc)
        };

        Ok(Self { conn, durable_conn, signing_key: None })
    }

    /// Durability-critical read/single-write connection: the FULL handle when
    /// present (file-backed), else the main connection (in-memory fallback).
    fn durable_ref(&self) -> &Connection {
        self.durable_conn.as_ref().unwrap_or(&self.conn)
    }

    /// Durability-critical transaction connection (mutable): FULL handle when
    /// present, else the main connection.
    fn durable_mut(&mut self) -> &mut Connection {
        match self.durable_conn {
            Some(ref mut c) => c,
            None => &mut self.conn,
        }
    }

    /// Force a durable checkpoint: `wal_checkpoint(TRUNCATE)` on the FULL
    /// connection fsyncs the shared WAL into the main DB file, making ALL
    /// committed data durable — including the per-command audit rows written on
    /// the NORMAL connection. Call on safe-stop / shutdown (and optionally
    /// periodically) to bound the audit loss window WITHOUT per-row fsync. No-op
    /// for in-memory stores. Idempotent and cheap when the WAL is already small.
    ///
    /// DURABILITY BOUNDARY (#74) — by design, NOT a bug: the audit-chain tail is
    /// durable only to the LAST checkpoint. The HA epoch claim and federation
    /// nonce burn are `synchronous=FULL` (fsync per commit, survive a hard power
    /// loss); the audit chain stays `synchronous=NORMAL` (no per-row fsync —
    /// throughput-safe at 20 Hz+) and relies on this checkpoint (graceful
    /// safe-stop/shutdown + SQLite auto-checkpoint). So the final audit rows
    /// before an UNGRACEFUL power cut may be lost — a forensic gap, never a
    /// safety-state gap (the verdict path is store-free). Do NOT assume the audit
    /// tail is hard-power-loss-durable. Tighter durability (a periodic fsync'd
    /// checkpoint) is an available future knob, off by default. See
    /// docs/safety/CODING_GUIDELINES.md INV-12.
    pub fn durable_checkpoint(&self) -> Result<()> {
        if let Some(dc) = &self.durable_conn {
            dc.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        }
        Ok(())
    }

    pub fn set_signing_key(&mut self, key: ed25519_dalek::SigningKey) {
        self.signing_key = Some(key);
    }

    /// The PUBLIC half of the in-memory audit signing key, or `None` if no key is
    /// installed. Read-only exposure (#329 residual): lets the
    /// [`crate::key_registry::KeyRegistry`] resolve the chain's verifying key through
    /// the unified registry. There is exactly ONE audit signer and it is volatile
    /// (no rotation, no persisted history) — that wider residual is still deferred.
    pub fn audit_verifying_key(&self) -> Option<ed25519_dalek::VerifyingKey> {
        self.signing_key.as_ref().map(|sk| sk.verifying_key())
    }

    /// TEST-ONLY tamper seam (SG-010): hands a test the raw rusqlite connection
    /// so it can mutate a previously-written `audit_log_chain` row out of band —
    /// exactly what a tamperer with disk access would do. Used to prove
    /// `verify_audit_chain_full` detects the tamper. `#[cfg(test)]` so it never
    /// exists in a release build, and `pub(crate)` so only in-crate tests reach
    /// it (an external integration crate cannot, by design).
    #[cfg(test)]
    pub(crate) fn raw_conn(&mut self) -> &mut Connection {
        &mut self.conn
    }

    // --- #165 durable audit-key trust map -----------------------------------

    /// The durable trust anchor's genesis key-id, if an anchor has been written.
    pub fn audit_trust_anchor_genesis_id(&self) -> Result<Option<String>> {
        let r = self.durable_ref().query_row(
            "SELECT genesis_key_id FROM audit_trust_anchor WHERE id = 1",
            [],
            |row| row.get::<_, String>(0),
        );
        match r {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// All ledger rows in `seq` order.
    fn audit_key_ledger_rows(&self) -> Result<Vec<LedgerRow>> {
        let mut stmt = self.durable_ref().prepare(
            "SELECT key_id, role, pubkey_b64, signature_b64, prev_key_id, created_at_ms \
             FROM audit_key_ledger ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(LedgerRow {
                key_id: row.get(0)?,
                role: row.get(1)?,
                pubkey_b64: row.get(2)?,
                signature_b64: row.get(3)?,
                prev_key_id: row.get(4)?,
                created_at_ms: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    /// The active key-id: the highest-`seq` ledger row that is NOT a forensic
    /// `backfill` row (those record lost-private-key history, never an active
    /// signer). `None` when the ledger is empty.
    pub fn audit_key_ledger_active_id(&self) -> Result<Option<String>> {
        let r = self.durable_ref().query_row(
            "SELECT key_id FROM audit_key_ledger WHERE role != 'backfill' \
             ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        );
        match r {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Resolve the durable genesis verifying key from the anchor + the ledger's
    /// genesis row. `None` when no anchor exists (pre-#165 chains).
    fn audit_genesis_vk(&self) -> Result<Option<ed25519_dalek::VerifyingKey>> {
        let Some(genesis_id) = self.audit_trust_anchor_genesis_id()? else {
            return Ok(None);
        };
        for r in self.audit_key_ledger_rows()? {
            if r.key_id == genesis_id {
                if let Some(vk) = audit_decode_vk(&r.pubkey_b64) {
                    if crate::audit_chain::verifying_key_id(&vk) == genesis_id {
                        return Ok(Some(vk));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Seed the per-row verification keyring (#76 + #165): genesis from the
    /// DURABLE anchor (falling back to `fallback_vk` only when no anchor exists,
    /// i.e. a pre-#165 chain), plus every self-attested ledger key. Returns the
    /// keyring and the genesis key-id used to attribute NULL-key_id legacy rows.
    fn audit_keyring_seed(
        &self,
        fallback_vk: Option<&ed25519_dalek::VerifyingKey>,
    ) -> Result<(std::collections::HashMap<String, ed25519_dalek::VerifyingKey>, Option<String>)> {
        let mut keyring = std::collections::HashMap::new();

        let durable_genesis = self.audit_genesis_vk()?;
        let genesis_id = match (&durable_genesis, fallback_vk) {
            // Durable anchor wins — a mutated env key can never re-root trust.
            (Some(gvk), _) => {
                let gid = crate::audit_chain::verifying_key_id(gvk);
                keyring.insert(gid.clone(), *gvk);
                Some(gid)
            }
            // Pre-#165 fallback: the passed-in (env) key is the genesis.
            (None, Some(fvk)) => {
                let gid = crate::audit_chain::verifying_key_id(fvk);
                keyring.insert(gid.clone(), *fvk);
                Some(gid)
            }
            (None, None) => None,
        };

        // Extend with every self-attested ledger key (rotations + reanchors +
        // genesis). Forensic `backfill` rows (no self-signature) are skipped;
        // their keys remain reachable via the in-chain KEY_ROTATION replay.
        for r in self.audit_key_ledger_rows()? {
            if ledger_row_is_self_attested(&r) {
                if let Some(vk) = audit_decode_vk(&r.pubkey_b64) {
                    keyring.insert(r.key_id.clone(), vk);
                }
            }
        }
        Ok((keyring, genesis_id))
    }

    /// Collect the (key_id, pubkey_b64) of each in-chain KEY_ROTATION event in
    /// id order — used at first-boot to backfill forensic ledger rows so the
    /// ledger reflects pre-#165 in-process rotation history.
    fn collect_chain_key_rotations(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT event_json FROM audit_log_chain \
             WHERE event_type = 'KEY_ROTATION' ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for ej in rows {
            let ej = ej?;
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&ej) {
                if let (Some(pk), Some(kid)) =
                    (v["new_public_key_b64"].as_str(), v["new_key_id"].as_str())
                {
                    // Content-addressed sanity: only carry rows whose announced
                    // id matches the announced pubkey.
                    if let Some(vk) = audit_decode_vk(pk) {
                        if crate::audit_chain::verifying_key_id(&vk) == kid {
                            out.push((kid.to_string(), pk.to_string()));
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// Admit the env-loaded signing key against the durable trust map (#165),
    /// returning a [`KeyAdmission`] the caller acts on. Fail-closed variants do
    /// NOT set the in-memory signing key. See [`KeyAdmission`] for the cases.
    ///
    /// - No anchor → FIRST-BOOT BACKFILL: write anchor{genesis = env} + a
    ///   self-signed genesis ledger row, and reconcile any pre-existing in-chain
    ///   KEY_ROTATION rows into forensic `backfill` ledger rows. Adopt env —
    ///   UNLESS a migration reversion is detected (see runbook below).
    /// - Anchor + env == active → resume.
    /// - Anchor + env is a RETIRED ledger id → fail-closed (gap-1).
    /// - Anchor + env is a NEW id → fail-closed unless `adopt`, in which case a
    ///   self-signed `reanchor` ledger row is recorded and env adopted (gap-2).
    /// - `pinned_genesis` (optional) is checked against the anchor; mismatch is
    ///   fail-closed.
    ///
    /// All durable writes ride a single `synchronous=FULL` transaction.
    ///
    /// # Migration-reversion runbook (`KeyAdmission::MigrationReversionRejected`)
    ///
    /// At the upgrade moment, a pre-#165 chain may record an in-process
    /// KEY_ROTATION (e.g. A→B) whose latest result key the env key does NOT
    /// match — i.e. env has reverted to a pre-rotation key (A), or is foreign to
    /// the chain. Those pre-#165 in-process rotations were never durable (the
    /// very bug #165 closes), so anchoring genesis on the reverted env key would
    /// silently re-root audit trust away from what the chain records as active.
    /// This is the PRIMARY safeguard: fail closed. It fires ONLY when the chain
    /// has ≥1 rotation AND its latest key != env; clean upgrades (no pre-#165
    /// rotations) and correct upgrades (env updated to the latest rotation key)
    /// are unaffected. RESOLUTION, operator's choice:
    ///   1. Supply the correct active private key in `KIRRA_LOG_SIGNING_KEY`
    ///      (the key the chain's latest rotation names), then restart; OR
    ///   2. Set `KIRRA_LOG_SIGNING_KEY_ADOPT=1` to consent to anchoring on the
    ///      env key — recorded as an explicit, self-signed `reanchor` ledger row
    ///      (a logged operator decision, never a silent anchor).
    pub fn admit_signing_key(
        &mut self,
        env_key: ed25519_dalek::SigningKey,
        adopt: bool,
        pinned_genesis: Option<&str>,
        now_ms: u64,
    ) -> Result<KeyAdmission> {
        // #79 fence exemption (NARROW, deliberate): this is a BOOTSTRAP write.
        // It runs once during startup signing-key admission — BEFORE the Active
        // epoch arbitration in `main()` claims an epoch — so `held_epoch` is
        // legitimately 0 here and no fence applies. It is not reachable on the
        // Active request path (its only production caller is startup), so a
        // superseded node cannot reach it. The two REQUEST-PATH durable writes
        // (`save_federated_report_chained`, `record_key_rotation`) ARE fenced
        // via `assert_epoch_held`. Do not broaden this exemption.
        use ed25519_dalek::Signer;
        let env_vk = env_key.verifying_key();
        let k_env = crate::audit_chain::verifying_key_id(&env_vk);
        let env_pub_b64 = b64e.encode(env_vk.as_bytes());

        // Optional operator-pinned genesis check (only meaningful once an anchor
        // exists; on first boot the pin is established by the backfill below).
        if let (Some(pin), Some(genesis)) = (pinned_genesis, self.audit_trust_anchor_genesis_id()?) {
            if pin != genesis {
                return Ok(KeyAdmission::GenesisPinMismatch);
            }
        }

        match self.audit_trust_anchor_genesis_id()? {
            // ---- FIRST-BOOT BACKFILL (no durable anchor yet) -----------------
            None => {
                let rotations = self.collect_chain_key_rotations()?;
                // The key the chain asserts SHOULD be active = the result of its
                // latest KEY_ROTATION (rotations are in id order).
                let chain_latest = rotations.last().map(|(kid, _)| kid.clone());
                // #165 migration hardening: a pre-#165 chain whose latest rotation
                // is NOT the env key means env has reverted to a pre-rotation key
                // (or is foreign to the chain). Anchoring genesis on it would
                // silently re-root trust away from what the chain records as
                // active — fail closed unless the operator explicitly consents.
                // Covers both "env is an earlier key in the lineage" and "env is
                // foreign to the chain": both are the same re-rooting risk.
                let reversion = matches!(&chain_latest, Some(latest) if *latest != k_env);
                if reversion && !adopt {
                    return Ok(KeyAdmission::MigrationReversionRejected {
                        chain_latest_key_id: chain_latest.unwrap(),
                        env_key_id: k_env,
                    });
                }

                let genesis_sig = b64e.encode(
                    env_key
                        .sign(ledger_signing_payload(&k_env, None, "genesis", &env_pub_b64, now_ms as i64).as_bytes())
                        .to_bytes(),
                );
                let tx = self.durable_mut().transaction()?;
                tx.execute(
                    "INSERT INTO audit_trust_anchor (id, genesis_key_id, created_at_ms) \
                     VALUES (1, ?1, ?2)",
                    params![k_env, now_ms as i64],
                )?;
                tx.execute(
                    "INSERT INTO audit_key_ledger \
                     (key_id, prev_key_id, role, pubkey_b64, signature_b64, created_at_ms) \
                     VALUES (?1, NULL, 'genesis', ?2, ?3, ?4)",
                    params![k_env, env_pub_b64, genesis_sig, now_ms as i64],
                )?;
                // Forensic reconcile of pre-#165 in-process rotations. These keys'
                // private halves are gone (the very bug #165 closes), so the rows
                // carry an EMPTY self-signature (role='backfill') — history for
                // audit, never an active signer. The running env key stays active.
                let mut prev = k_env.clone();
                for (kid, pk) in rotations {
                    if kid == k_env {
                        continue; // env key already represented by the genesis row
                    }
                    tx.execute(
                        "INSERT INTO audit_key_ledger \
                         (key_id, prev_key_id, role, pubkey_b64, signature_b64, created_at_ms) \
                         VALUES (?1, ?2, 'backfill', ?3, '', ?4)",
                        params![kid, prev, pk, now_ms as i64],
                    )?;
                    prev = kid;
                }
                // CONSENTED reversion (reversion && adopt): record an explicit,
                // self-signed `reanchor` ledger row (prev = the chain's latest
                // key) so the operator's decision is a logged record, not a
                // silent anchor. This also makes K_env unambiguously the active
                // (max-seq non-backfill) key.
                if reversion {
                    let reanchor_sig = b64e.encode(
                        env_key
                            .sign(
                                ledger_signing_payload(
                                    &k_env,
                                    chain_latest.as_deref(),
                                    "reanchor",
                                    &env_pub_b64,
                                    now_ms as i64,
                                )
                                .as_bytes(),
                            )
                            .to_bytes(),
                    );
                    tx.execute(
                        "INSERT INTO audit_key_ledger \
                         (key_id, prev_key_id, role, pubkey_b64, signature_b64, created_at_ms) \
                         VALUES (?1, ?2, 'reanchor', ?3, ?4, ?5)",
                        params![k_env, chain_latest, env_pub_b64, reanchor_sig, now_ms as i64],
                    )?;
                }
                tx.commit()?; // FULL → fsync
                self.signing_key = Some(env_key);
                Ok(if reversion {
                    KeyAdmission::AdoptedReanchor
                } else {
                    KeyAdmission::BackfilledGenesis
                })
            }
            // ---- ANCHOR EXISTS: strict admission -----------------------------
            Some(_genesis_id) => {
                let active = self.audit_key_ledger_active_id()?;
                if active.as_deref() == Some(k_env.as_str()) {
                    self.signing_key = Some(env_key);
                    return Ok(KeyAdmission::Resumed);
                }
                // Is the env key present anywhere in the ledger (retired key)?
                let in_ledger = self
                    .audit_key_ledger_rows()?
                    .iter()
                    .any(|r| r.key_id == k_env);
                if in_ledger {
                    // Gap-1: a restart reverted to a retired key. Refuse to sign.
                    return Ok(KeyAdmission::RetiredKeyRejected);
                }
                // New id. Gap-2: only adopt with an explicit operator signal.
                if !adopt {
                    return Ok(KeyAdmission::UnadoptedNewKeyRejected);
                }
                // Consented re-anchor. We cannot sign an in-chain KEY_ROTATION
                // under the old active key (its private half is not in env at
                // boot), so the adopt is recorded as a self-signed `reanchor`
                // ledger row. Prior rows keep verifying under the durable genesis
                // anchor; new rows verify under this adopted key (it enters the
                // keyring via `audit_keyring_seed`'s self-attested-ledger pass).
                let prev_active = active.clone();
                let reanchor_sig = b64e.encode(
                    env_key
                        .sign(
                            ledger_signing_payload(
                                &k_env,
                                prev_active.as_deref(),
                                "reanchor",
                                &env_pub_b64,
                                now_ms as i64,
                            )
                            .as_bytes(),
                        )
                        .to_bytes(),
                );
                let tx = self.durable_mut().transaction()?;
                tx.execute(
                    "INSERT INTO audit_key_ledger \
                     (key_id, prev_key_id, role, pubkey_b64, signature_b64, created_at_ms) \
                     VALUES (?1, ?2, 'reanchor', ?3, ?4, ?5)",
                    params![k_env, prev_active, env_pub_b64, reanchor_sig, now_ms as i64],
                )?;
                tx.commit()?; // FULL → fsync
                self.signing_key = Some(env_key);
                Ok(KeyAdmission::AdoptedReanchor)
            }
        }
    }

    pub fn save_node(&self, node: &RegisteredNode) -> Result<()> {
        let status_json = serde_json::to_string(&node.status)
            .map_err(|_| rusqlite::Error::InvalidQuery)?;

        self.conn.execute(
            "INSERT OR REPLACE INTO nodes
             (node_id, status_json, registered_at_ms, last_trust_update_ms,
              ak_public_pem, expected_pcr16_digest_hex)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                node.node_id,
                status_json,
                node.registered_at_ms as i64,
                node.last_trust_update_ms as i64,
                node.ak_public_pem,
                node.expected_pcr16_digest_hex,
            ],
        )?;

        Ok(())
    }

    pub fn load_nodes(&self) -> Result<Vec<RegisteredNode>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, status_json, registered_at_ms, last_trust_update_ms,
                    ak_public_pem, expected_pcr16_digest_hex
             FROM nodes",
        )?;

        let rows = stmt.query_map([], |row| {
            let status_json: String = row.get(1)?;
            let status: NodeTrustState = serde_json::from_str(&status_json)
                .unwrap_or(NodeTrustState::Unknown);

            Ok(RegisteredNode {
                node_id: row.get(0)?,
                status,
                registered_at_ms: row.get::<_, i64>(2)? as u64,
                last_trust_update_ms: row.get::<_, i64>(3)? as u64,
                ak_public_pem: row.get(4)?,
                expected_pcr16_digest_hex: row.get(5)?,
            })
        })?;

        rows.collect()
    }

    /// Load a single registered node by id, or `None` if unregistered. Additive
    /// single-row loader (mirrors [`load_operator`] / `load_trusted_federation_controller_key`)
    /// — the targeted lookup [`crate::key_registry::KeyRegistry`] uses to resolve a
    /// node's `ak_public_pem` without scanning the whole registry.
    pub fn load_node(&self, node_id: &str) -> Result<Option<RegisteredNode>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT node_id, status_json, registered_at_ms, last_trust_update_ms,
                        ak_public_pem, expected_pcr16_digest_hex
                 FROM nodes WHERE node_id = ?1",
                params![node_id],
                |row| {
                    let status_json: String = row.get(1)?;
                    let status: NodeTrustState =
                        serde_json::from_str(&status_json).unwrap_or(NodeTrustState::Unknown);
                    Ok(RegisteredNode {
                        node_id: row.get(0)?,
                        status,
                        registered_at_ms: row.get::<_, i64>(2)? as u64,
                        last_trust_update_ms: row.get::<_, i64>(3)? as u64,
                        ak_public_pem: row.get(4)?,
                        expected_pcr16_digest_hex: row.get(5)?,
                    })
                },
            )
            .optional()
    }

    pub fn save_dependencies(&self, node_id: &str, deps: &[String]) -> Result<()> {
        self.conn.execute(
            "DELETE FROM dependencies WHERE node_id = ?1",
            params![node_id],
        )?;

        for dep in deps {
            self.conn.execute(
                "INSERT OR REPLACE INTO dependencies (node_id, dep_id) VALUES (?1, ?2)",
                params![node_id, dep],
            )?;
        }

        Ok(())
    }

    pub fn load_dependencies(&self) -> Result<HashMap<String, Vec<String>>> {
        let mut stmt = self.conn.prepare("SELECT node_id, dep_id FROM dependencies")?;
        let mut map: HashMap<String, Vec<String>> = HashMap::new();

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        for row in rows {
            let (node_id, dep_id) = row?;
            map.entry(node_id).or_default().push(dep_id);
        }

        Ok(map)
    }

    // --- v0.9.7 posture event log -------------------------------------------

    /// Plain (non-chained) posture-event insert. **TEST-ONLY** — gated
    /// `#[cfg(test)]` after the audit-chain-bypass fix so production code
    /// cannot reintroduce a write that misses the SHA-256 hash chain.
    /// Production writes go through `save_posture_event_chained` exclusively.
    #[cfg(test)]
    pub(crate) fn save_posture_event(
        &self,
        node_id: &str,
        event_type: &str,
        posture_json: &str,
        reason: Option<&str>,
        created_at_ms: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO posture_events
             (node_id, event_type, posture_json, reason, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![node_id, event_type, posture_json, reason, created_at_ms as i64],
        )?;
        Ok(())
    }

    pub fn load_node_history(&self, node_id: &str) -> Result<Vec<serde_json::Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT event_type, posture_json, reason, created_at_ms
             FROM posture_events
             WHERE node_id = ?1
             ORDER BY created_at_ms DESC",
        )?;

        let rows = stmt.query_map(params![node_id], |row| {
            let event_type: String = row.get(0)?;
            let posture_json: String = row.get(1)?;
            let reason: Option<String> = row.get(2)?;
            let created_at_ms: i64 = row.get(3)?;

            let posture: serde_json::Value = serde_json::from_str(&posture_json)
                .unwrap_or(serde_json::Value::Null);

            Ok(serde_json::json!({
                "event_type": event_type,
                "posture": posture,
                "reason": reason,
                "created_at_ms": created_at_ms as u64,
            }))
        })?;

        rows.collect()
    }

    pub fn count_recent_posture_events(&self, node_id: &str, since_ms: u64) -> Result<u64> {
        self.conn.query_row(
            "SELECT COUNT(*) FROM posture_events
             WHERE node_id = ?1 AND created_at_ms >= ?2",
            params![node_id, since_ms as i64],
            |row| row.get(0),
        )
    }

    // --- v0.9.8 HA probes & backup export ---

    pub fn health_check(&self) -> Result<()> {
        self.conn.query_row("SELECT 1", [], |_| Ok(()))
    }

    /// SG-008 startup-invariant support: true when the hot connection reports
    /// `journal_mode = wal`. `new()` sets `PRAGMA journal_mode=WAL`, so a
    /// file-backed store reports "wal"; a `:memory:` store reports "memory"
    /// (WAL is unavailable for in-memory DBs). The startup sentinel reads this
    /// to fail closed before binding the listener if the DB is not in WAL mode.
    pub fn is_wal_mode(&self) -> bool {
        self.conn
            .query_row("PRAGMA journal_mode;", [], |r| r.get::<_, String>(0))
            .map(|m| m.eq_ignore_ascii_case("wal"))
            .unwrap_or(false)
    }

    pub fn load_all_posture_events(&self) -> Result<Vec<serde_json::Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, event_type, posture_json, reason, created_at_ms
             FROM posture_events
             ORDER BY created_at_ms ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            let node_id: String = row.get(0)?;
            let event_type: String = row.get(1)?;
            let posture_json: String = row.get(2)?;
            let reason: Option<String> = row.get(3)?;
            let created_at_ms: i64 = row.get(4)?;

            let posture: serde_json::Value = serde_json::from_str(&posture_json)
                .unwrap_or(serde_json::Value::Null);

            Ok(serde_json::json!({
                "node_id": node_id,
                "event_type": event_type,
                "posture": posture,
                "reason": reason,
                "created_at_ms": created_at_ms as u64,
            }))
        })?;

        rows.collect()
    }

    // --- v1.1 tamper-evident audit chain ------------------------------------

    fn init_audit_chain_schema(conn: &Connection) -> Result<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS audit_log_chain (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type        TEXT NOT NULL,
                event_json        TEXT NOT NULL,
                previous_hash_hex TEXT NOT NULL,
                record_hash_hex   TEXT NOT NULL,
                created_at_ms     INTEGER NOT NULL,
                signature_b64     TEXT
            )",
            [],
        )?;
        // Ignore "duplicate column name" error — column may already exist on upgraded databases
        if let Err(e) = conn.execute("ALTER TABLE audit_log_chain ADD COLUMN signature_b64 TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
        // Hash-v2 migration columns (additive, defaulted, idempotent).
        // Existing rows: hash_version=1, sequence=NULL — verified with v1 algorithm.
        // New rows: hash_version=2 + monotonic sequence — see audit_chain::compute_record_hash_v2.
        if let Err(e) = conn.execute(
            "ALTER TABLE audit_log_chain ADD COLUMN hash_version INTEGER NOT NULL DEFAULT 1",
            [],
        ) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
        if let Err(e) = conn.execute(
            "ALTER TABLE audit_log_chain ADD COLUMN sequence INTEGER",
            [],
        ) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
        // Key-rotation support (#76): content-addressed id of the signing key
        // per row. NULL on pre-upgrade rows (all signed under the genesis key);
        // backfilled by ensure_key_id_backfill_migration.
        if let Err(e) = conn.execute(
            "ALTER TABLE audit_log_chain ADD COLUMN key_id TEXT",
            [],
        ) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
        conn.execute(
            "CREATE TABLE IF NOT EXISTS federated_trust_reports (
                id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                source_controller_id TEXT NOT NULL,
                asset_id             TEXT NOT NULL,
                posture_json         TEXT NOT NULL,
                issued_at_ms         INTEGER NOT NULL,
                expires_at_ms        INTEGER NOT NULL,
                received_at_ms       INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS trusted_federation_controllers (
                controller_id    TEXT PRIMARY KEY,
                public_key_b64   TEXT NOT NULL,
                registered_at_ms INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS federation_report_nonces (
                nonce_hex            TEXT PRIMARY KEY,
                source_controller_id TEXT NOT NULL,
                seen_at_ms           INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS attestation_identity_registry (
                node_id                    TEXT PRIMARY KEY,
                ak_public_fingerprint_hex  TEXT NOT NULL,
                registered_at_ms           INTEGER NOT NULL,
                registration_source        TEXT NOT NULL
            )",
            [],
        )?;
        Ok(())
    }

    /// Audit-chained posture-event insert. **All production posture-event
    /// writes MUST go through this function**; the non-chained inserter is
    /// `#[cfg(test)]`-only so events cannot bypass the audit chain. Writes
    /// the posture row and the corresponding `AuditChainLinker` entry in
    /// the same SQLite transaction so the chain is never partially updated.
    pub fn save_posture_event_chained(
        &mut self,
        node_id: &str,
        event_type: &str,
        posture_json: &str,
        reason: Option<&str>,
        created_at_ms: u64,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO posture_events
             (node_id, event_type, posture_json, reason, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![node_id, event_type, posture_json, reason, created_at_ms as i64],
        )?;
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx, event_type, posture_json, created_at_ms as i64, self.signing_key.as_ref(),
        )?;
        tx.commit()
    }

    /// True iff `node_id` is a registered node — clearance-grant well-formedness
    /// (operator-console Phase A; mirrors `OperatorClearanceGrant::is_well_formed`'s
    /// "the node must exist" half).
    pub fn node_exists(&self, node_id: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM nodes WHERE node_id = ?1",
            params![node_id],
            |row| row.get(0),
        )?;
        Ok(n > 0)
    }

    /// Persist a supervisor clearance grant — **RECORD-ONLY** (operator-console
    /// Phase A). One transaction writes the `clearance_grants` row (Phase-B
    /// pickup, `delivery = PENDING-NODE-TRANSPORT`) AND appends a signed,
    /// hash-chained `OperatorClearanceGrantIssued` audit event. It records and
    /// signs the grant; it does **NOT** release the node — delivery to the node's
    /// `ClearanceLoop` is Phase B. Mirrors `save_posture_event_chained`'s
    /// table+chain transaction. Does not touch nodes / posture. Returns the row id.
    pub fn save_clearance_grant_chained(
        &mut self,
        node_id: &str,
        operator_id: &str,
        granted_at_ms: u64,
    ) -> Result<i64> {
        // Backward-compatible: existing callers (Phase-B tests, the fleet relay,
        // the demo seed) record with auth_method = "unspecified". The auth-aware
        // path below is what the upgraded console route uses.
        self.save_clearance_grant_chained_with_auth(
            node_id, operator_id, granted_at_ms, "unspecified", None,
        )
    }

    /// Record a clearance grant with its **authorization provenance** (#314 Phase
    /// 1) — `auth_method` (`operator-signed` / `supervisor-break-glass` /
    /// `unspecified`) and the signing operator's key `fingerprint`. Both are
    /// written to the (additive) grant columns AND embedded in the
    /// `OperatorClearanceGrantIssued` chain event — the non-repudiation payoff: the
    /// signed ledger records WHO authorized the release and with WHICH key. The
    /// `PENDING-NODE-TRANSPORT` row + the columns Phase-B reads are unchanged.
    pub fn save_clearance_grant_chained_with_auth(
        &mut self,
        node_id: &str,
        operator_id: &str,
        granted_at_ms: u64,
        auth_method: &str,
        operator_key_fingerprint: Option<&str>,
    ) -> Result<i64> {
        let payload = serde_json::json!({
            "node_id": node_id,
            "operator_id": operator_id,
            "granted_at_ms": granted_at_ms,
            "delivery": "PENDING-NODE-TRANSPORT",
            "auth_method": auth_method,
            "operator_key_fingerprint": operator_key_fingerprint,
        })
        .to_string();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO clearance_grants
             (node_id, operator_id, granted_at_ms, delivery, created_at_ms,
              auth_method, operator_key_fingerprint)
             VALUES (?1, ?2, ?3, 'PENDING-NODE-TRANSPORT', ?4, ?5, ?6)",
            params![
                node_id,
                operator_id,
                granted_at_ms as i64,
                granted_at_ms as i64,
                auth_method,
                operator_key_fingerprint,
            ],
        )?;
        let id = tx.last_insert_rowid();
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx,
            "OperatorClearanceGrantIssued",
            &payload,
            granted_at_ms as i64,
            self.signing_key.as_ref(),
        )?;
        tx.commit()?;
        Ok(id)
    }

    // --- #314 Phase 1 — operator registry -----------------------------------

    /// Register (or re-register / rotate) an operator's Ed25519 PUBLIC key.
    /// Re-registration overwrites the key and CLEARS any prior revocation (a fresh
    /// key for an operator is an active operator). Admin-gated at the route layer.
    pub fn register_operator(
        &mut self,
        operator_id: &str,
        pubkey_pem: &str,
        now_ms: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO operators (operator_id, pubkey_pem, registered_at_ms, revoked_at_ms)
             VALUES (?1, ?2, ?3, NULL)
             ON CONFLICT(operator_id) DO UPDATE SET
                 pubkey_pem = excluded.pubkey_pem,
                 registered_at_ms = excluded.registered_at_ms,
                 revoked_at_ms = NULL",
            params![operator_id, pubkey_pem, now_ms as i64],
        )?;
        Ok(())
    }

    /// Revoke an operator (sets `revoked_at_ms`). Returns `true` if an ACTIVE
    /// operator was revoked, `false` if absent or already revoked.
    pub fn revoke_operator(&mut self, operator_id: &str, now_ms: u64) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE operators SET revoked_at_ms = ?2
             WHERE operator_id = ?1 AND revoked_at_ms IS NULL",
            params![operator_id, now_ms as i64],
        )?;
        Ok(n > 0)
    }

    /// Load an operator record (active or revoked), or `None` if unregistered.
    pub fn load_operator(&self, operator_id: &str) -> Result<Option<OperatorRecord>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT operator_id, pubkey_pem, registered_at_ms, revoked_at_ms
                 FROM operators WHERE operator_id = ?1",
                params![operator_id],
                |row| {
                    Ok(OperatorRecord {
                        operator_id: row.get(0)?,
                        pubkey_pem: row.get(1)?,
                        registered_at_ms: row.get::<_, i64>(2)? as u64,
                        revoked_at_ms: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                    })
                },
            )
            .optional()
    }

    /// Read-only listing of every registered operator. `pubkey_pem` is the
    /// PUBLIC key; the handler exposes only its fingerprint.
    pub fn load_operators(&self) -> Result<Vec<OperatorRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT operator_id, pubkey_pem, registered_at_ms, revoked_at_ms
             FROM operators ORDER BY operator_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(OperatorRecord {
                operator_id: row.get(0)?,
                pubkey_pem: row.get(1)?,
                registered_at_ms: row.get::<_, i64>(2)? as u64,
                revoked_at_ms: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
            })
        })?;
        rows.collect()
    }

    /// Append a signed, hash-chained console audit event with **no** other table
    /// write — used for REJECTED clearance attempts (unauthorized / malformed).
    /// The `payload_json` must NEVER contain the supervisor key bytes (outcome +
    /// reason + attempted ids only).
    pub fn append_clearance_audit_event(
        &mut self,
        event_type: &str,
        payload_json: &str,
        created_at_ms: u64,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx,
            event_type,
            payload_json,
            created_at_ms as i64,
            self.signing_key.as_ref(),
        )?;
        tx.commit()
    }

    /// **THE ONE-SHOT CONSUME** (operator-console Phase B). In a SINGLE atomic
    /// `UPDATE … RETURNING`, marks the OLDEST unconsumed grant for `node_id`
    /// consumed (`consumed_at_ms = now_ms`) and returns it. A grant can be taken
    /// **exactly once, ever** — double-pickup is impossible by the
    /// `consumed_at_ms IS NULL` guard combined with the atomic single-statement
    /// update (SQLite serializes writers), NOT by convention. This is the
    /// store-level verify-then-consume pattern — the same single-use discipline
    /// as the attestation nonce (`AppState::consume_challenge`, `src/verifier.rs`:
    /// atomically removes the pending entry so a replay finds nothing). Returns
    /// `None` when no pending grant exists (idempotent-empty pickup).
    pub fn take_pending_clearance_grant(
        &self,
        node_id: &str,
        now_ms: u64,
    ) -> Result<Option<PendingClearanceGrant>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "UPDATE clearance_grants
                    SET consumed_at_ms = ?2
                  WHERE id = (
                    SELECT id FROM clearance_grants
                     WHERE node_id = ?1 AND consumed_at_ms IS NULL
                     ORDER BY id ASC LIMIT 1)
                  RETURNING id, node_id, operator_id, granted_at_ms",
                params![node_id, now_ms as i64],
                |row| {
                    Ok(PendingClearanceGrant {
                        rowid: row.get(0)?,
                        node_id: row.get(1)?,
                        operator_id: row.get(2)?,
                        granted_at_ms: row.get::<_, i64>(3)? as u64,
                    })
                },
            )
            .optional()
    }

    /// Record a delivered grant's OUTCOME (operator-console Phase B): the
    /// `ClearanceLoop` verdict at delivery, plus the chained audit event — ONE
    /// signed transaction, same shape as Phase A's writes. `outcome == "Cleared"`
    /// → a `ClearanceDelivered` event; any other `outcome` (a rejection reason
    /// code) → a `ClearanceDeliveryRejected` event. `now_ms` is supplied (no
    /// `SystemTime::now()` in the store).
    pub fn record_grant_outcome(
        &mut self,
        grant_rowid: i64,
        outcome: &str,
        detail: Option<&str>,
        now_ms: u64,
    ) -> Result<()> {
        let event_type = if outcome == "Cleared" {
            "ClearanceDelivered"
        } else {
            "ClearanceDeliveryRejected"
        };
        let payload = serde_json::json!({
            "grant_rowid": grant_rowid,
            "outcome": outcome,
            "detail": detail,
        })
        .to_string();
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE clearance_grants SET outcome = ?2, outcome_detail = ?3 WHERE id = ?1",
            params![grant_rowid, outcome, detail],
        )?;
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx,
            event_type,
            &payload,
            now_ms as i64,
            self.signing_key.as_ref(),
        )?;
        tx.commit()
    }

    /// The most recent clearance grant's delivery state for `node_id` (console
    /// read surface, Phase B). `None` if the node has no grants.
    pub fn latest_clearance_grant(&self, node_id: &str) -> Result<Option<ClearanceGrantState>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT granted_at_ms, consumed_at_ms, outcome, outcome_detail
                   FROM clearance_grants WHERE node_id = ?1 ORDER BY id DESC LIMIT 1",
                params![node_id],
                |row| {
                    Ok(ClearanceGrantState {
                        granted_at_ms: row.get::<_, i64>(0)? as u64,
                        consumed_at_ms: row.get::<_, Option<i64>>(1)?.map(|v| v as u64),
                        outcome: row.get(2)?,
                        outcome_detail: row.get(3)?,
                    })
                },
            )
            .optional()
    }

    pub fn save_federated_report_chained(
        &mut self,
        report: &FederatedTrustReport,
        received_at_ms: u64,
        held_epoch: u64,
    ) -> std::result::Result<(), DurableWriteError> {
        // #74: route the whole federation commit — report + NONCE BURN + audit —
        // through the FULL (force-synced) connection. A burned nonce must survive
        // power-loss or anti-replay is defeated (the 5 s freshness window only
        // partially bounds replay). Federation reports are rare, so the per-commit
        // fsync is off the hot path and inconsequential to throughput.
        let signing_key = self.signing_key.clone(); // durable_mut() borrows self
        // #79: IMMEDIATE so the durable write lock is held before the epoch
        // re-check below — no concurrent `try_claim_epoch` can interleave
        // between the fence read and this commit.
        let tx = self
            .durable_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // #79 HA epoch fence — FIRST statement, before any mutation. A node
        // fenced after the request-path gate check cannot land a stale report.
        Self::assert_epoch_held(&tx, held_epoch)?;

        let posture_json = serde_json::to_string(&report.posture)
            .map_err(|_| rusqlite::Error::InvalidQuery)?;

        tx.execute(
            "INSERT INTO federated_trust_reports
             (source_controller_id, asset_id, posture_json, issued_at_ms, expires_at_ms, received_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                report.source_controller_id, report.asset_id, posture_json,
                report.issued_at_ms as i64, report.expires_at_ms as i64, received_at_ms as i64,
            ],
        )?;

        tx.execute(
            "INSERT INTO federation_report_nonces (nonce_hex, source_controller_id, seen_at_ms)
             VALUES (?1, ?2, ?3)",
            params![report.nonce_hex, report.source_controller_id, received_at_ms as i64],
        )?;

        let audit = serde_json::json!({
            "source_controller_id": report.source_controller_id,
            "asset_id": report.asset_id,
            "posture": posture_json,
            "issued_at_ms": report.issued_at_ms,
            "expires_at_ms": report.expires_at_ms,
            "nonce_hex": report.nonce_hex,
            "received_at_ms": received_at_ms,
        });
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx,
            "FEDERATED_TRUST_REPORT_ACCEPTED",
            &audit.to_string(),
            received_at_ms as i64,
            signing_key.as_ref(),
        )?;

        tx.commit()?;
        Ok(())
    }

    // --- v1.1 trusted federation controller registry ------------------------

    pub fn save_trusted_federation_controller(
        &self,
        controller_id: &str,
        public_key_b64: &str,
        registered_at_ms: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO trusted_federation_controllers
             (controller_id, public_key_b64, registered_at_ms)
             VALUES (?1, ?2, ?3)",
            params![controller_id, public_key_b64, registered_at_ms as i64],
        )?;
        Ok(())
    }

    pub fn load_trusted_federation_controller_key(
        &self,
        controller_id: &str,
    ) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT public_key_b64 FROM trusted_federation_controllers
             WHERE controller_id = ?1",
        )?;
        match stmt.query_row(params![controller_id], |row| row.get::<_, String>(0)) {
            Ok(key) => Ok(Some(key)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn has_seen_federation_nonce(&self, nonce_hex: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM federation_report_nonces WHERE nonce_hex = ?1",
            params![nonce_hex],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Atomically *burn* a nonce: claim it on first use, reject it on replay.
    ///
    /// Returns `Ok(true)` if the nonce was newly recorded (first time seen →
    /// the caller may proceed) and `Ok(false)` if it was already present (a
    /// replay → the caller must reject). This is the verify-AND-consume primitive
    /// the untrusted fleet carrier needs: a single `INSERT OR IGNORE` against the
    /// `nonce_hex PRIMARY KEY` makes the claim atomic — there is no check-then-act
    /// window for two concurrent ingests of the same captured payload to both win.
    ///
    /// The write rides the `synchronous=FULL` durable connection (same as the
    /// federation report nonce burn), falling back to the main connection for an
    /// in-memory store. The `seen_at_ms` column is diagnostic only — replay
    /// correctness depends solely on the primary-key conflict, never on the clock.
    pub fn burn_federation_nonce(&self, nonce_hex: &str) -> Result<bool> {
        let seen_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let changed = self.durable_ref().execute(
            "INSERT OR IGNORE INTO federation_report_nonces
                 (nonce_hex, source_controller_id, seen_at_ms)
             VALUES (?1, ?2, ?3)",
            params![nonce_hex, "fleet-grant-lane", seen_at_ms],
        )?;
        Ok(changed > 0)
    }

    pub fn load_federated_reports_for_asset(
        &self,
        asset_id: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_controller_id, asset_id, posture_json, issued_at_ms, expires_at_ms
             FROM federated_trust_reports
             WHERE asset_id = ?1
             ORDER BY received_at_ms DESC",
        )?;
        let rows = stmt.query_map(params![asset_id], |row| {
            let source: String = row.get(0)?;
            let aid: String = row.get(1)?;
            let posture_json: String = row.get(2)?;
            let issued: i64 = row.get(3)?;
            let expires: i64 = row.get(4)?;
            Ok(serde_json::json!({
                "source_controller_id": source,
                "asset_id": aid,
                "posture": posture_json,
                "issued_at_ms": issued as u64,
                "expires_at_ms": expires as u64,
            }))
        })?;
        rows.collect()
    }

    /// Reconstruct the audit-chain keyring (key_id → VerifyingKey) by replaying
    /// the chain's `KEY_ROTATION` events in id order, bootstrapped from the
    /// GENESIS verifying key (#76). Each rotation row is signed by the PRIOR
    /// (already-trusted) key and carries the NEW key's pubkey + key_id; a
    /// rotation is only honored if it verifies under a key already in the ring
    /// AND the announced key_id matches the announced pubkey's fingerprint.
    /// The chain is thus self-describing — no external key-registry table (which
    /// would be mutable, un-anchored trust state). Genesis is the only anchor.
    fn build_audit_keyring(
        &self,
        genesis_vk: &ed25519_dalek::VerifyingKey,
    ) -> Result<std::collections::HashMap<String, ed25519_dalek::VerifyingKey>> {
        // #165: seed genesis from the DURABLE anchor (self-attested ledger keys
        // included); the passed-in `genesis_vk` is only the pre-#165 fallback.
        let (mut keyring, genesis_id_opt) = self.audit_keyring_seed(Some(genesis_vk))?;
        let genesis_id =
            genesis_id_opt.unwrap_or_else(|| crate::audit_chain::verifying_key_id(genesis_vk));

        let mut stmt = self.conn.prepare(
            "SELECT event_json, previous_hash_hex, record_hash_hex, created_at_ms, \
             signature_b64, hash_version, sequence, key_id \
             FROM audit_log_chain WHERE event_type = 'KEY_ROTATION' ORDER BY id ASC",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let event_json: String = row.get(0)?;
            let prev: String = row.get(1)?;
            let rec: String = row.get(2)?;
            let ts: i64 = row.get(3)?;
            let sig_b64: Option<String> = row.get(4)?;
            let hash_version: i64 = row.get(5)?;
            let seq: Option<i64> = row.get(6)?;
            let key_id: Option<String> = row.get(7)?;

            // The rotation must be signed by a key already trusted (the prior
            // key). A NULL key_id means a legacy/genesis-signed row.
            let signer_id = key_id.unwrap_or_else(|| genesis_id.clone());
            let Some(signer_vk) = keyring.get(&signer_id).copied() else { continue };
            let Some(ref sig) = sig_b64 else { continue };
            let payload = audit_signing_payload(hash_version, &prev, &rec, "KEY_ROTATION", ts, seq);
            if !audit_verify_sig(&signer_vk, &payload, sig) {
                continue; // an unverifiable rotation introduces no new trust
            }
            extend_keyring_from_rotation(&mut keyring, &event_json);
        }
        Ok(keyring)
    }

    pub fn verify_audit_chain_full(
        &self,
        verifying_key: Option<&ed25519_dalek::VerifyingKey>,
    ) -> Result<AuditChainVerifyResult> {
        // SELECT now includes event_type + hash_version + sequence + key_id so
        // the verifier can dispatch per hash_version and select the verifying
        // key PER ROW (#76).
        let mut stmt = self.conn.prepare(
            "SELECT id, event_type, event_json, previous_hash_hex, record_hash_hex, \
             created_at_ms, signature_b64, hash_version, sequence, key_id \
             FROM audit_log_chain ORDER BY id ASC",
        )?;

        // Keyring seeded from the DURABLE anchor (#165) — genesis from
        // `audit_trust_anchor` + self-attested ledger keys — falling back to the
        // passed-in env key only for pre-#165 chains. Extended in id order as
        // verified KEY_ROTATION rows are encountered. A signed row is verified
        // under the key its key_id names — old rows under their ORIGINAL key.
        let (mut keyring, genesis_id) = self.audit_keyring_seed(verifying_key)?;

        let mut chain_intact = true;
        let mut total_entries: u64 = 0;
        let mut latest_hash = "0".repeat(64);
        let mut expected_previous_hash = "0".repeat(64);
        let mut signed_entries: u64 = 0;
        let mut unsigned_entries: u64 = 0;
        let mut signature_valid = true;
        let mut first_invalid_signature_index: Option<u64> = None;
        let mut first_signed_at_ms: Option<u64> = None;
        // Last-seen v2 sequence; v2 rows must monotonically increment by 1.
        let mut prev_v2_seq: Option<i64> = None;
        // #77: the last row's sequence (chain tail), for the anchor-head check.
        let mut last_sequence: Option<i64> = None;

        let mut rows = stmt.query([])?;

        while let Some(row) = rows.next()? {
            let _id: i64 = row.get(0)?;
            let event_type: String = row.get(1)?;
            let event_json: String = row.get(2)?;
            let previous_hash_hex: String = row.get(3)?;
            let record_hash_hex: String = row.get(4)?;
            let created_at_ms: i64 = row.get(5)?;
            let sig_b64: Option<String> = row.get(6)?;
            let hash_version: i64 = row.get(7)?;
            let sequence_opt: Option<i64> = row.get(8)?;
            let key_id_opt: Option<String> = row.get(9)?;

            // Chain linkage check applies to every row regardless of version.
            if previous_hash_hex != expected_previous_hash {
                chain_intact = false;
            }
            // Recompute hash per version. v1 omits event_type (relabeling
            // weakness retained for legacy rows); v2 binds event_type and
            // sequence so this same cheap check catches relabeling/reorder.
            let recalc = match hash_version {
                1 => crate::audit_chain::AuditChainLinker::compute_record_hash_v1(
                    &previous_hash_hex,
                    &event_json,
                    created_at_ms,
                ),
                2 => {
                    let seq = sequence_opt.unwrap_or(-1).max(0) as u64;
                    // v2 sequence monotonicity: each v2 row must be prev_v2 + 1.
                    if let Some(prev) = prev_v2_seq {
                        if sequence_opt != Some(prev + 1) {
                            chain_intact = false;
                        }
                    } else {
                        // First v2 row must start at sequence 0.
                        if sequence_opt != Some(0) {
                            chain_intact = false;
                        }
                    }
                    prev_v2_seq = sequence_opt;
                    crate::audit_chain::AuditChainLinker::compute_record_hash_v2(
                        &previous_hash_hex,
                        &event_type,
                        &event_json,
                        created_at_ms,
                        seq,
                    )
                }
                _ => {
                    // Unknown hash version — fail closed.
                    chain_intact = false;
                    String::new()
                }
            };
            if recalc != record_hash_hex {
                chain_intact = false;
            }
            expected_previous_hash = record_hash_hex.clone();
            latest_hash = record_hash_hex.clone();
            last_sequence = sequence_opt; // #77: track the chain tail's sequence

            // Signature verification — select the verifying key PER ROW by its
            // key_id from the keyring (#76). Old rows verify under their ORIGINAL
            // key; a verified KEY_ROTATION extends the keyring for later rows.
            match &sig_b64 {
                None => {
                    unsigned_entries += 1;
                }
                Some(s) => {
                    signed_entries += 1;
                    if first_signed_at_ms.is_none() {
                        first_signed_at_ms = Some(created_at_ms as u64);
                    }
                    if verifying_key.is_some() {
                        // NULL key_id = a pre-backfill row, signed under genesis.
                        let signer_id = key_id_opt
                            .clone()
                            .or_else(|| genesis_id.clone())
                            .unwrap_or_default();
                        let ok = match keyring.get(&signer_id) {
                            Some(vk) => {
                                let payload = audit_signing_payload(
                                    hash_version, &previous_hash_hex, &record_hash_hex,
                                    &event_type, created_at_ms, sequence_opt,
                                );
                                audit_verify_sig(vk, &payload, s)
                            }
                            // Unknown key_id → FAIL-CLOSED (not a skip).
                            None => false,
                        };
                        if !ok && first_invalid_signature_index.is_none() {
                            first_invalid_signature_index = Some(total_entries);
                            signature_valid = false;
                        }
                        // Extend trust only via a row that itself verified.
                        if ok && event_type == "KEY_ROTATION" {
                            extend_keyring_from_rotation(&mut keyring, &event_json);
                        }
                    }
                }
            }

            total_entries += 1;
        }

        let signing_enabled = verifying_key.is_some();
        let public_key_b64 = verifying_key.map(|vk| {
            b64e.encode(vk.as_bytes())
        });

        // #77: anchor-HEAD high-water check — detects tail TRUNCATION/DELETION
        // (which the per-row chain walk above cannot see: deleting the last rows
        // leaves the surviving prefix internally consistent). Compare the signed
        // head to the chain tail. Fail-closed (`head_verified = false`) on: head
        // absent on a non-empty chain; tail behind the head (truncation); head
        // signature invalid / unknown key (tamper). Kept SEPARATE from
        // `chain_intact` so an unanchored legacy chain (rows present, no head)
        // does not retroactively flip the row-walk verdict.
        let (head_verified, head_status): (bool, String) = if total_entries == 0 {
            // Empty chain → no head required.
            (true, "EMPTY_CHAIN".to_string())
        } else {
            let head = self.conn.query_row(
                "SELECT sequence, record_hash_hex, signature_b64, key_id \
                 FROM audit_anchor_head WHERE id = 1",
                [],
                |r| Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                )),
            );
            match head {
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    // A properly-opened store backfills the head at startup
                    // (`ensure_audit_anchor_head`); its absence on a non-empty
                    // chain is fail-closed (deleted head or un-migrated store).
                    (false, "HEAD_ABSENT".to_string())
                }
                Err(e) => return Err(e),
                Ok((h_seq, h_hash, h_sig, h_key_id)) => {
                    if Some(h_seq) != last_sequence || h_hash != latest_hash {
                        // Position mismatch. Tail strictly behind the head is the
                        // truncation/deletion case; anything else is a head/tail
                        // divergence (forged rows past the head, reorder, etc.).
                        let truncated = match last_sequence {
                            Some(t) => t < h_seq,
                            None => true,
                        };
                        let status = if truncated { "TRUNCATION_DETECTED" } else { "HEAD_TAIL_MISMATCH" };
                        (false, status.to_string())
                    } else if signing_enabled {
                        // Signed chain ⇒ the head must carry a valid signature
                        // under a known key (same #76 keyring as the rows).
                        match h_sig {
                            None => (false, "HEAD_UNSIGNED".to_string()),
                            Some(sig) => {
                                let signer_id = h_key_id
                                    .or_else(|| genesis_id.clone())
                                    .unwrap_or_default();
                                match keyring.get(&signer_id) {
                                    None => (false, "HEAD_KEY_UNKNOWN".to_string()),
                                    Some(vk) => {
                                        let payload = crate::audit_chain::canonical_anchor_head_payload(
                                            h_seq.max(0) as u64,
                                            &h_hash,
                                        );
                                        if audit_verify_sig(vk, &payload, &sig) {
                                            (true, "OK".to_string())
                                        } else {
                                            (false, "HEAD_SIGNATURE_INVALID".to_string())
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        // Unsigned chain (no verifying key supplied): the head
                        // position matches the tail; there is no signature to check.
                        (true, "OK_UNSIGNED".to_string())
                    }
                }
            }
        };

        Ok(AuditChainVerifyResult {
            chain_intact,
            total_entries,
            latest_hash,
            signing_enabled,
            signed_entries,
            unsigned_entries,
            signature_valid,
            first_invalid_signature_index,
            first_signed_at_ms,
            public_key_b64,
            head_verified,
            head_status,
        })
    }

    pub fn load_audit_chain_page(
        &self,
        limit: u64,
        offset: u64,
        verifying_key: Option<&ed25519_dalek::VerifyingKey>,
    ) -> Result<AuditExportPage> {
        let total: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain",
            [],
            |row| row.get::<_, i64>(0),
        ).map(|n| n as u64)?;

        // Reconstruct the full keyring once (the page is DESC/paginated, so we
        // can't replay rotations within a page) — then annotate each row's
        // signature status under the key its key_id names (#76).
        let genesis_id = verifying_key.map(crate::audit_chain::verifying_key_id);
        let keyring = match verifying_key {
            Some(g) => self.build_audit_keyring(g)?,
            None => std::collections::HashMap::new(),
        };

        let mut stmt = self.conn.prepare(
            "SELECT id, event_type, event_json, previous_hash_hex, record_hash_hex, \
             created_at_ms, signature_b64, hash_version, sequence, key_id \
             FROM audit_log_chain ORDER BY id DESC LIMIT ?1 OFFSET ?2",
        )?;

        let public_key_b64 = verifying_key.map(|vk| b64e.encode(vk.as_bytes()));

        let rows = stmt.query_map(rusqlite::params![limit as i64, offset as i64], |row| {
            let id: i64 = row.get(0)?;
            let event_type: String = row.get(1)?;
            let event_json: String = row.get(2)?;
            let prev_hash: String = row.get(3)?;
            let entry_hash: String = row.get(4)?;
            let timestamp_ms: i64 = row.get(5)?;
            let sig_b64: Option<String> = row.get(6)?;
            let hash_version: i64 = row.get(7)?;
            let sequence_opt: Option<i64> = row.get(8)?;
            let key_id: Option<String> = row.get(9)?;

            Ok((id, event_type, event_json, prev_hash, entry_hash, timestamp_ms,
                sig_b64, hash_version, sequence_opt, key_id))
        })?;

        let mut entries = Vec::new();
        for row_result in rows {
            let (id, event_type, event_json, prev_hash, entry_hash, timestamp_ms,
                 sig_b64, hash_version, sequence_opt, key_id) = row_result?;

            let signature_status = match &sig_b64 {
                None => "unsigned".to_string(),
                Some(s) => {
                    if verifying_key.is_some() {
                        // NULL key_id = pre-backfill row signed under genesis.
                        let signer_id = key_id
                            .clone()
                            .or_else(|| genesis_id.clone())
                            .unwrap_or_default();
                        let verified = match keyring.get(&signer_id) {
                            Some(vk) => {
                                let payload = audit_signing_payload(
                                    hash_version, &prev_hash, &entry_hash,
                                    &event_type, timestamp_ms, sequence_opt,
                                );
                                audit_verify_sig(vk, &payload, s)
                            }
                            None => false, // unknown key_id → fail-closed
                        };
                        if verified { "valid".to_string() } else { "invalid".to_string() }
                    } else {
                        "invalid".to_string()
                    }
                }
            };

            entries.push(AuditExportEntry {
                id,
                timestamp_ms: timestamp_ms as u64,
                event_type,
                source: "verifier".to_string(),
                payload: event_json,
                prev_hash,
                entry_hash,
                signature_b64: sig_b64,
                signature_status,
            });
        }

        let chain_intact = self.verify_audit_chain_integrity()?;

        Ok(AuditExportPage {
            entries,
            total,
            public_key_b64,
            chain_intact,
        })
    }

    /// Rotate the audit signing key (#76). The KEY_ROTATION row is signed by the
    /// OLD key (so it verifies under the prior, trusted key) and records the NEW
    /// key's pubkey + content-addressed key_id in its payload. The in-memory
    /// signing key is then swapped to the NEW key, so subsequent rows sign under
    /// it. The whole operation runs under the store mutex (callers hold the lock)
    /// and the append+swap is one critical section — atomic w.r.t. concurrent
    /// appends, and never a cosmetic rotation.
    ///
    /// Receives the new `SigningKey` (not just the pubkey): the store cannot sign
    /// future rows under a key it does not hold the private half of, so the
    /// public-key-only flow could never actually swap signing.
    pub fn record_key_rotation(
        &mut self,
        new_signing_key: ed25519_dalek::SigningKey,
        reason: &str,
        now_ms: u64,
        held_epoch: u64,
    ) -> std::result::Result<(), DurableWriteError> {
        use ed25519_dalek::Signer;
        let new_vk = new_signing_key.verifying_key();
        let new_public_key_b64 = b64e.encode(new_vk.as_bytes());
        let new_key_id = crate::audit_chain::verifying_key_id(&new_vk);

        // The OLD key signs the in-chain KEY_ROTATION row (so it verifies under a
        // key already trusted). Clone it out before borrowing self mutably for
        // the durable transaction.
        let old_key = self.signing_key.clone();
        let old_key_id = old_key
            .as_ref()
            .map(|k| crate::audit_chain::verifying_key_id(&k.verifying_key()));

        let payload = serde_json::json!({
            "new_public_key_b64": new_public_key_b64,
            "new_key_id": new_key_id,
            "reason": reason,
            "rotated_at_ms": now_ms,
        });

        // The NEW key self-signs its ledger row (binds key_id ↔ pubkey).
        let ledger_sig = b64e.encode(
            new_signing_key
                .sign(
                    ledger_signing_payload(
                        &new_key_id,
                        old_key_id.as_deref(),
                        "rotation",
                        &new_public_key_b64,
                        now_ms as i64,
                    )
                    .as_bytes(),
                )
                .to_bytes(),
        );

        // #165: ONE durable (synchronous=FULL) transaction commits BOTH the
        // in-chain KEY_ROTATION row (signed by the OLD key) AND the durable
        // audit_key_ledger row (self-signed by the NEW key). They are atomic and
        // fsync'd together — both-present-or-neither across a hard restart. Only
        // this rare, security-critical event rides FULL; the per-command audit
        // path stays on the NORMAL connection.
        {
            // #79: IMMEDIATE so the durable write lock is held before the epoch
            // re-check — no concurrent claim can interleave before this commit.
            let tx = self
                .durable_mut()
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            // #79 HA epoch fence — FIRST statement, before any mutation. A node
            // fenced after the request-path gate cannot land a stale rotation.
            Self::assert_epoch_held(&tx, held_epoch)?;
            crate::audit_chain::AuditChainLinker::append_audit_event_tx(
                &tx,
                "KEY_ROTATION",
                &payload.to_string(),
                now_ms as i64,
                old_key.as_ref(),
            )?;
            tx.execute(
                "INSERT INTO audit_key_ledger \
                 (key_id, prev_key_id, role, pubkey_b64, signature_b64, created_at_ms) \
                 VALUES (?1, ?2, 'rotation', ?3, ?4, ?5)",
                params![new_key_id, old_key_id, new_public_key_b64, ledger_sig, now_ms as i64],
            )?;
            tx.commit()?; // FULL → fsync; durable active-key record updated
        }

        // Swap the in-memory signing key to the NEW key AFTER the durable commit
        // (atomic under the store lock the caller holds — no append interleaves).
        // The durable ledger — not the dropped advisory engine-state row — is now
        // the authoritative record of the active key.
        self.signing_key = Some(new_signing_key);
        Ok(())
    }

    /// One-time, idempotent backfill (#76): existing rows have a NULL `key_id`
    /// (they were all signed under the genesis key, since rotation was
    /// previously cosmetic). Assign them the genesis key's id and anchor the
    /// migration with a signed `KEY_ID_BACKFILL` event. NO signatures are
    /// rewritten — only the new `key_id` column is populated. Rides the same
    /// boot-time pattern as `ensure_hash_v2_migration_anchor`.
    pub fn ensure_key_id_backfill_migration(&mut self, now_ms: u64) -> Result<()> {
        // Genesis id from the current signing key (the only key the chain has
        // ever been signed under, pre-rotation). No signing key → nothing to do.
        let genesis_id = match self.signing_key.as_ref() {
            Some(sk) => crate::audit_chain::verifying_key_id(&sk.verifying_key()),
            None => return Ok(()),
        };
        // Idempotent: already anchored?
        let existing: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE event_type = 'KEY_ID_BACKFILL'",
            [],
            |r| r.get(0),
        )?;
        if existing > 0 {
            return Ok(());
        }
        let null_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE key_id IS NULL",
            [],
            |r| r.get(0),
        )?;
        if null_count == 0 {
            // Brand-new chain (rows already carry key_id) — nothing to backfill.
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE audit_log_chain SET key_id = ?1 WHERE key_id IS NULL",
            params![genesis_id],
        )?;
        let payload = format!(
            "{{\"genesis_key_id\":\"{genesis_id}\",\"backfilled_rows\":{null_count},\"migrated_at_ms\":{now_ms}}}"
        );
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx,
            "KEY_ID_BACKFILL",
            &payload,
            now_ms as i64,
            self.signing_key.as_ref(),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn verify_audit_chain_integrity(&self) -> Result<bool> {
        // Cheap hash-only integrity check. Post hash-v2 migration this
        // catches event_type relabeling and v2 sequence reorder/gaps on
        // v2 rows without needing the signing key. v1 rows retain the
        // pre-migration relabeling weakness (cannot be retroactively
        // strengthened without destructive rewrite) — that boundary is
        // anchored by the HASH_V2_MIGRATION event.
        let mut stmt = self.conn.prepare(
            "SELECT event_type, event_json, previous_hash_hex, record_hash_hex, \
             created_at_ms, hash_version, sequence \
             FROM audit_log_chain ORDER BY id ASC",
        )?;

        let mut expected_previous_hash = "0".repeat(64);
        let mut prev_v2_seq: Option<i64> = None;
        let mut rows = stmt.query([])?;

        while let Some(row) = rows.next()? {
            let event_type: String = row.get(0)?;
            let event_json: String = row.get(1)?;
            let previous_hash_hex: String = row.get(2)?;
            let record_hash_hex: String = row.get(3)?;
            let created_at_ms: i64 = row.get(4)?;
            let hash_version: i64 = row.get(5)?;
            let sequence_opt: Option<i64> = row.get(6)?;

            if previous_hash_hex != expected_previous_hash {
                return Ok(false);
            }
            let recalc = match hash_version {
                1 => crate::audit_chain::AuditChainLinker::compute_record_hash_v1(
                    &previous_hash_hex,
                    &event_json,
                    created_at_ms,
                ),
                2 => {
                    let seq = sequence_opt.unwrap_or(-1).max(0) as u64;
                    if let Some(prev) = prev_v2_seq {
                        if sequence_opt != Some(prev + 1) {
                            return Ok(false);
                        }
                    } else if sequence_opt != Some(0) {
                        return Ok(false);
                    }
                    prev_v2_seq = sequence_opt;
                    crate::audit_chain::AuditChainLinker::compute_record_hash_v2(
                        &previous_hash_hex,
                        &event_type,
                        &event_json,
                        created_at_ms,
                        seq,
                    )
                }
                _ => return Ok(false), // unknown version → fail closed
            };
            if recalc != record_hash_hex {
                return Ok(false);
            }
            expected_previous_hash = record_hash_hex;
        }

        Ok(true)
    }

    // --- Patch 1: attestation identity registry ----------------------------

    pub fn register_attestation_identity(
        &mut self,
        node_id: &str,
        fingerprint_hex: &str,
        source: &str,
        registered_at_ms: u64,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;

        tx.execute(
            "INSERT OR REPLACE INTO attestation_identity_registry
             (node_id, ak_public_fingerprint_hex, registered_at_ms, registration_source)
             VALUES (?1, ?2, ?3, ?4)",
            params![node_id, fingerprint_hex, registered_at_ms as i64, source],
        )?;

        let audit_payload = serde_json::json!({
            "node_id": node_id,
            "ak_public_fingerprint_hex": fingerprint_hex,
            "registration_source": source,
            "registered_at_ms": registered_at_ms,
        });
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx,
            "NODE_IDENTITY_REGISTERED",
            &audit_payload.to_string(),
            registered_at_ms as i64,
            self.signing_key.as_ref(),
        )?;

        tx.commit()
    }

    pub fn load_registered_fingerprint(&self, node_id: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT ak_public_fingerprint_hex FROM attestation_identity_registry
             WHERE node_id = ?1",
        )?;
        match stmt.query_row(params![node_id], |row| row.get::<_, String>(0)) {
            Ok(fp) => Ok(Some(fp)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    // --- AV subsystem metadata ---------------------------------------------

    pub fn register_av_subsystem_meta(
        &self,
        node_id: &str,
        subsystem_type: &str,
        hardware_id: &str,
        confidence_floor: f64,
        initial_telemetry_ms: u64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO av_subsystem_meta
             (node_id, subsystem_type, hardware_id, confidence_floor, last_telemetry_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![node_id, subsystem_type, hardware_id, confidence_floor, initial_telemetry_ms as i64],
        )?;
        Ok(())
    }

    pub fn load_av_confidence_floor(&self, node_id: &str) -> Result<Option<f64>> {
        match self.conn.query_row(
            "SELECT confidence_floor FROM av_subsystem_meta WHERE node_id = ?1",
            params![node_id],
            |row| row.get::<_, f64>(0),
        ) {
            Ok(f) => Ok(Some(f)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn touch_av_telemetry_timestamp(&self, node_id: &str, now_ms: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE av_subsystem_meta SET last_telemetry_ms = ?1 WHERE node_id = ?2",
            params![now_ms as i64, node_id],
        )?;
        Ok(())
    }

    pub fn get_last_telemetry_timestamp(&self, node_id: &str) -> Result<u64> {
        match self.conn.query_row(
            "SELECT last_telemetry_ms FROM av_subsystem_meta WHERE node_id = ?1",
            params![node_id],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(ts) => Ok(ts as u64),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(e),
        }
    }

    pub fn load_all_registered_av_node_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT node_id FROM av_subsystem_meta")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect()
    }

    /// Read-only listing of every registered AV subsystem's diagnostic meta
    /// (confidence floor, recovery streak, last telemetry). No secrets.
    pub fn load_av_subsystems(&self) -> Result<Vec<AvSubsystemRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, subsystem_type, hardware_id, confidence_floor,
                    last_telemetry_ms, recovery_streak_count, recovery_streak_start_ms
             FROM av_subsystem_meta ORDER BY node_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(AvSubsystemRecord {
                node_id: row.get(0)?,
                subsystem_type: row.get(1)?,
                hardware_id: row.get(2)?,
                confidence_floor: row.get(3)?,
                last_telemetry_ms: row.get::<_, i64>(4)? as u64,
                recovery_streak_count: row.get::<_, i64>(5)? as u32,
                recovery_streak_start_ms: row.get::<_, i64>(6)? as u64,
            })
        })?;
        rows.collect()
    }

    pub fn load_recovery_streak(&self, node_id: &str) -> Result<(u32, u64)> {
        match self.conn.query_row(
            "SELECT recovery_streak_count, recovery_streak_start_ms
             FROM av_subsystem_meta WHERE node_id = ?1",
            params![node_id],
            |row| Ok((row.get::<_, i64>(0)? as u32, row.get::<_, i64>(1)? as u64)),
        ) {
            Ok(data) => Ok(data),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok((0, 0)),
            Err(e) => Err(e),
        }
    }

    pub fn reset_recovery_streak(&self, node_id: &str, now_ms: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE av_subsystem_meta
             SET recovery_streak_count = 0, recovery_streak_start_ms = 0,
                 last_telemetry_ms = ?1
             WHERE node_id = ?2",
            params![now_ms as i64, node_id],
        )?;
        Ok(())
    }

    pub fn increment_recovery_streak(&self, node_id: &str, now_ms: u64) -> Result<u32> {
        self.conn.execute(
            "UPDATE av_subsystem_meta
             SET recovery_streak_count = recovery_streak_count + 1,
                 recovery_streak_start_ms = CASE
                     WHEN recovery_streak_count = 0 THEN ?1
                     ELSE recovery_streak_start_ms
                 END,
                 last_telemetry_ms = ?1
             WHERE node_id = ?2",
            params![now_ms as i64, node_id],
        )?;
        self.conn.query_row(
            "SELECT recovery_streak_count FROM av_subsystem_meta WHERE node_id = ?1",
            params![node_id],
            |row| row.get::<_, i64>(0).map(|v| v as u32),
        )
    }

    // --- Posture engine persistent state -----------------------------------

    pub fn load_last_generation(&self) -> Result<u64> {
        match self.conn.query_row(
            "SELECT value FROM posture_engine_state WHERE key = 'last_generation'",
            [],
            |row| row.get::<_, String>(0),
        ) {
            Ok(s)  => Ok(s.parse::<u64>().unwrap_or(0)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(e),
        }
    }

    pub fn save_last_generation(&self, generation: u64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO posture_engine_state (key, value)
             VALUES ('last_generation', ?1)",
            params![generation.to_string()],
        )?;
        Ok(())
    }

    /// Reads an arbitrary key from the posture_engine_state key-value store.
    /// Returns None if the key doesn't exist.
    pub fn load_engine_state(&self, key: &str) -> Result<Option<String>> {
        match self.conn.query_row(
            "SELECT value FROM posture_engine_state WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        ) {
            Ok(v)  => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Writes an arbitrary key to the posture_engine_state key-value store (upsert).
    pub fn save_engine_state(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO posture_engine_state (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    // --- HA epoch fence (durable split-brain guard) -------------------------
    //
    // SQLite serializes write transactions, so a conditional UPDATE on the
    // singleton `ha_state` row gives a real distributed compare-and-set:
    // two racers that both read the same `observed` epoch will serialize at
    // commit time and only one of them will see `rows_affected == 1`.
    // The atomic on AppState is per-process and CANNOT do this — that is
    // why we keep the durable epoch as source of truth.

    /// In-transaction HA epoch fence (issue #79). Closes the residual TOCTOU in
    /// the request-path gate (`enforce_posture_routing`): that gate compares a
    /// CACHED epoch, but the durable epoch can advance (another instance's
    /// `try_claim_epoch`) in the window between the gate check and the write
    /// commit. By re-reading `ha_state.epoch` on the SAME serialized write
    /// transaction handle and comparing it to this instance's `held_epoch`
    /// BEFORE any mutation, a superseded node cannot land even one stale write:
    /// on any mismatch this returns `Err` and the caller drops the transaction
    /// without committing.
    ///
    /// MUST be called as the FIRST statement inside a top-tier durable
    /// transaction (the callers begin the transaction with
    /// `TransactionBehavior::Immediate`, so the write lock is held before this
    /// read — the durable epoch we observe here cannot change before we commit).
    ///
    /// Fail-closed on every non-match:
    ///   - `held == 0` → never legitimately claimed an epoch → reject.
    ///   - `durable != held` (including `durable < held`) → superseded → reject.
    ///   - SELECT error / row absent → [`FenceError::EpochUnreadable`] → reject.
    fn assert_epoch_held(
        tx: &rusqlite::Transaction,
        held_epoch: u64,
    ) -> std::result::Result<(), FenceError> {
        let durable: u64 = match tx.query_row(
            "SELECT epoch FROM ha_state WHERE id = 1",
            [],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(e) => e as u64,
            // SELECT failed or the singleton row is absent — never write blind.
            Err(_) => return Err(FenceError::EpochUnreadable),
        };
        // `held == 0` is fenced explicitly: it must reject even when the durable
        // epoch is also 0 (genesis, no claim anywhere) — a node that never
        // claimed must not perform a top-tier write.
        if held_epoch == 0 || durable != held_epoch {
            return Err(FenceError::EpochSuperseded { held: held_epoch, durable });
        }
        Ok(())
    }

    /// Current durable HA epoch. Source of truth for "who owns writes."
    pub fn current_epoch(&self) -> Result<u64> {
        let e: i64 = self.conn.query_row(
            "SELECT epoch FROM ha_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(e as u64)
    }

    /// Returns (current_epoch, active_instance_id) for startup arbitration.
    pub fn current_active_holder(&self) -> Result<(u64, Option<String>)> {
        let (e, holder): (i64, Option<String>) = self.conn.query_row(
            "SELECT epoch, active_instance_id FROM ha_state WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((e as u64, holder))
    }

    /// Conditional claim: bump epoch from `observed` to `observed + 1` and
    /// record this instance as the new holder, IFF the DB epoch still equals
    /// `observed`. Returns the new epoch on a win, or None if another
    /// instance already moved the epoch (claim aborted, fence held).
    ///
    /// `rows_affected == 1` is the durable compare-and-set: two concurrent
    /// callers reading the same `observed` will serialize at the write
    /// transaction boundary and only one will see the row update.
    pub fn try_claim_epoch(
        &mut self,
        observed: u64,
        instance_id: &str,
        now_ms: u64,
    ) -> Result<Option<u64>> {
        // #74 CORRECTNESS FIX: the epoch CAS goes through the FULL (force-synced)
        // connection, so the claim is DURABLE (fsync'd) before this returns —
        // and the caller (standby_monitor) only sets the in-memory held_epoch /
        // acts as Active AFTER this returns. A claimed epoch can no longer
        // regress on power-loss recovery, closing the split-brain window.
        let n = self.durable_ref().execute(
            "UPDATE ha_state SET epoch = epoch + 1, active_instance_id = ?2, updated_at_ms = ?3 \
             WHERE id = 1 AND epoch = ?1",
            params![observed as i64, instance_id, now_ms as i64],
        )?;
        if n == 1 {
            Ok(Some(observed + 1))
        } else {
            Ok(None)
        }
    }

    // --- Fabric asset persistence -------------------------------------------

    pub fn save_fabric_asset(&self, asset: &crate::fabric::asset::FabricAsset) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO fabric_assets
             (asset_id, asset_type, display_name, kinematic_profile, registered_at_ms, last_seen_ms, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                asset.asset_id,
                serde_json::to_string(&asset.asset_type).unwrap_or_default(),
                asset.display_name,
                serde_json::to_string(&asset.kinematic_profile).unwrap_or_default(),
                asset.registered_at_ms as i64,
                asset.last_seen_ms as i64,
                serde_json::to_string(&asset.metadata).unwrap_or_else(|_| "{}".to_string()),
            ],
        )?;
        Ok(())
    }

    pub fn load_fabric_assets(&self) -> Result<Vec<crate::fabric::asset::FabricAsset>> {
        let mut stmt = self.conn.prepare(
            "SELECT asset_id, asset_type, display_name, kinematic_profile, registered_at_ms, last_seen_ms, metadata_json
             FROM fabric_assets ORDER BY registered_at_ms"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        let mut assets = Vec::new();
        for row in rows {
            let (asset_id, asset_type_s, display_name, profile_s, reg_ms, last_ms, meta_s) = row?;
            let asset_type = serde_json::from_str(&asset_type_s)
                .unwrap_or(crate::fabric::asset::AssetType::Unknown);
            let kinematic_profile = serde_json::from_str(&profile_s)
                .unwrap_or(crate::fabric::asset::KinematicProfileType::Custom);
            let metadata = serde_json::from_str(&meta_s).unwrap_or_default();
            assets.push(crate::fabric::asset::FabricAsset {
                asset_id,
                asset_type,
                display_name,
                kinematic_profile,
                registered_at_ms: reg_ms as u64,
                last_seen_ms: last_ms as u64,
                metadata,
            });
        }
        Ok(assets)
    }

    // --- #87: forensic causal-log forensic chain ---------------------------

    /// Append a causal-log event to the hash-chained, signed, persisted ledger.
    ///
    /// Mirrors [`crate::audit_chain::AuditChainLinker::append_audit_event_tx`]:
    /// reads the prev `(record_hash, sequence)` (fail-closed on real read
    /// errors; only `QueryReturnedNoRows` is genesis), computes the record hash
    /// (binding the causality edges), signs the canonical causal payload, records
    /// the content-addressed `key_id`, INSERTs the row, and advances the signed
    /// causal anchor-head in the SAME transaction. Returns the fully-populated
    /// `CausalLogEntry`.
    pub fn append_causal_event(
        &mut self,
        event: &CausalEventInput<'_>,
        signing_key: Option<&ed25519_dalek::SigningKey>,
    ) -> Result<crate::fabric::causal_log::CausalLogEntry> {
        let CausalEventInput {
            entry_id,
            asset_id,
            event_type,
            payload,
            caused_by,
            affects_assets,
            fabric_generation,
            timestamp_ms,
        } = *event;
        use crate::audit_chain::{
            canonical_causal_anchor_head_payload, canonical_causal_signing_payload,
            compute_causal_record_hash, verifying_key_id,
        };
        use ed25519_dalek::Signer;

        let tx = self.conn.unchecked_transaction()?;

        // Read previous (record_hash, sequence). FAIL CLOSED on real read errors;
        // only an empty table is legitimate genesis.
        let prev = tx.query_row(
            "SELECT record_hash_hex, sequence FROM fabric_causal_log \
             ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        );
        let (previous_hash, prev_seq) = match prev {
            Ok((h, seq)) => (h, seq),
            Err(rusqlite::Error::QueryReturnedNoRows) => ("0".repeat(64), -1),
            Err(e) => return Err(e), // FAIL CLOSED — never fork-to-genesis on read error
        };
        let sequence: u64 = (prev_seq + 1) as u64;

        let record_hash =
            compute_causal_record_hash(&crate::audit_chain::CausalRecordHashInput {
                previous_hash: &previous_hash,
                entry_id,
                asset_id,
                event_type,
                payload,
                caused_by,
                affects_assets,
                timestamp_ms,
                fabric_generation,
                sequence,
            });

        let signature_b64: Option<String> = signing_key.map(|k| {
            let payload_str = canonical_causal_signing_payload(
                &previous_hash, &record_hash, event_type, timestamp_ms, sequence,
            );
            b64e.encode(k.sign(payload_str.as_bytes()).to_bytes())
        });
        let key_id: Option<String> =
            signing_key.map(|k| verifying_key_id(&k.verifying_key()));

        let caused_by_json = serde_json::to_string(caused_by).unwrap_or_else(|_| "[]".to_string());
        let affects_json =
            serde_json::to_string(affects_assets).unwrap_or_else(|_| "[]".to_string());

        tx.execute(
            "INSERT INTO fabric_causal_log
             (entry_id, sequence, timestamp_ms, asset_id, event_type, payload,
              caused_by, affects_assets, fabric_generation,
              previous_hash_hex, record_hash_hex, signature_b64, key_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                entry_id,
                sequence as i64,
                timestamp_ms as i64,
                asset_id,
                event_type,
                payload,
                caused_by_json,
                affects_json,
                fabric_generation as i64,
                previous_hash,
                record_hash,
                signature_b64,
                key_id,
            ],
        )?;

        // Advance the signed anchor-HEAD high-water mark in the SAME tx.
        let head_sig: Option<String> = signing_key.map(|k| {
            let payload_str = canonical_causal_anchor_head_payload(sequence, &record_hash);
            b64e.encode(k.sign(payload_str.as_bytes()).to_bytes())
        });
        tx.execute(
            "INSERT INTO fabric_causal_anchor_head (id, sequence, record_hash_hex, signature_b64, key_id)
             VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                 sequence        = excluded.sequence,
                 record_hash_hex = excluded.record_hash_hex,
                 signature_b64   = excluded.signature_b64,
                 key_id          = excluded.key_id",
            params![sequence as i64, record_hash, head_sig, key_id],
        )?;

        tx.commit()?;

        Ok(crate::fabric::causal_log::CausalLogEntry {
            entry_id: entry_id.to_string(),
            sequence,
            timestamp_ms,
            asset_id: asset_id.to_string(),
            event_type: event_type.to_string(),
            payload: payload.to_string(),
            caused_by: caused_by.to_vec(),
            affects_assets: affects_assets.to_vec(),
            fabric_generation,
            previous_hash,
            record_hash,
            signature_b64,
            key_id,
        })
    }

    /// Decode one `fabric_causal_log` row from a query row. Column order must
    /// match the SELECT in the loaders below.
    fn causal_entry_from_row(
        row: &rusqlite::Row,
    ) -> Result<crate::fabric::causal_log::CausalLogEntry> {
        let entry_id: String = row.get(0)?;
        let sequence: i64 = row.get(1)?;
        let timestamp_ms: i64 = row.get(2)?;
        let asset_id: String = row.get(3)?;
        let event_type: String = row.get(4)?;
        let payload: String = row.get(5)?;
        let caused_by_json: String = row.get(6)?;
        let affects_json: String = row.get(7)?;
        let fabric_generation: i64 = row.get(8)?;
        let previous_hash: String = row.get(9)?;
        let record_hash: String = row.get(10)?;
        let signature_b64: Option<String> = row.get(11)?;
        let key_id: Option<String> = row.get(12)?;
        Ok(crate::fabric::causal_log::CausalLogEntry {
            entry_id,
            sequence: sequence.max(0) as u64,
            timestamp_ms: timestamp_ms.max(0) as u64,
            asset_id,
            event_type,
            payload,
            caused_by: serde_json::from_str(&caused_by_json).unwrap_or_default(),
            affects_assets: serde_json::from_str(&affects_json).unwrap_or_default(),
            fabric_generation: fabric_generation.max(0) as u64,
            previous_hash,
            record_hash,
            signature_b64,
            key_id,
        })
    }

    /// Load every causal-log entry in chain (id ASC) order.
    pub fn load_causal_entries(&self) -> Result<Vec<crate::fabric::causal_log::CausalLogEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT entry_id, sequence, timestamp_ms, asset_id, event_type, payload, \
             caused_by, affects_assets, fabric_generation, previous_hash_hex, \
             record_hash_hex, signature_b64, key_id \
             FROM fabric_causal_log ORDER BY id ASC",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(Self::causal_entry_from_row(row)?);
        }
        Ok(out)
    }

    /// Load causal-log entries whose timestamp falls in `[from_ms, to_ms]`,
    /// in chain order, bounded by `limit`/`offset`.
    pub fn load_causal_entries_in_range(
        &self,
        from_ms: u64,
        to_ms: u64,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<crate::fabric::causal_log::CausalLogEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT entry_id, sequence, timestamp_ms, asset_id, event_type, payload, \
             caused_by, affects_assets, fabric_generation, previous_hash_hex, \
             record_hash_hex, signature_b64, key_id \
             FROM fabric_causal_log \
             WHERE timestamp_ms BETWEEN ?1 AND ?2 \
             ORDER BY id ASC LIMIT ?3 OFFSET ?4",
        )?;
        let mut rows = stmt.query(params![
            from_ms as i64,
            to_ms.min(i64::MAX as u64) as i64,
            limit as i64,
            offset as i64,
        ])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(Self::causal_entry_from_row(row)?);
        }
        Ok(out)
    }

    /// Count of causal-log entries.
    pub fn count_causal_entries(&self) -> Result<u64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM fabric_causal_log", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n as u64)
    }

    /// Verify the causal-log forensic chain (#87). Mirrors
    /// [`Self::verify_audit_chain_full`]: walks the rows checking prev-linkage,
    /// recomputed record hash (which binds the edges), and sequence monotonicity;
    /// verifies each row's signature under the key its `key_id` names (selected
    /// from the SHARED audit keyring — causal rows are signed by the same audit
    /// key and rotations live in the audit chain); then checks the signed
    /// anchor-head high-water mark for tail truncation/tamper.
    pub fn verify_causal_chain_integrity(
        &self,
        verifying_key: Option<&ed25519_dalek::VerifyingKey>,
    ) -> Result<CausalChainVerifyResult> {
        use crate::audit_chain::{
            canonical_causal_anchor_head_payload, canonical_causal_signing_payload,
            compute_causal_record_hash,
        };

        // REUSE the #76 audit keyring (genesis from durable anchor + verified
        // rotations). Causal rows are signed by the SAME audit key. If no
        // verifying key is supplied, skip signature verification (like audit).
        let (keyring, genesis_id) = self.audit_keyring_seed(verifying_key)?;
        let keyring = match verifying_key {
            Some(g) => self.build_audit_keyring(g)?,
            None => keyring,
        };

        let mut stmt = self.conn.prepare(
            "SELECT entry_id, sequence, timestamp_ms, asset_id, event_type, payload, \
             caused_by, affects_assets, fabric_generation, previous_hash_hex, \
             record_hash_hex, signature_b64, key_id \
             FROM fabric_causal_log ORDER BY id ASC",
        )?;

        let mut chain_intact = true;
        let mut total_entries: u64 = 0;
        let mut latest_hash = "0".repeat(64);
        let mut expected_previous_hash = "0".repeat(64);
        let mut signed_entries: u64 = 0;
        let mut unsigned_entries: u64 = 0;
        let mut signature_valid = true;
        let mut first_invalid_signature_index: Option<u64> = None;
        let mut first_signed_at_ms: Option<u64> = None;
        let mut prev_seq: Option<i64> = None;
        let mut last_sequence: Option<i64> = None;

        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let entry_id: String = row.get(0)?;
            let sequence: i64 = row.get(1)?;
            let timestamp_ms: i64 = row.get(2)?;
            let asset_id: String = row.get(3)?;
            let event_type: String = row.get(4)?;
            let payload: String = row.get(5)?;
            let caused_by_json: String = row.get(6)?;
            let affects_json: String = row.get(7)?;
            let fabric_generation: i64 = row.get(8)?;
            let previous_hash_hex: String = row.get(9)?;
            let record_hash_hex: String = row.get(10)?;
            let sig_b64: Option<String> = row.get(11)?;
            let key_id_opt: Option<String> = row.get(12)?;

            // Prev-linkage.
            if previous_hash_hex != expected_previous_hash {
                chain_intact = false;
            }
            // Sequence monotonicity: first row 0, each next prev+1.
            match prev_seq {
                None => {
                    if sequence != 0 {
                        chain_intact = false;
                    }
                }
                Some(p) => {
                    if sequence != p + 1 {
                        chain_intact = false;
                    }
                }
            }
            prev_seq = Some(sequence);

            let caused_by: Vec<String> = serde_json::from_str(&caused_by_json).unwrap_or_default();
            let affects_assets: Vec<String> =
                serde_json::from_str(&affects_json).unwrap_or_default();
            let recalc = compute_causal_record_hash(&crate::audit_chain::CausalRecordHashInput {
                previous_hash: &previous_hash_hex,
                entry_id: &entry_id,
                asset_id: &asset_id,
                event_type: &event_type,
                payload: &payload,
                caused_by: &caused_by,
                affects_assets: &affects_assets,
                timestamp_ms: timestamp_ms.max(0) as u64,
                fabric_generation: fabric_generation.max(0) as u64,
                sequence: sequence.max(0) as u64,
            });
            if recalc != record_hash_hex {
                chain_intact = false;
            }
            expected_previous_hash = record_hash_hex.clone();
            latest_hash = record_hash_hex.clone();
            last_sequence = Some(sequence);

            match &sig_b64 {
                None => unsigned_entries += 1,
                Some(s) => {
                    signed_entries += 1;
                    if first_signed_at_ms.is_none() {
                        first_signed_at_ms = Some(timestamp_ms.max(0) as u64);
                    }
                    if verifying_key.is_some() {
                        let signer_id = key_id_opt
                            .clone()
                            .or_else(|| genesis_id.clone())
                            .unwrap_or_default();
                        let ok = match keyring.get(&signer_id) {
                            Some(vk) => {
                                let payload_str = canonical_causal_signing_payload(
                                    &previous_hash_hex,
                                    &record_hash_hex,
                                    &event_type,
                                    timestamp_ms.max(0) as u64,
                                    sequence.max(0) as u64,
                                );
                                audit_verify_sig(vk, &payload_str, s)
                            }
                            None => false, // unknown key_id → FAIL-CLOSED
                        };
                        if !ok && first_invalid_signature_index.is_none() {
                            first_invalid_signature_index = Some(total_entries);
                            signature_valid = false;
                        }
                    }
                }
            }

            total_entries += 1;
        }

        let signing_enabled = verifying_key.is_some();
        let public_key_b64 = verifying_key.map(|vk| b64e.encode(vk.as_bytes()));

        // Anchor-HEAD high-water check — detects tail truncation/deletion.
        let (head_verified, head_status): (bool, String) = if total_entries == 0 {
            (true, "EMPTY_CHAIN".to_string())
        } else {
            let head = self.conn.query_row(
                "SELECT sequence, record_hash_hex, signature_b64, key_id \
                 FROM fabric_causal_anchor_head WHERE id = 1",
                [],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                },
            );
            match head {
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    (false, "HEAD_ABSENT".to_string())
                }
                Err(e) => return Err(e),
                Ok((h_seq, h_hash, h_sig, h_key_id)) => {
                    if Some(h_seq) != last_sequence || h_hash != latest_hash {
                        let truncated = match last_sequence {
                            Some(t) => t < h_seq,
                            None => true,
                        };
                        let status = if truncated {
                            "TRUNCATION_DETECTED"
                        } else {
                            "HEAD_TAIL_MISMATCH"
                        };
                        (false, status.to_string())
                    } else if signing_enabled {
                        match h_sig {
                            None => (false, "HEAD_UNSIGNED".to_string()),
                            Some(sig) => {
                                let signer_id =
                                    h_key_id.or_else(|| genesis_id.clone()).unwrap_or_default();
                                match keyring.get(&signer_id) {
                                    None => (false, "HEAD_KEY_UNKNOWN".to_string()),
                                    Some(vk) => {
                                        let payload_str = canonical_causal_anchor_head_payload(
                                            h_seq.max(0) as u64,
                                            &h_hash,
                                        );
                                        if audit_verify_sig(vk, &payload_str, &sig) {
                                            (true, "OK".to_string())
                                        } else {
                                            (false, "HEAD_SIGNATURE_INVALID".to_string())
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        (true, "OK_UNSIGNED".to_string())
                    }
                }
            }
        };

        Ok(CausalChainVerifyResult {
            chain_intact,
            total_entries,
            latest_hash,
            signing_enabled,
            signed_entries,
            unsigned_entries,
            signature_valid,
            first_invalid_signature_index,
            first_signed_at_ms,
            public_key_b64,
            head_verified,
            head_status,
        })
    }
}

#[cfg(test)]
mod attestation_registry_tests {
    use super::*;

    fn in_memory() -> VerifierStore {
        VerifierStore::new(":memory:").unwrap()
    }

    #[test]
    fn test_load_av_subsystems_lists_registered_rows() {
        let store = in_memory();
        store.register_av_subsystem_meta("lidar-1", "Perception", "LIDAR-001", 0.65, 1_000).unwrap();
        store.register_av_subsystem_meta("radar-1", "Perception", "RADAR-002", 0.70, 2_000).unwrap();
        store.increment_recovery_streak("lidar-1", 1_500).unwrap();
        let rows = store.load_av_subsystems().unwrap();
        assert_eq!(rows.len(), 2);
        let lidar = rows.iter().find(|r| r.node_id == "lidar-1").unwrap();
        assert_eq!(lidar.subsystem_type, "Perception");
        assert_eq!(lidar.hardware_id, "LIDAR-001");
        assert!((lidar.confidence_floor - 0.65).abs() < 1e-9);
        assert_eq!(lidar.recovery_streak_count, 1);
    }

    #[test]
    fn test_load_operators_lists_registered() {
        let mut store = in_memory();
        store.register_operator("op-2", "pem-b", 2_000).unwrap();
        store.register_operator("op-1", "pem-a", 1_000).unwrap();
        let ops = store.load_operators().unwrap();
        assert_eq!(ops.len(), 2);
        assert!(ops.iter().all(|o| o.revoked_at_ms.is_none() && o.is_active()));
        assert_eq!(ops[0].operator_id, "op-1", "ordered by operator_id");
    }

    #[test]
    fn test_register_and_load_fingerprint() {
        let mut store = in_memory();
        let fp = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(store.register_attestation_identity("node-01", fp, "admin", 1_000).is_ok());
        assert_eq!(store.load_registered_fingerprint("node-01").unwrap(), Some(fp.to_string()));
    }

    #[test]
    fn test_load_fingerprint_missing_node_returns_none() {
        let store = in_memory();
        assert_eq!(store.load_registered_fingerprint("ghost-node").unwrap(), None);
    }

    #[test]
    fn test_identity_registration_chains_audit_entry() {
        let mut store = in_memory();
        let fp = "abc123def456";
        store.register_attestation_identity("node-02", fp, "admin", 2_000).unwrap();
        assert!(store.verify_audit_chain_integrity().unwrap());
    }

    #[test]
    fn test_identity_registration_is_idempotent_on_rotate() {
        let mut store = in_memory();
        let fp1 = "aaaa";
        let fp2 = "bbbb";
        store.register_attestation_identity("node-03", fp1, "admin", 1_000).unwrap();
        store.register_attestation_identity("node-03", fp2, "admin", 2_000).unwrap();
        assert_eq!(store.load_registered_fingerprint("node-03").unwrap(), Some(fp2.to_string()));
        assert!(store.verify_audit_chain_integrity().unwrap());
    }

    #[test]
    fn test_av_subsystem_meta_round_trip() {
        let store = in_memory();
        store.register_av_subsystem_meta("lidar_front", "Perception", "LIDAR-001", 0.70, 0).unwrap();
        let floor = store.load_av_confidence_floor("lidar_front").unwrap();
        assert_eq!(floor, Some(0.70));
    }

    #[test]
    fn test_recovery_streak_increments_and_resets() {
        let store = in_memory();
        store.register_av_subsystem_meta("cam", "Perception", "CAM-001", 0.70, 0).unwrap();
        let n1 = store.increment_recovery_streak("cam", 1000).unwrap();
        let n2 = store.increment_recovery_streak("cam", 1100).unwrap();
        assert_eq!(n1, 1);
        assert_eq!(n2, 2);
        store.reset_recovery_streak("cam", 1200).unwrap();
        let (count, start) = store.load_recovery_streak("cam").unwrap();
        assert_eq!(count, 0);
        assert_eq!(start, 0);
    }

    #[test]
    fn test_generation_persistence() {
        let store = in_memory();
        assert_eq!(store.load_last_generation().unwrap(), 0);
        store.save_last_generation(42).unwrap();
        assert_eq!(store.load_last_generation().unwrap(), 42);
    }
}

#[cfg(test)]
mod standby_store_tests {
    use super::*;

    fn in_memory() -> VerifierStore {
        VerifierStore::new(":memory:").unwrap()
    }

    #[test]
    fn test_load_engine_state_absent_key_returns_none() {
        let store = in_memory();
        assert_eq!(store.load_engine_state("nonexistent_key").unwrap(), None);
    }

    #[test]
    fn test_save_and_load_engine_state_round_trip() {
        let store = in_memory();
        store.save_engine_state("primary_heartbeat_ms", "12345").unwrap();
        let val = store.load_engine_state("primary_heartbeat_ms").unwrap();
        assert_eq!(val, Some("12345".to_string()));
    }

    #[test]
    fn test_save_engine_state_is_idempotent_upsert() {
        let store = in_memory();
        store.save_engine_state("key", "first").unwrap();
        store.save_engine_state("key", "second").unwrap();
        assert_eq!(store.load_engine_state("key").unwrap(), Some("second".to_string()));
    }

    #[test]
    fn test_multiple_keys_are_independent() {
        let store = in_memory();
        store.save_engine_state("key_a", "value_a").unwrap();
        store.save_engine_state("key_b", "value_b").unwrap();
        assert_eq!(store.load_engine_state("key_a").unwrap(), Some("value_a".to_string()));
        assert_eq!(store.load_engine_state("key_b").unwrap(), Some("value_b".to_string()));
    }

    #[test]
    fn test_heartbeat_age_parse_from_stored_string() {
        let store = in_memory();
        let ts: u64 = 1_700_000_000_000;
        store.save_engine_state("primary_heartbeat_ms", &ts.to_string()).unwrap();
        let loaded = store.load_engine_state("primary_heartbeat_ms").unwrap().unwrap();
        let parsed: u64 = loaded.parse().expect("must parse as u64");
        assert_eq!(parsed, ts);
    }
}

impl VerifierStore {
    /// Idempotent one-time anchor for the v1 → v2 hash boundary. Should
    /// be called at service startup after `VerifierStore::new`. If a
    /// `HASH_V2_MIGRATION` event already exists in the chain this is a
    /// no-op; otherwise it appends one event whose payload records the
    /// pre-migration v1 head and v1 row count, providing a partial defence
    /// against silent truncation at the boundary.
    ///
    /// Note: v1 rows retain the pre-migration relabeling weakness (cannot
    /// be retroactively strengthened without destructive re-hashing).
    /// Only v2 and future rows benefit from event_type being bound into
    /// the cheap hash-only integrity check.
    pub fn ensure_hash_v2_migration_anchor(&mut self, now_ms: u64) -> rusqlite::Result<()> {
        // Already anchored?
        let existing: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE event_type = 'HASH_V2_MIGRATION'",
            [],
            |r| r.get(0),
        )?;
        if existing > 0 {
            return Ok(());
        }
        // Snapshot the v1 head (last row with hash_version=1) and v1 count.
        let v1_total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE hash_version = 1",
            [],
            |r| r.get(0),
        )?;
        if v1_total == 0 {
            // Nothing to anchor — a brand-new chain skips the marker.
            return Ok(());
        }
        let v1_head: String = self.conn.query_row(
            "SELECT record_hash_hex FROM audit_log_chain \
             WHERE hash_version = 1 ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )?;
        let payload = format!(
            "{{\"v1_head_record_hash\":\"{v1_head}\",\"v1_total_count\":{v1_total},\"migrated_at_ms\":{now_ms}}}"
        );
        let tx = self.conn.transaction()?;
        crate::audit_chain::AuditChainLinker::append_audit_event_tx(
            &tx,
            "HASH_V2_MIGRATION",
            &payload,
            now_ms as i64,
            self.signing_key.as_ref(),
        )?;
        tx.commit()
    }

    /// #77 backfill: ensure the signed anchor-HEAD exists for an already-populated
    /// chain — e.g. a chain written by a pre-#77 binary and opened after upgrade,
    /// whose rows predate head maintenance. If the chain is non-empty and no head
    /// row exists, sign the current tail's `(sequence, record_hash)` with the
    /// loaded signing key and write the head. Idempotent: a no-op once the head
    /// exists or while the chain is empty. Runs at startup AFTER the signing key
    /// is admitted (same point as `ensure_hash_v2_migration_anchor`), so the
    /// backfilled head is signed — and so a legitimately-upgraded store presents a
    /// head BEFORE it serves `/system/audit/verify` (no false `HEAD_ABSENT`).
    /// `_now_ms` is accepted only for call-site symmetry with the other `ensure_*`
    /// migrations (the head payload binds sequence+hash, not a timestamp).
    pub fn ensure_audit_anchor_head(&mut self, _now_ms: u64) -> rusqlite::Result<()> {
        let head_exists: bool = self.conn.query_row(
            "SELECT COUNT(*) FROM audit_anchor_head WHERE id = 1",
            [],
            |r| r.get::<_, i64>(0),
        )? > 0;
        if head_exists {
            return Ok(());
        }
        // Current tail (highest id). Empty chain → nothing to anchor.
        let tail = self.conn.query_row(
            "SELECT record_hash_hex, sequence FROM audit_log_chain ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?)),
        );
        let (record_hash, seq_opt) = match tail {
            Ok(t) => t,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(()), // empty chain
            Err(e) => return Err(e),
        };
        // Only anchor a v2 tail (sequence present); a v1-only tail predates the
        // sequence/head model and is anchored once the hash-v2 migration appends.
        let Some(seq) = seq_opt else { return Ok(()) };
        let seq = seq.max(0) as u64;

        let (signature_b64, key_id): (Option<String>, Option<String>) =
            match self.signing_key.as_ref() {
                Some(key) => {
                    use ed25519_dalek::Signer;
                    let payload = crate::audit_chain::canonical_anchor_head_payload(seq, &record_hash);
                    let sig = b64e.encode(key.sign(payload.as_bytes()).to_bytes());
                    let kid = crate::audit_chain::verifying_key_id(&key.verifying_key());
                    (Some(sig), Some(kid))
                }
                None => (None, None),
            };
        self.conn.execute(
            "INSERT INTO audit_anchor_head (id, sequence, record_hash_hex, signature_b64, key_id)
             VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                 sequence        = excluded.sequence,
                 record_hash_hex = excluded.record_hash_hex,
                 signature_b64   = excluded.signature_b64,
                 key_id          = excluded.key_id",
            params![seq as i64, record_hash, signature_b64, key_id],
        )?;
        Ok(())
    }

    /// TEST-ONLY: drop `audit_log_chain` so the next
    /// `ensure_hash_v2_migration_anchor` (and any chained audit write) fails —
    /// used to exercise the fail-closed promotion-abort path (#78). Never
    /// compiled into production builds.
    #[cfg(test)]
    pub fn break_audit_chain_table_for_test(&self) {
        self.conn
            .execute("DROP TABLE IF EXISTS audit_log_chain", [])
            .expect("test seam: drop audit_log_chain");
    }

    /// TEST-ONLY: seed one legacy `hash_version = 1` row so a subsequent
    /// `ensure_hash_v2_migration_anchor` actually WRITES the `HASH_V2_MIGRATION`
    /// marker (on a clean chain `v1_total == 0`, so the anchor is a no-op). Lets
    /// a test prove the anchor was ensured during promotion (#78).
    #[cfg(test)]
    pub fn seed_legacy_v1_audit_row_for_test(&self) {
        self.conn
            .execute(
                "INSERT INTO audit_log_chain \
                 (event_type, event_json, previous_hash_hex, record_hash_hex, created_at_ms, hash_version) \
                 VALUES ('LEGACY_V1', '{}', '', 'deadbeef', 1, 1)",
                [],
            )
            .expect("test seam: seed legacy v1 audit row");
    }

    /// TEST-ONLY: count `audit_log_chain` rows of a given `event_type`.
    #[cfg(test)]
    pub fn count_audit_events_for_test(&self, event_type: &str) -> i64 {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log_chain WHERE event_type = ?1",
                params![event_type],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }
}

/// Regression suite for the audit-chain bypass fix.
///
/// Before this fix, `save_posture_event` (plain INSERT) was the writer at
/// six production call sites, so events like `ATTESTATION_TRUSTED` and
/// `MOTION_COMMAND_ADMITTED` were written to `posture_events` but NOT
/// appended to the SHA-256 hash chain — meaning `verify_audit_chain_*`
/// could not detect tampering of those events. This test proves the
/// chained writer covers a posture event and the chain remains verifiable.
#[cfg(test)]
mod audit_chain_bypass_tests {
    use super::*;

    fn in_memory() -> VerifierStore {
        VerifierStore::new(":memory:").unwrap()
    }

    #[test]
    fn test_posture_event_is_covered_by_audit_chain() {
        let mut store = in_memory();

        // Write an ATTESTATION_TRUSTED event through the chained writer.
        store
            .save_posture_event_chained(
                "node-x",
                "ATTESTATION_TRUSTED",
                r#"{"trusted":true}"#,
                None,
                1_000,
            )
            .expect("chained write succeeds");

        // Chain verifies clean — the event landed as a chain link.
        assert!(
            store
                .verify_audit_chain_integrity()
                .expect("verify_audit_chain_integrity should succeed on a healthy chain"),
            "Chained posture write must produce a verifiable chain link"
        );

        // Stronger integration assertion: load_audit_chain_page reports
        // at least one entry, proving the event really IS in the chain
        // (not just that the chain happens to verify with zero entries).
        let page = store
            .load_audit_chain_page(10, 0, None)
            .expect("load_audit_chain_page should succeed");
        assert!(
            page.total >= 1,
            "Audit chain page must contain the just-written event; total={}",
            page.total
        );
        assert!(
            page.chain_intact,
            "Audit chain page must self-report intact after a chained write"
        );

        // Add a second event of a different type — chain must still verify
        // (covers the multi-link case, not just the first-write case).
        store
            .save_posture_event_chained(
                "node-x",
                "DEPENDENCY_UPDATED",
                r#"{"parent":"node-y"}"#,
                Some("test reason"),
                2_000,
            )
            .expect("second chained write succeeds");

        assert!(
            store
                .verify_audit_chain_integrity()
                .expect("verify_audit_chain_integrity should succeed"),
            "Multi-link chain must still verify after a second chained posture write"
        );

        // TODO: negative test — mutate the persisted posture_events row
        // directly and assert `verify_audit_chain_integrity` returns false.
        // Skipped here because VerifierStore does not expose raw
        // `Connection` access for tests (intentional encapsulation); the
        // chain-tamper-detection property is covered separately by the
        // SG-010 fault-injection-suite stub in
        // `tests/cert_003_rtm_gap_stubs.rs`.
    }
}

/// Tests for the v2 hash + sequence binding. The CORE WIN is that the
/// cheap hash-only `verify_audit_chain_integrity` now catches event_type
/// relabeling and v2 sequence reorder/gaps on v2 rows — without needing
/// signatures. Pre-v2 these were undetected by the hash-only check.
#[cfg(test)]
mod audit_hash_v2_tests {
    use super::*;
    use crate::audit_chain::AuditChainLinker;

    fn in_memory() -> VerifierStore {
        VerifierStore::new(":memory:").unwrap()
    }

    /// CORE WIN: relabeling a v2 row's event_type is now caught by
    /// `verify_audit_chain_integrity`. Pre-v2 this was undetected — the
    /// row's event_type wasn't bound into the hash, so the cheap check
    /// returned true after relabeling.
    #[test]
    fn test_v2_event_type_relabel_detected_by_hash_only_check() {
        let mut store = in_memory();
        store
            .save_posture_event_chained("node", "ATTESTATION_TRUSTED", "{}", None, 1_000)
            .unwrap();
        // Sanity: chain verifies clean.
        assert!(store.verify_audit_chain_integrity().unwrap());

        // Tamper: relabel the just-written event_type via direct UPDATE.
        // (Both the row's `event_type` and any other tampering of the
        // row's content must now make the hash mismatch under v2.)
        store
            .conn
            .execute(
                "UPDATE audit_log_chain SET event_type = 'FEDERATION_ACCEPTED' \
                 WHERE id = (SELECT MAX(id) FROM audit_log_chain)",
                [],
            )
            .unwrap();

        // Cheap hash-only verifier must now reject — event_type is bound
        // into compute_record_hash_v2.
        assert!(
            !store.verify_audit_chain_integrity().unwrap(),
            "v2 hash must catch event_type relabeling; this is the relabeling-hole fix"
        );
    }

    /// V2 sequence tampering (gap / reorder) is caught.
    #[test]
    fn test_v2_sequence_tamper_detected() {
        let mut store = in_memory();
        for i in 0..3 {
            store
                .save_posture_event_chained("n", "EVT", "{}", None, 1_000 + i)
                .unwrap();
        }
        assert!(store.verify_audit_chain_integrity().unwrap());

        // Tamper: bump the middle row's sequence so it skips a value.
        store
            .conn
            .execute(
                "UPDATE audit_log_chain SET sequence = 99 \
                 WHERE id = (SELECT MIN(id) + 1 FROM audit_log_chain)",
                [],
            )
            .unwrap();

        assert!(
            !store.verify_audit_chain_integrity().unwrap(),
            "v2 verifier must reject when sequence is non-monotonic"
        );
    }

    /// V2 hash has no field-splicing ambiguity: ("AB","C") and ("A","BC")
    /// must produce different hashes. Length-prefixing every variable
    /// field prevents the boundary from sliding.
    #[test]
    fn test_v2_hash_no_field_splicing() {
        let prev = "0".repeat(64);
        let ts = 1_000;
        let seq = 0;
        let h_ab_c = AuditChainLinker::compute_record_hash_v2(&prev, "AB", "C", ts, seq);
        let h_a_bc = AuditChainLinker::compute_record_hash_v2(&prev, "A", "BC", ts, seq);
        assert_ne!(
            h_ab_c, h_a_bc,
            "v2 must not collide on field-boundary slides — length-prefixing prevents this"
        );
    }

    /// Mixed v1+v2 chain still verifies. We can't create a v1 row through
    /// the current append (which always writes v2) without raw insert, so
    /// this test uses raw INSERT to simulate a pre-migration v1 row, then
    /// chains a v2 row after it.
    #[test]
    fn test_mixed_v1_v2_chain_verifies() {
        let mut store = in_memory();

        // Manually insert a v1-shape row at the start of the chain (the
        // way upgraded databases will look).
        let prev_v1 = "0".repeat(64);
        let v1_ts: i64 = 1_000;
        let v1_payload = "{\"legacy\":true}";
        let v1_hash =
            AuditChainLinker::compute_record_hash_v1(&prev_v1, v1_payload, v1_ts);
        store
            .conn
            .execute(
                "INSERT INTO audit_log_chain
                 (event_type, event_json, previous_hash_hex, record_hash_hex,
                  created_at_ms, signature_b64, hash_version, sequence)
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL, 1, NULL)",
                rusqlite::params!["LEGACY_V1", v1_payload, prev_v1, v1_hash, v1_ts],
            )
            .unwrap();

        // Now append a v2 event via the chained writer. It must chain to
        // the v1 head and start at sequence 0.
        store
            .save_posture_event_chained("n", "NEW_V2", "{}", None, 2_000)
            .unwrap();

        assert!(
            store.verify_audit_chain_integrity().unwrap(),
            "mixed v1+v2 chain must verify under the version-dispatching verifier"
        );
    }

    /// V2 payload tamper (event_json changed) is still detected — the
    /// existing pre-v2 guarantee survives the migration.
    #[test]
    fn test_v2_payload_tamper_still_detected() {
        let mut store = in_memory();
        store
            .save_posture_event_chained("n", "EVT", r#"{"x":1}"#, None, 1_000)
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE audit_log_chain SET event_json = '{\"x\":2}' \
                 WHERE id = (SELECT MAX(id) FROM audit_log_chain)",
                [],
            )
            .unwrap();
        assert!(
            !store.verify_audit_chain_integrity().unwrap(),
            "v2 must still detect event_json tampering"
        );
    }

    /// Migration anchor is idempotent and is a no-op on a brand-new chain.
    #[test]
    fn test_migration_anchor_idempotent_and_noop_on_empty_chain() {
        let mut store = in_memory();
        // No v1 rows present → anchor is a no-op.
        store.ensure_hash_v2_migration_anchor(5_000).unwrap();
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log_chain WHERE event_type='HASH_V2_MIGRATION'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "no v1 rows → no migration marker needed");

        // Simulate an upgraded DB: one v1 row, then run the anchor.
        let h = AuditChainLinker::compute_record_hash_v1(&"0".repeat(64), "{}", 100);
        store
            .conn
            .execute(
                "INSERT INTO audit_log_chain
                 (event_type, event_json, previous_hash_hex, record_hash_hex,
                  created_at_ms, signature_b64, hash_version, sequence)
                 VALUES ('LEGACY', '{}', ?1, ?2, 100, NULL, 1, NULL)",
                rusqlite::params![&"0".repeat(64), &h],
            )
            .unwrap();
        store.ensure_hash_v2_migration_anchor(5_000).unwrap();
        let count_after: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log_chain WHERE event_type='HASH_V2_MIGRATION'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count_after, 1, "exactly one anchor written");

        // Second call must NOT write a second anchor.
        store.ensure_hash_v2_migration_anchor(6_000).unwrap();
        let count_idem: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log_chain WHERE event_type='HASH_V2_MIGRATION'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count_idem, 1, "anchor is idempotent — second call no-ops");
    }
}

/// Issue #76 — audit key-rotation: cross-rotation verify, the sign-side swap
/// proof, the on-vs-off negative control, tamper-evidence, migration, and the
/// fail-closed unknown-key-id case.
#[cfg(test)]
mod audit_key_rotation_tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use crate::audit_chain::{AuditChainLinker, verifying_key_id};

    fn store_with_key(seed: u8) -> (VerifierStore, SigningKey) {
        let mut s = VerifierStore::new(":memory:").expect("store");
        let sk = SigningKey::from_bytes(&[seed; 32]);
        s.set_signing_key(sk.clone());
        (s, sk)
    }

    /// Claim the first HA epoch on a fresh store and return the held fencing
    /// token an Active node holds — the only legitimate context for the fenced
    /// top-tier writes (`record_key_rotation`, `save_federated_report_chained`).
    /// (#79: those methods re-check this token inside their write transaction.)
    fn claim_epoch(s: &mut VerifierStore) -> u64 {
        s.try_claim_epoch(0, "test-node", 0)
            .unwrap()
            .expect("first epoch claim must win on a fresh store")
    }

    fn append(s: &mut VerifierStore, event_type: &str, ts: i64) {
        let sk = s.signing_key.clone();
        let tx = s.conn.transaction().unwrap();
        AuditChainLinker::append_audit_event_tx(&tx, event_type, "{}", ts, sk.as_ref()).unwrap();
        tx.commit().unwrap();
    }

    fn max_id(s: &VerifierStore) -> i64 {
        s.conn.query_row("SELECT MAX(id) FROM audit_log_chain", [], |r| r.get(0)).unwrap()
    }

    /// (payload, signature_b64, key_id) for a row, for direct sig checks.
    fn row_payload_sig(s: &VerifierStore, id: i64) -> (String, String, String) {
        s.conn.query_row(
            "SELECT event_type, previous_hash_hex, record_hash_hex, created_at_ms, \
             signature_b64, hash_version, sequence, key_id \
             FROM audit_log_chain WHERE id = ?1",
            [id],
            |r| {
                let et: String = r.get(0)?; let prev: String = r.get(1)?; let rec: String = r.get(2)?;
                let ts: i64 = r.get(3)?; let sig: String = r.get(4)?; let hv: i64 = r.get(5)?;
                let seq: Option<i64> = r.get(6)?; let kid: String = r.get(7)?;
                Ok((audit_signing_payload(hv, &prev, &rec, &et, ts, seq), sig, kid))
            },
        ).unwrap()
    }

    /// CROSS-ROTATION VERIFY: sign under A → rotate to B → append under B →
    /// verify_audit_chain_full asserts ALL rows (A and B) verify.
    #[test]
    fn cross_rotation_all_rows_verify() {
        let (mut s, a) = store_with_key(1);
        let held = claim_epoch(&mut s);
        append(&mut s, "E1", 100);
        append(&mut s, "E2", 200);
        let b = SigningKey::from_bytes(&[2; 32]);
        s.record_key_rotation(b.clone(), "scheduled", 300, held).unwrap();
        append(&mut s, "E3", 400);
        append(&mut s, "E4", 500);

        let r = s.verify_audit_chain_full(Some(&a.verifying_key())).unwrap();
        assert!(r.chain_intact, "hash chain intact across rotation");
        assert!(r.signature_valid, "all A-rows AND B-rows must verify");
        assert_eq!(r.first_invalid_signature_index, None);
        assert!(r.signed_entries >= 5);
    }

    /// SIGN-SIDE PROOF: the signing key actually swapped — a post-rotation row
    /// is signed by B (verifies under B, FAILS under A), and the store's
    /// in-memory key is now B. Directly kills the old cosmetic-rotation bug.
    #[test]
    fn rotation_actually_swaps_signing_key() {
        let (mut s, a) = store_with_key(1);
        let held = claim_epoch(&mut s);
        append(&mut s, "E1", 100);
        let b = SigningKey::from_bytes(&[2; 32]);
        s.record_key_rotation(b.clone(), "swap", 200, held).unwrap();
        // In-memory key swapped to B.
        assert_eq!(
            s.signing_key.as_ref().unwrap().verifying_key(),
            b.verifying_key(),
            "record_key_rotation must swap self.signing_key (not cosmetic)"
        );
        append(&mut s, "E2", 300);
        let id = max_id(&s);
        let (payload, sig, kid) = row_payload_sig(&s, id);
        assert_eq!(kid, verifying_key_id(&b.verifying_key()), "post-rotation row's key_id is B");
        assert!(audit_verify_sig(&b.verifying_key(), &payload, &sig), "verifies under B");
        assert!(!audit_verify_sig(&a.verifying_key(), &payload, &sig), "FAILS under A — signing swapped");
    }

    /// NEGATIVE CONTROL: the OLD single-key verify (one vk = A for every row)
    /// WOULD fail the post-rotation B-row; the new per-row keyring verify passes.
    /// The delta is the evidence the fix changed the outcome.
    #[test]
    fn negative_control_old_single_key_would_fail() {
        let (mut s, a) = store_with_key(1);
        let held = claim_epoch(&mut s);
        append(&mut s, "E1", 100);
        let b = SigningKey::from_bytes(&[2; 32]);
        s.record_key_rotation(b.clone(), "rot", 200, held).unwrap();
        append(&mut s, "E2", 300);
        let id = max_id(&s);
        let (payload, sig, _kid) = row_payload_sig(&s, id);

        // OLD behavior: verify the B-row under the single key A → fails.
        assert!(!audit_verify_sig(&a.verifying_key(), &payload, &sig),
            "old single-key(A) verify WOULD have failed the B-row (false tamper alarm)");
        // NEW behavior: full per-row keyring verify passes.
        let r = s.verify_audit_chain_full(Some(&a.verifying_key())).unwrap();
        assert!(r.signature_valid, "new keyring verify passes for the same chain");
    }

    /// TAMPER: mutating a row's payload is detected (tamper-evidence intact).
    #[test]
    fn tamper_is_detected() {
        let (mut s, a) = store_with_key(1);
        append(&mut s, "E1", 100);
        append(&mut s, "E2", 200);
        s.conn.execute("UPDATE audit_log_chain SET event_json = '{\"x\":1}' WHERE id = 1", []).unwrap();
        let r = s.verify_audit_chain_full(Some(&a.verifying_key())).unwrap();
        assert!(!r.chain_intact || !r.signature_valid, "tamper must be detected");
    }

    /// MIGRATION: existing rows with NULL key_id are backfilled with the genesis
    /// key's id by ensure_key_id_backfill_migration, and still verify.
    #[test]
    fn migration_backfills_genesis_key_id() {
        let (mut s, a) = store_with_key(1);
        append(&mut s, "E1", 100);
        append(&mut s, "E2", 200);
        // Simulate a pre-upgrade chain: drop the key_id the new append recorded.
        s.conn.execute("UPDATE audit_log_chain SET key_id = NULL", []).unwrap();

        s.ensure_key_id_backfill_migration(999).unwrap();
        let gid = verifying_key_id(&a.verifying_key());
        let nulls: i64 = s.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE key_id IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(nulls, 0, "all rows backfilled");
        let backfilled: i64 = s.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE key_id = ?1", [&gid], |r| r.get(0)).unwrap();
        assert!(backfilled >= 2, "rows carry the genesis key_id");
        // A signed KEY_ID_BACKFILL anchor exists, and the chain still verifies.
        let anchors: i64 = s.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE event_type = 'KEY_ID_BACKFILL'", [], |r| r.get(0)).unwrap();
        assert_eq!(anchors, 1, "migration anchored by a signed event");
        let r = s.verify_audit_chain_full(Some(&a.verifying_key())).unwrap();
        assert!(r.chain_intact && r.signature_valid, "backfilled rows still verify under genesis");
        // Idempotent.
        s.ensure_key_id_backfill_migration(1000).unwrap();
        let anchors2: i64 = s.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE event_type = 'KEY_ID_BACKFILL'", [], |r| r.get(0)).unwrap();
        assert_eq!(anchors2, 1, "migration is idempotent");
    }

    /// UNKNOWN KEY-ID: a row whose key_id isn't in the keyring fails closed
    /// (not skipped).
    #[test]
    fn unknown_key_id_fails_closed() {
        let (mut s, a) = store_with_key(1);
        append(&mut s, "E1", 100);
        s.conn.execute(
            "UPDATE audit_log_chain SET key_id = 'deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef' WHERE id = 1",
            [],
        ).unwrap();
        let r = s.verify_audit_chain_full(Some(&a.verifying_key())).unwrap();
        assert!(!r.signature_valid, "unknown key_id must fail closed, not skip");
        assert_eq!(r.first_invalid_signature_index, Some(0));
    }
}

/// Issue #74 — SQLite durability at power-loss: durable (FULL) connection
/// routing, epoch non-regression (the fence-correctness proof), nonce
/// durability, the in-memory fallback, and the shutdown checkpoint.
#[cfg(test)]
mod durability_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static CTR: AtomicU64 = AtomicU64::new(0);

    /// Temp DB file (+ -wal/-shm) cleaned up on drop.
    struct TmpDb(String);
    impl TmpDb {
        fn new(tag: &str) -> Self {
            let n = CTR.fetch_add(1, Ordering::SeqCst);
            let p = std::env::temp_dir()
                .join(format!("kirra74_{tag}_{}_{n}.db", std::process::id()));
            TmpDb(p.to_string_lossy().into_owned())
        }
        fn path(&self) -> &str { &self.0 }
    }
    impl Drop for TmpDb {
        fn drop(&mut self) {
            for ext in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{}", self.0, ext));
            }
        }
    }

    fn pragma_synchronous(c: &Connection) -> i64 {
        c.query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap()
    }

    fn report(nonce: &str) -> crate::federation::FederatedTrustReport {
        crate::federation::FederatedTrustReport {
            source_controller_id: "ctrl-A".to_string(),
            asset_id: "asset-1".to_string(),
            posture: crate::verifier::FleetPosture::Nominal,
            issued_at_ms: 1_000,
            expires_at_ms: 9_000,
            nonce_hex: nonce.to_string(),
            signature_b64: "sig".to_string(),
        }
    }

    /// DURABLE ROUTING + config: a file-backed store has a FULL durable
    /// connection distinct from the NORMAL main connection.
    #[test]
    fn durable_connection_is_full_main_is_normal() {
        let db = TmpDb::new("routing");
        let s = VerifierStore::new(db.path()).unwrap();
        assert_eq!(pragma_synchronous(&s.conn), 1, "main conn is NORMAL (1)");
        let dc = s.durable_conn.as_ref().expect("file store must have a durable connection");
        assert_eq!(pragma_synchronous(dc), 2, "durable conn is FULL (2)");
    }

    /// IN-MEMORY FALLBACK: no separate durable conn (a 2nd :memory: open would be
    /// a distinct db), and epoch/nonce still work via the main connection.
    #[test]
    fn memory_store_has_no_durable_conn_but_works() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        assert!(s.durable_conn.is_none(), ":memory: must fall back to the main conn");
        assert_eq!(s.try_claim_epoch(0, "A", 1).unwrap(), Some(1));
        // #79: held == durable epoch (1) → the fence admits the legitimate write.
        s.save_federated_report_chained(&report("aa"), 2_000, 1).unwrap();
        assert!(s.has_seen_federation_nonce("aa").unwrap());
    }

    /// EPOCH NON-REGRESSION (the fence-correctness core of #74): a claim
    /// committed via the FULL path survives a store reopen ("recovery") and does
    /// NOT regress — a stale-observed re-claim then fails (no double-claim).
    #[test]
    fn epoch_claim_durable_across_reopen_and_fence_holds() {
        let db = TmpDb::new("epoch");
        {
            let mut s = VerifierStore::new(db.path()).unwrap();
            assert_eq!(s.try_claim_epoch(0, "primary", 100).unwrap(), Some(1),
                "primary claims epoch 1 (FULL-synced)");
        } // drop → simulate process loss; the claim was fsync'd on its FULL commit.

        // Recover: reopen the SAME file.
        let mut s2 = VerifierStore::new(db.path()).unwrap();
        assert_eq!(s2.try_claim_epoch(0, "ghost", 200).unwrap(), None,
            "a stale-observed (epoch 0) re-claim MUST fail — the epoch did not regress to 0");
        assert_eq!(s2.try_claim_epoch(1, "standby", 300).unwrap(), Some(2),
            "the durable epoch is 1; the legitimate next claim advances to 2 (fence intact)");
    }

    /// NONCE DURABILITY: a burned federation nonce survives reopen → no replay.
    #[test]
    fn nonce_burn_durable_across_reopen() {
        let db = TmpDb::new("nonce");
        {
            let mut s = VerifierStore::new(db.path()).unwrap();
            let held = s.try_claim_epoch(0, "test-node", 0).unwrap().unwrap();
            s.save_federated_report_chained(&report("deadbeef"), 2_000, held).unwrap();
            assert!(s.has_seen_federation_nonce("deadbeef").unwrap(), "burned before reopen");
        } // drop → simulate process loss.
        let s2 = VerifierStore::new(db.path()).unwrap();
        assert!(s2.has_seen_federation_nonce("deadbeef").unwrap(),
            "burned nonce must survive recovery — no replay window");
    }

    /// AUDIT-CHAIN INTEGRITY + shutdown checkpoint: appends stay sequenced and
    /// hash-linked under the dual-connection setup; durable_checkpoint() flushes
    /// without breaking verification.
    #[test]
    fn audit_chain_intact_after_checkpoint() {
        use ed25519_dalek::SigningKey;
        let db = TmpDb::new("audit");
        let key = SigningKey::from_bytes(&[7; 32]);
        let mut s = VerifierStore::new(db.path()).unwrap();
        s.set_signing_key(key.clone());
        // Append a few chained rows via a real store write path.
        for i in 0..3 {
            s.save_posture_event_chained("n", "EVT", "{}", None, 100 + i).unwrap();
        }
        // Force the shutdown-style durable checkpoint.
        s.durable_checkpoint().unwrap();
        let r = s.verify_audit_chain_full(Some(&key.verifying_key())).unwrap();
        assert!(r.chain_intact, "hash chain intact across the dual-conn + checkpoint");
        assert!(r.signature_valid, "signatures verify");
        // Reopen and re-verify — checkpointed rows are durable.
        drop(s);
        let s2 = VerifierStore::new(db.path()).unwrap();
        let r2 = s2.verify_audit_chain_full(Some(&key.verifying_key())).unwrap();
        assert!(r2.chain_intact && r2.signed_entries >= 3, "rows durable + intact after reopen");
    }
}

#[cfg(test)]
mod key_durability_165_tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use crate::audit_chain::{verifying_key_id, AuditChainLinker};
    use std::sync::atomic::{AtomicU64, Ordering};

    static CTR: AtomicU64 = AtomicU64::new(0);

    /// Temp DB file (+ -wal/-shm) cleaned up on drop — a file store so the
    /// FULL durable_conn exists and survives reopen.
    struct TmpDb(String);
    impl TmpDb {
        fn new(tag: &str) -> Self {
            let n = CTR.fetch_add(1, Ordering::SeqCst);
            let p = std::env::temp_dir()
                .join(format!("kirra165_{tag}_{}_{n}.db", std::process::id()));
            TmpDb(p.to_string_lossy().into_owned())
        }
        fn path(&self) -> &str { &self.0 }
    }
    impl Drop for TmpDb {
        fn drop(&mut self) {
            for ext in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{}", self.0, ext));
            }
        }
    }

    fn key(seed: u8) -> SigningKey { SigningKey::from_bytes(&[seed; 32]) }
    fn kid(k: &SigningKey) -> String { verifying_key_id(&k.verifying_key()) }

    // --- Test 1: DURABLE ROTATION (gap-1 proof) -----------------------------
    #[test]
    fn durable_rotation_then_reverted_env_is_fail_closed() {
        let db = TmpDb::new("g1");
        let (a, b) = (key(1), key(2));
        {
            let mut s = VerifierStore::new(db.path()).unwrap();
            assert_eq!(
                s.admit_signing_key(a.clone(), false, None, 1_000).unwrap(),
                KeyAdmission::BackfilledGenesis
            );
            assert_eq!(s.audit_key_ledger_active_id().unwrap().as_deref(), Some(kid(&a).as_str()));
            // #79: an Active node holds the epoch it claimed; the rotation fence
            // re-checks it inside the write transaction.
            let held = s.try_claim_epoch(0, "test-node", 0).unwrap().unwrap();
            s.record_key_rotation(b.clone(), "scheduled", 2_000, held).unwrap();
            assert_eq!(s.audit_key_ledger_active_id().unwrap().as_deref(), Some(kid(&b).as_str()));
        }
        // Reopen with env reverted to A (the retired key) → FAIL CLOSED.
        {
            let mut s = VerifierStore::new(db.path()).unwrap();
            assert_eq!(s.audit_key_ledger_active_id().unwrap().as_deref(), Some(kid(&b).as_str()),
                "active=B is durable across reopen");
            assert_eq!(
                s.admit_signing_key(a.clone(), false, None, 3_000).unwrap(),
                KeyAdmission::RetiredKeyRejected
            );
            assert!(s.signing_key.is_none(), "must NOT adopt a retired key for signing");
        }
        // Reopen with the correct active key B → resume.
        {
            let mut s = VerifierStore::new(db.path()).unwrap();
            assert_eq!(
                s.admit_signing_key(b.clone(), false, None, 4_000).unwrap(),
                KeyAdmission::Resumed
            );
            assert!(s.signing_key.is_some());
        }
    }

    // --- Test 2: ENV-ROTATION (adopt vs fail-closed, gap-2) -----------------
    #[test]
    fn env_rotation_new_key_requires_explicit_adopt() {
        let db = TmpDb::new("g2");
        let (a, c) = (key(1), key(3));
        { let mut s = VerifierStore::new(db.path()).unwrap();
          s.admit_signing_key(a.clone(), false, None, 1_000).unwrap(); }
        // New env key, NO adopt → fail closed.
        { let mut s = VerifierStore::new(db.path()).unwrap();
          assert_eq!(
              s.admit_signing_key(c.clone(), false, None, 2_000).unwrap(),
              KeyAdmission::UnadoptedNewKeyRejected);
          assert!(s.signing_key.is_none()); }
        // New env key, WITH adopt → records reanchor, adopts C.
        { let mut s = VerifierStore::new(db.path()).unwrap();
          assert_eq!(
              s.admit_signing_key(c.clone(), true, None, 3_000).unwrap(),
              KeyAdmission::AdoptedReanchor);
          assert_eq!(s.audit_key_ledger_active_id().unwrap().as_deref(), Some(kid(&c).as_str()));
          assert!(s.signing_key.is_some()); }
        // Subsequent boot with C (now active) resumes without adopt.
        { let mut s = VerifierStore::new(db.path()).unwrap();
          assert_eq!(
              s.admit_signing_key(c.clone(), false, None, 4_000).unwrap(),
              KeyAdmission::Resumed); }
    }

    // --- Test 3: GENESIS ANCHOR (gap-2) — mutated env can't re-root ---------
    #[test]
    fn genesis_comes_from_durable_anchor_not_env() {
        let db = TmpDb::new("g3");
        let (a, mutated) = (key(1), key(9));
        let mut s = VerifierStore::new(db.path()).unwrap();
        s.admit_signing_key(a.clone(), false, None, 1_000).unwrap(); // anchor genesis = A
        // Append a normal signed row under A.
        {
            let sk = s.signing_key.clone();
            let tx = s.conn.transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(&tx, "TEST", "{}", 1_500, sk.as_ref()).unwrap();
            tx.commit().unwrap();
        }
        // Verify while passing a MUTATED key: genesis must resolve from the
        // durable anchor (A), so the prior rows still verify and the mutated
        // key cannot re-root the keyring.
        let r = s.verify_audit_chain_full(Some(&mutated.verifying_key())).unwrap();
        assert!(r.chain_intact, "chain intact");
        assert!(r.signature_valid, "prior rows verify under the durable genesis anchor, not the mutated env key");
    }

    // --- Test 4: FIRST-BOOT BACKFILL + idempotency --------------------------
    #[test]
    fn first_boot_backfill_writes_anchor_and_is_idempotent() {
        let db = TmpDb::new("g4");
        let a = key(1);
        let mut s = VerifierStore::new(db.path()).unwrap();
        assert_eq!(
            s.admit_signing_key(a.clone(), false, None, 1_000).unwrap(),
            KeyAdmission::BackfilledGenesis);
        assert_eq!(s.audit_trust_anchor_genesis_id().unwrap().as_deref(), Some(kid(&a).as_str()));
        let genesis_rows = s.audit_key_ledger_rows().unwrap()
            .into_iter().filter(|r| r.role == "genesis").count();
        assert_eq!(genesis_rows, 1, "exactly one genesis ledger row");
        // Re-run admission with the same key → resume, no second backfill.
        assert_eq!(
            s.admit_signing_key(a.clone(), false, None, 2_000).unwrap(),
            KeyAdmission::Resumed);
        let genesis_rows2 = s.audit_key_ledger_rows().unwrap()
            .into_iter().filter(|r| r.role == "genesis").count();
        assert_eq!(genesis_rows2, 1, "backfill is idempotent — still exactly one genesis row");
    }

    /// Inject a pre-#165 in-chain `KEY_ROTATION` (old→new) with NO ledger row,
    /// simulating an in-process rotation done before #165.
    fn inject_chain_rotation(s: &mut VerifierStore, old: &SigningKey, new: &SigningKey, ts: i64) {
        let payload = serde_json::json!({
            "new_public_key_b64": b64e.encode(new.verifying_key().as_bytes()),
            "new_key_id": kid(new),
            "reason": "preexisting",
            "rotated_at_ms": ts,
        }).to_string();
        let tx = s.conn.transaction().unwrap();
        AuditChainLinker::append_audit_event_tx(&tx, "KEY_ROTATION", &payload, ts, Some(old)).unwrap();
        tx.commit().unwrap();
    }

    // --- Test 5: MIGRATION RECONCILE (consented reversion via adopt) --------
    #[test]
    fn migration_reconcile_with_adopt_records_consented_reanchor() {
        let db = TmpDb::new("g5");
        let (a, b) = (key(1), key(2));
        let mut s = VerifierStore::new(db.path()).unwrap();
        s.set_signing_key(a.clone());
        inject_chain_rotation(&mut s, &a, &b, 500); // chain A→B, env will be A

        // Env reverted to A while the chain's latest rotation is B → consented
        // adopt is required; it backfills the ledger AND logs a reanchor.
        assert_eq!(
            s.admit_signing_key(a.clone(), true, None, 1_000).unwrap(),
            KeyAdmission::AdoptedReanchor);
        let rows = s.audit_key_ledger_rows().unwrap();
        assert!(rows.iter().any(|r| r.role == "genesis" && r.key_id == kid(&a)),
            "genesis ledger row for A");
        assert!(rows.iter().any(|r| r.role == "backfill" && r.key_id == kid(&b)),
            "forensic backfill ledger row matching the pre-existing chain rotation to B");
        assert!(rows.iter().any(|r| r.role == "reanchor"
                && r.key_id == kid(&a)
                && r.prev_key_id.as_deref() == Some(kid(&b).as_str())),
            "consented reanchor row: A adopted over the chain's latest (B)");
        // The consented env key (A) is the active key.
        assert_eq!(s.audit_key_ledger_active_id().unwrap().as_deref(), Some(kid(&a).as_str()));
    }

    // --- Migration hardening: reversion at first boot, no adopt → fail-closed
    #[test]
    fn migration_reversion_no_adopt_is_fail_closed() {
        let db = TmpDb::new("g5b");
        let (a, b) = (key(1), key(2));
        let mut s = VerifierStore::new(db.path()).unwrap();
        s.set_signing_key(a.clone());
        inject_chain_rotation(&mut s, &a, &b, 500); // chain A→B
        // Env = A (reverted to a pre-rotation key), no adopt → FAIL CLOSED.
        assert_eq!(
            s.admit_signing_key(a.clone(), false, None, 1_000).unwrap(),
            KeyAdmission::MigrationReversionRejected {
                chain_latest_key_id: kid(&b),
                env_key_id: kid(&a),
            });
        // Fail-closed: nothing durable was written — no anchor, no ledger rows.
        assert!(s.audit_trust_anchor_genesis_id().unwrap().is_none(), "no anchor written on reject");
        assert!(s.audit_key_ledger_active_id().unwrap().is_none(), "no ledger row written on reject");
    }

    // --- Migration hardening: env matches the chain's latest rotation → OK ---
    #[test]
    fn migration_env_matches_latest_rotation_does_not_fire() {
        let db = TmpDb::new("g5c");
        let (a, b) = (key(1), key(2));
        let mut s = VerifierStore::new(db.path()).unwrap();
        s.set_signing_key(a.clone());
        inject_chain_rotation(&mut s, &a, &b, 500); // chain A→B
        // Env = B (correctly updated to the latest rotation) → normal backfill.
        assert_eq!(
            s.admit_signing_key(b.clone(), false, None, 1_000).unwrap(),
            KeyAdmission::BackfilledGenesis);
        assert_eq!(s.audit_trust_anchor_genesis_id().unwrap().as_deref(), Some(kid(&b).as_str()));
        assert_eq!(s.audit_key_ledger_active_id().unwrap().as_deref(), Some(kid(&b).as_str()));
    }

    // --- Migration hardening: no rotations in chain → unaffected ------------
    #[test]
    fn migration_no_rotations_is_unaffected() {
        let db = TmpDb::new("g5d");
        let a = key(1);
        let mut s = VerifierStore::new(db.path()).unwrap();
        s.set_signing_key(a.clone());
        // A signed non-rotation row, but NO KEY_ROTATION.
        {
            let sk = s.signing_key.clone();
            let tx = s.conn.transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(&tx, "TEST", "{}", 10, sk.as_ref()).unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(
            s.admit_signing_key(a.clone(), false, None, 1_000).unwrap(),
            KeyAdmission::BackfilledGenesis);
        assert_eq!(s.audit_key_ledger_active_id().unwrap().as_deref(), Some(kid(&a).as_str()));
    }

    // --- Test 6: ATOMICITY — chain row + ledger row both-or-neither ---------
    #[test]
    fn rotation_chain_row_and_ledger_row_are_atomic_across_reopen() {
        let db = TmpDb::new("g6");
        let (a, b) = (key(1), key(2));
        {
            let mut s = VerifierStore::new(db.path()).unwrap();
            s.admit_signing_key(a.clone(), false, None, 1).unwrap();
            let held = s.try_claim_epoch(0, "test-node", 0).unwrap().unwrap();
            s.record_key_rotation(b.clone(), "r", 2, held).unwrap();
        }
        let s = VerifierStore::new(db.path()).unwrap();
        let chain_rot: i64 = s.conn.query_row(
            "SELECT COUNT(*) FROM audit_log_chain WHERE event_type='KEY_ROTATION'", [], |r| r.get(0)).unwrap();
        let ledger_rot: i64 = s.durable_ref().query_row(
            "SELECT COUNT(*) FROM audit_key_ledger WHERE role='rotation'", [], |r| r.get(0)).unwrap();
        assert_eq!(chain_rot, 1, "the KEY_ROTATION chain row is durable across reopen");
        assert_eq!(ledger_rot, 1, "the ledger rotation row is durable across reopen");
        assert_eq!(chain_rot, ledger_rot, "both-present (single FULL transaction)");
    }

    // --- Regression: a rotated chain still verifies under the ledger seed ----
    #[test]
    fn rotated_chain_still_verifies_with_durable_seed() {
        let db = TmpDb::new("g7");
        let (a, b) = (key(1), key(2));
        let mut s = VerifierStore::new(db.path()).unwrap();
        s.admit_signing_key(a.clone(), false, None, 1).unwrap();
        let held = s.try_claim_epoch(0, "test-node", 0).unwrap().unwrap();
        // a signed row under A, rotate to B, a signed row under B.
        {
            let sk = s.signing_key.clone();
            let tx = s.conn.transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(&tx, "TEST", "{}", 10, sk.as_ref()).unwrap();
            tx.commit().unwrap();
        }
        s.record_key_rotation(b.clone(), "r", 20, held).unwrap();
        {
            let sk = s.signing_key.clone();
            let tx = s.conn.transaction().unwrap();
            AuditChainLinker::append_audit_event_tx(&tx, "TEST", "{}", 30, sk.as_ref()).unwrap();
            tx.commit().unwrap();
        }
        let r = s.verify_audit_chain_full(Some(&a.verifying_key())).unwrap();
        assert!(r.chain_intact, "hash chain intact across rotation");
        assert!(r.signature_valid, "all rows (A-signed AND B-signed) verify under the durable seed");
    }

    // --- Test 7: WCET — verdict path is independent of the key ledger --------
    #[test]
    fn wcet_verdict_path_does_not_touch_key_ledger() {
        // The per-command verdict (validate_vehicle_command) is a pure function
        // of (command, contract) — it takes no store, no connection, no key.
        // #165 work is entirely boot-time (admit_signing_key) + rotation-time
        // (record_key_rotation), off the hot path. This compiles & runs with no
        // VerifierStore in scope, demonstrating the independence.
        use crate::gateway::kinematics_contract::{
            validate_vehicle_command, EnforceAction, ProposedVehicleCommand,
            VehicleKinematicsContract,
        };
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 10.0,   // zero implied accel
            delta_time_s: 0.05,
            steering_angle_deg: 1.0,
            current_steering_angle_deg: 1.0, // zero steering rate
        };
        assert_eq!(validate_vehicle_command(&cmd, &contract), EnforceAction::Allow);
    }
}

// ---------------------------------------------------------------------------
// SG-010 (ASIL B) — Audit Chain Tamper Detection
// ---------------------------------------------------------------------------
//
// Verifies: SG-010. `verify_audit_chain_full` is the mechanism: every row binds
// to its predecessor through a recomputed hash AND (when signing is enabled) an
// Ed25519 signature over that hash, so any out-of-band edit to a stored row is
// detected — `chain_intact` goes false and `first_invalid_signature_index`
// pinpoints the first tampered row.
//
// These tests use a FILE-BACKED DB (tempfile): a SQLite `:memory:` database is
// per-connection, so to model a real tamperer we open a SECOND connection to the
// same file and mutate a row the FIRST connection wrote (via the `raw_conn`
// test seam), then verify through the original connection.
//
// SCOPE / HONEST GAP: SG-010's full statement also requires that audit-chain
// verification runs AUTOMATICALLY on service startup BEFORE the listener binds.
// That mechanism does NOT exist today: `src/bin/kirra_verifier_service.rs` runs
// only `check_startup_invariants` (admin-token / WAL / watchdog / posture-engine)
// before `TcpListener::bind`, and verifies the chain only on demand via the
// `/system/audit/verify` endpoint (plus a durable checkpoint on shutdown). Wiring
// a verify-and-abort into startup is a BEHAVIOR change, out of scope for a
// test-only change, so it is reported as a mechanism gap rather than asserted
// here. See RTM_GAP_REPORT.md (SG-010).
#[cfg(test)]
mod sg_010_audit_tamper_tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    /// Writes three signed audit rows to a file-backed store, returning the
    /// store, its verifying key, and the DB path string. The `TempDir` is
    /// returned so the caller keeps it alive (drop = cleanup of db + -wal/-shm).
    fn signed_chain_on_disk() -> (tempfile::TempDir, String, ed25519_dalek::VerifyingKey) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.sqlite");
        let path_str = path.to_str().unwrap().to_string();

        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let vk = sk.verifying_key();

        let mut writer = VerifierStore::new(&path_str).expect("writer store");
        writer.set_signing_key(sk);
        writer.save_posture_event_chained("n1", "E1", "{}", None, 100).unwrap();
        writer.save_posture_event_chained("n1", "E2", "{}", None, 200).unwrap();
        writer.save_posture_event_chained("n1", "E3", "{}", None, 300).unwrap();

        // Control: the untampered chain verifies clean. This proves the tamper —
        // not some pre-existing breakage — is what trips the later assertions.
        let clean = writer.verify_audit_chain_full(Some(&vk)).unwrap();
        assert!(clean.chain_intact, "freshly written chain must be intact");
        assert!(clean.signature_valid, "freshly written rows must all verify");
        assert_eq!(clean.first_invalid_signature_index, None,
            "no tampered index in a clean chain");
        assert_eq!(clean.total_entries, 3, "exactly the three rows we wrote");

        (dir, path_str, vk)
    }

    /// Tampering a previously-written row (out of band, via a SECOND connection)
    /// is detected: chain_intact == false AND the first tampered index is named.
    #[test]
    fn test_tamper_via_second_connection_detected_with_first_index() {
        let (_dir, path_str, vk) = signed_chain_on_disk();

        // A separate connection to the SAME file — the "attacker with disk
        // access". On :memory: this row would be invisible to it; file-backed,
        // it sees and can mutate what the writer committed.
        let mut tamperer = VerifierStore::new(&path_str).expect("tamperer store");
        let (tampered_id, ordinal): (i64, u64) = {
            let conn = tamperer.raw_conn();
            // Target the MIDDLE row (E2) so we assert the index points at it,
            // not trivially at row 0.
            let id: i64 = conn
                .query_row(
                    "SELECT id FROM audit_log_chain ORDER BY id ASC LIMIT 1 OFFSET 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            // 0-based position in id order = the index verify_audit_chain_full reports.
            let ord: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM audit_log_chain WHERE id < ?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
            // Back-date the event: created_at_ms is bound into BOTH the record
            // hash and the signature payload, so this single edit breaks the
            // hash chain and invalidates the signature on exactly this row.
            conn.execute(
                "UPDATE audit_log_chain SET created_at_ms = created_at_ms + 99999 WHERE id = ?1",
                [id],
            )
            .unwrap();
            (id, ord as u64)
        };

        // Verify through a FRESH store on the same file (independent of the
        // tamperer) to prove the tamper is durable, not connection-local.
        let reader = VerifierStore::new(&path_str).expect("reader store");
        let r = reader.verify_audit_chain_full(Some(&vk)).unwrap();

        assert!(!r.chain_intact,
            "back-dating row id={tampered_id} must break the hash chain");
        assert!(!r.signature_valid,
            "the tampered row's signature must no longer verify");
        assert_eq!(r.first_invalid_signature_index, Some(ordinal),
            "verify must pinpoint the FIRST tampered row's index ({ordinal})");
    }

    /// Even an unsigned chain detects tampering via the hash linkage alone
    /// (chain_intact), independent of signatures.
    #[test]
    fn test_hash_linkage_detects_tamper_without_signing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit_unsigned.sqlite");
        let path_str = path.to_str().unwrap().to_string();

        // No signing key set ⇒ unsigned rows.
        let mut store = VerifierStore::new(&path_str).expect("store");
        store.save_posture_event_chained("n1", "E1", "{}", None, 100).unwrap();
        store.save_posture_event_chained("n1", "E2", "{}", None, 200).unwrap();

        let clean = store.verify_audit_chain_full(None).unwrap();
        assert!(clean.chain_intact, "unsigned chain still hash-links");

        // Tamper the payload of the first row via the raw connection.
        store
            .raw_conn()
            .execute(
                "UPDATE audit_log_chain SET event_json = '{\"x\":1}' WHERE id = \
                 (SELECT id FROM audit_log_chain ORDER BY id ASC LIMIT 1)",
                [],
            )
            .unwrap();

        let r = store.verify_audit_chain_full(None).unwrap();
        assert!(!r.chain_intact,
            "tampering event_json must break the recomputed hash even with no signatures");
    }
}

// ---------------------------------------------------------------------------
// Issue #79 — in-transaction HA epoch fence (closes the residual gate TOCTOU).
//
// These tests prove that a node SUPERSEDED between the request-path gate check
// and the durable commit cannot land even one stale top-tier write: the fence
// re-reads `ha_state.epoch` inside the write transaction and rejects on any
// mismatch, dropping the transaction with NO partial mutation. The legitimate
// path (held == durable) still commits — so the fence is demonstrably the only
// thing rejecting in the race case.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod epoch_fence_79_tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn report(nonce: &str) -> FederatedTrustReport {
        FederatedTrustReport {
            source_controller_id: "ctrl-A".to_string(),
            asset_id: "asset-1".to_string(),
            posture: crate::verifier::FleetPosture::Nominal,
            issued_at_ms: 1_000,
            expires_at_ms: 9_000,
            nonce_hex: nonce.to_string(),
            signature_b64: "sig".to_string(),
        }
    }

    /// Claim the first epoch (held == durable == 1) — what an Active node holds.
    fn claimed(s: &mut VerifierStore) -> u64 {
        s.try_claim_epoch(0, "self", 0)
            .unwrap()
            .expect("first epoch claim wins on a fresh store")
    }

    /// CORE PROOF — race closure: held == 1 (the request-path gate would pass),
    /// then a concurrent instance claims and the durable epoch advances to 2.
    /// A top-tier durable write with the now-stale held == 1 is REJECTED with
    /// `EpochSuperseded`, and NOTHING partial lands (nonce not burned, no report
    /// row, `ha_state` untouched). Contrast with the legitimate-path test below,
    /// where the identical write commits because held == durable — so the fence
    /// is the sole reason for rejection here; the TOCTOU window is closed.
    #[test]
    fn fenced_federation_write_superseded_lands_no_partial() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        let held = claimed(&mut s); // held == durable == 1
        assert_eq!(held, 1);

        // Concurrent claim advances the durable epoch to 2 — we are now stale.
        assert_eq!(s.try_claim_epoch(1, "other", 5).unwrap(), Some(2));
        assert_eq!(s.current_epoch().unwrap(), 2);

        let err = s
            .save_federated_report_chained(&report("cafe"), 9_000, held)
            .unwrap_err();
        match err {
            DurableWriteError::Fenced(FenceError::EpochSuperseded { held: h, durable: d }) => {
                assert_eq!((h, d), (1, 2), "fence reports stale-held vs durable epoch");
            }
            other => panic!("expected EpochSuperseded, got {other:?}"),
        }

        assert!(
            !s.has_seen_federation_nonce("cafe").unwrap(),
            "fenced write must NOT burn the nonce"
        );
        assert!(
            s.load_federated_reports_for_asset("asset-1").unwrap().is_empty(),
            "fenced write must NOT persist the report row"
        );
        assert_eq!(s.current_epoch().unwrap(), 2, "fenced attempt must not touch ha_state");
    }

    /// LEGITIMATE PATH: held == durable → the identical write commits. The only
    /// delta from the race test is the matching epoch, isolating the fence as
    /// the cause of the rejection there.
    #[test]
    fn legitimate_federation_write_commits_when_held_matches_durable() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        let held = claimed(&mut s);
        s.save_federated_report_chained(&report("beef"), 9_000, held)
            .unwrap();
        assert!(
            s.has_seen_federation_nonce("beef").unwrap(),
            "held == durable must commit and burn the nonce"
        );
    }

    /// FAIL-CLOSED when the durable epoch is unreadable (ha_state row absent):
    /// the fence returns `EpochUnreadable` and the write never proceeds blind.
    #[test]
    fn fenced_fail_closed_when_epoch_unreadable() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        let held = claimed(&mut s);
        s.conn.execute("DELETE FROM ha_state WHERE id = 1", []).unwrap();

        let err = s
            .save_federated_report_chained(&report("f00d"), 9_000, held)
            .unwrap_err();
        assert!(
            matches!(err, DurableWriteError::Fenced(FenceError::EpochUnreadable)),
            "absent ha_state row must fail closed (EpochUnreadable), not write blind"
        );
        assert!(!s.has_seen_federation_nonce("f00d").unwrap());
    }

    /// NEVER-CLAIMED (held == 0) is fenced even at genesis (durable == 0): a node
    /// that never legitimately claimed an epoch must not perform a top-tier write.
    #[test]
    fn fenced_when_never_claimed_held_zero() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        assert_eq!(s.current_epoch().unwrap(), 0, "genesis durable epoch is 0");

        let err = s
            .save_federated_report_chained(&report("0000"), 9_000, 0)
            .unwrap_err();
        assert!(
            matches!(
                err,
                DurableWriteError::Fenced(FenceError::EpochSuperseded { held: 0, durable: 0 })
            ),
            "held == 0 must be fenced even when durable == 0"
        );
        assert!(!s.has_seen_federation_nonce("0000").unwrap());
    }

    /// The SECOND fenced site — `record_key_rotation` — is covered too: a
    /// superseded rotation is rejected and swaps NOTHING (no KEY_ROTATION chain
    /// row, in-memory signing key unchanged).
    #[test]
    fn fenced_key_rotation_superseded_lands_no_partial() {
        let mut s = VerifierStore::new(":memory:").unwrap();
        let a = SigningKey::from_bytes(&[1; 32]);
        s.set_signing_key(a.clone());
        let held = claimed(&mut s); // held == durable == 1

        assert_eq!(s.try_claim_epoch(1, "other", 5).unwrap(), Some(2)); // superseded
        let b = SigningKey::from_bytes(&[2; 32]);

        let err = s.record_key_rotation(b.clone(), "fenced", 9, held).unwrap_err();
        assert!(matches!(
            err,
            DurableWriteError::Fenced(FenceError::EpochSuperseded { held: 1, durable: 2 })
        ));

        let rotations: i64 = s
            .conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log_chain WHERE event_type = 'KEY_ROTATION'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rotations, 0, "fenced rotation must not append a KEY_ROTATION row");
        assert_eq!(
            s.signing_key.as_ref().unwrap().verifying_key(),
            a.verifying_key(),
            "fenced rotation must NOT swap the in-memory signing key"
        );
    }
}

// ---------------------------------------------------------------------------
// #77 — signed anchor-HEAD high-water mark: tail-truncation / deletion + head
// tamper detection, and the #74 power-loss interaction.
//
// The per-row chain walk cannot see a TRUNCATED tail: deleting the last k rows
// leaves the surviving prefix internally hash-consistent. The signed head closes
// that gap by recording the highest committed (sequence, record_hash). These
// tests mutate the chain/head out-of-band via the `raw_conn` seam (the same
// thing a tamperer with disk access would do).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod audit_anchor_head_77_tests {
    use super::*;
    use rusqlite::params;
    use ed25519_dalek::SigningKey;
    use base64::engine::general_purpose::STANDARD as b64e;

    fn signed_store() -> (VerifierStore, ed25519_dalek::VerifyingKey) {
        let mut s = VerifierStore::new(":memory:").expect("store");
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let vk = sk.verifying_key();
        s.set_signing_key(sk);
        (s, vk)
    }

    fn append3(s: &mut VerifierStore) {
        s.save_posture_event_chained("n", "E1", "{}", None, 100).unwrap();
        s.save_posture_event_chained("n", "E2", "{}", None, 200).unwrap();
        s.save_posture_event_chained("n", "E3", "{}", None, 300).unwrap();
    }

    fn read_head(s: &mut VerifierStore) -> (i64, String, Option<String>, Option<String>) {
        s.raw_conn()
            .query_row(
                "SELECT sequence, record_hash_hex, signature_b64, key_id \
                 FROM audit_anchor_head WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
    }

    /// LEGITIMATE: a freshly-written signed chain verifies clean AND the head
    /// matches the tail.
    #[test]
    fn clean_chain_head_verifies() {
        let (mut s, vk) = signed_store();
        append3(&mut s);
        let r = s.verify_audit_chain_full(Some(&vk)).unwrap();
        assert!(r.chain_intact && r.signature_valid, "control: chain itself is intact");
        assert!(r.head_verified, "head must match the tail on a clean chain");
        assert_eq!(r.head_status, "OK");
        assert_eq!(r.total_entries, 3);
    }

    /// EMPTY chain → no head required, clean.
    #[test]
    fn empty_chain_needs_no_head() {
        let (s, vk) = signed_store();
        let r = s.verify_audit_chain_full(Some(&vk)).unwrap();
        assert_eq!(r.total_entries, 0);
        assert!(r.head_verified);
        assert_eq!(r.head_status, "EMPTY_CHAIN");
    }

    /// TAIL TRUNCATION: delete the last row out-of-band, leaving the head
    /// pointing past the surviving tail → DETECTED. The surviving prefix is still
    /// internally consistent (`chain_intact == true`), which is exactly the gap
    /// the head closes.
    #[test]
    fn tail_truncation_is_detected() {
        let (mut s, vk) = signed_store();
        append3(&mut s);
        // Delete the last row (E3, sequence 2) but NOT the head (still seq 2).
        s.raw_conn()
            .execute(
                "DELETE FROM audit_log_chain \
                 WHERE id = (SELECT MAX(id) FROM audit_log_chain)",
                [],
            )
            .unwrap();

        let r = s.verify_audit_chain_full(Some(&vk)).unwrap();
        assert!(
            r.chain_intact,
            "the surviving 2-row prefix is still hash-consistent — the walk alone cannot see the truncation"
        );
        assert!(!r.head_verified, "the head high-water mark must detect the deleted tail");
        assert_eq!(r.head_status, "TRUNCATION_DETECTED");
        assert_eq!(r.total_entries, 2, "only the prefix survived");
    }

    /// Deleting MORE than one tail row is still caught.
    #[test]
    fn multi_row_truncation_is_detected() {
        let (mut s, vk) = signed_store();
        append3(&mut s);
        s.raw_conn()
            .execute(
                "DELETE FROM audit_log_chain WHERE id IN \
                 (SELECT id FROM audit_log_chain ORDER BY id DESC LIMIT 2)",
                [],
            )
            .unwrap();
        let r = s.verify_audit_chain_full(Some(&vk)).unwrap();
        assert!(!r.head_verified);
        assert_eq!(r.head_status, "TRUNCATION_DETECTED");
        assert_eq!(r.total_entries, 1);
    }

    /// HEAD TAMPER: corrupt the head signature → verify fails closed.
    #[test]
    fn head_signature_tamper_is_detected() {
        let (mut s, vk) = signed_store();
        append3(&mut s);
        // A well-formed but WRONG signature (64 zero bytes) — decodes, never verifies.
        let bogus = b64e.encode([0u8; 64]);
        s.raw_conn()
            .execute(
                "UPDATE audit_anchor_head SET signature_b64 = ?1 WHERE id = 1",
                params![bogus],
            )
            .unwrap();
        let r = s.verify_audit_chain_full(Some(&vk)).unwrap();
        assert!(!r.head_verified, "a tampered head signature must fail closed");
        assert_eq!(r.head_status, "HEAD_SIGNATURE_INVALID");
    }

    /// HEAD ABSENT on a non-empty chain → fail closed (deleted head / unmigrated).
    #[test]
    fn absent_head_on_nonempty_chain_fails_closed() {
        let (mut s, vk) = signed_store();
        append3(&mut s);
        s.raw_conn().execute("DELETE FROM audit_anchor_head", []).unwrap();
        let r = s.verify_audit_chain_full(Some(&vk)).unwrap();
        assert!(!r.head_verified);
        assert_eq!(r.head_status, "HEAD_ABSENT");
    }

    /// #74 POWER-LOSS interaction: the last commit (row + its head update) is lost
    /// TOGETHER on an ungraceful cut. Simulate by dropping the last row AND
    /// restoring the head to its prior (committed) value. Verify must PASS — head
    /// stays consistent with the recovered tail → NO false truncation alarm.
    #[test]
    fn power_loss_of_last_commit_does_not_false_alarm() {
        let (mut s, vk) = signed_store();
        s.save_posture_event_chained("n", "E1", "{}", None, 100).unwrap();
        s.save_posture_event_chained("n", "E2", "{}", None, 200).unwrap();
        // Head as committed after E2 (the state the head reverts to on rollback).
        let head_after_e2 = read_head(&mut s);
        // E3 commits: row + head→E3 atomically.
        s.save_posture_event_chained("n", "E3", "{}", None, 300).unwrap();

        // Ungraceful power loss of E3's single commit: row AND head update vanish
        // together (same NORMAL transaction). Recover to the post-E2 state.
        {
            let c = s.raw_conn();
            c.execute(
                "DELETE FROM audit_log_chain WHERE id = (SELECT MAX(id) FROM audit_log_chain)",
                [],
            )
            .unwrap();
            c.execute(
                "UPDATE audit_anchor_head \
                 SET sequence = ?1, record_hash_hex = ?2, signature_b64 = ?3, key_id = ?4 \
                 WHERE id = 1",
                params![head_after_e2.0, head_after_e2.1, head_after_e2.2, head_after_e2.3],
            )
            .unwrap();
        }

        let r = s.verify_audit_chain_full(Some(&vk)).unwrap();
        assert!(r.chain_intact, "recovered 2-row prefix is intact");
        assert!(
            r.head_verified,
            "head and tail lost the SAME commit together → consistent → NO false alarm (#74)"
        );
        assert_eq!(r.head_status, "OK");
        assert_eq!(r.total_entries, 2);
    }

    /// BACKFILL: a chain that has rows but no head (a pre-#77 on-disk chain) gets
    /// a signed head from `ensure_audit_anchor_head`, after which it verifies clean.
    #[test]
    fn ensure_audit_anchor_head_backfills_legacy_chain() {
        let (mut s, vk) = signed_store();
        append3(&mut s);
        // Model a pre-#77 chain: rows present, head missing.
        s.raw_conn().execute("DELETE FROM audit_anchor_head", []).unwrap();
        assert!(!s.verify_audit_chain_full(Some(&vk)).unwrap().head_verified,
            "precondition: no head → fail closed");

        s.ensure_audit_anchor_head(999).unwrap();

        let r = s.verify_audit_chain_full(Some(&vk)).unwrap();
        assert!(r.head_verified, "backfilled signed head must verify");
        assert_eq!(r.head_status, "OK");
        // Idempotent: a second call is a no-op and stays clean.
        s.ensure_audit_anchor_head(1000).unwrap();
        assert!(s.verify_audit_chain_full(Some(&vk)).unwrap().head_verified);
    }
}

// --- #87: fabric causal-log forensic chain tests ---------------------------
//
// The KEY WIN over the prior in-memory log: the record hash binds the causality
// edges, so tampering an edge (caused_by / affects_assets / fabric_generation)
// is DETECTED by `verify_causal_chain_integrity`. These tests mutate the chain
// out-of-band via the `raw_conn` seam (what a tamperer with disk access does).
#[cfg(test)]
mod causal_chain_87_tests {
    use super::*;
    use rusqlite::params;
    use ed25519_dalek::SigningKey;

    fn signed_store() -> (VerifierStore, ed25519_dalek::VerifyingKey) {
        let mut s = VerifierStore::new(":memory:").expect("store");
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let vk = sk.verifying_key();
        s.set_signing_key(sk);
        (s, vk)
    }

    /// Append three causal events signed by the store's key.
    fn append3_causal(s: &mut VerifierStore) {
        let sk = s.signing_key.clone();
        let id1 = "entry-1".to_string();
        s.append_causal_event(&CausalEventInput {
            entry_id: "entry-1", asset_id: "leader", event_type: "FAULT", payload: "{}",
            caused_by: &[], affects_assets: &["follower".to_string()],
            fabric_generation: 1, timestamp_ms: 100,
        }, sk.as_ref()).unwrap();
        s.append_causal_event(&CausalEventInput {
            entry_id: "entry-2", asset_id: "follower", event_type: "DEGRADE", payload: "{}",
            caused_by: std::slice::from_ref(&id1), affects_assets: &[],
            fabric_generation: 1, timestamp_ms: 200,
        }, sk.as_ref()).unwrap();
        s.append_causal_event(&CausalEventInput {
            entry_id: "entry-3", asset_id: "follower", event_type: "STOP", payload: "{}",
            caused_by: &[id1], affects_assets: &["leader".to_string()],
            fabric_generation: 2, timestamp_ms: 300,
        }, sk.as_ref()).unwrap();
    }

    #[test]
    fn clean_signed_causal_chain_verifies() {
        let (mut s, vk) = signed_store();
        append3_causal(&mut s);
        let r = s.verify_causal_chain_integrity(Some(&vk)).unwrap();
        assert!(r.chain_intact, "chain must be intact");
        assert!(r.signature_valid, "all sigs must verify");
        assert!(r.head_verified, "head must verify: {}", r.head_status);
        assert_eq!(r.head_status, "OK");
        assert_eq!(r.total_entries, 3);
        assert_eq!(r.signed_entries, 3);
    }

    /// KEY WIN: tampering `caused_by` breaks the recomputed record hash.
    #[test]
    fn tampering_caused_by_edge_is_detected() {
        let (mut s, vk) = signed_store();
        append3_causal(&mut s);
        // Precondition: clean.
        assert!(s.verify_causal_chain_integrity(Some(&vk)).unwrap().chain_intact);
        // Rewrite the caused_by edge of the middle row.
        s.raw_conn().execute(
            "UPDATE fabric_causal_log SET caused_by = ?1 WHERE entry_id = 'entry-2'",
            params![r#"["forged-cause"]"#],
        ).unwrap();
        let r = s.verify_causal_chain_integrity(Some(&vk)).unwrap();
        assert!(!r.chain_intact, "tampered caused_by edge MUST break chain_intact");
    }

    /// KEY WIN: tampering `affects_assets` breaks the recomputed record hash.
    #[test]
    fn tampering_affects_assets_edge_is_detected() {
        let (mut s, vk) = signed_store();
        append3_causal(&mut s);
        assert!(s.verify_causal_chain_integrity(Some(&vk)).unwrap().chain_intact);
        s.raw_conn().execute(
            "UPDATE fabric_causal_log SET affects_assets = ?1 WHERE entry_id = 'entry-1'",
            params![r#"["forged-asset"]"#],
        ).unwrap();
        let r = s.verify_causal_chain_integrity(Some(&vk)).unwrap();
        assert!(!r.chain_intact, "tampered affects_assets edge MUST break chain_intact");
    }

    /// KEY WIN: tampering `fabric_generation` breaks the recomputed record hash.
    #[test]
    fn tampering_fabric_generation_edge_is_detected() {
        let (mut s, vk) = signed_store();
        append3_causal(&mut s);
        assert!(s.verify_causal_chain_integrity(Some(&vk)).unwrap().chain_intact);
        s.raw_conn().execute(
            "UPDATE fabric_causal_log SET fabric_generation = 99 WHERE entry_id = 'entry-3'",
            [],
        ).unwrap();
        let r = s.verify_causal_chain_integrity(Some(&vk)).unwrap();
        assert!(!r.chain_intact, "tampered fabric_generation edge MUST break chain_intact");
    }

    /// TRUNCATION: delete the tail row; the surviving prefix is internally
    /// consistent but the signed head still points past it → detected.
    #[test]
    fn truncation_of_causal_tail_is_detected() {
        let (mut s, vk) = signed_store();
        append3_causal(&mut s);
        s.raw_conn().execute(
            "DELETE FROM fabric_causal_log WHERE id = (SELECT MAX(id) FROM fabric_causal_log)",
            [],
        ).unwrap();
        let r = s.verify_causal_chain_integrity(Some(&vk)).unwrap();
        assert!(r.chain_intact, "surviving 2-row prefix is still hash-consistent");
        assert!(!r.head_verified, "the signed head must detect the deleted tail");
        assert_eq!(r.head_status, "TRUNCATION_DETECTED");
    }

    /// HEAD SIGNATURE TAMPER: a well-formed but wrong head signature fails closed.
    #[test]
    fn causal_head_signature_tamper_is_detected() {
        let (mut s, vk) = signed_store();
        append3_causal(&mut s);
        let bogus = b64e.encode([0u8; 64]);
        s.raw_conn().execute(
            "UPDATE fabric_causal_anchor_head SET signature_b64 = ?1 WHERE id = 1",
            params![bogus],
        ).unwrap();
        let r = s.verify_causal_chain_integrity(Some(&vk)).unwrap();
        assert!(!r.head_verified);
        assert_eq!(r.head_status, "HEAD_SIGNATURE_INVALID");
    }

    /// PERSISTENCE ROUND-TRIP: append on a temp-file DB, drop, reopen, reload.
    #[test]
    fn causal_entries_persist_across_reopen() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("kirra_causal_test_{}.sqlite", std::process::id()));
        let path_str = path.to_str().unwrap().to_string();
        // Clean any prior artifact.
        let _ = std::fs::remove_file(&path);

        let entry_id;
        {
            let mut s = VerifierStore::new(&path_str).expect("file store");
            let e = s.append_causal_event(&CausalEventInput {
                entry_id: "persist-1", asset_id: "a", event_type: "EVT", payload: "{}",
                caused_by: &[], affects_assets: &["x".to_string()],
                fabric_generation: 3, timestamp_ms: 1000,
            }, None).unwrap();
            entry_id = e.entry_id.clone();
            s.append_causal_event(&CausalEventInput {
                entry_id: "persist-2", asset_id: "b", event_type: "EVT2", payload: "{}",
                caused_by: std::slice::from_ref(&entry_id), affects_assets: &[],
                fabric_generation: 3, timestamp_ms: 2000,
            }, None).unwrap();
        } // drop closes the connection

        {
            let s = VerifierStore::new(&path_str).expect("reopen file store");
            let rows = s.load_causal_entries().unwrap();
            assert_eq!(rows.len(), 2, "rows must survive reopen");
            assert_eq!(rows[0].entry_id, "persist-1");
            assert_eq!(rows[0].affects_assets, vec!["x".to_string()]);
            assert_eq!(rows[1].caused_by, vec![entry_id]);
            // Chain still verifies after reopen.
            let r = s.verify_causal_chain_integrity(None).unwrap();
            assert!(r.chain_intact && r.head_verified, "reopened chain must verify");
        }

        let _ = std::fs::remove_file(&path);
        // WAL sidecar files.
        let _ = std::fs::remove_file(format!("{path_str}-wal"));
        let _ = std::fs::remove_file(format!("{path_str}-shm"));
    }
}
