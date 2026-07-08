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

// L5 — SDK-free audit seam for the ML decision path (AuditClient + Noop/Mock).
pub mod audit;
pub mod backend;
// PARK-022 — descriptor → backend resolution (factory registry + stub fallback).
pub mod backend_selector;
pub mod backends;
pub mod clock;
pub mod commands;
// SG5 — map-anchored COMMIT_ZONE_BLOCKED veto foundation (#106).
pub mod commit_zone;
pub mod control_loop;
// Object detector path: sensor → backend → decode (NMS) → detections (#2a / P2).
pub mod detector;
// SG6 — post-collision impact latch (IMU/contact/vanished fusion, #102).
pub mod impact;
// SG2/SG5 — localization-integrity gate over the map-anchored checks (#123).
pub mod localization;
// #G16 — model-integrity allow-list: SHA-256 the model file and reject a
// substituted artifact against the operator allow-list before it can run.
pub mod model_integrity;
// WP-24 (G-15 software half, part a) — model lineage: rollback-to-last-good over
// the integrity verdict (deny-by-default when there is no known-good artifact).
pub mod model_lineage;
// WP-24 (G-15 software half, part b) — OOD / input-shift monitor: confidence-
// distribution drift vs a calibration baseline → an escalation-only posture
// recommendation (derate-only, fail-closed).
pub mod ood;
pub mod rss;
pub mod runtime;
// SG4 — WATER_UNTRAVERSABLE governor veto (depth-free, bounded-worst-case, #98).
pub mod water;
pub mod safety;
pub mod scheduler;
pub mod sensor;
pub mod telemetry;
// Q-0 (doer chipset tuning): the performance contract + eval harness — the
// pass/fail measuring stick for per-silicon inference (parko/QUANTIZATION_DESIGN.md).
pub mod perf_contract;
// Q-2: precision-aware selection — the evidence-derived precision ladder walked
// by operational proof, degrading visibly (parko/QUANTIZATION_DESIGN.md §5).
pub mod precision;

pub use audit::{
    AuditClient, DecisionRecord, FaultRecord, HealthRecord, MockAuditClient, NoopAuditClient,
    OverrideRecord,
};

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

pub use model_integrity::{
    sha256_file, verify_model_file, ModelAllowList, VerifiedModel, MODEL_ALLOWLIST_ENV,
    MODEL_ALLOWLIST_STRICT_ENV,
};

pub use model_lineage::{LineageDecision, ModelLineage};

pub use ood::{
    CalibrationBaseline, OodAssessment, OodError, OodMonitor, OodReason, DEFAULT_FAULT_PSI,
    DEFAULT_MIN_WINDOW, DEFAULT_WARN_PSI, MIN_CALIBRATION_SAMPLES,
};

pub use backend_selector::{
    backend_permitted, current_platform, descriptor_from_env_str, register_backend_factory,
    BackendFactory, BackendSelector, KIRRA_BACKEND_ENV, TargetPlatform,
};

pub use perf_contract::{
    evaluate, run_latency, ContractFailure, ContractVerdict, EvalRow, LatencyStats, PerfContract,
};

pub use precision::{
    select_by_ladder, LadderSelection, PrecisionLadder, KIRRA_PRECISION_LADDER_ENV,
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
    impact_cfg_for_class, is_impact, ClearanceLoop, ClearanceRejection, ClearanceState, ImpactCfg,
    ImpactEvidence, ImpactLatch, OperatorClearanceGrant, VanishedCfg, VanishedObjectDetector,
    VehicleClass, DEFAULT_MAX_GRANT_AGE_MS,
};
pub use localization::{gate_commit_zone_scene, gate_water_scene};
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

