// src/posture_cache.rs

use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use crate::verifier::{FleetNodePosture, FleetPosture, NodeTrustState};
use serde::{Deserialize, Serialize};

/// Maximum age of a cached posture entry before it is considered stale.
///
/// Rationale: the verifier polling loop refreshes the cache at 1 Hz (1000 ms period).
/// A 2000 ms TTL provides one full polling interval of tolerance for scheduling jitter
/// or transient CPU starvation before the gateway begins denying commands.
///
/// For environments where the actuated hardware has sub-second response requirements
/// (e.g., servo loops, hydraulic valves), reduce this to match the worst-case latency
/// budget — the TTL must be less than the time it takes for a physical state change
/// to become hazardous if undetected.
const CACHE_TTL_MS: u64 = 2_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedFleetPosture {
    pub node_id: String,
    pub local_status: NodeTrustState,
    pub propagated_status: FleetPosture,
    pub blocked_by: Vec<String>,
    pub updated_at_epoch_ms: u64,
}

impl CachedFleetPosture {
    pub fn from_posture(posture: &FleetNodePosture, now_ms: u64) -> Self {
        Self {
            node_id: posture.node_id.clone(),
            local_status: posture.local_status.clone(),
            propagated_status: posture.propagated_status.clone(),
            blocked_by: posture.blocked_by.clone(),
            updated_at_epoch_ms: now_ms,
        }
    }
}

pub type SharedPostureCache = Arc<RwLock<Option<CachedFleetPosture>>>;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Returns true only if the cached posture is fresh AND Nominal.
/// All other conditions — stale, missing, poisoned lock — deny routing.
pub fn should_route_sensitive_command(cache: &CachedFleetPosture, now_ms: u64) -> bool {
    let age_ms = now_ms.saturating_sub(cache.updated_at_epoch_ms);
    if age_ms > CACHE_TTL_MS { return false; }
    matches!(cache.propagated_status, FleetPosture::Nominal)
}

/// Fail-closed gateway check. Returns false on any form of uncertainty:
///   - RwLock poisoned (writer panicked)           → deny
///   - Cache not yet populated                     → deny
///   - Cache stale (> CACHE_TTL_MS)                → deny
///   - Posture is Degraded or LockedOut            → deny
pub fn should_route_from_cache(cache: &SharedPostureCache) -> bool {
    let now = now_ms();
    let guard = match cache.read() {
        Ok(g) => g,
        Err(_) => return false,
    };
    match guard.as_ref() {
        Some(posture) => should_route_sensitive_command(posture, now),
        None => false,
    }
}
