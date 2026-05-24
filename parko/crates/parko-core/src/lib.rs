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
pub mod commands;
pub mod control_loop;
pub mod runtime;
pub mod scheduler;
pub mod sensor;
pub mod telemetry;

pub use backend::{
    BackendCapabilities,
    BackendError,
    InferenceBackend,
    ModelHandle,
    PrecisionMode,
    TensorBatch,
    TensorStorage,
};

pub use commands::ControlCommand;
pub use control_loop::ControlLoop;
pub use runtime::{RuntimeClock, RuntimeState, TickStatus};
pub use scheduler::{DegradationThresholds, InferenceLoop};
pub use sensor::{SensorFrame, SensorStream};
pub use telemetry::{
    CumulativeJitterEvaluator,
    PostureSnapshot,
    RuntimeTelemetry,
    ThermalState,
};
