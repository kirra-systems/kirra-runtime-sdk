// crates/kirra-trajectory/src/lib.rs
//
// R1 — the lean trajectory CHECKER, extracted from `kirra-ros2-adapter`.
//
// The doer side (planner / perception / examples) and host tests depend on this
// crate for the *contract + verdict* (`validate_trajectory_slow`, `VehicleConfig`,
// the trajectory/corridor types) WITHOUT pulling the ROS 2 integration crate.
// `kirra-ros2-adapter` re-exports these modules so its ros2/lanelet2 node code and
// every `kirra_ros2_adapter::{validation,prediction,perception_redundancy,config,state}`
// path keep resolving unchanged.

// Mirrors the adapter's lint posture: column-aligned ASCII derivation tables in the
// safety doc-comments (notably the per-class kinematic-budget tables in `config.rs`)
// read as evidence — the alignment wins over the markdown-nesting pedantry.
#![allow(clippy::doc_lazy_continuation)]

pub mod config;
pub mod corridor;
pub mod frenet;
pub mod state;
pub mod validation;
// WS-2 — pedestrian / VRU RSS primitive (KIRRA-VRU-RSS-001).
pub mod vru;
// Multi-modal predictive-RSS mode producer (gap #3) — rolls live perceived objects
// into CV/CTRV `PredictedMode` hypotheses so the checker's multi-modal pass runs
// against real perception. Pure, no ROS.
pub mod prediction;
// Perception redundancy cross-check (True-Redundancy analog) — pure, no ROS.
pub mod perception_redundancy;

// Phase 2A Adversarial Review Hardening
pub mod redundancy_hardening;
pub mod validation_hardening;

// Public surface — the symbols downstream consumers import directly. Kept identical
// to the adapter's prior re-exports so a consumer switch is a pure path rename.
pub use crate::config::VehicleConfig;
pub use crate::corridor::{CorridorSource, MockCorridorSource, Point};
pub use crate::state::{
    AcceptedTrajectory, EgoOdom, PerceivedObject, Pose, TrajectoryPoint, TrajectoryVerdict,
};
pub use crate::validation::validate_trajectory_slow;
