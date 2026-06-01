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

#[cfg(feature = "ros2")]
pub mod node;

#[cfg(feature = "ros2")]
pub mod parsing;

// Re-exports for downstream consumers (Phase 2 will be the verifier service
// binary; for now these are the public surface).
pub use crate::config::VehicleConfig;
pub use crate::corridor::{CorridorSource, MockCorridorSource, Point};
#[cfg(feature = "ros2")]
pub use crate::corridor::{Lanelet2CorridorSource, Lanelet2Error};
pub use crate::geometry::quat_to_yaw;
pub use crate::state::{
    AcceptedTrajectory, AdaptorState, IncomingTrajectory, PerceivedObject, Pose,
    TrajectoryPoint, TrajectoryVerdict,
};
pub use crate::validation::validate_trajectory_slow;
