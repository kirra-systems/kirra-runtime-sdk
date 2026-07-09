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

// Mirrors the root crate's decision: this lint fires on intentionally
// column-aligned ASCII derivation tables in safety doc-comments (e.g. the
// per-class kinematic-budget tables in `config.rs`), which are read as evidence —
// the alignment wins over the markdown-nesting pedantry.
#![allow(clippy::doc_lazy_continuation)]

mod control_ingress;
// L3.2 — the guest-side producer: map the fast-loop ingress command to the
// frozen Clause-2 payload and publish it over a ContractWriter. Non-ros2-gated
// so the producer contract is host-tested over InProcessRegion; the node.rs
// call-site + hypervisor-writer binding are L3.3.
mod contract_producer;
pub mod corridor;
pub mod geometry;
pub mod state;
// KIRRA-OCCY-PMON-003 slice-1 — pure perception-ingest shim/orchestration
// (non-ros2-gated; safety logic tested under default features).
pub mod perception_ingest;

// R1: the trajectory CHECKER + its contract config/prediction/redundancy were
// extracted to the lean `kirra-trajectory` crate. Re-export the modules here so
// every existing `kirra_ros2_adapter::{config,validation,prediction,
// perception_redundancy}` path AND the ros2/lanelet2 node code's
// `crate::{config,validation,prediction,perception_redundancy}::*` imports resolve
// unchanged. `state` stays local (it owns the ROS runtime `AdaptorState`) but
// re-exports the relocated `AcceptedTrajectory` / `EgoOdom` contract types.
pub use kirra_trajectory::{config, perception_redundancy, prediction, validation};

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
pub use crate::state::{
    AcceptedTrajectory, AdaptorState, IncomingTrajectory, PerceivedObject, Pose, TrajectoryPoint,
    TrajectoryVerdict,
};
pub use crate::validation::validate_trajectory_slow;
pub use kirra_core::posture_tracker::{PostureTracker, POSTURE_STALENESS_TIMEOUT_MS};
