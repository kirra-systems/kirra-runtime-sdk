// src/kinematics_sim.rs
//
// The kinematic forward simulator moved VERBATIM to the lean `kirra-core` crate
// (de-monolith Stage 7) so the scenario harness and the ROS2 adapter's tests can use it
// without pulling the verifier service's heavy tree. Re-exported here so every existing
// `crate::kinematics_sim::*` (and `kirra_verifier::kinematics_sim::*`) path keeps the
// SAME type — zero churn, zero logic change.
//
// The simulator and its tests now live in `kirra_core::kinematics_sim`. It is pure
// deterministic physics over the kinematics-contract talisman — no service/runtime deps.
pub use kirra_core::kinematics_sim::*;
