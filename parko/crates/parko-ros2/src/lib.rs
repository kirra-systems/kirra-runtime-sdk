// parko/crates/parko-ros2/src/lib.rs
//
// M2 — Parko ROS 2 node crate root. Two-lane layout, mirroring the
// kirra-ros2-adapter convention:
//
//   - `config`, `command_mapping`, `sensor_mapping` — pure (no ROS, no
//     async, no I/O). Unit-testable on stable. These are the seams
//     the integrator overrides per platform.
//   - `tick_pipeline`   — the heart of the loop: drive a configured
//     `InferenceLoop` one step with a given `SensorFrame` + posture,
//     receive the post-governor `ControlCommand`, map to an
//     `OutgoingTwist` via `command_mapping`. Async but
//     transport-independent: tests exercise this via parko-core's
//     `MockBackend` without touching r2r.
//   - `node`           — r2r-backed adapter task: subscribes to the
//     configured sensor topic, drives the tick pipeline, publishes
//     `OutgoingTwist` to the actuator topic. Feature-gated on `ros2`.
//
// Design tie-in: `docs/safety/PARKO_OCCY_TOPOLOGY.md`
// (KIRRA-OCCY-TOPOLOGY-001) — the parallel-paths L1 decision Parko +
// Occy run side by side, sharing safety primitives, never chained.

// clippy doc-list lints allowed: `command_mapping.rs` documents the Twist 2D
// subset as an aligned list the markdown-nesting lint would reformat.
#![allow(clippy::doc_lazy_continuation, clippy::doc_overindented_list_items)]

pub mod backend_select;
pub mod clearance_gate;
pub mod command_mapping;
pub mod comparator_adapter;
pub mod config;
pub mod containment_gate;
pub mod image_shim;
pub mod imu_shim;
pub mod odometry_shim;
pub mod ood_feed;
pub mod platform_profile;
pub mod pointcloud2_shim;
pub mod posture_state;
pub mod radar_shim;
pub mod scene_vetoes;
pub mod sensor_mapping;
pub mod taj_corridor;
pub mod taj_objects;
pub mod tick_pipeline;

// Re-export the PostureTracker so parko-ros2 consumers can refer to
// `parko_ros2::PostureTracker` directly — single implementation, shared with
// kirra-ros2-adapter via the lean `kirra-core` crate (de-monolith Stage 5).
pub use crate::posture_state::{fleet_to_safety, ParkoPostureState};
pub use kirra_core::posture_tracker::{PostureTracker, POSTURE_STALENESS_TIMEOUT_MS};

#[cfg(feature = "ros2")]
pub mod node;

pub use crate::clearance_gate::{
    run_pipeline_tick_with_clearance, ClearedTickOutcome, NodeClearance,
};
pub use crate::command_mapping::{enforce_outgoing_twist, OutgoingTwist};
pub use crate::comparator_adapter::ComparatorAsGovernor;
pub use crate::config::ParkoNodeConfig;
pub use crate::containment_gate::{
    apply_containment_gate, command_stays_in_corridor, command_stays_in_corridor_default,
    CONTAINMENT_HORIZON_S, CONTAINMENT_STEP_S,
};
pub use crate::ood_feed::{
    load_baseline, ood_feed_from_env, ood_feed_from_env_values, OodFeed, OodFeedError,
    KIRRA_OOD_BASELINE_PATH_ENV, KIRRA_OOD_ENABLED_ENV,
};
pub use crate::platform_profile::CourierPlatformProfile;
pub use crate::scene_vetoes::{
    apply_commit_zone_gate, apply_occlusion_gate, apply_water_gate, StampedScene,
};
pub use crate::sensor_mapping::{
    CameraConfig, CameraEncoding, CameraLayout, CameraMapping, CameraMappingError,
    CameraNormalization, CameraResize, CameraSample, OdomConfig, OdomMapping, OdomMappingError,
    OdomOrientation, OdomSample, OwnedCameraSample, SensorInputMapping,
};
pub use crate::taj_corridor::{
    corridor_from_scan, taj_scan_from_raw, CorridorSnapshot, LaserScanRawFields, EGO_REAR_COVER_M,
};
pub use crate::tick_pipeline::{
    run_pipeline_tick, run_pipeline_tick_with_ood, TickError, TickOutcome,
};
