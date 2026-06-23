// src/posture_tracker.rs
//
// The fail-closed fleet-posture state machine (`PostureTracker` /
// `POSTURE_STALENESS_TIMEOUT_MS`) moved VERBATIM to the lean `kirra-core` crate
// (de-monolith Stage 5) so the certified governor surface (the ROS2 adapter, the
// parko-ros2 node) need not pull the verifier service's heavy tree. Re-exported here so
// every existing `crate::posture_tracker::*` (and `kirra_verifier::posture_tracker::*`)
// path keeps the SAME type. The tracker is unchanged: a pure, deterministic,
// clock-injected state machine whose only input is `FleetPosture`.
pub use kirra_core::posture_tracker::*;
