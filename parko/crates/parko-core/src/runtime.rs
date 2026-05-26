use std::time::{Duration, Instant};

/// Lifecycle states of the runtime loop.
///
/// Variants are flat (no reason codes) for now. When degradation logic
/// matures, consider attaching a `DegradationReason` to `Degraded` and an
/// `EmergencyReason` to `EmergencyStop`, mirroring Aegis's reason-coded
/// fleet posture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeState {
    Initializing,
    Warmup,
    Nominal,
    Degraded,
    Recovery,
    EmergencyStop,
}

/// Outcome of a single tick wait.
#[derive(Debug, Clone, Copy)]
pub enum TickStatus {
    /// Slept until the next scheduled tick.
    OnSchedule,
    /// The previous tick body ran past its deadline; this tick is late.
    /// `by` is how long past the deadline we are.
    Overrun { by: Duration },
}

/// A fixed-frequency tick driver using monotonic time.
///
/// Maintains an absolute schedule — each tick targets a grid point computed
/// from the original start time, not relative to the most recent wakeup.
/// This prevents drift accumulation when individual ticks overrun.
///
/// Note: backed by `tokio::time::sleep`, which has millisecond-class
/// precision on Linux and no hard-real-time guarantees. Suitable for
/// control-loop rates up to ~100Hz on a quiet system. For sub-millisecond
/// timing, replace with `clock_nanosleep` and `SCHED_FIFO`.
pub struct RuntimeClock {
    next_tick: Instant,
    target_period: Duration,
}

impl RuntimeClock {
    /// Construct a new clock targeting `hz` ticks per second.
    ///
    /// Panics if `hz` is not finite or not positive.
    pub fn new(hz: f64) -> Self {
        assert!(
            hz.is_finite() && hz > 0.0,
            "RuntimeClock requires positive finite hz, got {}",
            hz
        );
        Self {
            next_tick: Instant::now(),
            target_period: Duration::from_secs_f64(1.0 / hz),
        }
    }

    pub fn target_period(&self) -> Duration {
        self.target_period
    }

    /// Sleep until the next scheduled tick.
    ///
    /// Returns `OnSchedule` if we slept (i.e., the previous tick body finished
    /// on time). Returns `Overrun { by }` if the previous tick overran its
    /// deadline; in that case no sleep occurs and the call returns immediately.
    /// The next tick targets the original grid, not "now + period."
    pub async fn wait_for_next_tick(&mut self) -> TickStatus {
        self.next_tick += self.target_period;
        let now = Instant::now();
        if self.next_tick > now {
            tokio::time::sleep(self.next_tick - now).await;
            TickStatus::OnSchedule
        } else {
            TickStatus::Overrun {
                by: now - self.next_tick,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "positive finite hz")]
    fn new_panics_on_zero_hz() {
        let _ = RuntimeClock::new(0.0);
    }

    #[test]
    #[should_panic(expected = "positive finite hz")]
    fn new_panics_on_nan_hz() {
        let _ = RuntimeClock::new(f64::NAN);
    }

    #[test]
    #[should_panic(expected = "positive finite hz")]
    fn new_panics_on_negative_hz() {
        let _ = RuntimeClock::new(-10.0);
    }

    #[test]
    fn target_period_matches_hz() {
        let clock = RuntimeClock::new(100.0);
        assert_eq!(clock.target_period(), Duration::from_millis(10));
    }
}
