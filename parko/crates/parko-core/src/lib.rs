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
pub mod control_loop;
pub mod rss;
pub mod runtime;
pub mod safety;
pub mod scheduler;
pub mod sensor;
pub mod telemetry;

pub use backend::{
    BackendCapabilities,
    BackendDescriptor,
    BackendError,
    InferenceBackend,
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
pub use rss::{lateral_safe_distance, longitudinal_safe_distance, RssState};
pub use safety::{EnforcementAction, SafetyGovernor, SafetyPosture};
pub use scheduler::{DegradationThresholds, InferenceLoop};
pub use sensor::{SensorFrame, SensorStream};
pub use telemetry::{
    CumulativeJitterEvaluator,
    PostureSnapshot,
    RuntimeTelemetry,
    ThermalState,
};

