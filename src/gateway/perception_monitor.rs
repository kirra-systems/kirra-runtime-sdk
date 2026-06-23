// src/gateway/perception_monitor.rs
//
// The Track-C perception-derate monitor (KIRRA-OCCY-PMON-001) moved VERBATIM to the
// lean `kirra-core` crate (de-monolith Stage 7a) so the ROS2 adapter can consume it
// without the verifier service's heavy tree, and the QNX-cert governor surface stays
// lean. Re-exported here so every existing `crate::gateway::perception_monitor::*`
// (and `kirra_runtime_sdk::gateway::perception_monitor::*`) path — the policy layer,
// the fabric governor, posture_cache's `SharedPerceptionCap`, the wcet_gate tests —
// keeps the SAME types. Pure stateless guards + an stdlib `Arc<RwLock>` cache; no async.
pub use kirra_core::perception_monitor::*;
