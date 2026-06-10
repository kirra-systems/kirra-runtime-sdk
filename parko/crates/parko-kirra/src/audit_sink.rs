//! CERT-006 — durable, signed production sink for `ComparatorDivergence` events.
//!
//! [`crate::comparator::InMemoryDivergenceSink`] is ephemeral + unsigned (dev /
//! test only). A production deployment MUST wire a sink that persists every
//! divergence to a tamper-evident record — this module provides it.
//!
//! [`AuditChainLinkerDivergenceSink`] holds the SDK's [`VerifierStore`] (the
//! handle that owns the hash-chained `audit_log_chain` ledger + the Ed25519
//! signing key) and records each divergence via
//! [`VerifierStore::save_posture_event_chained`], which appends through
//! `AuditChainLinker::append_audit_event_tx` — the same signed, hash-linked
//! ledger the verifier service writes — with event type `"ComparatorDivergence"`
//! and the JSON-serialised [`DivergenceEvent`] as the body.
//!
//! NOTE: `save_posture_event_chained` also writes a `posture_events` row in the
//! same transaction; that row is incidental — the authoritative, contract-
//! specified artifact is the signed `audit_log_chain` entry.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use ed25519_dalek::SigningKey;
use kirra_runtime_sdk::verifier_store::VerifierStore;

use crate::comparator::{DivergenceEvent, DivergenceEventSink, InMemoryDivergenceSink};

/// The audit-log event type for a comparator divergence (the doc-spec name).
pub const COMPARATOR_DIVERGENCE_EVENT_TYPE: &str = "ComparatorDivergence";

/// A fail-closed misconfiguration of the durable divergence sink (CERT-006).
///
/// The reference node treats every variant as FATAL: a deployment that asked
/// for a durable audit (`PARKO_DIVERGENCE_AUDIT_DB` set) but cannot produce a
/// *signed, persisted* record must NOT silently fall back to the ephemeral
/// in-memory sink — that would leave comparator divergences unaudited while the
/// operator believes they are captured.
#[derive(Debug)]
pub enum FatalAuditConfig {
    /// A durable DB was requested but no signing key was supplied. The audit
    /// chain would be persisted but UNSIGNED — not tamper-evident — so reject.
    MissingSigningKey,
    /// The supplied signing key could not be decoded into an Ed25519 key
    /// (bad base64, or not 32 bytes).
    InvalidSigningKey(String),
    /// The SQLite audit store at the requested path could not be opened.
    StoreOpenFailed(String),
}

impl std::fmt::Display for FatalAuditConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FatalAuditConfig::MissingSigningKey => write!(
                f,
                "PARKO_DIVERGENCE_AUDIT_DB is set but KIRRA_LOG_SIGNING_KEY is unset — \
                 a durable divergence audit must be signed (tamper-evident); refusing to \
                 persist an unsigned chain"
            ),
            FatalAuditConfig::InvalidSigningKey(why) => write!(
                f,
                "KIRRA_LOG_SIGNING_KEY is not a valid base64 Ed25519 signing key: {why}"
            ),
            FatalAuditConfig::StoreOpenFailed(why) => write!(
                f,
                "could not open the divergence audit store (PARKO_DIVERGENCE_AUDIT_DB): {why}"
            ),
        }
    }
}

impl std::error::Error for FatalAuditConfig {}

/// Decode a base64-encoded 32-byte Ed25519 signing key (the same encoding the
/// verifier service accepts for `KIRRA_LOG_SIGNING_KEY`).
fn parse_signing_key(key_b64: &str) -> Result<SigningKey, FatalAuditConfig> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(key_b64.trim())
        .map_err(|e| FatalAuditConfig::InvalidSigningKey(e.to_string()))?;
    let bytes: [u8; 32] = raw.as_slice().try_into().map_err(|_| {
        FatalAuditConfig::InvalidSigningKey(format!(
            "expected 32 key bytes, got {}",
            raw.len()
        ))
    })?;
    Ok(SigningKey::from_bytes(&bytes))
}

/// Durable, signed [`DivergenceEventSink`] (CERT-006).
///
/// Persists every divergence to the SDK's hash-chained, Ed25519-signed audit
/// ledger. `record` is infallible by the trait contract — but a divergence that
/// is *detected yet not durably recorded* is itself safety-relevant, so a
/// persistence failure is never silently swallowed: it increments the
/// operator-observable [`write_failures`](Self::write_failures) counter and logs
/// loudly to stderr (matching the in-crate sink convention).
pub struct AuditChainLinkerDivergenceSink {
    store: Arc<Mutex<VerifierStore>>,
    write_failures: AtomicU64,
}

impl AuditChainLinkerDivergenceSink {
    /// Build a sink over an SDK store. The store MUST own the audit chain (it
    /// does — `VerifierStore::new` creates `audit_log_chain`) and a signing key
    /// (set via `VerifierStore::set_signing_key` / `admit_signing_key`) for the
    /// entries to be signed.
    pub fn new(store: Arc<Mutex<VerifierStore>>) -> Self {
        Self {
            store,
            write_failures: AtomicU64::new(0),
        }
    }

    /// Open a durable, *signed* divergence sink from a DB path and a base64
    /// Ed25519 signing key. Fail-closed: a store that cannot be opened, or a key
    /// that cannot be decoded, is a [`FatalAuditConfig`] — never a silent
    /// fallback to an unsigned or ephemeral sink.
    pub fn open(db_path: &str, key_b64: &str) -> Result<Self, FatalAuditConfig> {
        let key = parse_signing_key(key_b64)?;
        let mut store = VerifierStore::new(db_path)
            .map_err(|e| FatalAuditConfig::StoreOpenFailed(e.to_string()))?;
        store.set_signing_key(key);
        Ok(Self::new(Arc::new(Mutex::new(store))))
    }

    /// Number of divergences that were DETECTED but could NOT be durably +
    /// signed. MUST be `0` in a healthy deployment; a non-zero value means the
    /// tamper-evident record is MISSING for that many divergences — observe it.
    pub fn write_failures(&self) -> u64 {
        self.write_failures.load(Ordering::SeqCst)
    }

    fn note_failure(&self) {
        self.write_failures.fetch_add(1, Ordering::SeqCst);
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

impl DivergenceEventSink for AuditChainLinkerDivergenceSink {
    fn record(&self, event: DivergenceEvent) {
        let body = match serde_json::to_string(&event) {
            Ok(s) => s,
            Err(e) => {
                self.note_failure();
                eprintln!(
                    "[CERT-006] ComparatorDivergence NOT recorded — JSON serialization failed: \
                     {e} (divergence is UNAUDITED)"
                );
                return;
            }
        };

        let outcome = match self.store.lock() {
            Ok(mut store) => store.save_posture_event_chained(
                "governor_comparator",
                COMPARATOR_DIVERGENCE_EVENT_TYPE,
                &body,
                None,
                Self::now_ms(),
            ),
            Err(_) => {
                self.note_failure();
                eprintln!(
                    "[CERT-006] ComparatorDivergence NOT recorded — audit store mutex poisoned \
                     (divergence is UNAUDITED)"
                );
                return;
            }
        };

        if let Err(e) = outcome {
            self.note_failure();
            eprintln!(
                "[CERT-006] AUDIT-CHAIN WRITE FAILED for ComparatorDivergence: {e} — \
                 divergence detected but NOT in the tamper-evident log"
            );
        }
    }
}

/// Select the divergence sink for a deployment from its two environment
/// inputs, applying the CERT-006 fail-closed contract:
///
/// | `db` (`PARKO_DIVERGENCE_AUDIT_DB`) | `key` (`KIRRA_LOG_SIGNING_KEY`) | result |
/// |---|---|---|
/// | unset | unset | `Ok` ephemeral in-memory sink — caller MUST warn (non-cert) |
/// | unset | set   | `Ok` ephemeral in-memory sink — caller MUST warn (non-cert) |
/// | set   | set, valid, store opens | `Ok` durable + signed sink |
/// | set   | unset | `Err(MissingSigningKey)` — would be unsigned |
/// | set   | invalid key OR store unopenable | `Err(...)` — no silent fallback |
///
/// The key insight: a durable audit was *requested* (db set) but cannot be made
/// tamper-evident → FATAL. The caller (the reference node) exits non-zero.
pub fn select_divergence_sink(
    db: Option<String>,
    key: Option<String>,
) -> Result<Arc<dyn DivergenceEventSink>, FatalAuditConfig> {
    match db.as_deref() {
        // No durable DB requested → ephemeral sink (the caller warns it is
        // non-cert / divergences are not persisted). A stray signing key with
        // no DB is harmless: nothing to sign.
        None | Some("") => Ok(Arc::new(InMemoryDivergenceSink::new())),
        Some(db_path) => match key.as_deref() {
            None | Some("") => Err(FatalAuditConfig::MissingSigningKey),
            Some(key_b64) => Ok(Arc::new(AuditChainLinkerDivergenceSink::open(
                db_path, key_b64,
            )?)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn sample_event() -> DivergenceEvent {
        DivergenceEvent {
            primary_lin: 3.0,
            shadow_lin: 0.0,
            delta_lin: 3.0,
            primary_ang: 0.1,
            shadow_ang: 0.0,
            delta_ang: 0.1,
            accumulator: 7,
            current_speed_mps: Some(2.5),
            reconciled_lin: 0.0,
            reconciled_ang: 0.0,
            escalated_to_lockout: true,
        }
    }

    /// TASK 2 — a real signing key + file-backed audit chain: the recorded
    /// divergence is DURABLE, hash-linked, and its signature VERIFIES (distinct
    /// from the in-memory emission test, which only proves buffering).
    #[test]
    fn divergence_is_durably_recorded_signed_and_hash_linked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("divergence_audit.sqlite");
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let vk = key.verifying_key();

        let mut store = VerifierStore::new(db.to_str().unwrap()).expect("store");
        store.set_signing_key(key);
        let store = Arc::new(Mutex::new(store));

        let sink = AuditChainLinkerDivergenceSink::new(Arc::clone(&store));
        sink.record(sample_event());
        assert_eq!(sink.write_failures(), 0, "the divergence must have been durably recorded");

        let guard = store.lock().unwrap();

        // Durable + hash-linked + SIGNED (verifies under the real key).
        let v = guard.verify_audit_chain_full(Some(&vk)).expect("verify");
        assert!(v.chain_intact, "audit chain must be hash-intact");
        assert!(v.signature_valid, "the signature must verify under the signing key");
        assert!(v.signed_entries >= 1, "the divergence entry must be signed, got {}", v.signed_entries);

        // The entry is a `ComparatorDivergence` carrying the event body.
        let events = guard.load_all_posture_events().expect("load events");
        let div = events
            .iter()
            .find(|e| e["event_type"] == COMPARATOR_DIVERGENCE_EVENT_TYPE)
            .expect("a ComparatorDivergence audit entry must exist");
        assert_eq!(div["posture"]["escalated_to_lockout"], true);
        assert_eq!(div["posture"]["accumulator"], 7);
    }

    /// A persistence failure (poisoned store) is surfaced via `write_failures`,
    /// never silently swallowed — a detected-but-unaudited divergence is itself
    /// safety-relevant.
    #[test]
    fn persistence_failure_is_surfaced_not_swallowed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("divergence_audit.sqlite");
        let store = Arc::new(Mutex::new(VerifierStore::new(db.to_str().unwrap()).expect("store")));

        // Poison the store mutex so the audit write cannot land.
        let s = Arc::clone(&store);
        let _ = std::thread::spawn(move || {
            let _g = s.lock().unwrap();
            panic!("poison the audit store for the failure test");
        })
        .join();

        let sink = AuditChainLinkerDivergenceSink::new(Arc::clone(&store));
        sink.record(sample_event());
        assert_eq!(
            sink.write_failures(),
            1,
            "a divergence that could not be durably recorded MUST be counted, not swallowed"
        );
    }

    /// Base64-encode a 32-byte key the way `KIRRA_LOG_SIGNING_KEY` is supplied.
    fn key_b64(seed: u8) -> String {
        base64::engine::general_purpose::STANDARD.encode([seed; 32])
    }

    // --- TASK 3a: `select_divergence_sink` fail-closed contract -------------

    /// db unset + key unset → ephemeral in-memory sink (caller warns).
    #[test]
    fn select_neither_set_yields_in_memory_sink() {
        let sink = select_divergence_sink(None, None).expect("in-memory is Ok");
        // It records without panicking and is NOT durable (nothing to assert on
        // disk) — exercising it proves the trait object is usable.
        sink.record(sample_event());
    }

    /// db unset + key set → still ephemeral (a key with no DB is harmless).
    #[test]
    fn select_key_without_db_yields_in_memory_sink() {
        let sink =
            select_divergence_sink(None, Some(key_b64(9))).expect("in-memory is Ok");
        sink.record(sample_event());
    }

    /// An empty DB string is treated as unset (env vars are often set to "").
    #[test]
    fn select_empty_db_string_is_treated_as_unset() {
        let sink = select_divergence_sink(Some(String::new()), Some(key_b64(9)))
            .expect("empty db == unset → in-memory Ok");
        sink.record(sample_event());
    }

    /// db set + key UNSET → fatal: a durable audit was requested but would be
    /// unsigned. No silent fallback.
    #[test]
    fn select_db_without_key_is_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("audit.sqlite");
        let res = select_divergence_sink(Some(db.to_str().unwrap().to_string()), None);
        assert!(
            matches!(res, Err(FatalAuditConfig::MissingSigningKey)),
            "durable audit with no signing key must be fatal"
        );
    }

    /// db set + key set but UNDECODABLE → fatal, not a fallback.
    #[test]
    fn select_db_with_invalid_key_is_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("audit.sqlite");
        let res = select_divergence_sink(
            Some(db.to_str().unwrap().to_string()),
            Some("not-valid-base64-!!!".to_string()),
        );
        assert!(
            matches!(res, Err(FatalAuditConfig::InvalidSigningKey(_))),
            "an undecodable signing key must be fatal"
        );
    }

    /// A well-formed base64 string of the wrong length is also fatal.
    #[test]
    fn select_db_with_wrong_length_key_is_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("audit.sqlite");
        let short = base64::engine::general_purpose::STANDARD.encode([1u8; 16]);
        let res = select_divergence_sink(Some(db.to_str().unwrap().to_string()), Some(short));
        assert!(
            matches!(res, Err(FatalAuditConfig::InvalidSigningKey(_))),
            "a 16-byte key must be fatal"
        );
    }

    /// db set + valid key + openable store → durable, SIGNED sink, end-to-end:
    /// a recorded divergence verifies under the supplied key's public half.
    #[test]
    fn select_db_and_valid_key_yields_durable_signed_sink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("audit.sqlite");
        let db_path = db.to_str().unwrap().to_string();

        let key = SigningKey::from_bytes(&[5u8; 32]);
        let vk = key.verifying_key();
        let b64 = base64::engine::general_purpose::STANDARD.encode(key.to_bytes());

        let sink = select_divergence_sink(Some(db_path.clone()), Some(b64))
            .expect("durable sink with a valid key is Ok");
        sink.record(sample_event());

        // Re-open the same DB and prove the entry is durable + signed.
        let verifier = VerifierStore::new(&db_path).expect("re-open store");
        let v = verifier
            .verify_audit_chain_full(Some(&vk))
            .expect("verify chain");
        assert!(v.chain_intact, "chain must be hash-intact");
        assert!(v.signature_valid, "signature must verify under the key");
        assert!(v.signed_entries >= 1, "the divergence entry must be signed");

        let events = verifier.load_all_posture_events().expect("load events");
        assert!(
            events
                .iter()
                .any(|e| e["event_type"] == COMPARATOR_DIVERGENCE_EVENT_TYPE),
            "a ComparatorDivergence audit entry must be persisted"
        );
    }
}
