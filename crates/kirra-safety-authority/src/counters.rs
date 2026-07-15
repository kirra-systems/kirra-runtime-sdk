//! ADR-0035 Stage 3 (slice 3e) — the OFF-verdict-path write-failure / drop
//! counters, lifted VERBATIM off the `AppState` god-object into the safety-authority
//! crate (per-field semantics preserved on each field below).
//!
//! These five counters are one cohesive thing: operator-observable counts of
//! OFF-verdict-path write failures or drops that MUST be 0 in a healthy deployment
//! and NEVER gate the verdict path (a durability/observability fault must never
//! suppress a safety decision). Each field is `Arc<AtomicU64>` interior-mutable
//! (shared-ref access only — no `&mut self`), so `AppState` embeds this as one
//! field and callers reach a counter as `app.off_path_writes.<field>` (the
//! field-façade step of the decomposition, exactly like `EscalationState` in 3c).
//! std-only — no new crate dependency.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;

/// The off-verdict-path write-failure / drop observability counters (ADR-0035
/// slice 3e). Every field MUST be 0 in a healthy deployment and NONE ever gates
/// the verdict path.
pub struct OffPathWriteCounters {
    /// #104 — operator-observable count of post-incident audit writes that were
    /// detected but could not be durably recorded (#245/#247 pattern). MUST be 0
    /// in a healthy deployment; never gates the verdict path.
    pub post_incident_write_failures: Arc<AtomicU64>,
    /// WS-0.3 / #772 F3 — operator-observable count of INCIDENT-CLASS posture
    /// transitions whose hard-power-loss-durable (FULL-connection) write failed
    /// and fell back to the checkpoint-bounded NORMAL write. The row IS in the
    /// chain (durable to the next checkpoint), only its at-write-time power-loss
    /// durability was degraded — DISTINCT from `post_incident_write_failures`
    /// (row MISSING from the chain). MUST be 0 in a healthy deployment; never
    /// gates the verdict path (a durability fault must not suppress an escalation).
    pub incident_durability_failures: Arc<AtomicU64>,
    /// #112 — operator-observable count of command-source handoff audit writes
    /// that were detected but could not be durably recorded (#245/#247 pattern).
    /// MUST be 0 in a healthy deployment; never gates the verdict path.
    pub command_source_write_failures: Arc<AtomicU64>,
    /// A3 — operator-observable count of kinematic-DenyBreach AUDIT records that
    /// were dropped because the bounded audit-writer channel was Full/Closed
    /// (drop-on-full, INV-4: safety never waits). Drops were previously LOGGED
    /// only; this counter makes the loss-rate observable (a non-zero / rising
    /// value = the audit chain has sequence gaps — under-provisioned channel or a
    /// dead writer). Never gates the verdict path.
    pub audit_write_drops: Arc<AtomicU64>,
    /// A3 — operator-observable count of learning-capture verdict records dropped
    /// on a Full/Closed capture channel. Non-safety (capture is an off-verdict-path
    /// side channel); surfaced so an integrator can see how much training data the
    /// channel sizing is shedding.
    pub capture_drops: Arc<AtomicU64>,
}

impl OffPathWriteCounters {
    /// All counters at 0 — the exact defaults the prior `AppState::new` set inline
    /// (byte-identical initial state).
    pub fn new() -> Self {
        Self {
            post_incident_write_failures: Arc::new(AtomicU64::new(0)),
            incident_durability_failures: Arc::new(AtomicU64::new(0)),
            command_source_write_failures: Arc::new(AtomicU64::new(0)),
            audit_write_drops: Arc::new(AtomicU64::new(0)),
            capture_drops: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl Default for OffPathWriteCounters {
    fn default() -> Self {
        Self::new()
    }
}
