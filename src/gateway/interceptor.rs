// src/gateway/interceptor.rs
//
// Re-exports the posture-cache and verifier types needed by policy_layer.
// All logic lives in posture_cache / verifier — this module is a stable
// import surface so that policy_layer doesn't need long cross-crate paths.

pub use crate::posture_cache::{
    CachedFleetPosture, SharedPostureCache, now_ms, should_route_command,
};
// From the lean foundation, not the heavy `verifier` module (Stage 2) — same types.
pub use kirra_core::{FleetPosture, NodeTrustState};
