// crates/parko-core/src/control_loop.rs

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::backend::{InferenceBackend, ModelHandle};
use crate::commands::ControlCommand;
use crate::runtime::{RuntimeClock, RuntimeState};
use crate::scheduler::InferenceLoop;
use crate::sensor::SensorStream;
use crate::telemetry::PostureSnapshot;

/// Clock-driven control loop wrapping an InferenceLoop with a lifecycle
/// state machine.
///
/// This is one orchestration pattern over the primitives in parko-core.
/// Other consumers may prefer event-driven or externally-clocked patterns;
/// this is the canonical "pull a frame each tick, run inference, transition
/// state" loop suitable for demos and real-time control.
pub struct ControlLoop<B: InferenceBackend, S: SensorStream> {
    state: RuntimeState,
    inner: InferenceLoop<B>,
    sensor: S,
    clock: RuntimeClock,
}

impl<B, S> ControlLoop<B, S>
where
    B: InferenceBackend + 'static,
    S: SensorStream + 'static,
{
    pub fn new(
        backend: Arc<B>,
        model: ModelHandle,
        sensor: S,
        actuator_tx: mpsc::Sender<ControlCommand>,
        hz: f64,
    ) -> Self {
        let inner = InferenceLoop::new(backend, model, actuator_tx);
        Self {
            state: RuntimeState::Warmup,
            inner,
            sensor,
            clock: RuntimeClock::new(hz),
        }
    }

    pub fn state(&self) -> RuntimeState {
        self.state
    }

    /// Force the state machine to a specific state. Test-only — bypasses
    /// the normal transition logic. The "for_test" naming substitutes for
    /// compile-time gating; do not call this outside of test contexts.
    pub fn set_state_for_test(&mut self, state: RuntimeState) {
        self.state = state;
    }

    pub fn with_governor(
        mut self,
        governor: Box<dyn crate::safety::SafetyGovernor>,
    ) -> Self {
        self.inner = self.inner.with_governor(governor);
        self
    }

    pub async fn tick(&mut self) -> Result<PostureSnapshot, String> {
        let _tick_status = self.clock.wait_for_next_tick().await;

        let Some(current_frame) = self.sensor.next_frame() else {
            self.state = RuntimeState::EmergencyStop;
            return Err("sensor stream exhausted".to_string());
        };

        let safety_posture = match self.state {
            RuntimeState::Nominal => crate::safety::SafetyPosture::Nominal,
            RuntimeState::EmergencyStop => crate::safety::SafetyPosture::LockedOut,
            _ => crate::safety::SafetyPosture::Degraded,
        };
        let snapshot = self.inner.tick(current_frame, safety_posture).await?;

        self.state = next_state(self.state, snapshot.active_state_degraded);

        Ok(snapshot)
    }
}

/// Pure state-transition function — extracted for testability.
///
/// Note: Recovery is a single-tick hysteresis state. A real safety
/// integration would likely require N consecutive non-degraded ticks
/// before fully transitioning to Nominal.
fn next_state(current: RuntimeState, degraded: bool) -> RuntimeState {
    match current {
        RuntimeState::Initializing => RuntimeState::Warmup,
        RuntimeState::Warmup => {
            if degraded {
                RuntimeState::Warmup
            } else {
                RuntimeState::Nominal
            }
        }
        RuntimeState::Nominal => {
            if degraded {
                RuntimeState::Degraded
            } else {
                RuntimeState::Nominal
            }
        }
        RuntimeState::Degraded => {
            if degraded {
                RuntimeState::Degraded
            } else {
                RuntimeState::Recovery
            }
        }
        RuntimeState::Recovery => {
            if degraded {
                RuntimeState::Degraded
            } else {
                RuntimeState::Nominal
            }
        }
        // EmergencyStop is terminal; no transitions out.
        RuntimeState::EmergencyStop => RuntimeState::EmergencyStop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warmup_stays_warmup_while_degraded() {
        assert_eq!(next_state(RuntimeState::Warmup, true), RuntimeState::Warmup);
    }

    #[test]
    fn warmup_transitions_to_nominal_when_healthy() {
        assert_eq!(
            next_state(RuntimeState::Warmup, false),
            RuntimeState::Nominal
        );
    }

    #[test]
    fn nominal_transitions_to_degraded_when_degraded() {
        assert_eq!(
            next_state(RuntimeState::Nominal, true),
            RuntimeState::Degraded
        );
    }

    #[test]
    fn nominal_stays_nominal_when_healthy() {
        assert_eq!(
            next_state(RuntimeState::Nominal, false),
            RuntimeState::Nominal
        );
    }

    #[test]
    fn degraded_transitions_to_recovery_when_healthy() {
        assert_eq!(
            next_state(RuntimeState::Degraded, false),
            RuntimeState::Recovery
        );
    }

    #[test]
    fn degraded_stays_degraded_when_still_degraded() {
        assert_eq!(
            next_state(RuntimeState::Degraded, true),
            RuntimeState::Degraded
        );
    }

    #[test]
    fn recovery_transitions_to_nominal_when_confirmed_healthy() {
        assert_eq!(
            next_state(RuntimeState::Recovery, false),
            RuntimeState::Nominal
        );
    }

    #[test]
    fn recovery_returns_to_degraded_when_flapping() {
        assert_eq!(
            next_state(RuntimeState::Recovery, true),
            RuntimeState::Degraded
        );
    }

    #[test]
    fn emergency_stop_is_sticky() {
        assert_eq!(
            next_state(RuntimeState::EmergencyStop, false),
            RuntimeState::EmergencyStop
        );
        assert_eq!(
            next_state(RuntimeState::EmergencyStop, true),
            RuntimeState::EmergencyStop
        );
    }

    #[test]
    fn initializing_transitions_unconditionally_to_warmup() {
        assert_eq!(
            next_state(RuntimeState::Initializing, false),
            RuntimeState::Warmup
        );
        assert_eq!(
            next_state(RuntimeState::Initializing, true),
            RuntimeState::Warmup
        );
    }
}
