//! Kirra **safety-authority** core (ADR-0035 Stage 3, slice 3a — the leading edge).
//!
//! This crate is the first extraction toward a `SafetyAuthority` aggregate that
//! will eventually own the posture engine, the gray/black DAG traversal, the
//! actuator gate, and attestation — lifted off the `AppState` god-object
//! (referenced across ~42 files). Per the Stage-3 risk register, the DAG and
//! actuator core move LAST, behind the full Kani/loom/power-loss/replay net; this
//! slice moves only the **pure, `AppState`-free safety-DECISION surface** so the
//! crate / shim / MSRV / guardrail mechanics are proven first:
//!
//! - [`FleetNodePosture`] — the per-node posture record the DAG emits (3a);
//! - [`derive_fleet_posture`] — the pure fleet-fold (LockedOut > Degraded > Nominal) (3a);
//! - [`RssRecoveryStreak`] — the in-memory RSS recovery-streak counter (3a);
//! - [`LockoutReason`] — the structured fail-closed reason codes (3a);
//! - [`attestation`] — issue #73's per-node Ed25519 challenge-response + PCR16
//!   measured-boot binding (INVARIANT #3's real crypto) (3b);
//! - [`EscalationState`] — the fleet-escalation / hysteresis flags + streaks the
//!   posture engine / supervisor / watchdog read, embedded on `AppState` as
//!   `app.escalation` (3c — the first `AppState` FIELD migration).
//!
//! `kirra-verifier` re-exports the pure-decision items and the attestation module
//! from their original module paths (a `pub use` shim) and embeds `EscalationState`
//! as one `AppState` field, so consumers reach a flag as `app.escalation.<field>`.

use std::sync::Arc;

use kirra_core::{FleetPosture, NodeTrustState};
use serde::{Deserialize, Serialize};

// ADR-0035 Stage 3 (slice 3b): node-attestation proof verification (issue #73 —
// INVARIANT #3's per-node Ed25519 challenge-response + PCR16 measured-boot binding).
// Pure crypto, no AppState coupling; the root re-exports it via a `crate::attestation`
// shim. Its own test suite (moved with it) is the behaviour-preservation proof.
pub mod attestation;

/// One node's contribution to the fleet posture: its local trust state, the
/// posture propagated to it through the dependency DAG, and the interned ids of
/// the dependencies that blocked it (if any).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetNodePosture {
    /// Interned node id (review P5). One `Arc<str>` per distinct id is minted
    /// per whole-fleet recalc (in `recursive_calculate`) and shared by
    /// `Arc::clone` into the gray set, the `black` memo key, this field, and
    /// every parent's `blocked_by` — so a node depended on by K others costs one
    /// id allocation, not K+. Serializes/compares exactly like the prior
    /// `String` (it derefs to `str`).
    pub node_id: Arc<str>,
    pub local_status: NodeTrustState,
    pub propagated_status: FleetPosture,
    /// Interned blocking-dependency ids — each is an `Arc::clone` of that dep's
    /// own `node_id`, not a fresh allocation (review P5).
    pub blocked_by: Vec<Arc<str>>,
}

/// In-memory streak counter for RSS recovery hysteresis.
/// Tracks consecutive safe RSS reports and the streak start timestamp.
pub struct RssRecoveryStreak {
    pub count: u32,
    pub start_ms: u64,
}

/// ADR-0035 Stage 3 (slice 3c) — the fleet-escalation / hysteresis state the
/// posture engine, supervisor, and telemetry watchdog read to escalate or force
/// posture, lifted verbatim off the `AppState` god-object into the safety-authority
/// crate. Every field is `Arc<…>` interior-mutable (shared-ref access only — no
/// `&mut self`), so `AppState` embeds this as one field and callers reach a flag as
/// `app.escalation.<field>` (the field-façade step of the decomposition).
///
/// Field semantics are UNCHANGED from their prior `AppState` definitions:
/// - `rss_active_violation` / `flood_condition_active` — Nominal→Degraded escalators;
/// - `supervisor_tripped` — sticky, forces LockedOut when a critical loop wedges;
/// - `rss_recovery_streak` — clears an active RSS violation;
/// - `frame_degraded_active` (escalator) / `frame_lockout_active` (sticky) +
///   `frame_recovery_streak` / `frame_untrusted_streak` (S-FI1d localization integrity);
/// - `divergence_degraded_active` (escalator) / `divergence_lockout_active` (sticky) +
///   `divergence_recovery_streak` (S-DG1 diverse-governor disagreement);
/// - `av_registry_dirty` — H-3 watchdog watched-node-list refresh flag.
pub struct EscalationState {
    pub rss_active_violation: Arc<std::sync::atomic::AtomicBool>,
    pub flood_condition_active: Arc<std::sync::atomic::AtomicBool>,
    pub supervisor_tripped: Arc<std::sync::atomic::AtomicBool>,
    pub rss_recovery_streak: Arc<std::sync::Mutex<RssRecoveryStreak>>,
    pub frame_degraded_active: Arc<std::sync::atomic::AtomicBool>,
    pub frame_lockout_active: Arc<std::sync::atomic::AtomicBool>,
    pub frame_recovery_streak: Arc<std::sync::Mutex<RssRecoveryStreak>>,
    pub frame_untrusted_streak: Arc<std::sync::Mutex<RssRecoveryStreak>>,
    pub divergence_degraded_active: Arc<std::sync::atomic::AtomicBool>,
    pub divergence_lockout_active: Arc<std::sync::atomic::AtomicBool>,
    pub divergence_recovery_streak: Arc<std::sync::Mutex<RssRecoveryStreak>>,
    pub av_registry_dirty: Arc<std::sync::atomic::AtomicBool>,
}

impl EscalationState {
    /// All flags cleared, all streaks at `(0, 0)` — the exact defaults the prior
    /// `AppState::new` set inline (byte-identical initial state).
    pub fn new() -> Self {
        use std::sync::atomic::AtomicBool;
        use std::sync::Mutex;
        let streak = || {
            Arc::new(Mutex::new(RssRecoveryStreak {
                count: 0,
                start_ms: 0,
            }))
        };
        Self {
            rss_active_violation: Arc::new(AtomicBool::new(false)),
            flood_condition_active: Arc::new(AtomicBool::new(false)),
            supervisor_tripped: Arc::new(AtomicBool::new(false)),
            rss_recovery_streak: streak(),
            frame_degraded_active: Arc::new(AtomicBool::new(false)),
            frame_lockout_active: Arc::new(AtomicBool::new(false)),
            frame_recovery_streak: streak(),
            frame_untrusted_streak: streak(),
            divergence_degraded_active: Arc::new(AtomicBool::new(false)),
            divergence_lockout_active: Arc::new(AtomicBool::new(false)),
            divergence_recovery_streak: streak(),
            av_registry_dirty: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Default for EscalationState {
    fn default() -> Self {
        Self::new()
    }
}

/// Folds per-node postures into the single fleet posture, fail-closed:
/// any `LockedOut` wins outright; else any `Degraded` yields `Degraded`;
/// else `Nominal`.
pub fn derive_fleet_posture(node_postures: &[FleetNodePosture]) -> FleetPosture {
    let mut any_degraded = false;
    for np in node_postures {
        match np.propagated_status {
            FleetPosture::LockedOut => return FleetPosture::LockedOut,
            FleetPosture::Degraded => any_degraded = true,
            FleetPosture::Nominal => {}
        }
    }
    if any_degraded {
        FleetPosture::Degraded
    } else {
        FleetPosture::Nominal
    }
}

/// Structured reason code for any fail-closed LockedOut condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LockoutReason {
    /// Gray/black DAG traversal produced LockedOut (cycle or depth exceeded).
    DagLockedOut,
    /// Posture cache entry has aged beyond POSTURE_CACHE_TTL_MS.
    PostureCacheStale,
    /// Posture cache contains None (cold start or operator reset).
    PostureCacheEmpty,
    /// Posture cache RwLock was poisoned. Requires process restart.
    PostureCachePoisoned,
    /// Posture engine failed to complete a recalculation cycle.
    PostureEngineFailure,
    /// Watchdog task determined a node's telemetry has timed out.
    WatchdogTimeout,
    /// An operator or administrative action explicitly locked out the fleet.
    ManualLockout,
    /// S-FI1d — frame/localization integrity was `Untrusted` for a sustained
    /// run (sensor failure / possible GNSS spoofing). Sticky human-reset lockout.
    FrameIntegrityUntrusted,
    /// S-DG1 — the two diverse governors sustained a significant disagreement
    /// (the parko comparator's own `escalated_to_lockout`). One of the two
    /// safety authorities is wrong and we cannot tell which — a genuine fault,
    /// not a transient. Sticky human-reset lockout.
    GovernorDivergence,
}

impl std::fmt::Display for LockoutReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = match self {
            Self::DagLockedOut => "DAG_LOCKED_OUT",
            Self::PostureCacheStale => "POSTURE_CACHE_STALE",
            Self::PostureCacheEmpty => "POSTURE_CACHE_EMPTY",
            Self::PostureCachePoisoned => "POSTURE_CACHE_POISONED",
            Self::PostureEngineFailure => "POSTURE_ENGINE_FAILURE",
            Self::WatchdogTimeout => "WATCHDOG_TIMEOUT",
            Self::ManualLockout => "MANUAL_LOCKOUT",
            Self::FrameIntegrityUntrusted => "FRAME_INTEGRITY_UNTRUSTED",
            Self::GovernorDivergence => "GOVERNOR_DIVERGENCE",
        };
        write!(f, "{code}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_fold_is_fail_closed_lockedout_wins() {
        let mk = |p: FleetPosture| FleetNodePosture {
            node_id: Arc::from("n"),
            local_status: NodeTrustState::Trusted,
            propagated_status: p,
            blocked_by: vec![],
        };
        assert_eq!(derive_fleet_posture(&[]), FleetPosture::Nominal);
        assert_eq!(
            derive_fleet_posture(&[mk(FleetPosture::Nominal), mk(FleetPosture::Degraded)]),
            FleetPosture::Degraded
        );
        assert_eq!(
            derive_fleet_posture(&[
                mk(FleetPosture::Degraded),
                mk(FleetPosture::LockedOut),
                mk(FleetPosture::Nominal)
            ]),
            FleetPosture::LockedOut,
            "any LockedOut wins outright"
        );
    }

    #[test]
    fn lockout_reason_codes_are_stable() {
        assert_eq!(LockoutReason::DagLockedOut.to_string(), "DAG_LOCKED_OUT");
        assert_eq!(
            LockoutReason::GovernorDivergence.to_string(),
            "GOVERNOR_DIVERGENCE"
        );
    }
}
