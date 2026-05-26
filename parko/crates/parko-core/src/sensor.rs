use std::sync::OnceLock;
use std::time::Instant;

use crate::backend::TensorBatch;

/// Anchor point for monotonic timestamps. Set lazily on first call.
static MONOTONIC_START: OnceLock<Instant> = OnceLock::new();

/// Milliseconds since program start, from a monotonic clock.
///
/// Monotonic — never goes backwards, immune to wall-clock adjustments.
/// Suitable for ordering events and computing elapsed time within a single
/// process. NOT suitable for comparing timestamps across process restarts
/// or for human-readable time display; add a separate SystemTime field
/// for those uses.
pub fn current_time_ms() -> u64 {
    let start = MONOTONIC_START.get_or_init(Instant::now);
    start.elapsed().as_millis() as u64
}

/// A single frame from a sensor stream.
///
/// The payload is always owned (`TensorBatch<'static>`), meaning the producer
/// allocates the tensor data. True zero-copy from sensor DMA would require
/// a lifetime parameter and is not supported here.
#[derive(Debug)]
pub struct SensorFrame {
    pub frame_id: u64,
    pub timestamp_ms: u64,
    pub payload: TensorBatch<'static>,
}

impl SensorFrame {
    pub fn new(frame_id: u64, payload: TensorBatch<'static>) -> Self {
        Self {
            frame_id,
            timestamp_ms: current_time_ms(),
            payload,
        }
    }
}

/// A source of sensor frames.
///
/// Note: `Send` but not `Sync`. Streams are pulled by a single consumer.
/// If multiple threads need access, wrap in a Mutex.
pub trait SensorStream: Send {
    /// Pull the next available frame, or `None` if no frame is ready.
    ///
    /// `None` does not distinguish "temporarily empty" from "permanently
    /// ended"; sensor implementations that need that distinction should
    /// expose a separate `is_finished()` method.
    fn next_frame(&mut self) -> Option<SensorFrame>;
}
