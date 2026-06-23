// src/gateway/containment.rs
//
// The SG2 drivable-space containment checker moved VERBATIM to the lean `kirra-core`
// crate (de-monolith Stage 4) so the certified governor surface (the ROS2 adapter, the
// planner) need not pull the verifier service's heavy tree. Re-exported here so every
// existing `crate::gateway::containment::*` (and `kirra_runtime_sdk::gateway::
// containment::*`) path keeps the SAME type — zero churn, zero logic change.
//
// The checker and its tests now live in `kirra_core::containment`. SG2 wiring is
// unchanged: the kirra-ros2 adapter slow loop still calls `validate_trajectory_containment`
// per accepted trajectory (see `docs/safety/TRACEABILITY_MATRIX.md`, SG2 = ENFORCED).
pub use kirra_core::containment::*;
