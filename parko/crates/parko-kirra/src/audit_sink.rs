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
use kirra_verifier::verifier_store::VerifierStore;
use parko_core::{
    ClearanceLoop, ClearanceRejection, ClearanceState, ImpactCfg, ImpactEvidence, ImpactLatch,
    OperatorClearanceGrant,
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

/// Shared, fail-closed writer over the SDK's hash-chained, Ed25519-signed audit
/// ledger. Both the CERT-006 comparator-divergence sink and the SG6 impact-audit
/// sink record through this ONE struct (REUSE — a single write path and a single
/// `write_failures` accounting): it owns the [`VerifierStore`] handle (which owns
/// `audit_log_chain` + the signing key) and a detected-but-unrecorded counter,
/// and appends every event via [`VerifierStore::save_posture_event_chained`]
/// (which goes through `AuditChainLinker::append_audit_event_tx`).
struct ChainedAuditWriter {
    store: Arc<Mutex<VerifierStore>>,
    write_failures: AtomicU64,
}

impl ChainedAuditWriter {
    fn new(store: Arc<Mutex<VerifierStore>>) -> Self {
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
        Ok(Self::new(Arc::new(Mutex::new(store))))
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
        let outcome = match self.store.lock() {
            Ok(mut store) => {
                store.save_posture_event_chained(source, event_type, body, None, Self::now_ms())
            }
            Err(_) => {
                self.note_failure();
                eprintln!(
                    "[audit] {event_type} NOT recorded — audit store mutex poisoned \
                     (event is UNAUDITED)"
                );
                return;
            }
        };
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
    pub fn new(store: Arc<Mutex<VerifierStore>>) -> Self {
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
        self.writer
            .record("governor_comparator", COMPARATOR_DIVERGENCE_EVENT_TYPE, &body);
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
    pub fn new(store: Arc<Mutex<VerifierStore>>) -> Self {
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
// SAFETY: SG6 | REQ: impact-audit-bridge | TEST: test_rising_edge_emits_one_detected,test_clear_emits_one_cleared_relatch_emits_second_detected,test_impact_durably_recorded_signed_and_chained,test_sink_failure_counts_latch_and_veto_unchanged,test_no_sink_latch_behavior_identical,test_detected_payload_has_trigger_booleans,test_nonfinite_spike_magnitude_omitted
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
// SAFETY: SG6 | REQ: clearance-confirmation-loop | TEST: test_loop_escalation_raised_once,test_loop_clear_emits_impact_cleared,test_loop_rejection_recorded_state_unchanged,test_loop_sink_failure_state_unaffected,test_loop_veto_unchanged_without_sink
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
            self.sink.record_escalation_raised(&ImpactEscalationPayload {
                detail: "post-collision immobilization — operator clearance required".to_string(),
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
        let outcome = self
            .clearance
            .try_clear(grant, now_ms, max_grant_age_ms);
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

    // ───────────── SG6 impact-audit bridge (#102 → #104) tests ──────────────

    fn icfg() -> ImpactCfg {
        ImpactCfg::default() // spike_threshold = 30.0
    }
    fn clean_ev() -> ImpactEvidence {
        ImpactEvidence { imu_accel_spike_mps2: 0.5, contact_sensor: false, vanished_object: false }
    }
    fn contact_ev() -> ImpactEvidence {
        ImpactEvidence { contact_sensor: true, ..clean_ev() }
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

        assert_eq!(count_type(&sink, IMPACT_DETECTED_EVENT_TYPE), 2, "re-latch must emit a second ImpactDetected");
        assert_eq!(count_type(&sink, IMPACT_CLEARED_EVENT_TYPE), 1, "exactly one ImpactCleared on the single falling edge");
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
        let sink = Arc::new(ImpactAuditSink::new(Arc::new(Mutex::new(store))));
        // keep a separate handle to read back after.
        // (Re-open below to verify durability across a fresh store handle.)
        let db_path = db.to_str().unwrap().to_string();

        let mut latch = RecordedImpactLatch::new(sink.clone());
        latch.observe(&contact_ev(), &icfg());
        latch.clear(true, "supervisor_reset");
        assert_eq!(sink.write_failures(), 0, "both transitions must be durably recorded");

        let verifier = VerifierStore::new(&db_path).expect("re-open store");
        let v = verifier.verify_audit_chain_full(Some(&vk)).expect("verify");
        assert!(v.chain_intact, "audit chain must be hash-intact");
        assert!(v.signature_valid, "signatures must verify under the key");
        assert!(v.signed_entries >= 2, "both impact entries must be signed, got {}", v.signed_entries);

        let events = verifier.load_all_posture_events().expect("load events");
        let detected = events.iter().find(|e| e["event_type"] == IMPACT_DETECTED_EVENT_TYPE)
            .expect("an ImpactDetected entry must exist");
        assert_eq!(detected["posture"]["contact_sensor"], true);
        assert!(events.iter().any(|e| e["event_type"] == IMPACT_CLEARED_EVENT_TYPE),
            "an ImpactCleared entry must exist");
    }

    /// Sink failure (poisoned store) → `write_failures` increments, but the latch
    /// state and the motion veto are UNCHANGED (the infallibility proof).
    #[test]
    fn test_sink_failure_counts_latch_and_veto_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("impact_audit.sqlite");
        let store = Arc::new(Mutex::new(VerifierStore::new(db.to_str().unwrap()).expect("store")));

        // Poison the store mutex so the audit write cannot land.
        let s = Arc::clone(&store);
        let _ = std::thread::spawn(move || {
            let _g = s.lock().unwrap();
            panic!("poison the audit store for the failure test");
        })
        .join();

        let sink = Arc::new(ImpactAuditSink::new(Arc::clone(&store)));
        let mut latch = RecordedImpactLatch::new(sink.clone());
        latch.observe(&contact_ev(), &icfg());

        assert_eq!(sink.write_failures(), 1, "a transition that could not be recorded MUST be counted");
        assert!(latch.is_latched(), "the latch (motion veto) must be UNCHANGED by a sink failure");
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
            assert_eq!(recorded.is_latched(), bare.is_latched(),
                "wrapped latch must track the bare latch bit-for-bit");
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
        let ev = ImpactEvidence { imu_accel_spike_mps2: 1.0, contact_sensor: true, vanished_object: true };
        latch.observe(&ev, &icfg());

        let (_, body) = sink.events().into_iter().find(|(t, _)| t == IMPACT_DETECTED_EVENT_TYPE).expect("detected");
        let json: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(json["contact_sensor"], true);
        assert_eq!(json["vanished_object"], true);
        assert_eq!(json["spike_over_threshold"], false, "a sub-threshold spike must not read as fired");
        assert_eq!(json["spike_magnitude_mps2"], 1.0, "a finite magnitude is retained");
    }

    /// A non-finite spike magnitude is OMITTED from the payload entirely (it never
    /// latches on its own and is not a trustworthy datum). The latch here fires on
    /// the contact signal; the NaN IMU contributes no magnitude field.
    #[test]
    fn test_nonfinite_spike_magnitude_omitted() {
        let sink = Arc::new(InMemoryImpactSink::new());
        let mut latch = RecordedImpactLatch::new(sink.clone());
        let ev = ImpactEvidence { imu_accel_spike_mps2: f64::NAN, contact_sensor: true, vanished_object: false };
        latch.observe(&ev, &icfg());

        let (_, body) = sink.events().into_iter().find(|(t, _)| t == IMPACT_DETECTED_EVENT_TYPE).expect("detected");
        assert!(!body.contains("spike_magnitude_mps2"),
            "a non-finite spike magnitude must be omitted from the payload, got {body}");
        let json: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(json["contact_sensor"], true);
        assert_eq!(json["spike_over_threshold"], false, "a non-finite spike never reads as fired");
    }

    // ─────────────── #103 clearance-loop audit integration ──────────────────

    const LOOP_MAX_AGE: u64 = 60_000;

    fn good_grant(now: u64) -> OperatorClearanceGrant {
        OperatorClearanceGrant { operator_id: "op-7".to_string(), granted_at_ms: now - 100 }
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
        assert_eq!(count_type(&sink, IMPACT_ESCALATION_RAISED_EVENT_TYPE), 1,
            "exactly one ImpactEscalationRaised per incident");
        assert_eq!(count_type(&sink, IMPACT_DETECTED_EVENT_TYPE), 1,
            "exactly one ImpactDetected on the incident-open edge");
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
        assert_eq!(count_type(&sink, IMPACT_CLEARED_EVENT_TYPE), 1, "one ImpactCleared on success");
        assert_eq!(count_type(&sink, IMPACT_CLEARANCE_REJECTED_EVENT_TYPE), 0, "no rejection on success");
        let (_, body) = sink.events().into_iter().find(|(t, _)| t == IMPACT_CLEARED_EVENT_TYPE).unwrap();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["clearance_source"], "op-7", "the clearing operator id is the source");
    }

    /// A rejected clearance is RECORDED (reason + operator id) and leaves the
    /// state unchanged (still immobilized) — never silently absorbed.
    #[test]
    fn test_loop_rejection_recorded_state_unchanged() {
        let sink = Arc::new(InMemoryImpactSink::new());
        let mut loop_ = RecordedClearanceLoop::new(sink.clone());
        escalate(&mut loop_);
        let now = 5_000u64;
        let malformed = OperatorClearanceGrant { operator_id: "op-9".to_string(), granted_at_ms: now + 10 }; // future
        let r = loop_.try_clear(&malformed, now, LOOP_MAX_AGE);
        assert_eq!(r, Err(ClearanceRejection::MalformedGrant));
        assert!(loop_.is_immobilized(), "state must be unchanged after a rejected grant");
        assert_eq!(count_type(&sink, IMPACT_CLEARANCE_REJECTED_EVENT_TYPE), 1);
        assert_eq!(count_type(&sink, IMPACT_CLEARED_EVENT_TYPE), 0, "no ImpactCleared on rejection");
        let (_, body) = sink.events().into_iter().find(|(t, _)| t == IMPACT_CLEARANCE_REJECTED_EVENT_TYPE).unwrap();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["reason"], "malformed_grant");
        assert_eq!(json["operator_id"], "op-9", "the operator id (audit subject) is recorded");
    }

    /// Sink failure (poisoned store) does NOT affect the state machine or the
    /// motion veto — the #263 infallibility proof, extended to the loop.
    #[test]
    fn test_loop_sink_failure_state_unaffected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("impact_audit.sqlite");
        let store = Arc::new(Mutex::new(VerifierStore::new(db.to_str().unwrap()).expect("store")));
        // Poison the store mutex.
        let s = Arc::clone(&store);
        let _ = std::thread::spawn(move || {
            let _g = s.lock().unwrap();
            panic!("poison the audit store for the failure test");
        })
        .join();

        let sink = Arc::new(ImpactAuditSink::new(Arc::clone(&store)));
        let mut loop_ = RecordedClearanceLoop::new(sink.clone());
        loop_.observe(&contact_ev(), &icfg(), 1_000);
        loop_.observe(&clean_ev(), &icfg(), 1_001);
        assert!(loop_.is_immobilized(), "veto unaffected by sink failure");
        assert!(loop_.escalation_pending(), "escalation state unaffected by sink failure");
        assert!(sink.write_failures() >= 1, "the failed writes must be counted");
        // A good grant still clears the state machine regardless of audit outcome.
        let now = 5_000u64;
        assert!(loop_.try_clear(&good_grant(now), now, LOOP_MAX_AGE).is_ok());
        assert!(!loop_.is_immobilized(), "clearance state machine unaffected by sink failure");
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
            assert_eq!(recorded.is_immobilized(), bare.is_immobilized(),
                "wrapped loop veto must track the bare loop");
        }
        let now = 9_000u64;
        let g = OperatorClearanceGrant { operator_id: "op".to_string(), granted_at_ms: now - 10 };
        let _ = recorded.try_clear(&g, now, LOOP_MAX_AGE);
        let _ = bare.try_clear(&g, now, LOOP_MAX_AGE);
        assert_eq!(recorded.is_immobilized(), bare.is_immobilized());
        assert!(!recorded.is_immobilized());
    }
}
