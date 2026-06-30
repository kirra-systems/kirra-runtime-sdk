//! SDK-free audit-recording seam for the ML decision path (roadmap L5).
//!
//! The goal of L5 is to **remove the SDK dependency from the ML decision path**:
//! Parko's decision code should record decisions / overrides / faults / health
//! through a small trait it owns, never by reaching into `kirra-verifier`'s
//! `AuditChainLinker` / `VerifierStore` directly. This module is that trait plus
//! the two dependency-free implementations every consumer needs (a test mock and
//! a no-op); the durable, hash-chained, Ed25519-signed implementation lives in
//! `parko-kirra` behind its existing `verifier-sink` feature and is injected as a
//! `dyn AuditClient`.
//!
//! This mirrors the established `DivergenceEventSink` pattern in `parko-kirra`
//! (trait + `InMemory*` + SDK-backed impl) but unifies the four record kinds the
//! roadmap calls out into one seam, and places the trait in `parko-core` (which
//! has **no** SDK dependency) so the decision path can depend on it without
//! pulling the verifier crate.
//!
//! Implementations MUST be cheap and non-panicking on the decision path: a
//! recording failure is handled *inside* the implementation (fail-closed for the
//! ledger), never propagated up to crash the governor — exactly the contract the
//! `DivergenceEventSink` doc already states for the audit path.

use serde::{Serialize, Serializer};
use std::sync::Mutex;

use crate::safety::SafetyPosture;

/// Stable lowercase audit label for a posture — matches the `recommended_posture`
/// convention used by `parko-kirra`'s `DivergenceEvent` so all audit bodies use
/// the same posture spelling.
#[must_use]
pub fn posture_label(posture: SafetyPosture) -> &'static str {
    match posture {
        SafetyPosture::Nominal => "nominal",
        SafetyPosture::Degraded => "degraded",
        SafetyPosture::LockedOut => "locked_out",
    }
}

/// Serialize a [`SafetyPosture`] as its [`posture_label`] string. Lets the
/// records carry the real (type-safe) enum while still producing a stable JSON
/// body, without requiring `Serialize` on the shared `SafetyPosture` type.
fn serialize_posture<S: Serializer>(posture: &SafetyPosture, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(posture_label(*posture))
}

/// A governed per-tick **decision**: the doer's (ML) proposal and the command the
/// governor actually allowed, under the resulting posture. The normal-operation
/// record (no override).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecisionRecord {
    /// Monotonic decision timestamp (ms).
    pub tick_ms: u64,
    /// Proposed forward velocity from the doer/ML (m/s).
    pub proposed_linear_mps: f64,
    /// Proposed yaw rate from the doer/ML (rad/s).
    pub proposed_angular_rps: f64,
    /// Commanded forward velocity after governance (m/s).
    pub commanded_linear_mps: f64,
    /// Commanded yaw rate after governance (rad/s).
    pub commanded_angular_rps: f64,
    /// Posture the decision was made under.
    #[serde(serialize_with = "serialize_posture")]
    pub posture: SafetyPosture,
}

/// A governor **override** of the doer output: the safety layer changed the
/// command (envelope clamp, comparator-divergence reconcile, MRC floor, …).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OverrideRecord {
    /// Monotonic decision timestamp (ms).
    pub tick_ms: u64,
    /// Short, stable reason code for the override, e.g. `"envelope_clamp"`,
    /// `"comparator_divergence"`, `"mrc_floor"`, `"degraded_decel"`.
    pub reason: &'static str,
    /// Proposed forward velocity from the doer/ML (m/s).
    pub proposed_linear_mps: f64,
    /// Proposed yaw rate from the doer/ML (rad/s).
    pub proposed_angular_rps: f64,
    /// Commanded forward velocity the governor substituted (m/s).
    pub commanded_linear_mps: f64,
    /// Commanded yaw rate the governor substituted (rad/s).
    pub commanded_angular_rps: f64,
    /// Posture the override was applied under.
    #[serde(serialize_with = "serialize_posture")]
    pub posture: SafetyPosture,
}

/// A **fault** detected on the decision path: a non-finite command, a backend
/// failure, sensor staleness, a comparator escalation to lockout, etc.
/// Fail-closed evidence.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FaultRecord {
    /// Monotonic detection timestamp (ms).
    pub tick_ms: u64,
    /// Short, stable fault code, e.g. `"nonfinite_command"`, `"backend_failure"`,
    /// `"sensor_stale"`, `"divergence_lockout"`.
    pub code: &'static str,
    /// Human-readable detail for the audit body.
    pub detail: String,
    /// Posture in effect when the fault was detected.
    #[serde(serialize_with = "serialize_posture")]
    pub posture: SafetyPosture,
}

/// A periodic **health** / posture snapshot of the decision loop.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HealthRecord {
    /// Monotonic snapshot timestamp (ms).
    pub tick_ms: u64,
    /// Current posture.
    #[serde(serialize_with = "serialize_posture")]
    pub posture: SafetyPosture,
    /// Most recent inference latency (ms), if measured this tick.
    pub inference_latency_ms: Option<u64>,
    /// Governor-comparator divergence accumulator value.
    pub divergence_accumulator: u32,
    /// Total ticks processed so far.
    pub ticks_processed: u64,
}

/// The SDK-free audit seam for the ML decision path. Implementations route the
/// records to a durable tamper-evident ledger (the `parko-kirra` SDK impl), a
/// test buffer ([`MockAuditClient`]), or nowhere ([`NoopAuditClient`]). Parko
/// depends only on this trait, never on `kirra-verifier` directly.
///
/// Contract: methods are called on the decision path, so they must be cheap and
/// **must not panic** — a recording failure is the implementation's to handle
/// (fail-closed for the ledger), never propagated to crash the governor.
pub trait AuditClient: Send + Sync {
    /// Record a normal governed decision.
    fn record_decision(&self, record: DecisionRecord);
    /// Record a governor override of the doer output.
    fn record_override(&self, record: OverrideRecord);
    /// Record a fault detected on the decision path.
    fn record_fault(&self, record: FaultRecord);
    /// Record a periodic health/posture snapshot.
    fn record_health(&self, record: HealthRecord);
}

/// An [`AuditClient`] that records **nothing**. The explicit "audit disabled"
/// choice — e.g. a bring-up build with no ledger wired.
///
/// Note: a no-op leaves the decision path **unaudited**. Production deployments
/// MUST inject a durable client (the `parko-kirra` SDK impl); this exists so the
/// decision path can be constructed and tested without a ledger, not as a
/// production default.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopAuditClient;

impl AuditClient for NoopAuditClient {
    #[inline]
    fn record_decision(&self, _record: DecisionRecord) {}
    #[inline]
    fn record_override(&self, _record: OverrideRecord) {}
    #[inline]
    fn record_fault(&self, _record: FaultRecord) {}
    #[inline]
    fn record_health(&self, _record: HealthRecord) {}
}

/// An [`AuditClient`] that buffers every record in memory, per category, for
/// tests. Thread-safe; tests inspect the accessors to assert what was emitted —
/// the same role [`crate`]'s `InMemoryDivergenceSink` plays for divergences.
#[derive(Debug, Default)]
pub struct MockAuditClient {
    decisions: Mutex<Vec<DecisionRecord>>,
    overrides: Mutex<Vec<OverrideRecord>>,
    faults: Mutex<Vec<FaultRecord>>,
    health: Mutex<Vec<HealthRecord>>,
}

impl MockAuditClient {
    /// A fresh, empty mock.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of recorded decisions.
    #[must_use]
    pub fn decisions(&self) -> Vec<DecisionRecord> {
        self.decisions.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// Snapshot of recorded overrides.
    #[must_use]
    pub fn overrides(&self) -> Vec<OverrideRecord> {
        self.overrides.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// Snapshot of recorded faults.
    #[must_use]
    pub fn faults(&self) -> Vec<FaultRecord> {
        self.faults.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// Snapshot of recorded health records.
    #[must_use]
    pub fn health(&self) -> Vec<HealthRecord> {
        self.health.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// `(decisions, overrides, faults, health)` counts — convenient for asserts.
    #[must_use]
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        (
            self.decisions.lock().map(|v| v.len()).unwrap_or(0),
            self.overrides.lock().map(|v| v.len()).unwrap_or(0),
            self.faults.lock().map(|v| v.len()).unwrap_or(0),
            self.health.lock().map(|v| v.len()).unwrap_or(0),
        )
    }
}

impl AuditClient for MockAuditClient {
    fn record_decision(&self, record: DecisionRecord) {
        if let Ok(mut v) = self.decisions.lock() {
            v.push(record);
        }
    }
    fn record_override(&self, record: OverrideRecord) {
        if let Ok(mut v) = self.overrides.lock() {
            v.push(record);
        }
    }
    fn record_fault(&self, record: FaultRecord) {
        if let Ok(mut v) = self.faults.lock() {
            v.push(record);
        }
    }
    fn record_health(&self, record: HealthRecord) {
        if let Ok(mut v) = self.health.lock() {
            v.push(record);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision() -> DecisionRecord {
        DecisionRecord {
            tick_ms: 1,
            proposed_linear_mps: 1.0,
            proposed_angular_rps: 0.1,
            commanded_linear_mps: 1.0,
            commanded_angular_rps: 0.1,
            posture: SafetyPosture::Nominal,
        }
    }
    fn override_rec() -> OverrideRecord {
        OverrideRecord {
            tick_ms: 2,
            reason: "envelope_clamp",
            proposed_linear_mps: 5.0,
            proposed_angular_rps: 0.0,
            commanded_linear_mps: 2.0,
            commanded_angular_rps: 0.0,
            posture: SafetyPosture::Degraded,
        }
    }
    fn fault() -> FaultRecord {
        FaultRecord {
            tick_ms: 3,
            code: "nonfinite_command",
            detail: "linear was NaN".to_string(),
            posture: SafetyPosture::LockedOut,
        }
    }
    fn health() -> HealthRecord {
        HealthRecord {
            tick_ms: 4,
            posture: SafetyPosture::Nominal,
            inference_latency_ms: Some(12),
            divergence_accumulator: 0,
            ticks_processed: 100,
        }
    }

    #[test]
    fn noop_records_nothing_and_never_panics() {
        let c = NoopAuditClient;
        c.record_decision(decision());
        c.record_override(override_rec());
        c.record_fault(fault());
        c.record_health(health());
        // Nothing to assert beyond "did not panic"; the no-op has no state.
    }

    #[test]
    fn mock_captures_each_category() {
        let c = MockAuditClient::new();
        c.record_decision(decision());
        c.record_decision(decision());
        c.record_override(override_rec());
        c.record_fault(fault());
        c.record_health(health());
        c.record_health(health());
        c.record_health(health());

        assert_eq!(c.counts(), (2, 1, 1, 3));
        assert_eq!(c.decisions().len(), 2);
        assert_eq!(c.overrides()[0].reason, "envelope_clamp");
        assert_eq!(c.faults()[0].code, "nonfinite_command");
        assert_eq!(c.faults()[0].detail, "linear was NaN");
        assert_eq!(c.health()[0].inference_latency_ms, Some(12));
    }

    #[test]
    fn usable_as_a_trait_object() {
        let mock = MockAuditClient::new();
        let client: &dyn AuditClient = &mock;
        client.record_decision(decision());
        client.record_fault(fault());
        assert_eq!(mock.counts(), (1, 0, 1, 0));

        // No-op behind the same trait object type.
        let noop = NoopAuditClient;
        let client2: &dyn AuditClient = &noop;
        client2.record_decision(decision());
    }

    #[test]
    fn records_serialize_to_json_bodies() {
        // The durable parko-kirra impl serializes records into JSON audit bodies
        // (it never deserializes them — matching the Serialize-only DivergenceEvent
        // pattern, since `&'static str` codes can't derive Deserialize). Confirm
        // each record serializes and carries its key fields.
        let dj = serde_json::to_string(&decision()).expect("serialize decision");
        assert!(dj.contains("\"posture\":\"nominal\""), "{dj}");
        assert!(dj.contains("\"tick_ms\":1"), "{dj}");

        let oj = serde_json::to_string(&override_rec()).expect("serialize override");
        assert!(oj.contains("\"reason\":\"envelope_clamp\""), "{oj}");

        let fj = serde_json::to_string(&fault()).expect("serialize fault");
        assert!(fj.contains("\"code\":\"nonfinite_command\""), "{fj}");
        assert!(fj.contains("\"detail\":\"linear was NaN\""), "{fj}");

        let hj = serde_json::to_string(&health()).expect("serialize health");
        assert!(hj.contains("\"inference_latency_ms\":12"), "{hj}");
    }

    #[test]
    fn audit_client_is_object_safe_and_thread_shareable() {
        // Compile-time: AuditClient must be Send + Sync (shared across the
        // inference loop threads) and object-safe (used as `dyn`).
        fn assert_send_sync<T: Send + Sync + ?Sized>() {}
        assert_send_sync::<dyn AuditClient>();
        assert_send_sync::<MockAuditClient>();
        assert_send_sync::<NoopAuditClient>();
    }
}
