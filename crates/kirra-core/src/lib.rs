//! **kirra-core** — the lean safety/contract foundation of the Kirra stack.
//!
//! This crate carries the dependency-light types and (in later de-monolith stages) the
//! governor / kinematics-contract talisman, extracted from `kirra-runtime-sdk` so that
//! the certified-governor surface does not pull the verifier service's heavy tree
//! (tokio / axum / rusqlite / reqwest) or any ML backend. Consumers that only need the
//! foundation (the ROS2 adapter, the planner, the lane map, parko) depend on this crate
//! directly; `kirra-runtime-sdk` re-exports everything here so existing paths are
//! unchanged.
//!
//! Stage 1: the fleet posture / node trust types, previously defined in the heavy
//! `kirra_runtime_sdk::verifier` module and imported across the whole stack.

use serde::{Deserialize, Serialize};

/// A registered node's trust state, as decided by attestation and the recovery
/// hysteresis. `Untrusted` carries a human-readable reason tag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NodeTrustState {
    Trusted,
    Untrusted(String),
    Unknown,
}

/// The fleet's safety posture — the spine the whole governor hangs on. `Nominal` →
/// full operation; `Degraded` → controlled decel-to-stop-and-hold envelope; `LockedOut`
/// → MRC, human reset required.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FleetPosture {
    Nominal,
    Degraded,
    LockedOut,
}
