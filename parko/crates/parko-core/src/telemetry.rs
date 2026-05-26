use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::backend::PrecisionMode;
use crate::commands::ControlCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThermalState {
    Normal,
    Warning,
    Critical,
}

/// Cumulative running variance using Welford's online algorithm.
///
/// Tracks the mean and standard deviation of all samples seen since
/// construction. Memory: O(1). Time: O(1) per update.
///
/// Note: this is *cumulative*, not rolling. Old samples are never forgotten,
/// so long runtimes will have a stddev that responds slowly to changes in
/// the input distribution. For true rolling behavior (responsive to recent
/// behavior, ignoring distant history), replace with an exponentially
/// weighted moving variance.
#[derive(Debug, Clone)]
pub struct CumulativeJitterEvaluator {
    count: u64,
    mean_ms: f64,
    m2_ms: f64,
}

impl CumulativeJitterEvaluator {
    pub fn new() -> Self {
        Self {
            count: 0,
            mean_ms: 0.0,
            m2_ms: 0.0,
        }
    }

    pub fn update(&mut self, latency_ms: u64) {
        self.count += 1;
        let value = latency_ms as f64;
        let delta = value - self.mean_ms;
        self.mean_ms += delta / self.count as f64;
        let delta2 = value - self.mean_ms;
        self.m2_ms += delta * delta2;
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn mean_ms(&self) -> f64 {
        self.mean_ms
    }

    /// Sample standard deviation (Bessel-corrected, denominator n-1).
    /// Returns 0.0 when fewer than two samples have been observed.
    pub fn std_dev_ms(&self) -> f64 {
        if self.count < 2 {
            0.0
        } else {
            (self.m2_ms / (self.count - 1) as f64).sqrt()
        }
    }
}

impl Default for CumulativeJitterEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeTelemetry {
    pub inference_latency_ms: u64,
    pub rolling_jitter_ms: f64,
    pub dropped_frames: u64,
    pub thermal_state: ThermalState,
    pub frame_age_ms: u64,
    pub tensor_payload_bytes: usize,
    pub backend_precision: PrecisionMode,
    pub backend_vendor: Cow<'static, str>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostureSnapshot {
    pub frame_id: u64,
    pub active_command: ControlCommand,
    pub telemetry: RuntimeTelemetry,
    /// True if any degradation condition was active this tick.
    /// Reason codes will replace this bool when degradation logic matures.
    pub active_state_degraded: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_samples_returns_zero_std_dev() {
        let tracker = CumulativeJitterEvaluator::new();
        assert_eq!(tracker.std_dev_ms(), 0.0);
        assert_eq!(tracker.count(), 0);
    }

    #[test]
    fn single_sample_returns_zero_std_dev() {
        let mut tracker = CumulativeJitterEvaluator::new();
        tracker.update(42);
        assert_eq!(tracker.std_dev_ms(), 0.0);
        assert_eq!(tracker.mean_ms(), 42.0);
    }

    #[test]
    fn identical_samples_return_zero_std_dev() {
        let mut tracker = CumulativeJitterEvaluator::new();
        for _ in 0..10 {
            tracker.update(50);
        }
        assert_eq!(tracker.mean_ms(), 50.0);
        assert!(tracker.std_dev_ms() < 1e-9);
    }

    #[test]
    fn two_samples_match_manual_calculation() {
        let mut tracker = CumulativeJitterEvaluator::new();
        tracker.update(10);
        tracker.update(20);
        assert_eq!(tracker.mean_ms(), 15.0);
        assert!((tracker.std_dev_ms() - 50.0_f64.sqrt()).abs() < 1e-9);
    }

    #[test]
    fn four_sample_sequence_matches_offline_calculation() {
        let mut tracker = CumulativeJitterEvaluator::new();
        for v in [10, 12, 30, 11] {
            tracker.update(v);
        }
        assert!((tracker.mean_ms() - 15.75).abs() < 1e-9);
        assert!((tracker.std_dev_ms() - 9.535).abs() < 0.01);
    }
}
