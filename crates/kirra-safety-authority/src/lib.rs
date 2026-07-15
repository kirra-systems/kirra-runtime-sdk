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
//! - [`FleetNodePosture`] — the per-node posture record the DAG emits;
//! - [`derive_fleet_posture`] — the pure fleet-fold (LockedOut > Degraded > Nominal);
//! - [`RssRecoveryStreak`] — the in-memory RSS recovery-streak counter;
//! - [`LockoutReason`] — the structured fail-closed reason codes.
//!
//! `kirra-verifier` re-exports every item from its original module path (a
//! `pub use` shim), so no consumer is touched by this extraction.

use std::sync::Arc;

use kirra_core::{FleetPosture, NodeTrustState};
use serde::{Deserialize, Serialize};

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
