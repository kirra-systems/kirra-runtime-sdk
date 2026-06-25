// crates/kirra-ros2-adapter/src/lib.rs
//
// S131 Phase 1 — Option-B per-trajectory wiring adapter (skeleton).
//
// Module layout:
//   - `state`     — `AdaptorState`, `AcceptedTrajectory`, `TrajectoryVerdict`,
//                   `Pose`, `TrajectoryPoint`. The per-asset accepted-trajectory
//                   store. Always built; no ROS deps.
//   - `corridor`  — `CorridorSource` trait + `MockCorridorSource`. The seam
//                   the Phase 2 Lanelet2 impl plugs into. Always built; no
//                   ROS deps.
//   - `node`      — r2r-backed ROS 2 node skeleton with stubbed subscriptions
//                   and slow / fast loop task stubs. Gated behind the `ros2`
//                   feature so default builds don't pull r2r.
//
// Design tie-in: see `docs/safety/OCCY_131_OPTIONB_DESIGN.md`
// (KIRRA-OCCY-OPTIONB-001) for the architecture this skeleton instantiates.

pub mod config;
pub mod corridor;
pub mod geometry;
pub mod state;
pub mod validation;
// Multi-modal predictive-RSS mode producer (gap #3) — rolls live perceived objects into
// CV/CTRV `PredictedMode` hypotheses so the checker's multi-modal pass runs against real
// perception. Pure, non-ros2-gated; tested under default features.
pub mod prediction;
// Perception redundancy cross-check (True-Redundancy analog) — pure, non-ros2-gated.
pub mod perception_redundancy;
// KIRRA-OCCY-PMON-003 slice-1 — pure perception-ingest shim/orchestration
// (non-ros2-gated; safety logic tested under default features).
pub mod perception_ingest;

// `posture_tracker` was relocated to the kernel
// (`kirra_core::posture_tracker`) by M2b so the parko-ros2 node
// can share the SAME fail-closed state machine. Re-export here so
// existing consumers of `kirra_ros2_adapter::PostureTracker` keep
// working without a code change. Single implementation, two consumers.
pub use kirra_core::posture_tracker;

#[cfg(feature = "ros2")]
pub mod node;

#[cfg(feature = "ros2")]
pub mod parsing;

#[cfg(feature = "ros2")]
pub mod posture_source;

// Re-exports for downstream consumers (Phase 2 will be the verifier service
// binary; for now these are the public surface).
pub use crate::config::VehicleConfig;
pub use crate::corridor::{CorridorSource, MockCorridorSource, Point};
#[cfg(feature = "lanelet2")]
pub use crate::corridor::{Lanelet2CorridorSource, Lanelet2Error};
pub use crate::geometry::quat_to_yaw;
pub use kirra_core::posture_tracker::{PostureTracker, POSTURE_STALENESS_TIMEOUT_MS};
pub use crate::state::{
    AcceptedTrajectory, AdaptorState, IncomingTrajectory, PerceivedObject, Pose,
    TrajectoryPoint, TrajectoryVerdict,
};
pub use crate::validation::validate_trajectory_slow;
