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
use kirra_verifier::store_handle::StoreHandle;
use kirra_verifier::verifier_store::VerifierStore;
use parko_core::{
    AuditClient, ClearanceLoop, ClearanceRejection, ClearanceState, DecisionRecord, FaultRecord,
    HealthRecord, ImpactCfg, ImpactEvidence, ImpactLatch, NoopAuditClient, OperatorClearanceGrant,
    OverrideRecord,
};

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
            // Subsystem-neutral: this error type is shared by every audit-sink
            // selector (divergence, impact, decision-path AuditClient), so the
            // message names only the SHARED signing-key var, never one selector's
            // DB var (the caller knows which DB env var it read).
            FatalAuditConfig::MissingSigningKey => write!(
                f,
                "a durable audit DB is configured but KIRRA_LOG_SIGNING_KEY is unset — \
                 a durable audit must be signed (tamper-evident); refusing to persist an \
                 unsigned chain"
            ),
            FatalAuditConfig::InvalidSigningKey(why) => write!(
                f,
                "KIRRA_LOG_SIGNING_KEY is not a valid base64 Ed25519 signing key: {why}"
            ),
            FatalAuditConfig::StoreOpenFailed(why) => write!(
                f,
                "could not open the audit store at the configured path: {why}"
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
        FatalAuditConfig::InvalidSigningKey(format!("expected 32 key bytes, got {}", raw.len()))
    })?;
    Ok(SigningKey::from_bytes(&bytes))
}

/// Shared, fail-closed writer over the SDK's hash-chained, Ed25519-signed audit
/// ledger. Both the CERT-006 comparator-divergence sink and the SG6 impact-audit
/// sink record through this ONE struct (REUSE — a single write path and a single
/// `write_failures` accounting): it owns the [`VerifierStore`] handle (which owns
/// `audit_log_chain` + the signing key) and a detected-but-unrecorded counter,
/// and appends every event via [`VerifierStore::save_posture_event_chained`]
/// (which goes through `AuditChainLinker::append_audit_event_tx`).
struct ChainedAuditWriter {
    store: StoreHandle,
    write_failures: AtomicU64,
}

impl ChainedAuditWriter {
    fn new(store: StoreHandle) -> Self {
        Self {
            store,
            write_failures: AtomicU64::new(0),
        }
    }

    /// Open a store from a path + base64 Ed25519 key. Fail-closed: an unopenable
    /// store or an undecodable key is a [`FatalAuditConfig`] — never a silent
    /// fallback to an unsigned or ephemeral sink.
    fn open(db_path: &str, key_b64: &str) -> Result<Self, FatalAuditConfig> {
        let key = parse_signing_key(key_b64)?;
        let mut store = VerifierStore::new(db_path)
            .map_err(|e| FatalAuditConfig::StoreOpenFailed(e.to_string()))?;
        store.set_signing_key(key);
        Ok(Self::new(StoreHandle::new(store)))
    }

    fn write_failures(&self) -> u64 {
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

    /// Append one already-serialised event body under `(source, event_type)`.
    /// Infallible toward the caller: a lock-poison or write error increments
    /// `write_failures` and logs loudly — never propagated, never panics.
    fn record(&self, source: &str, event_type: &str, body: &str) {
        let outcome = self.store.with(|store| {
            store.save_posture_event_chained(source, event_type, body, None, Self::now_ms())
        });
        if let Err(e) = outcome {
            self.note_failure();
            eprintln!(
                "[audit] AUDIT-CHAIN WRITE FAILED for {event_type}: {e} — \
                 event detected but NOT in the tamper-evident log"
            );
        }
    }
}

/// Durable, signed [`DivergenceEventSink`] (CERT-006).
///
/// Persists every divergence to the SDK's hash-chained, Ed25519-signed audit
/// ledger via the shared [`ChainedAuditWriter`]. `record` is infallible by the
/// trait contract — but a divergence that is *detected yet not durably recorded*
/// is itself safety-relevant, so a persistence failure is never silently
/// swallowed: it increments the operator-observable
/// [`write_failures`](Self::write_failures) counter and logs loudly to stderr.
pub struct AuditChainLinkerDivergenceSink {
    writer: ChainedAuditWriter,
}

impl AuditChainLinkerDivergenceSink {
    /// Build a sink over an SDK store. The store MUST own the audit chain (it
    /// does — `VerifierStore::new` creates `audit_log_chain`) and a signing key
    /// (set via `VerifierStore::set_signing_key` / `admit_signing_key`) for the
    /// entries to be signed.
    pub fn new(store: StoreHandle) -> Self {
        Self {
            writer: ChainedAuditWriter::new(store),
        }
    }

    /// Open a durable, *signed* divergence sink from a DB path and a base64
    /// Ed25519 signing key. Fail-closed: a store that cannot be opened, or a key
    /// that cannot be decoded, is a [`FatalAuditConfig`] — never a silent
    /// fallback to an unsigned or ephemeral sink.
    pub fn open(db_path: &str, key_b64: &str) -> Result<Self, FatalAuditConfig> {
        Ok(Self {
            writer: ChainedAuditWriter::open(db_path, key_b64)?,
        })
    }

    /// Number of divergences that were DETECTED but could NOT be durably +
    /// signed. MUST be `0` in a healthy deployment; a non-zero value means the
    /// tamper-evident record is MISSING for that many divergences — observe it.
    pub fn write_failures(&self) -> u64 {
        self.writer.write_failures()
    }
}

impl DivergenceEventSink for AuditChainLinkerDivergenceSink {
    fn record(&self, event: DivergenceEvent) {
        let body = match serde_json::to_string(&event) {
            Ok(s) => s,
            Err(e) => {
                self.writer.note_failure();
                eprintln!(
                    "[CERT-006] ComparatorDivergence NOT recorded — JSON serialization failed: \
                     {e} (divergence is UNAUDITED)"
                );
                return;
            }
        };
        self.writer.record(
            "governor_comparator",
            COMPARATOR_DIVERGENCE_EVENT_TYPE,
            &body,
        );
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

// ───────────────────── L5 AuditClient bridge (decision-path audit) ──────────
//
// The SDK-backed implementation of `parko_core::AuditClient`. It routes the
// decision-path records (decision / override / fault / health) into the SAME
// signed, hash-chained ledger via the shared `ChainedAuditWriter`, so the ML
// decision path can record audit events through the SDK-free trait without
// depending on `kirra-verifier` itself — only this adapter does.

/// Audit-log event type for a normal governed decision. PascalCase, matching the
/// `"ComparatorDivergence"` / `"ImpactDetected"` convention.
pub const PARKO_DECISION_EVENT_TYPE: &str = "ParkoDecision";
/// Audit-log event type for a governor override of the doer output.
pub const PARKO_OVERRIDE_EVENT_TYPE: &str = "ParkoOverride";
/// Audit-log event type for a decision-path fault.
pub const PARKO_FAULT_EVENT_TYPE: &str = "ParkoFault";
/// Audit-log event type for a periodic health/posture snapshot.
pub const PARKO_HEALTH_EVENT_TYPE: &str = "ParkoHealth";

/// Durable, signed implementation of [`parko_core::AuditClient`].
///
/// Persists every decision-path record to the SDK's hash-chained, Ed25519-signed
/// ledger via the shared [`ChainedAuditWriter`] — same fail-closed contract as
/// the divergence and impact sinks: a record detected-but-not-recorded increments
/// [`write_failures`](Self::write_failures) and logs loudly; it never panics and
/// never propagates an error onto the decision path.
pub struct AuditChainLinkerAuditClient {
    writer: ChainedAuditWriter,
    source: String,
}

impl AuditChainLinkerAuditClient {
    /// Build over an SDK store (must own the audit chain + a signing key). `source`
    /// is the audit `source` column (e.g. the node id).
    pub fn new(store: StoreHandle, source: impl Into<String>) -> Self {
        Self {
            writer: ChainedAuditWriter::new(store),
            source: source.into(),
        }
    }

    /// Open a durable, *signed* client from a DB path + base64 Ed25519 key.
    /// Fail-closed: an unopenable store or undecodable key is a [`FatalAuditConfig`].
    pub fn open(
        db_path: &str,
        key_b64: &str,
        source: impl Into<String>,
    ) -> Result<Self, FatalAuditConfig> {
        Ok(Self {
            writer: ChainedAuditWriter::open(db_path, key_b64)?,
            source: source.into(),
        })
    }

    /// Records detected but NOT durably + signed. MUST be `0` in a healthy
    /// deployment; non-zero means the tamper-evident record is MISSING.
    pub fn write_failures(&self) -> u64 {
        self.writer.write_failures()
    }

    fn record_event<P: serde::Serialize>(&self, event_type: &str, payload: &P) {
        match serde_json::to_string(payload) {
            Ok(body) => self.writer.record(&self.source, event_type, &body),
            Err(e) => {
                self.writer.note_failure();
                eprintln!(
                    "[L5] {event_type} NOT recorded — JSON serialization failed: {e} \
                     (decision-path record is UNAUDITED)"
                );
            }
        }
    }
}

impl AuditClient for AuditChainLinkerAuditClient {
    fn record_decision(&self, record: DecisionRecord) {
        self.record_event(PARKO_DECISION_EVENT_TYPE, &record);
    }
    fn record_override(&self, record: OverrideRecord) {
        self.record_event(PARKO_OVERRIDE_EVENT_TYPE, &record);
    }
    fn record_fault(&self, record: FaultRecord) {
        self.record_event(PARKO_FAULT_EVENT_TYPE, &record);
    }
    fn record_health(&self, record: HealthRecord) {
        self.record_event(PARKO_HEALTH_EVENT_TYPE, &record);
    }
}

/// Select the decision-path [`AuditClient`] from the deployment's two environment
/// inputs, applying the SAME fail-closed contract as [`select_divergence_sink`]:
///
/// | `db` | `key` | result |
/// |---|---|---|
/// | unset | any   | `Ok` [`NoopAuditClient`] — caller MUST warn (decision path UNAUDITED) |
/// | set   | set, valid, store opens | `Ok` durable + signed [`AuditChainLinkerAuditClient`] |
/// | set   | unset | `Err(MissingSigningKey)` — would be unsigned |
/// | set   | invalid key OR store unopenable | `Err(...)` — no silent fallback |
///
/// A durable audit *requested* (db set) that cannot be made tamper-evident is
/// FATAL — the caller exits non-zero rather than run with an unsigned ledger.
pub fn select_audit_client(
    db: Option<String>,
    key: Option<String>,
    source: impl Into<String>,
) -> Result<Arc<dyn AuditClient>, FatalAuditConfig> {
    match db.as_deref() {
        None | Some("") => Ok(Arc::new(NoopAuditClient)),
        Some(db_path) => match key.as_deref() {
            None | Some("") => Err(FatalAuditConfig::MissingSigningKey),
            Some(key_b64) => Ok(Arc::new(AuditChainLinkerAuditClient::open(
                db_path, key_b64, source,
            )?)),
        },
    }
}

// ───────────────────── SG6 impact-audit bridge (#102 → #104) ────────────────
//
// Record `ImpactLatch` transitions as signed, hash-chained audit events through
// the SAME #247 sink crossing (parko-kirra → VerifierStore → the
// `append_audit_event_tx` ledger). The impact rows land in the SAME ledger the
// #104 post-incident sequence writes to (forensic adjacency) — no cross-subsystem
// plumbing: the latch already drives deny/posture, and the incident opens via the
// existing posture path. parko-kirra ONLY; node wiring is a deferred deploy step.

/// The audit-log event type for a post-collision impact LATCH (false→true).
/// PascalCase, matching the #247 `"ComparatorDivergence"` convention in the same
/// table.
pub const IMPACT_DETECTED_EVENT_TYPE: &str = "ImpactDetected";
/// The audit-log event type for an impact-latch CLEARANCE (true→false).
pub const IMPACT_CLEARED_EVENT_TYPE: &str = "ImpactCleared";
/// The audit-log event type for the once-per-incident operator-escalation edge
/// (#103). PascalCase, same table/convention.
pub const IMPACT_ESCALATION_RAISED_EVENT_TYPE: &str = "ImpactEscalationRaised";
/// The audit-log event type for a REJECTED clearance attempt (#103) — a
/// malformed grant, or a clear attempt with nothing to clear.
pub const IMPACT_CLEARANCE_REJECTED_EVENT_TYPE: &str = "ImpactClearanceRejected";

/// Audit source tag for SG6 impact events (the `governor_comparator` analogue).
const IMPACT_AUDIT_SOURCE: &str = "governor_impact_latch";

/// The trigger breakdown recorded with an `ImpactDetected` event: WHICH fusion
/// signals fired — NEVER raw sensor streams. The IMU magnitude is included ONLY
/// when finite (a non-finite reading never latches on its own and is not a
/// trustworthy datum to retain).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImpactDetectedPayload {
    /// The physical contact sensor fired (a definitive impact).
    pub contact_sensor: bool,
    /// A FINITE IMU spike above the threshold fired (the `is_impact` IMU term).
    pub spike_over_threshold: bool,
    /// A close-range tracked agent vanished (person-under-vehicle).
    pub vanished_object: bool,
    /// The IMU spike magnitude (m/s²) — present ONLY if finite; omitted entirely
    /// on a non-finite reading.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spike_magnitude_mps2: Option<f64>,
}

impl ImpactDetectedPayload {
    /// Derive the trigger breakdown from the latching evidence + fusion config.
    /// Mirrors the `is_impact` IMU term exactly (finite AND above threshold).
    fn from_evidence(evidence: &ImpactEvidence, cfg: &ImpactCfg) -> Self {
        let finite = evidence.imu_accel_spike_mps2.is_finite();
        Self {
            contact_sensor: evidence.contact_sensor,
            spike_over_threshold: finite
                && evidence.imu_accel_spike_mps2 > cfg.spike_threshold_mps2,
            vanished_object: evidence.vanished_object,
            // Retain the magnitude ONLY when finite — a non-finite reading
            // serialises to NO field (see `skip_serializing_if`).
            spike_magnitude_mps2: finite.then_some(evidence.imu_accel_spike_mps2),
        }
    }
}

/// The note recorded with an `ImpactCleared` event — the clearance source. On the
/// #103 clearance loop this carries the clearing operator's id (an audit subject).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImpactClearedPayload {
    /// A short note on WHAT cleared the latch. For the #103 loop this is the
    /// clearing operator's id. The authenticated-clearance mechanism itself is a
    /// named-boundary deferral — parko records the asserted source, it does not
    /// authenticate it (auth lives in the verifier / #255 reset key).
    pub clearance_source: String,
}

/// The context recorded with an `ImpactEscalationRaised` event (#103) — the
/// once-per-incident operator-intervention-required signal.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImpactEscalationPayload {
    /// A short, fixed description of why the escalation was raised.
    pub detail: String,
}

/// The context recorded with an `ImpactClearanceRejected` event (#103). Carries
/// the rejection reason and the operator id (an audit SUBJECT — id is fine to
/// record; there is no operator token at this layer to leak).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImpactClearanceRejectedPayload {
    /// The clearing operator's id from the rejected grant (audit subject).
    pub operator_id: String,
    /// The stable rejection reason code (`malformed_grant` / `not_immobilized`).
    pub reason: String,
}

/// Infallible sink for SG6 impact-latch transitions. `record_*` NEVER returns an
/// error and NEVER blocks the latch — a failed audit write is the sink's problem,
/// not the motion veto's (see [`RecordedImpactLatch`] / [`RecordedClearanceLoop`]).
pub trait ImpactEventSink: Send + Sync {
    /// Record a false→true latch transition (exactly once per rising edge).
    fn record_detected(&self, payload: &ImpactDetectedPayload);
    /// Record a true→false clearance (exactly once per falling edge).
    fn record_cleared(&self, payload: &ImpactClearedPayload);
    /// Record the once-per-incident operator-escalation edge (#103).
    fn record_escalation_raised(&self, payload: &ImpactEscalationPayload);
    /// Record a rejected clearance attempt (#103).
    fn record_clearance_rejected(&self, payload: &ImpactClearanceRejectedPayload);
}

/// Durable, signed [`ImpactEventSink`] — the SG6 analogue of
/// [`AuditChainLinkerDivergenceSink`], sharing the same [`ChainedAuditWriter`]
/// write path so impact rows land in the SAME signed ledger the #104
/// post-incident sequence writes to (forensic adjacency).
pub struct ImpactAuditSink {
    writer: ChainedAuditWriter,
}

impl ImpactAuditSink {
    /// Build a sink over an SDK store (must own the audit chain + a signing key).
    pub fn new(store: StoreHandle) -> Self {
        Self {
            writer: ChainedAuditWriter::new(store),
        }
    }

    /// Open a durable, *signed* impact sink from a DB path + base64 Ed25519 key.
    /// Same fail-closed contract as [`AuditChainLinkerDivergenceSink::open`].
    pub fn open(db_path: &str, key_b64: &str) -> Result<Self, FatalAuditConfig> {
        Ok(Self {
            writer: ChainedAuditWriter::open(db_path, key_b64)?,
        })
    }

    /// Impact transitions that were DETECTED but could NOT be durably + signed.
    /// MUST be `0` in a healthy deployment (mirrors the divergence counter).
    pub fn write_failures(&self) -> u64 {
        self.writer.write_failures()
    }

    fn record_event<P: serde::Serialize>(&self, event_type: &str, payload: &P) {
        match serde_json::to_string(payload) {
            Ok(body) => self.writer.record(IMPACT_AUDIT_SOURCE, event_type, &body),
            Err(e) => {
                self.writer.note_failure();
                eprintln!(
                    "[SG6] {event_type} NOT recorded — JSON serialization failed: {e} \
                     (impact transition is UNAUDITED)"
                );
            }
        }
    }
}

impl ImpactEventSink for ImpactAuditSink {
    fn record_detected(&self, payload: &ImpactDetectedPayload) {
        self.record_event(IMPACT_DETECTED_EVENT_TYPE, payload);
    }
    fn record_cleared(&self, payload: &ImpactClearedPayload) {
        self.record_event(IMPACT_CLEARED_EVENT_TYPE, payload);
    }
    fn record_escalation_raised(&self, payload: &ImpactEscalationPayload) {
        self.record_event(IMPACT_ESCALATION_RAISED_EVENT_TYPE, payload);
    }
    fn record_clearance_rejected(&self, payload: &ImpactClearanceRejectedPayload) {
        self.record_event(IMPACT_CLEARANCE_REJECTED_EVENT_TYPE, payload);
    }
}

/// Ephemeral, in-memory [`ImpactEventSink`] for dev / test / the no-durable-audit
/// fallback. Buffers `(event_type, json_body)` pairs; never persists, never signs.
#[derive(Default)]
pub struct InMemoryImpactSink {
    events: Mutex<Vec<(String, String)>>,
}

impl InMemoryImpactSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of the buffered `(event_type, json_body)` pairs.
    pub fn events(&self) -> Vec<(String, String)> {
        self.events.lock().map(|v| v.clone()).unwrap_or_default()
    }

    fn push<P: serde::Serialize>(&self, event_type: &str, payload: &P) {
        let body = serde_json::to_string(payload).unwrap_or_else(|_| "<unserializable>".into());
        if let Ok(mut v) = self.events.lock() {
            v.push((event_type.to_string(), body));
        }
    }
}

impl ImpactEventSink for InMemoryImpactSink {
    fn record_detected(&self, payload: &ImpactDetectedPayload) {
        self.push(IMPACT_DETECTED_EVENT_TYPE, payload);
    }
    fn record_cleared(&self, payload: &ImpactClearedPayload) {
        self.push(IMPACT_CLEARED_EVENT_TYPE, payload);
    }
    fn record_escalation_raised(&self, payload: &ImpactEscalationPayload) {
        self.push(IMPACT_ESCALATION_RAISED_EVENT_TYPE, payload);
    }
    fn record_clearance_rejected(&self, payload: &ImpactClearanceRejectedPayload) {
        self.push(IMPACT_CLEARANCE_REJECTED_EVENT_TYPE, payload);
    }
}

/// Select the impact-audit sink from the same two env inputs as
/// [`select_divergence_sink`], applying the identical CERT-006 fail-closed
/// contract (durable+signed when both set; in-memory when no DB; FATAL when a DB
/// is requested but cannot be made tamper-evident).
pub fn select_impact_sink(
    db: Option<String>,
    key: Option<String>,
) -> Result<Arc<dyn ImpactEventSink>, FatalAuditConfig> {
    match db.as_deref() {
        None | Some("") => Ok(Arc::new(InMemoryImpactSink::new())),
        Some(db_path) => match key.as_deref() {
            None | Some("") => Err(FatalAuditConfig::MissingSigningKey),
            Some(key_b64) => Ok(Arc::new(ImpactAuditSink::open(db_path, key_b64)?)),
        },
    }
}

/// SG6 — an [`ImpactLatch`] wrapped with a RISING-EDGE audit recorder. Delegates
/// `observe` / `clear` to the inner latch and emits EXACTLY ONE audit event per
/// transition: `ImpactDetected` on false→true, `ImpactCleared` on true→false. No
/// per-tick spam while latched; a cleared latch that latches AGAIN emits a second
/// `ImpactDetected`.
///
/// INFALLIBLE toward the control path: the latch is mutated FIRST, then the
/// (best-effort) audit write happens — so the latch's safety behavior (the motion
/// veto) is BIT-IDENTICAL with or without a sink, and a failed write only
/// increments the sink's `write_failures` counter.
// SAFETY: SG6 | REQ: impact-audit-bridge | TEST: test_rising_edge_emits_one_detected,test_clear_emits_one_cleared_relatch_emits_second_detected,test_impact_durably_recorded_signed_and_chained,test_sink_recovers_after_transient_poison_latch_and_veto_unchanged,test_no_sink_latch_behavior_identical,test_detected_payload_has_trigger_booleans,test_nonfinite_spike_magnitude_omitted
pub struct RecordedImpactLatch {
    latch: ImpactLatch,
    sink: Arc<dyn ImpactEventSink>,
    last_latched: bool,
}

impl RecordedImpactLatch {
    /// Wrap a fresh latch with a recorder over `sink`.
    pub fn new(sink: Arc<dyn ImpactEventSink>) -> Self {
        Self {
            latch: ImpactLatch::new(),
            sink,
            last_latched: false,
        }
    }

    /// True while latched — the governor immobilizes. Identical to the inner
    /// [`ImpactLatch::is_latched`].
    pub fn is_latched(&self) -> bool {
        self.latch.is_latched()
    }

    /// Observe one tick. Delegates to [`ImpactLatch::observe`], then emits ONE
    /// `ImpactDetected` iff THIS tick caused a false→true transition (compared via
    /// the last-known state — no per-tick spam while latched).
    pub fn observe(&mut self, evidence: &ImpactEvidence, cfg: &ImpactCfg) {
        self.latch.observe(evidence, cfg);
        let now = self.latch.is_latched();
        if now && !self.last_latched {
            self.sink
                .record_detected(&ImpactDetectedPayload::from_evidence(evidence, cfg));
        }
        self.last_latched = now;
    }

    /// Clear on an explicit clearance signal. Delegates to [`ImpactLatch::clear`],
    /// then emits ONE `ImpactCleared` iff THIS caused a true→false transition.
    /// `source` is the clearance note recorded in the audit row.
    pub fn clear(&mut self, clearance: bool, source: &str) {
        self.latch.clear(clearance);
        let now = self.latch.is_latched();
        if !now && self.last_latched {
            self.sink.record_cleared(&ImpactClearedPayload {
                clearance_source: source.to_string(),
            });
        }
        self.last_latched = now;
    }
}

/// SG6 — a [`ClearanceLoop`] (#103) wrapped with the rising-edge audit recorder,
/// emitting through the same [`ImpactEventSink`] family #263 established.
///
/// Audit sequence per incident: `ImpactDetected` on the Normal→Latched edge
/// (incident open, with the trigger breakdown), then `ImpactEscalationRaised`
/// ONCE on the Latched→EscalationRaised edge (operator-intervention signal), then
/// either `ImpactCleared` (a well-formed grant) or `ImpactClearanceRejected` (a
/// rejected attempt, with the reason). No per-tick spam; re-impact while
/// escalated raises nothing new.
///
/// INFALLIBLE toward the control path: the state machine is mutated FIRST, then
/// the best-effort audit write — so [`is_immobilized`](Self::is_immobilized) (the
/// motion veto) is BIT-IDENTICAL with or without a sink, and a failed write only
/// increments the durable sink's `write_failures` counter.
// SAFETY: SG6 | REQ: clearance-confirmation-loop | TEST: test_loop_escalation_raised_once,test_loop_clear_emits_impact_cleared,test_loop_rejection_recorded_state_unchanged,test_loop_audit_path_state_unaffected_and_recovers_poison,test_loop_veto_unchanged_without_sink
pub struct RecordedClearanceLoop {
    clearance: ClearanceLoop,
    sink: Arc<dyn ImpactEventSink>,
    last_escalation_pending: bool,
}

impl RecordedClearanceLoop {
    /// Wrap a fresh clearance loop with a recorder over `sink`.
    pub fn new(sink: Arc<dyn ImpactEventSink>) -> Self {
        Self {
            clearance: ClearanceLoop::new(),
            sink,
            last_escalation_pending: false,
        }
    }

    /// The current lifecycle state.
    pub fn state(&self) -> ClearanceState {
        self.clearance.state()
    }

    /// True while immobilized (Latched OR EscalationRaised) — feeds the motion
    /// veto. Identical to [`ClearanceLoop::is_immobilized`].
    pub fn is_immobilized(&self) -> bool {
        self.clearance.is_immobilized()
    }

    /// True once the operator-escalation has been raised for the active incident.
    pub fn escalation_pending(&self) -> bool {
        self.clearance.escalation_pending()
    }

    /// Observe one tick. Delegates to [`ClearanceLoop::observe`] (state FIRST),
    /// then emits `ImpactDetected` on the incident-open edge and exactly ONE
    /// `ImpactEscalationRaised` on the false→true escalation edge.
    pub fn observe(&mut self, evidence: &ImpactEvidence, cfg: &ImpactCfg, now_ms: u64) {
        let before = self.clearance.state();
        self.clearance.observe(evidence, cfg, now_ms);
        let after = self.clearance.state();

        // Incident open (Normal → immobilized): record the trigger breakdown.
        // Post-#328 the latch raises escalation in one step, so the open edge is
        // `Normal → any immobilized state` (Latched OR EscalationRaised), not
        // specifically Latched.
        if before == ClearanceState::Normal && after != ClearanceState::Normal {
            self.sink
                .record_detected(&ImpactDetectedPayload::from_evidence(evidence, cfg));
        }
        // Operator-escalation rising edge (once per incident).
        let pending = self.clearance.escalation_pending();
        if pending && !self.last_escalation_pending {
            self.sink
                .record_escalation_raised(&ImpactEscalationPayload {
                    detail: "post-collision immobilization — operator clearance required"
                        .to_string(),
                });
        }
        self.last_escalation_pending = pending;
    }

    /// The ONLY clearance path. Delegates to [`ClearanceLoop::try_clear`] (state
    /// FIRST), then records the outcome: `ImpactCleared` on success (with the
    /// operator id as the clearance source — not duplicated elsewhere), or
    /// `ImpactClearanceRejected` (with the reason) on rejection. Returns the
    /// loop's `Result` unchanged.
    pub fn try_clear(
        &mut self,
        grant: &OperatorClearanceGrant,
        now_ms: u64,
        max_grant_age_ms: u64,
    ) -> Result<(), ClearanceRejection> {
        let outcome = self.clearance.try_clear(grant, now_ms, max_grant_age_ms);
        match &outcome {
            Ok(()) => {
                self.last_escalation_pending = false;
                self.sink.record_cleared(&ImpactClearedPayload {
                    clearance_source: grant.operator_id.clone(),
                });
            }
            Err(rej) => {
                self.sink
                    .record_clearance_rejected(&ImpactClearanceRejectedPayload {
                        operator_id: grant.operator_id.clone(),
                        reason: rej.reason_code().to_string(),
                    });
            }
        }
        outcome
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
            recommended_posture: "locked_out",
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
        let store = StoreHandle::new(store);

        let sink = AuditChainLinkerDivergenceSink::new(store.clone());
        sink.record(sample_event());
        assert_eq!(
            sink.write_failures(),
            0,
            "the divergence must have been durably recorded"
        );

        let (v, events) = store.with(|guard| {
            // Durable + hash-linked + SIGNED (verifies under the real key).
            let v = guard.verify_audit_chain_full(Some(&vk)).expect("verify");
            let events = guard.load_all_posture_events().expect("load events");
            (v, events)
        });
        assert!(v.chain_intact, "audit chain must be hash-intact");
        assert!(
            v.signature_valid,
            "the signature must verify under the signing key"
        );
        assert!(
            v.signed_entries >= 1,
            "the divergence entry must be signed, got {}",
            v.signed_entries
        );

        // The entry is a `ComparatorDivergence` carrying the event body.
        let div = events
            .iter()
            .find(|e| e["event_type"] == COMPARATOR_DIVERGENCE_EVENT_TYPE)
            .expect("a ComparatorDivergence audit entry must exist");
        assert_eq!(div["posture"]["escalated_to_lockout"], true);
        assert_eq!(div["posture"]["accumulator"], 7);
    }

    /// L5 — the SDK-backed `AuditClient` writes all four record kinds into the
    /// SAME hash-chained, Ed25519-signed ledger, and the chain verifies.
    #[test]
    fn audit_client_records_all_kinds_durably_signed_and_hash_linked() {
        use parko_core::safety::SafetyPosture;

        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("parko_audit.sqlite");
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let vk = key.verifying_key();

        let mut store = VerifierStore::new(db.to_str().unwrap()).expect("store");
        store.set_signing_key(key);
        let store = StoreHandle::new(store);

        let client = AuditChainLinkerAuditClient::new(store.clone(), "parko-test");
        client.record_decision(DecisionRecord {
            tick_ms: 1,
            proposed_linear_mps: 1.0,
            proposed_angular_rps: 0.0,
            commanded_linear_mps: 1.0,
            commanded_angular_rps: 0.0,
            posture: SafetyPosture::Nominal,
        });
        client.record_override(OverrideRecord {
            tick_ms: 2,
            reason: "envelope_clamp",
            proposed_linear_mps: 5.0,
            proposed_angular_rps: 0.0,
            commanded_linear_mps: 2.0,
            commanded_angular_rps: 0.0,
            posture: SafetyPosture::Degraded,
        });
        client.record_fault(FaultRecord {
            tick_ms: 3,
            code: "nonfinite_command",
            detail: "linear NaN".to_string(),
            posture: SafetyPosture::LockedOut,
        });
        client.record_health(HealthRecord {
            tick_ms: 4,
            posture: SafetyPosture::Nominal,
            inference_latency_ms: Some(7),
            divergence_accumulator: 0,
            ticks_processed: 4,
        });
        assert_eq!(
            client.write_failures(),
            0,
            "all four records must be durably recorded"
        );

        let (v, events) = store.with(|guard| {
            let v = guard.verify_audit_chain_full(Some(&vk)).expect("verify");
            let events = guard.load_all_posture_events().expect("load events");
            (v, events)
        });
        assert!(v.chain_intact, "audit chain must be hash-intact");
        assert!(
            v.signature_valid,
            "the signatures must verify under the signing key"
        );
        assert!(
            v.signed_entries >= 4,
            "all four entries must be signed, got {}",
            v.signed_entries
        );

        for et in [
            PARKO_DECISION_EVENT_TYPE,
            PARKO_OVERRIDE_EVENT_TYPE,
            PARKO_FAULT_EVENT_TYPE,
            PARKO_HEALTH_EVENT_TYPE,
        ] {
            assert!(
                events.iter().any(|e| e["event_type"] == et),
                "missing audit entry for event_type {et}"
            );
        }

        // The fault body (under the `posture` column key) carries the code and the
        // lowercase posture label from the SDK-free record.
        let fault = events
            .iter()
            .find(|e| e["event_type"] == PARKO_FAULT_EVENT_TYPE)
            .expect("a ParkoFault audit entry must exist");
        assert_eq!(fault["posture"]["code"], "nonfinite_command");
        assert_eq!(fault["posture"]["posture"], "locked_out");
    }

    /// DB-actor migration phase 1: `StoreHandle` RECOVERS a poisoned lock
    /// internally, so a transient panicking holder no longer blocks the audit
    /// write. The former "poison → write_failures incremented" arm is gone with
    /// the bare mutex; this test now pins the recovery behavior — a divergence is
    /// still durably recorded after a transient poison (write_failures stays 0).
    #[test]
    fn store_recovers_after_transient_poison_and_still_records() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("divergence_audit.sqlite");
        let store = StoreHandle::new(VerifierStore::new(db.to_str().unwrap()).expect("store"));

        // Poison the underlying mutex by panicking inside a `with` closure.
        let s = store.clone();
        let _ = std::thread::spawn(move || {
            s.with(|_g| panic!("poison the audit store for the recovery test"));
        })
        .join();

        let sink = AuditChainLinkerDivergenceSink::new(store.clone());
        sink.record(sample_event());
        assert_eq!(
            sink.write_failures(),
            0,
            "the handle recovers the poisoned lock and the divergence is still recorded"
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
        let sink = select_divergence_sink(None, Some(key_b64(9))).expect("in-memory is Ok");
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

    // ───────────── SG6 impact-audit bridge (#102 → #104) tests ──────────────

    fn icfg() -> ImpactCfg {
        ImpactCfg::default() // spike_threshold = 30.0
    }
    fn clean_ev() -> ImpactEvidence {
        ImpactEvidence {
            imu_accel_spike_mps2: 0.5,
            contact_sensor: false,
            vanished_object: false,
        }
    }
    fn contact_ev() -> ImpactEvidence {
        ImpactEvidence {
            contact_sensor: true,
            ..clean_ev()
        }
    }
    fn count_type(sink: &InMemoryImpactSink, ty: &str) -> usize {
        sink.events().iter().filter(|(t, _)| t == ty).count()
    }

    /// Rising edge: 3 latched ticks emit EXACTLY ONE `ImpactDetected` (no
    /// per-tick spam while latched stays true).
    #[test]
    fn test_rising_edge_emits_one_detected() {
        let sink = Arc::new(InMemoryImpactSink::new());
        let mut latch = RecordedImpactLatch::new(sink.clone());
        for _ in 0..3 {
            latch.observe(&contact_ev(), &icfg());
        }
        assert!(latch.is_latched());
        assert_eq!(
            count_type(&sink, IMPACT_DETECTED_EVENT_TYPE),
            1,
            "3 latched ticks must emit exactly ONE ImpactDetected"
        );
    }

    /// Clear emits exactly one `ImpactCleared`; a re-latch afterward emits a
    /// SECOND `ImpactDetected`.
    #[test]
    fn test_clear_emits_one_cleared_relatch_emits_second_detected() {
        let sink = Arc::new(InMemoryImpactSink::new());
        let mut latch = RecordedImpactLatch::new(sink.clone());

        latch.observe(&contact_ev(), &icfg()); // detect #1
        latch.clear(false, "noop"); // no-op → no event
        latch.clear(true, "supervisor_reset"); // clear #1
        assert!(!latch.is_latched());
        latch.observe(&contact_ev(), &icfg()); // detect #2 (re-latch)

        assert_eq!(
            count_type(&sink, IMPACT_DETECTED_EVENT_TYPE),
            2,
            "re-latch must emit a second ImpactDetected"
        );
        assert_eq!(
            count_type(&sink, IMPACT_CLEARED_EVENT_TYPE),
            1,
            "exactly one ImpactCleared on the single falling edge"
        );
    }

    /// File-backed durability + signing (mirrors the divergence sink's test): the
    /// impact transitions are durable, hash-linked, and verify under the key.
    #[test]
    fn test_impact_durably_recorded_signed_and_chained() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("impact_audit.sqlite");
        let key = SigningKey::from_bytes(&[11u8; 32]);
        let vk = key.verifying_key();

        let mut store = VerifierStore::new(db.to_str().unwrap()).expect("store");
        store.set_signing_key(key);
        let sink = Arc::new(ImpactAuditSink::new(StoreHandle::new(store)));
        // keep a separate handle to read back after.
        // (Re-open below to verify durability across a fresh store handle.)
        let db_path = db.to_str().unwrap().to_string();

        let mut latch = RecordedImpactLatch::new(sink.clone());
        latch.observe(&contact_ev(), &icfg());
        latch.clear(true, "supervisor_reset");
        assert_eq!(
            sink.write_failures(),
            0,
            "both transitions must be durably recorded"
        );

        let verifier = VerifierStore::new(&db_path).expect("re-open store");
        let v = verifier.verify_audit_chain_full(Some(&vk)).expect("verify");
        assert!(v.chain_intact, "audit chain must be hash-intact");
        assert!(v.signature_valid, "signatures must verify under the key");
        assert!(
            v.signed_entries >= 2,
            "both impact entries must be signed, got {}",
            v.signed_entries
        );

        let events = verifier.load_all_posture_events().expect("load events");
        let detected = events
            .iter()
            .find(|e| e["event_type"] == IMPACT_DETECTED_EVENT_TYPE)
            .expect("an ImpactDetected entry must exist");
        assert_eq!(detected["posture"]["contact_sensor"], true);
        assert!(
            events
                .iter()
                .any(|e| e["event_type"] == IMPACT_CLEARED_EVENT_TYPE),
            "an ImpactCleared entry must exist"
        );
    }

    /// DB-actor migration phase 1: `StoreHandle` recovers a transient poison, so
    /// the transition IS recorded (write_failures stays 0). The latch state and
    /// motion veto remain UNCHANGED — the infallibility-toward-the-latch proof
    /// still holds (the audit path never perturbs the veto regardless of outcome).
    #[test]
    fn test_sink_recovers_after_transient_poison_latch_and_veto_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("impact_audit.sqlite");
        let store = StoreHandle::new(VerifierStore::new(db.to_str().unwrap()).expect("store"));

        // Poison the underlying mutex by panicking inside a `with` closure.
        let s = store.clone();
        let _ = std::thread::spawn(move || {
            s.with(|_g| panic!("poison the audit store for the recovery test"));
        })
        .join();

        let sink = Arc::new(ImpactAuditSink::new(store.clone()));
        let mut latch = RecordedImpactLatch::new(sink.clone());
        latch.observe(&contact_ev(), &icfg());

        assert_eq!(
            sink.write_failures(),
            0,
            "the handle recovers the poison; the transition is recorded"
        );
        assert!(
            latch.is_latched(),
            "the latch (motion veto) must be UNCHANGED by the audit path"
        );
    }

    /// No durable sink (in-memory fallback) → the wrapped latch behaves IDENTICALLY
    /// to a bare `ImpactLatch` over the same evidence sequence.
    #[test]
    fn test_no_sink_latch_behavior_identical() {
        let sink = Arc::new(InMemoryImpactSink::new());
        let mut recorded = RecordedImpactLatch::new(sink);
        let mut bare = ImpactLatch::new();

        let seq = [clean_ev(), contact_ev(), clean_ev(), clean_ev()];
        for ev in &seq {
            recorded.observe(ev, &icfg());
            bare.observe(ev, &icfg());
            assert_eq!(
                recorded.is_latched(),
                bare.is_latched(),
                "wrapped latch must track the bare latch bit-for-bit"
            );
        }
        // and on clear
        recorded.clear(true, "reset");
        bare.clear(true);
        assert_eq!(recorded.is_latched(), bare.is_latched());
    }

    /// The detected payload carries the trigger booleans (which signals fired).
    #[test]
    fn test_detected_payload_has_trigger_booleans() {
        let sink = Arc::new(InMemoryImpactSink::new());
        let mut latch = RecordedImpactLatch::new(sink.clone());
        // contact + vanished fire; spike below threshold does not.
        let ev = ImpactEvidence {
            imu_accel_spike_mps2: 1.0,
            contact_sensor: true,
            vanished_object: true,
        };
        latch.observe(&ev, &icfg());

        let (_, body) = sink
            .events()
            .into_iter()
            .find(|(t, _)| t == IMPACT_DETECTED_EVENT_TYPE)
            .expect("detected");
        let json: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(json["contact_sensor"], true);
        assert_eq!(json["vanished_object"], true);
        assert_eq!(
            json["spike_over_threshold"], false,
            "a sub-threshold spike must not read as fired"
        );
        assert_eq!(
            json["spike_magnitude_mps2"], 1.0,
            "a finite magnitude is retained"
        );
    }

    /// A non-finite spike magnitude is OMITTED from the payload entirely (it never
    /// latches on its own and is not a trustworthy datum). The latch here fires on
    /// the contact signal; the NaN IMU contributes no magnitude field.
    #[test]
    fn test_nonfinite_spike_magnitude_omitted() {
        let sink = Arc::new(InMemoryImpactSink::new());
        let mut latch = RecordedImpactLatch::new(sink.clone());
        let ev = ImpactEvidence {
            imu_accel_spike_mps2: f64::NAN,
            contact_sensor: true,
            vanished_object: false,
        };
        latch.observe(&ev, &icfg());

        let (_, body) = sink
            .events()
            .into_iter()
            .find(|(t, _)| t == IMPACT_DETECTED_EVENT_TYPE)
            .expect("detected");
        assert!(
            !body.contains("spike_magnitude_mps2"),
            "a non-finite spike magnitude must be omitted from the payload, got {body}"
        );
        let json: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(json["contact_sensor"], true);
        assert_eq!(
            json["spike_over_threshold"], false,
            "a non-finite spike never reads as fired"
        );
    }

    // ─────────────── #103 clearance-loop audit integration ──────────────────

    const LOOP_MAX_AGE: u64 = 60_000;

    fn good_grant(now: u64) -> OperatorClearanceGrant {
        OperatorClearanceGrant {
            operator_id: "op-7".to_string(),
            granted_at_ms: now - 100,
        }
    }
    /// Drive a recorded loop into EscalationRaised.
    fn escalate(loop_: &mut RecordedClearanceLoop) {
        loop_.observe(&contact_ev(), &icfg(), 1_000); // Normal → Latched (Detected)
        loop_.observe(&clean_ev(), &icfg(), 1_001); // Latched → EscalationRaised (Raised)
        assert_eq!(loop_.state(), ClearanceState::EscalationRaised);
    }

    /// Escalation is recorded exactly ONCE per incident — re-impact while
    /// escalated raises nothing new.
    #[test]
    fn test_loop_escalation_raised_once() {
        let sink = Arc::new(InMemoryImpactSink::new());
        let mut loop_ = RecordedClearanceLoop::new(sink.clone());
        escalate(&mut loop_);
        // many more ticks, including a re-impact
        for t in 0..5 {
            loop_.observe(&clean_ev(), &icfg(), 2_000 + t);
        }
        loop_.observe(&contact_ev(), &icfg(), 3_000); // re-impact while escalated
        assert_eq!(
            count_type(&sink, IMPACT_ESCALATION_RAISED_EVENT_TYPE),
            1,
            "exactly one ImpactEscalationRaised per incident"
        );
        assert_eq!(
            count_type(&sink, IMPACT_DETECTED_EVENT_TYPE),
            1,
            "exactly one ImpactDetected on the incident-open edge"
        );
    }

    /// A well-formed grant emits ONE `ImpactCleared` (operator id as source) and
    /// no rejection.
    #[test]
    fn test_loop_clear_emits_impact_cleared() {
        let sink = Arc::new(InMemoryImpactSink::new());
        let mut loop_ = RecordedClearanceLoop::new(sink.clone());
        escalate(&mut loop_);
        let now = 5_000u64;
        assert!(loop_.try_clear(&good_grant(now), now, LOOP_MAX_AGE).is_ok());
        assert_eq!(loop_.state(), ClearanceState::Normal);
        assert_eq!(
            count_type(&sink, IMPACT_CLEARED_EVENT_TYPE),
            1,
            "one ImpactCleared on success"
        );
        assert_eq!(
            count_type(&sink, IMPACT_CLEARANCE_REJECTED_EVENT_TYPE),
            0,
            "no rejection on success"
        );
        let (_, body) = sink
            .events()
            .into_iter()
            .find(|(t, _)| t == IMPACT_CLEARED_EVENT_TYPE)
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            json["clearance_source"], "op-7",
            "the clearing operator id is the source"
        );
    }

    /// A rejected clearance is RECORDED (reason + operator id) and leaves the
    /// state unchanged (still immobilized) — never silently absorbed.
    #[test]
    fn test_loop_rejection_recorded_state_unchanged() {
        let sink = Arc::new(InMemoryImpactSink::new());
        let mut loop_ = RecordedClearanceLoop::new(sink.clone());
        escalate(&mut loop_);
        let now = 5_000u64;
        let malformed = OperatorClearanceGrant {
            operator_id: "op-9".to_string(),
            granted_at_ms: now + 10,
        }; // future
        let r = loop_.try_clear(&malformed, now, LOOP_MAX_AGE);
        assert_eq!(r, Err(ClearanceRejection::MalformedGrant));
        assert!(
            loop_.is_immobilized(),
            "state must be unchanged after a rejected grant"
        );
        assert_eq!(count_type(&sink, IMPACT_CLEARANCE_REJECTED_EVENT_TYPE), 1);
        assert_eq!(
            count_type(&sink, IMPACT_CLEARED_EVENT_TYPE),
            0,
            "no ImpactCleared on rejection"
        );
        let (_, body) = sink
            .events()
            .into_iter()
            .find(|(t, _)| t == IMPACT_CLEARANCE_REJECTED_EVENT_TYPE)
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["reason"], "malformed_grant");
        assert_eq!(
            json["operator_id"], "op-9",
            "the operator id (audit subject) is recorded"
        );
    }

    /// The audit sink does NOT affect the state machine or the motion veto — the
    /// #263 infallibility proof, extended to the loop. DB-actor migration phase 1:
    /// `StoreHandle` recovers a transient poison, so the writes now SUCCEED
    /// (write_failures stays 0); the veto/escalation invariants are unchanged
    /// regardless of audit outcome.
    #[test]
    fn test_loop_audit_path_state_unaffected_and_recovers_poison() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("impact_audit.sqlite");
        let store = StoreHandle::new(VerifierStore::new(db.to_str().unwrap()).expect("store"));
        // Poison the underlying mutex by panicking inside a `with` closure.
        let s = store.clone();
        let _ = std::thread::spawn(move || {
            s.with(|_g| panic!("poison the audit store for the recovery test"));
        })
        .join();

        let sink = Arc::new(ImpactAuditSink::new(store.clone()));
        let mut loop_ = RecordedClearanceLoop::new(sink.clone());
        loop_.observe(&contact_ev(), &icfg(), 1_000);
        loop_.observe(&clean_ev(), &icfg(), 1_001);
        assert!(loop_.is_immobilized(), "veto unaffected by the audit path");
        assert!(
            loop_.escalation_pending(),
            "escalation state unaffected by the audit path"
        );
        assert_eq!(
            sink.write_failures(),
            0,
            "the handle recovers the poison; writes land"
        );
        // A good grant still clears the state machine regardless of audit outcome.
        let now = 5_000u64;
        assert!(loop_.try_clear(&good_grant(now), now, LOOP_MAX_AGE).is_ok());
        assert!(
            !loop_.is_immobilized(),
            "clearance state machine unaffected by the audit path"
        );
    }

    /// The wrapped loop's veto tracks a bare `ClearanceLoop` bit-for-bit over a
    /// sequence + a clear (no-durable in-memory sink).
    #[test]
    fn test_loop_veto_unchanged_without_sink() {
        use parko_core::ClearanceLoop as BareLoop;
        let sink = Arc::new(InMemoryImpactSink::new());
        let mut recorded = RecordedClearanceLoop::new(sink);
        let mut bare = BareLoop::new();

        let seq = [clean_ev(), contact_ev(), clean_ev(), clean_ev()];
        for (i, ev) in seq.iter().enumerate() {
            let now = 1_000 + i as u64;
            recorded.observe(ev, &icfg(), now);
            bare.observe(ev, &icfg(), now);
            assert_eq!(
                recorded.is_immobilized(),
                bare.is_immobilized(),
                "wrapped loop veto must track the bare loop"
            );
        }
        let now = 9_000u64;
        let g = OperatorClearanceGrant {
            operator_id: "op".to_string(),
            granted_at_ms: now - 10,
        };
        let _ = recorded.try_clear(&g, now, LOOP_MAX_AGE);
        let _ = bare.try_clear(&g, now, LOOP_MAX_AGE);
        assert_eq!(recorded.is_immobilized(), bare.is_immobilized());
        assert!(!recorded.is_immobilized());
    }
}

// ---------------------------------------------------------------------------
// S-DG1c — the posture-engine adapter for `PostureSignalSink`.
// ---------------------------------------------------------------------------

/// Wires the comparator's [`crate::comparator::PostureSignalSink`] to the
/// verifier's posture engine: each tick becomes a typed
/// `PostureRecalcTrigger::GovernorDivergence` on the engine's coalescing
/// channel (S-DG1c; docs/safety/STAGE_S-DG1_DIVERGENCE_POSTURE.md).
///
/// Non-blocking by construction: `try_send`, never `send().await` — the
/// evaluate path must not stall on a busy engine. A FULL channel drops the
/// tick, which is fail-safe in both directions: a dropped SIGNIFICANT tick is
/// re-sent on the next tick (the divergence persists, and the comparator's
/// reconciliation already bounded THIS command regardless); a dropped
/// AGREEMENT tick merely delays recovery earn-back. A CLOSED channel (engine
/// gone) is logged loudly — with no engine the fleet's posture cache goes
/// stale within POSTURE_CACHE_TTL_MS and every gated route fails closed
/// anyway (the outer fail-closed backstop).
pub struct PostureEngineSenderSink {
    tx: kirra_verifier::posture_engine_v2::PostureEngineSender,
    /// Count of ticks dropped because the coalescing worker was saturated
    /// (`TrySendError::Full`). A relaxed counter, NOT an stderr write: the
    /// evaluate path must stay non-blocking (Copilot #855 — an `eprintln!`
    /// here takes the stdio lock and can both block and spam under sustained
    /// saturation). Observability reads this via [`dropped_full`]; the drop
    /// itself is fail-safe (a significant tick re-signals next tick, so a
    /// dropped one only delays, never loses, the posture consequence).
    dropped_full: std::sync::atomic::AtomicU64,
    /// Set once the channel is observed CLOSED (engine gone). A terminal,
    /// at-most-once transition — not a per-tick event — so it does not spam;
    /// exposed via [`channel_closed`] for a health surface to alarm on.
    closed: std::sync::atomic::AtomicBool,
}

impl PostureEngineSenderSink {
    #[must_use]
    pub fn new(tx: kirra_verifier::posture_engine_v2::PostureEngineSender) -> Self {
        Self {
            tx,
            dropped_full: std::sync::atomic::AtomicU64::new(0),
            closed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Ticks dropped so far because the posture-engine channel was full
    /// (fail-safe drops — the divergence persists and re-signals next tick).
    #[must_use]
    pub fn dropped_full(&self) -> u64 {
        self.dropped_full.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// `true` once the posture-engine channel has been observed closed (the
    /// engine is gone; the stale-cache TTL backstop is the only fail-closed
    /// path remaining). Terminal — a health surface should alarm on it.
    #[must_use]
    pub fn channel_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl crate::comparator::PostureSignalSink for PostureEngineSenderSink {
    fn divergence_posture_tick(&self, significant: bool, escalated: bool) {
        use kirra_verifier::posture_engine_v2::PostureRecalcTrigger;
        use std::sync::atomic::Ordering;
        match self.tx.try_send(PostureRecalcTrigger::GovernorDivergence {
            significant,
            escalated,
        }) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                // Coalescing worker saturated; next tick re-signals. Count it —
                // no stderr write on the hot path (non-blocking, no spam).
                self.dropped_full.fetch_add(1, Ordering::Relaxed);
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                // Terminal: latch once. `swap` makes the eprintln at-most-once
                // (the engine does not un-close), so no per-tick spam.
                if !self.closed.swap(true, Ordering::Relaxed) {
                    eprintln!(
                        "parko-kirra: posture engine channel CLOSED — divergence \
                         posture signal lost; the stale-cache TTL backstop is now \
                         the only fail-closed path"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod posture_sink_tests {
    use super::*;
    use crate::comparator::PostureSignalSink as _;
    use kirra_verifier::posture_engine_v2::PostureRecalcTrigger;

    /// The adapter forwards the tick as the typed trigger, non-blocking.
    #[test]
    fn adapter_forwards_typed_trigger_via_try_send() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let sink = PostureEngineSenderSink::new(tx);
        sink.divergence_posture_tick(true, true);
        match rx.try_recv().expect("trigger forwarded") {
            PostureRecalcTrigger::GovernorDivergence {
                significant,
                escalated,
            } => {
                assert!(significant && escalated);
            }
            other => panic!("wrong trigger: {other}"),
        }
    }

    /// A full channel drops rather than blocks (the evaluate path must not
    /// stall), counts the drop (no stderr on the hot path — Copilot #855), and
    /// the sink survives to forward the next tick.
    #[test]
    fn full_channel_drops_without_blocking() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let sink = PostureEngineSenderSink::new(tx);
        sink.divergence_posture_tick(true, false); // fills the channel
        sink.divergence_posture_tick(true, true); // dropped, no block/panic
        assert_eq!(sink.dropped_full(), 1, "the dropped tick is counted");
        assert!(!sink.channel_closed(), "a full channel is not a closed one");
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err(), "second tick was dropped by design");
    }

    /// A CLOSED channel latches `channel_closed` (terminal, at-most-once — no
    /// per-tick spam) and does not panic.
    #[test]
    fn closed_channel_latches_terminal_state() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let sink = PostureEngineSenderSink::new(tx);
        drop(rx); // engine gone
        sink.divergence_posture_tick(true, false);
        sink.divergence_posture_tick(false, false);
        assert!(sink.channel_closed());
        assert_eq!(sink.dropped_full(), 0, "closed is not counted as full-drop");
    }
}
