//! Parko Core
//!
//! Vendor-neutral robotics inference runtime core.
//!
//! This crate defines the foundational traits, tensor abstractions,
//! telemetry structures, and scheduling primitives used by all Parko
//! inference backends.
//!
//! Parko-core does not include any backend implementations.
//! Backends such as parko-onnx, parko-qnn, parko-eiq, etc.
//! depend on this crate and implement the `InferenceBackend` trait.

pub mod backend;
pub mod backends;
pub mod clock;
pub mod commands;
// SG5 — map-anchored COMMIT_ZONE_BLOCKED veto foundation (#106).
pub mod commit_zone;
pub mod control_loop;
// SG6 — post-collision impact latch (IMU/contact/vanished fusion, #102).
pub mod impact;
// SG2/SG5 — localization-integrity gate over the map-anchored checks (#123).
pub mod localization;
pub mod rss;
pub mod runtime;
// SG4 — WATER_UNTRAVERSABLE governor veto (depth-free, bounded-worst-case, #98).
pub mod water;
pub mod safety;
pub mod scheduler;
pub mod sensor;
pub mod telemetry;

pub use backend::{
    BackendCapabilities,
    BackendDescriptor,
    BackendError,
    InferenceBackend,
    InferenceThreads,
    ModelHandle,
    PrecisionMode,
    TensorBatch,
    TensorStorage,
};

pub use backends::mock::MockBackend;
#[cfg(feature = "backend-tensorrt")]
pub use backends::TensorRTStubBackend;
#[cfg(feature = "backend-qnn")]
pub use backends::QnnStubBackend;
#[cfg(feature = "backend-tidl")]
pub use backends::TidlStubBackend;
#[cfg(feature = "backend-openvino")]
pub use backends::OpenVinoStubBackend;
#[cfg(feature = "backend-amd")]
pub use backends::AmdStubBackend;
pub use clock::{Clock, MockClock, WallClock};
pub use commands::ControlCommand;
pub use control_loop::ControlLoop;
pub use runtime::{RuntimeClock, RuntimeState, TickStatus};
pub use rss::{
    lateral_safe_distance, longitudinal_safe_distance, occlusion_limited_speed, AgentScene,
    OcclusionScene, RssAgent, RssParams, RssState, MAX_RSS_AGENTS,
};
pub use commit_zone::{
    commit_zone_blocked, exit_clearance_verified, non_yielding_clearance, CommitZoneCfg,
    CommitZoneMap, CommitZoneScene, ExitClearanceEvidence, NonYieldingAgent, NonYieldingScene,
};
pub use impact::{
    is_impact, ClearanceLoop, ClearanceRejection, ClearanceState, ImpactCfg, ImpactEvidence,
    ImpactLatch, OperatorClearanceGrant, DEFAULT_MAX_GRANT_AGE_MS,
};
pub use localization::{
    gate_commit_zone_scene, gate_water_scene, localization_trusted, LocalizationCfg,
    LocalizationIntegrity,
};
pub use safety::{EnforcementAction, SafetyGovernor, SafetyPosture};
pub use water::{water_untraversable_veto, TraversalEvidence, WaterScene, WaterVetoConfig};
pub use scheduler::{DegradationThresholds, InferenceLoop};
pub use sensor::{SensorFrame, SensorStream};
pub use telemetry::{
    CumulativeJitterEvaluator,
    PostureSnapshot,
    RuntimeTelemetry,
    ThermalState,
};

