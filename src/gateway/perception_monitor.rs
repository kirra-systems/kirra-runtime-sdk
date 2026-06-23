// src/gateway/perception_monitor.rs
//
// The Track-C perception monitor (KIRRA-OCCY-PMON-001) moved VERBATIM to the lean
// `kirra-core` crate (de-monolith Stage 7) so the ROS2 adapter can call it without
// pulling the verifier service's heavy tree. Re-exported here so every existing
// `crate::gateway::perception_monitor::*` (and `kirra_runtime_sdk::gateway::
// perception_monitor::*`) path keeps the SAME type — zero churn, zero logic change.
//
// The monitor and its tests now live in `kirra_core::perception_monitor`. It is pure +
// dependency-light (std::sync + the kinematics-contract talisman + `PerceptionDerateEvent`),
// so it sits on the lean foundation alongside the contract it caps.
pub use kirra_core::perception_monitor::*;
