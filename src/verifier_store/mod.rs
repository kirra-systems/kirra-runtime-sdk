// src/verifier_store/mod.rs
//
// VerifierStore — rusqlite WAL-mode persistence. The struct, lifecycle/core
// methods, shared free helpers and durable-write types live here; per-table
// domain method groups are split into the child modules declared below. Every
// child shares the struct's private `conn`/`durable_conn` handles via Rust's
// descendant-module visibility, so the split is a pure move (no new public
// surface beyond a few `pub(crate)` helpers needed across domains).

use crate::federation::FederatedTrustReport;
use crate::verifier::{NodeTrustState, RegisteredNode};
use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
use rusqlite::{params, Connection, Result};
use std::collections::HashMap;

pub struct AuditChainVerifyResult {
    /// Hash-linkage integrity ONLY (recomputed record hash matches, prev-linkage
    /// unbroken, no sequence gap). NOT sufficient on its own: the record hash does
    /// not cover the row signature, so a tampered/invalid signature leaves this
    /// `true` while `signature_valid` is `false` (#690). Gate trust on
    /// [`verified()`](Self::verified), never on this field alone.
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

impl AuditChainVerifyResult {
    /// THE authoritative verdict (#690). The chain is trustworthy only if ALL three
    /// independent checks hold:
    /// - `chain_intact` — no in-place row edit / broken prev-linkage / sequence gap;
    /// - `signature_valid` — every signed row verifies under its key (a bad signature,
    ///   e.g. a failed `KEY_ROTATION` row, sets this `false` even though the hashes
    ///   still link, since the record hash does not cover the signature);
    /// - `head_verified` — no tail truncation/deletion (the signed anchor-head matches
    ///   the chain tail).
    ///
    /// Callers MUST gate trust on this, never on `chain_intact` alone.
    pub fn verified(&self) -> bool {
        self.chain_intact && self.signature_valid && self.head_verified
    }
}

/// Verification verdict for the fabric causal-log forensic chain (#87).
/// Same shape as [`AuditChainVerifyResult`]: `chain_intact` covers in-place
/// row edits (recomputed record hash mismatch, broken prev-linkage, sequence
/// gaps); `head_verified` covers tail truncation/deletion via the signed
/// anchor-head high-water mark. Together they cover edit + truncation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CausalChainVerifyResult {
    /// Hash-linkage integrity ONLY (recomputed record hash matches, prev-linkage
    /// unbroken, no sequence gap). NOT sufficient on its own: the record hash does
    /// not cover the row signature, so a tampered/invalid signature leaves this
    /// `true` while `signature_valid` is `false` (#690). Gate trust on
    /// [`verified()`](Self::verified), never on this field alone.
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

impl CausalChainVerifyResult {
    /// THE authoritative verdict (#690): `chain_intact && signature_valid &&
    /// head_verified`. Like [`AuditChainVerifyResult::verified`], `chain_intact`
    /// alone is NOT sufficient — a tampered signature leaves it `true` while
    /// `signature_valid` is `false`. Gate trust on this.
    pub fn verified(&self) -> bool {
        self.chain_intact && self.signature_valid && self.head_verified
    }
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

/// A registered API principal (WS-1 · G7). The bearer token is stored ONLY as its
/// SHA-256 hex (`token_sha256`); the plaintext is shown once at mint and never
/// persisted. `role` is the wire role string (parsed fail-closed by
/// `authz::ApiRole::parse_role`). `revoked_at_ms` NULL = active.
#[derive(Debug, Clone)]
pub struct ApiPrincipalRecord {
    pub principal_id: String,
    pub role: String,
    pub created_at_ms: u64,
    pub revoked_at_ms: Option<u64>,
}

impl ApiPrincipalRecord {
    /// True iff the principal is registered and not revoked.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.revoked_at_ms.is_none()
    }
}

/// A registered mTLS cert principal (WS-1 · G7 · Track 1.2 · WP-15). A client cert
/// — CA-verified by rustls — pinned to a principal by the SHA-256 hex of its leaf
/// DER. Distinct from [`ApiPrincipalRecord`] because a cert carries an EXPIRY
/// (`not_after_ms`, the X.509 notAfter) whereas a bearer token does not: a cert is
/// a lifecycle credential (issued → valid-window → renewed/expired), and the
/// resolution path fail-closes past its expiry exactly as it does on revocation.
#[derive(Debug, Clone)]
pub struct CertPrincipalRecord {
    pub principal_id: String,
    pub role: String,
    pub created_at_ms: u64,
    pub revoked_at_ms: Option<u64>,
    /// The pinned cert's X.509 notAfter, in ms. `None` = no expiry tracked (a
    /// legacy pin, or one registered without an expiry) — never expires on age.
    pub not_after_ms: Option<u64>,
}

impl CertPrincipalRecord {
    /// True iff not explicitly revoked. (Expiry is a SEPARATE, time-relative gate —
    /// see [`is_expired`](Self::is_expired) — so this stays a pure record predicate.)
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.revoked_at_ms.is_none()
    }

    /// True iff an expiry is tracked AND `now_ms` is at or past it. `notAfter` is an
    /// inclusive upper bound, so `now_ms >= not_after_ms` is expired (fail-closed:
    /// the boundary instant is already out of the valid window).
    #[must_use]
    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.not_after_ms.is_some_and(|exp| now_ms >= exp)
    }

    /// True iff the credential is VALID at `now_ms`: registered, not revoked, and
    /// not past its expiry. This is the single question the auth path asks.
    #[must_use]
    pub fn is_valid_at(&self, now_ms: u64) -> bool {
        self.is_active() && !self.is_expired(now_ms)
    }
}

/// WP-15 (MGA G-19) — a point-in-time census of the cert-principal registry by
/// lifecycle state, for the `/metrics` expiry gauges and the periodic warning
/// sweep. Every principal falls in exactly ONE of {revoked, expired, active}; the
/// active set is further split by whether it is within the warning window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CertExpirySummary {
    /// All registered cert principals.
    pub total: u64,
    /// Not revoked AND not expired at the census instant.
    pub active: u64,
    /// Explicitly revoked (revocation takes precedence over expiry in the census).
    pub revoked: u64,
    /// Not revoked but at/past `not_after_ms` — a live pin that no longer authorizes.
    pub expired: u64,
    /// Active AND with a tracked expiry falling inside the warning window
    /// (`not_after_ms - now_ms <= warn_window_ms`) — renew before it lapses.
    pub expiring_soon: u64,
    /// Active but with NO expiry tracked (a legacy pin — cannot be aged out).
    pub no_expiry: u64,
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
/// Returned by `VerifierStore::assert_epoch_held`, which re-reads the durable
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
    /// H1: the federation nonce was already burned — a replay that raced past the
    /// request-path `has_seen_federation_nonce` check and lost the durable
    /// single-use claim (the `nonce_hex PRIMARY KEY` UNIQUE violation aborted the
    /// transaction). Distinct from `Db` so the handler maps it to a clean
    /// `FEDERATED_NONCE_REPLAY` rejection (+ audit), NOT an opaque HTTP 500.
    NonceReplay,
    /// Item 20: the report's `source_generation` is <= the per-(controller, asset)
    /// high-water mark — a generation regression or replay of an older signed
    /// report that slipped inside the freshness window. The transaction is aborted
    /// (report NOT persisted, nonce NOT burned, high-water NOT advanced), so the
    /// handler maps it to a clean `FEDERATED_GENERATION_REGRESS` rejection (+ audit),
    /// NOT an opaque HTTP 500. `found` is the offered generation; `high_water` is the
    /// last accepted generation it failed to exceed.
    GenerationRegress {
        found: u64,
        high_water: u64,
    },
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
            DurableWriteError::NonceReplay => {
                write!(f, "durable write rejected: federation nonce already burned (replay)")
            }
            DurableWriteError::GenerationRegress { found, high_water } => write!(
                f,
                "durable write rejected: federation generation {found} <= high-water {high_water} (regress/replay)"
            ),
            DurableWriteError::Db(e) => write!(f, "durable write failed: {e}"),
        }
    }
}

impl std::error::Error for DurableWriteError {}

/// Retention horizon for burned federation nonces (review M2). Far larger than
/// `FEDERATION_REPLAY_WINDOW_MS` (5 s) so a pruned nonce can never reopen a replay
/// slot — a report bearing an aged nonce fails the freshness gate on its fixed,
/// signed `issued_at_ms` regardless of the nonce row. 1 hour absorbs clock skew.
const FEDERATION_NONCE_RETENTION_MS: i64 = 3_600_000;

/// True iff a rusqlite error is a UNIQUE / PRIMARY KEY constraint violation. Used
/// (H1) to map a raced federation-nonce double-INSERT — the durable single-use
/// claim losing to a concurrent replay — onto `DurableWriteError::NonceReplay`.
fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(err, _)
            if err.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

/// Busy timeout (ms) applied to EVERY connection — writer, durable, and read
/// replica (P2). Brief, so a connection rides out a concurrent WAL checkpoint
/// (which briefly locks the WAL) instead of immediately surfacing `SQLITE_BUSY`.
/// Without it on the writer/durable connections, a checkpoint transient surfaced
/// as a fail-closed error on the heartbeat/epoch path (a spurious self-demote
/// signal). 250 ms is well inside the heartbeat interval, so durability is
/// unaffected — the write either acquires the lock or fails as before, just
/// after a short, bounded wait.
const SQLITE_BUSY_TIMEOUT_MS: u64 = 250;

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
    /// The database path this store was opened from (`":memory:"` for in-memory).
    /// Retained so a [`StoreHandle`](crate::store_handle::StoreHandle) can open
    /// independent READ-ONLY replica connections to the same WAL file.
    db_path: String,
    /// #772 F6 — TEST-ONLY invocation counter for the incident-class durable
    /// posture-event writes (`save_posture_event_chained*_durable`). Lets the
    /// gating test prove that a TRANSITION takes the FULL-connection durable path
    /// while a 20 Hz `POSTURE_CACHE_REFRESHED` does NOT — the load-bearing INV-12
    /// gate that is otherwise invisible on an in-memory store (where `durable_conn`
    /// falls back to `conn`). Never compiled into production.
    #[cfg(test)]
    durable_posture_writes: std::sync::atomic::AtomicU64,
    /// #772 F3 — TEST-ONLY fault seam: when set, the durable posture-event writes
    /// return `Err` at entry WITHOUT touching the DB, so a test can exercise the
    /// recalc's "durable write failed → count it, fall back to the NORMAL write,
    /// do NOT suppress the transition" path. Never compiled into production.
    #[cfg(test)]
    fail_durable_posture_writes: std::sync::atomic::AtomicBool,
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
            prev,
            rec,
            event_type,
            created_at_ms,
            sequence.unwrap_or(0).max(0) as u64,
        ),
        _ => canonical_signing_payload(prev, rec, event_type, created_at_ms),
    }
}

/// Verify a base64 Ed25519 signature over `payload` under `vk`.
fn audit_verify_sig(vk: &ed25519_dalek::VerifyingKey, payload: &str, sig_b64: &str) -> bool {
    use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
    use ed25519_dalek::Signature;
    b64e.decode(sig_b64)
        .ok()
        .and_then(|b| <[u8; 64]>::try_from(b.as_slice()).ok())
        // L-2: verify_strict rejects malleable / non-canonical signatures, matching
        // the rest of the crate's crypto discipline (the federation path already
        // uses it). `verify_strict` is inherent on VerifyingKey — no Verifier trait.
        .map(|arr| {
            vk.verify_strict(payload.as_bytes(), &Signature::from_bytes(&arr))
                .is_ok()
        })
        .unwrap_or(false)
}

/// If a (already-signature-verified) `KEY_ROTATION` event's payload announces a
/// new pubkey + key_id whose fingerprint matches, add it to the keyring.
/// Content-addressed: a rotation cannot smuggle in a key under a wrong id.
fn extend_keyring_from_rotation(
    keyring: &mut std::collections::HashMap<String, ed25519_dalek::VerifyingKey>,
    event_json: &str,
) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(event_json) else {
        return;
    };
    let (Some(npk), Some(nkid)) = (v["new_public_key_b64"].as_str(), v["new_key_id"].as_str())
    else {
        return;
    };
    let Some(nvk) = audit_decode_vk(npk) else {
        return;
    };
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
pub(crate) struct LedgerRow {
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
    let Some(vk) = audit_decode_vk(&r.pubkey_b64) else {
        return false;
    };
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

// Domain submodules — split from the original monolithic verifier_store.rs.
// Each child module holds an `impl VerifierStore` block for one table-domain;
// all share the struct's private connection handles via descendant visibility.
mod nodes;
// WP-18 (G-9 store half) — the node-registry storage trait + its in-memory
// reference backend (the 2nd VerifierStorage-family seam after EpochFence).
pub use nodes::{assert_node_store_contract, InMemoryNodeStore, NodeStore};
mod attestation;
mod audit;
mod av_subsystem;
// ADR-0035 Stage 2 (trait-seam inversion) — the AV-subsystem diagnostic-meta storage
// trait + its in-memory reference backend (confidence floor + telemetry stamp +
// recovery-streak counters; models the increment-on-unregistered failure mode).
pub use av_subsystem::{
    assert_av_subsystem_store_contract, AvSubsystemStore, InMemAvError, InMemoryAvSubsystemStore,
};
mod cert_principals;
// ADR-0035 Stage 2 (trait-seam inversion) — the cert-principal storage trait + its
// in-memory reference backend (a richer seam: models the UNIQUE-fingerprint conflict
// and the fail-closed expiry-overflow refusal as portable contract failure modes).
pub use cert_principals::{
    assert_cert_principal_store_contract, CertPrincipalStore, InMemCertError,
    InMemoryCertPrincipalStore,
};
mod epoch;
mod federation;
mod operators;
// ADR-0035 Stage 2 (trait-seam inversion) — the operator-registry storage trait + its
// in-memory reference backend (registry CRUD only; the audit-chained clearance-grant
// methods stay inherent, belonging to the harder persistence tier).
pub use operators::{assert_operator_store_contract, InMemoryOperatorStore, OperatorStore};
mod ota_campaigns;
mod posture;
mod principals;
// ADR-0035 Stage 2 (trait-seam inversion) — the API-principal storage trait + its
// in-memory reference backend (the 3rd VerifierStorage-family seam after EpochFence
// and NodeStore), extending the seam program toward the persistence/authority split.
pub use principals::{
    assert_principal_store_contract, InMemPrincipalError, InMemoryPrincipalStore, PrincipalStore,
};
// WP-18 3/3 (G-9 store half) — the backend-portable HA epoch-fence trait + its
// in-memory reference backend (SQLite realizes it via `ha_state` + BEGIN IMMEDIATE,
// a future Postgres backend via SELECT … FOR UPDATE).
pub use epoch::{assert_fence_contract, EpochFence, HaLease, InMemFenceError, InMemoryEpochFence};
mod fabric;
pub mod migrations; // WP-18/G-20 versioned schema migration framework (user_version)
pub mod migrations_postgres; // WP-18 slice 3 — Postgres SchemaBackend over an injected executor seam

impl VerifierStore {
    pub fn new(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        // P2: ride out a concurrent WAL checkpoint instead of surfacing SQLITE_BUSY
        // as a fail-closed error (the read replica already does this).
        conn.busy_timeout(std::time::Duration::from_millis(SQLITE_BUSY_TIMEOUT_MS))?;

        // WP-18 (G-20): FAIL-CLOSED downgrade protection — refuse to open a database
        // a NEWER binary has migrated past this one (its `PRAGMA user_version` exceeds
        // our SCHEMA_VERSION) BEFORE touching any schema, so we never misread a column
        // we don't understand. A pre-framework DB reads version 0 and proceeds.
        migrations::assert_schema_not_future(&conn)?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS nodes (
                node_id                    TEXT PRIMARY KEY,
                status_json                TEXT NOT NULL,
                registered_at_ms           INTEGER NOT NULL DEFAULT 0,
                last_trust_update_ms       INTEGER NOT NULL DEFAULT 0,
                ak_public_pem              TEXT,
                expected_pcr16_digest_hex  TEXT,
                site                       TEXT,
                firmware_version           TEXT
            )",
            [],
        )?;
        // #397/#398 console rollups — additive, NULLABLE columns. Idempotent
        // ADD-COLUMN migration upgrading a pre-existing `nodes` table; tolerates
        // the "duplicate column name" error on re-run (mirrors the audit_log_chain
        // / clearance_grants convention). Never altered/dropped existing columns.
        for col_sql in [
            "ALTER TABLE nodes ADD COLUMN site TEXT",
            "ALTER TABLE nodes ADD COLUMN firmware_version TEXT",
        ] {
            if let Err(e) = conn.execute(col_sql, []) {
                if !e.to_string().contains("duplicate column name") {
                    return Err(e);
                }
            }
        }

        conn.execute(
            "CREATE TABLE IF NOT EXISTS dependencies (
                node_id  TEXT NOT NULL,
                dep_id   TEXT NOT NULL,
                PRIMARY KEY (node_id, dep_id)
            )",
            [],
        )?;

        // Per-node attestation POLICY (TPM-quote follow-up to #572). Kept as a
        // distinct table from the `nodes` identity/trust record: a security
        // policy is a separate concern, and an absent row is the fail-closed
        // default (no requirement — back-compat for nodes that never opted in).
        // `require_tpm_quote` = the node must present a hardware TPM quote on
        // `/attestation/verify`, not merely a self-reported PCR16 digest.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS node_attestation_policy (
                node_id           TEXT PRIMARY KEY,
                require_tpm_quote  INTEGER NOT NULL DEFAULT 0
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
        // History/flapping/analytics reads filter by node_id and order by
        // created_at_ms; without these the per-node and time-window queries are
        // full-table scans + filesorts.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_posture_events_node_time
                ON posture_events(node_id, created_at_ms);
             CREATE INDEX IF NOT EXISTS idx_posture_events_time
                ON posture_events(created_at_ms);",
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

        // WS-1 (#G7) — API principals: per-principal scoped bearer tokens. Only the
        // SHA-256 hex of the token is stored (UNIQUE, looked up by hash — never
        // plaintext). `role` is the wire role string; `revoked_at_ms` NULL = active.
        // Layers ON TOP of the `KIRRA_ADMIN_TOKEN` root (which stays the break-glass
        // superuser); this table only ADDS least-privilege sub-credentials.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS api_principals (
                principal_id   TEXT    PRIMARY KEY,
                token_sha256   TEXT    NOT NULL UNIQUE,
                role           TEXT    NOT NULL,
                created_at_ms  INTEGER NOT NULL,
                revoked_at_ms  INTEGER
            )",
            [],
        )?;

        // WS-1 (#G7) Track 1.2 — mTLS cert principals: a client X.509 certificate,
        // already CA-verified by rustls at the TLS layer, is pinned to a principal by
        // the SHA-256 hex of its leaf DER (UNIQUE, looked up by fingerprint). Same
        // role/revocation shape as `api_principals` — a cert is just another
        // least-privilege sub-credential on top of the `KIRRA_ADMIN_TOKEN` root.
        // WP-15 (MGA G-19) — `not_after_ms` records the pinned cert's X.509 notAfter
        // (the admin supplies it, computed offline alongside the fingerprint). NULL =
        // no expiry tracked (back-compat). A resolution past `not_after_ms` fail-closes
        // exactly like a revocation — a cert lifecycle, not just an on/off pin.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS cert_principals (
                principal_id   TEXT    PRIMARY KEY,
                cert_sha256    TEXT    NOT NULL UNIQUE,
                role           TEXT    NOT NULL,
                created_at_ms  INTEGER NOT NULL,
                revoked_at_ms  INTEGER,
                not_after_ms   INTEGER
            )",
            [],
        )?;
        // WP-15 ADD-COLUMN migration for a pre-existing cert_principals table (same
        // tolerate-duplicate convention as the ota_campaigns/nodes ALTERs; runs AFTER
        // the CREATE so a fresh database is never "no such table"). An older row keeps
        // a NULL `not_after_ms` (no expiry tracked) until it is re-registered.
        if let Err(e) = conn.execute(
            "ALTER TABLE cert_principals ADD COLUMN not_after_ms INTEGER",
            [],
        ) {
            if !e.to_string().contains("duplicate column name") {
                return Err(e);
            }
        }

        // WS-4 / Track 3 (Fleet Plane) — OTA governor-artifact campaigns. One row
        // per campaign; the control-plane state machine + fail-closed
        // halt-on-regression rule live in `crate::ota_campaign`, this table just
        // persists the campaign so a `Halted` verdict survives a restart (a halted
        // rollout must never silently resume). `cohorts_json` / `stages_json` are
        // JSON arrays; `state` is the lowercase `CampaignState` wire string.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS ota_campaigns (
                campaign_id       TEXT    PRIMARY KEY,
                artifact_digest   TEXT    NOT NULL,
                artifact_version  TEXT    NOT NULL,
                cohorts_json      TEXT    NOT NULL,
                stages_json       TEXT    NOT NULL,
                stage_index       INTEGER NOT NULL DEFAULT 0,
                rollout_percent   INTEGER NOT NULL DEFAULT 0,
                state             TEXT    NOT NULL,
                halt_reason       TEXT,
                created_at_ms     INTEGER NOT NULL,
                updated_at_ms     INTEGER NOT NULL,
                artifact_signature_b64 TEXT
            )",
            [],
        )?;
        // WP-12 ADD-COLUMN migration for a pre-existing ota_campaigns table
        // (same tolerate-duplicate convention as the nodes/clearance ALTERs;
        // runs AFTER the CREATE so a fresh database is never "no such table").
        if let Err(e) = conn.execute(
            "ALTER TABLE ota_campaigns ADD COLUMN artifact_signature_b64 TEXT",
            [],
        ) {
            if !e.to_string().contains("duplicate column name") {
                return Err(e);
            }
        }

        // WS-4 / Track 3 — node artifact adoption reports. Each node reports the
        // digest it is actually RUNNING (after an OTA commit); the fleet summary
        // joins this against the active campaigns to show real per-campaign
        // adoption. Keyed by node_id (latest report wins — a node runs one governor
        // at a time). Pure observability: never gates any actuator/posture decision,
        // so it is NOT audit-chained (unlike the lifecycle mutations).
        conn.execute(
            "CREATE TABLE IF NOT EXISTS node_artifact_status (
                node_id          TEXT    PRIMARY KEY,
                applied_digest   TEXT    NOT NULL,
                campaign_id      TEXT,
                artifact_version TEXT,
                reported_at_ms   INTEGER NOT NULL,
                attested         INTEGER NOT NULL DEFAULT 0
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

        // Per-source monotonic sequence high-water mark for industrial-message
        // replay protection (IEC 62443). One row per `source_id`; a message whose
        // sequence is <= the stored high-water mark is a replay/regress and is
        // rejected. Durable (survives restart) so a replay cannot ride a restart.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS industrial_message_seq (
                source_id     TEXT    PRIMARY KEY,
                last_sequence INTEGER NOT NULL,
                last_seen_ms  INTEGER NOT NULL
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
                ON fabric_causal_log(timestamp_ms);",
        )?;

        // WP-18 (G-20): the baseline DDL above (idempotent) IS schema version 1. Apply
        // any registered post-baseline migrations in order and stamp `user_version` up
        // to SCHEMA_VERSION — so a fresh or pre-framework (version-0) DB is recorded at
        // the baseline, and a future v2+ change upgrades in order. Runs on `conn`; the
        // stamp is a DB-header property shared by the durable connection to the same file.
        migrations::run_migrations(&conn)?;

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
            // P2: same checkpoint-transient absorption on the durable (epoch CAS /
            // nonce burn) connection — a SQLITE_BUSY here would fail an HA epoch or
            // federation write that is otherwise valid.
            dc.busy_timeout(std::time::Duration::from_millis(SQLITE_BUSY_TIMEOUT_MS))?;
            Some(dc)
        };

        Ok(Self {
            conn,
            durable_conn,
            signing_key: None,
            db_path: path.to_string(),
            #[cfg(test)]
            durable_posture_writes: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            fail_durable_posture_writes: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// The database path this store was opened from.
    pub fn path(&self) -> &str {
        &self.db_path
    }

    /// Open an independent READ-ONLY replica connection to an EXISTING WAL
    /// database (the writer owns all DDL, so this runs no schema setup). In WAL
    /// mode a read-only connection sees committed snapshots and neither blocks
    /// nor is blocked by the writer, so routing read-only routes here decouples
    /// them from the writer mutex (review P3). All read methods are `&self` and
    /// fresh-prepare their statements, so they work directly against the replica.
    ///
    /// Returns `Err` for `":memory:"` — a second `:memory:` open is a DISTINCT
    /// empty database (same caveat as `durable_conn`), so the caller falls back
    /// to the writer there.
    pub fn open_read_replica(path: &str) -> Result<Self> {
        if path == ":memory:" {
            return Err(rusqlite::Error::InvalidParameterName(
                ":memory: has no shareable read replica".to_string(),
            ));
        }
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_URI
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        // A read-only WAL reader can briefly contend with a checkpoint; a short
        // busy timeout rides it out rather than surfacing SQLITE_BUSY.
        conn.busy_timeout(std::time::Duration::from_millis(SQLITE_BUSY_TIMEOUT_MS))?;
        Ok(Self {
            conn,
            durable_conn: None,
            signing_key: None,
            db_path: path.to_string(),
            #[cfg(test)]
            durable_posture_writes: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            fail_durable_posture_writes: std::sync::atomic::AtomicBool::new(false),
        })
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

    /// Begin an `Immediate` transaction on the NORMAL connection for an
    /// audit-chain append (#685).
    ///
    /// `audit_log_chain` / `audit_anchor_head` are written from BOTH this
    /// NORMAL `conn` (per-command audit: posture events, operator grants,
    /// attestation, migrations) AND the FULL `durable_conn` (`record_key_rotation`,
    /// the federation commit — both already `Immediate`). `append_audit_event_tx`
    /// reads the chain tail and then INSERTs; under a DEFERRED transaction the
    /// read takes only a snapshot, so a write committed on the OTHER connection
    /// between the tail read and the INSERT could leave the new row linked off a
    /// stale `previous_hash`/`sequence` — a forked chain caught only later by
    /// verify. `Immediate` takes SQLite's single-writer WAL lock at `BEGIN`, so
    /// the tail read happens under the write lock and no other connection can
    /// interleave a commit: chain integrity rests on the WAL write lock, not on
    /// the process-wide store mutex. This does NOT change `synchronous` (the
    /// NORMAL/FULL #74 durability split is preserved) — only the lock-acquisition
    /// point. Takes `&mut self.conn` (a field borrow) so callers can still read
    /// `self.signing_key` for the append.
    fn audit_tx(conn: &mut Connection) -> Result<rusqlite::Transaction<'_>> {
        conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
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
    /// tail is hard-power-loss-durable — EXCEPT incident-class rows: since
    /// WS-0.3 (#772 F2), a posture TRANSITION and every post-incident sequence
    /// event are written DIRECTLY on the FULL connection
    /// (`save_posture_event_chained*_durable`), so the commit itself fsyncs the
    /// WAL and the record of the incident preceding a power cut is durable at
    /// write time, atomically. The periodic refresh tail keeps the
    /// checkpoint-bounded window. See docs/safety/CODING_GUIDELINES.md INV-12.
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

    // --- v0.9.7 posture event log -------------------------------------------

    // --- v0.9.8 HA probes & backup export ---

    pub fn health_check(&self) -> Result<()> {
        self.conn.query_row("SELECT 1", [], |_| Ok(()))
    }

    /// WP-18 (G-20) — the schema version stamped in this database's `PRAGMA
    /// user_version`. After a successful `new()` this equals
    /// [`migrations::SCHEMA_VERSION`]; a pre-framework database reads it as the
    /// value the migration on open stamped (the baseline). Surfaces the versioned
    /// migration state for observability + the schema-migration drill.
    pub fn schema_version(&self) -> Result<i64> {
        migrations::read_user_version(&self.conn)
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

    // --- v1.1 tamper-evident audit chain ------------------------------------

    // --- #314 Phase 1 — operator registry -----------------------------------

    // --- v1.1 trusted federation controller registry ------------------------

    // --- Patch 1: attestation identity registry ----------------------------

    // --- AV subsystem metadata ---------------------------------------------

    // --- Posture engine persistent state -----------------------------------

    // --- HA epoch fence (durable split-brain guard) -------------------------
    //
    // SQLite serializes write transactions, so a conditional UPDATE on the
    // singleton `ha_state` row gives a real distributed compare-and-set:
    // two racers that both read the same `observed` epoch will serialize at
    // commit time and only one of them will see `rows_affected == 1`.
    // The atomic on AppState is per-process and CANNOT do this — that is
    // why we keep the durable epoch as source of truth.

    // --- Fabric asset persistence -------------------------------------------

    // --- #87: forensic causal-log forensic chain ---------------------------
}

#[cfg(test)]
mod tests;
