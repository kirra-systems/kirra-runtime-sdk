// src/kinematics_sim.rs
//
// The deterministic vehicle kinematic simulator moved VERBATIM to the lean `kirra-core`
// crate (de-monolith Stage 7a). Re-exported here so every existing
// `crate::kinematics_sim::*` (and `kirra_runtime_sdk::kinematics_sim::*`) path — the
// verifier service binary, the CARLA client, the adapter tests — keeps the SAME types.
// Pure forward-integration math over the kinematics contract; no async, no heavy deps.
pub use kirra_core::kinematics_sim::*;
