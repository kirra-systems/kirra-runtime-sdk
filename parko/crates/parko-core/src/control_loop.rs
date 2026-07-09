// crates/parko-core/src/control_loop.rs

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::backend::{InferenceBackend, ModelHandle};
use crate::clock::{Clock, WallClock};
use crate::commands::ControlCommand;
use crate::runtime::RuntimeState;
use crate::scheduler::InferenceLoop;
use crate::sensor::SensorStream;
use crate::telemetry::PostureSnapshot;

/// Default number of consecutive non-degraded ticks required before the loop
/// recovers out of `Degraded` back to `Nominal`.
///
// SAFETY PARAMETER — default pending owner confirmation against the ODD's
// recovery-confirmation requirement (cf. Kirra SG-013 streak/window).
//
// (Note: the prior single-confirmation behavior — `Degraded -> Recovery ->
// Nominal` — was effectively a 2-tick threshold; this conservative default
// of 3 makes recovery strictly harder, never easier. The degrade path is
// unchanged: a single degraded tick still drops to `Degraded` immediately.)
pub const DEFAULT_RECOVERY_CONFIRM_TICKS: u32 = 3;

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
    /// Wall-clock abstraction. All timing reads use `clock.now_ms()` so
    /// tests can inject MockClock and advance time without sleeping (ADL-004).
    clock: Arc<dyn Clock>,
    tick_interval_ms: u64,
    /// `None` = never fired; `Some(t)` = last tick fired at wall-clock `t`.
    /// Stored as Option so the first tick always fires regardless of
    /// the clock's current value (including t=0 with MockClock).
    last_tick_ms: Option<u64>,
    /// Consecutive non-degraded ticks required to recover `Degraded -> Nominal`.
    /// SAFETY PARAMETER — see `DEFAULT_RECOVERY_CONFIRM_TICKS`.
    recovery_confirm_ticks: u32,
    /// Running count of consecutive non-degraded ticks. Reset to 0 on any
    /// degraded tick (the degrade path is immediate and unchanged); only the
    /// recovery transition consults it against `recovery_confirm_ticks`.
    recovery_streak: u32,
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
        assert!(
            hz.is_finite() && hz > 0.0,
            "ControlLoop hz must be positive finite, got {}",
            hz
        );
        let inner = InferenceLoop::new(backend, model, actuator_tx);
        Self {
            state: RuntimeState::Warmup,
            inner,
            sensor,
            clock: Arc::new(WallClock),
            tick_interval_ms: (1000.0 / hz).round() as u64,
            last_tick_ms: None,
            recovery_confirm_ticks: DEFAULT_RECOVERY_CONFIRM_TICKS,
            recovery_streak: 0,
        }
    }

    /// Override the recovery-confirmation threshold (consecutive non-degraded
    /// ticks required for `Degraded -> Nominal`). Tunable because the value is
    /// a SAFETY PARAMETER that must be set against the deployment ODD's
    /// recovery-confirmation requirement — see `DEFAULT_RECOVERY_CONFIRM_TICKS`.
    /// A threshold of 0 is treated as 1 (at least one good tick is always
    /// required to leave a degraded condition).
    pub fn with_recovery_confirm_ticks(mut self, ticks: u32) -> Self {
        self.recovery_confirm_ticks = ticks.max(1);
        self
    }

    /// Current recovery-confirmation threshold.
    pub fn recovery_confirm_ticks(&self) -> u32 {
        self.recovery_confirm_ticks
    }

    /// Override the clock. Primarily used in tests to inject a `MockClock`
    /// so tick-timing can be exercised without sleeping (ADL-004).
    pub fn with_clock(mut self, c: Arc<dyn Clock>) -> Self {
        self.clock = c;
        self
    }

    /// Override the tick interval in milliseconds.
    /// `#[cfg(test)]` — not compiled into release builds.
    #[cfg(test)]
    pub fn with_tick_interval_ms(mut self, ms: u64) -> Self {
        self.tick_interval_ms = ms;
        self
    }

    pub fn state(&self) -> RuntimeState {
        self.state
    }

    /// Force the state machine to a specific state; bypasses transition logic.
    /// Available under `cfg(test)` (unit tests) or the `test-helpers` feature
    /// (integration tests). Never compiled into release builds.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn set_state_for_test(&mut self, state: RuntimeState) {
        self.state = state;
    }

    /// Attach a KirraGovernor (or any `SafetyGovernor`) to this loop.
    /// The governor's decision takes precedence over the built-in degraded-mode
    /// clamp; both paths must not fire on the same tick (ADL-002).
    pub fn with_governor(mut self, governor: impl crate::safety::SafetyGovernor + 'static) -> Self {
        self.inner = self.inner.with_governor(governor);
        self
    }

    /// Drive one control tick.
    ///
    /// Returns `Ok(None)` when the tick interval has not yet elapsed since the
    /// last fired tick — callers should poll again later. Returns
    /// `Ok(Some(snapshot))` when a tick fires. Returns `Err` only on
    /// unrecoverable failures (e.g. sensor stream exhausted).
    ///
    /// The first call always fires (`last_tick_ms` starts at 0). All
    /// subsequent calls use `clock.now_ms()` for interval gating (ADL-004).
    pub async fn tick(&mut self) -> Result<Option<PostureSnapshot>, String> {
        let now = self.clock.now_ms();
        if let Some(last) = self.last_tick_ms {
            if now.saturating_sub(last) < self.tick_interval_ms {
                return Ok(None);
            }
        }
        self.last_tick_ms = Some(now);

        let Some(current_frame) = self.sensor.next_frame() else {
            self.state = RuntimeState::EmergencyStop;
            return Err("sensor stream exhausted".to_string());
        };

        let safety_posture = match self.state {
            RuntimeState::Nominal => crate::safety::SafetyPosture::Nominal,
            RuntimeState::EmergencyStop => crate::safety::SafetyPosture::LockedOut,
            _ => crate::safety::SafetyPosture::Degraded,
        };
        // Fail-closed: a propagated inner-tick error drives the runtime to
        // EmergencyStop BEFORE returning, matching the sensor-exhaustion path
        // above. This makes the runtime's own state reflect the safe condition
        // rather than relying on the caller honoring the "Err = unrecoverable"
        // contract.
        let snapshot = match self.inner.tick(current_frame, safety_posture).await {
            Ok(s) => s,
            Err(e) => {
                self.state = RuntimeState::EmergencyStop;
                return Err(e);
            }
        };

        // Recovery hysteresis: a degraded tick resets the streak immediately
        // (degrade path unchanged); a non-degraded tick advances it. The streak
        // gates only the `Degraded -> Nominal` recovery transition inside
        // `next_state`.
        let degraded = snapshot.active_state_degraded;
        if degraded {
            self.recovery_streak = 0;
        } else {
            self.recovery_streak = self.recovery_streak.saturating_add(1);
        }
        self.state = next_state(
            self.state,
            degraded,
            self.recovery_streak,
            self.recovery_confirm_ticks,
        );

        Ok(Some(snapshot))
    }
}

/// Pure state-transition function — extracted for testability.
///
/// Recovery hysteresis: leaving `Degraded` back to `Nominal` now requires
/// `recovery_confirm_ticks` consecutive non-degraded ticks. `streak` is the
/// caller-maintained count of consecutive non-degraded ticks INCLUDING the
/// current one (reset to 0 by the caller on any degraded tick). While the
/// streak is below the threshold the loop dwells in `Recovery` (the confirming
/// state); it promotes to `Nominal` only once `streak >= threshold`.
///
/// The degrade direction is unchanged and immediate: any degraded tick drops to
/// `Degraded` regardless of streak. `EmergencyStop` is terminal. Startup
/// (`Warmup -> Nominal`) is not gated — only the recovery transition is.
///
/// (A threshold of 2 reproduces the prior `Degraded -> Recovery -> Nominal`
/// two-tick behavior exactly; the default of 3 is strictly more conservative.)
fn next_state(
    current: RuntimeState,
    degraded: bool,
    streak: u32,
    recovery_confirm_ticks: u32,
) -> RuntimeState {
    let threshold = recovery_confirm_ticks.max(1);
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
            } else if streak >= threshold {
                // Single-tick threshold: recover directly.
                RuntimeState::Nominal
            } else {
                RuntimeState::Recovery
            }
        }
        RuntimeState::Recovery => {
            if degraded {
                RuntimeState::Degraded
            } else if streak >= threshold {
                RuntimeState::Nominal
            } else {
                // Still confirming — dwell in Recovery until the streak reaches
                // the threshold.
                RuntimeState::Recovery
            }
        }
        // EmergencyStop is terminal; no transitions out.
        RuntimeState::EmergencyStop => RuntimeState::EmergencyStop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Default-threshold (3) constant for these pure transition tests. `streak`
    // is "consecutive non-degraded ticks including the current one".
    const T: u32 = DEFAULT_RECOVERY_CONFIRM_TICKS;

    #[test]
    fn warmup_stays_warmup_while_degraded() {
        assert_eq!(
            next_state(RuntimeState::Warmup, true, 0, T),
            RuntimeState::Warmup
        );
    }

    #[test]
    fn warmup_transitions_to_nominal_when_healthy() {
        // Startup is NOT gated by the recovery streak — one good tick promotes.
        assert_eq!(
            next_state(RuntimeState::Warmup, false, 1, T),
            RuntimeState::Nominal
        );
    }

    #[test]
    fn nominal_transitions_to_degraded_when_degraded() {
        assert_eq!(
            next_state(RuntimeState::Nominal, true, 0, T),
            RuntimeState::Degraded
        );
    }

    #[test]
    fn nominal_stays_nominal_when_healthy() {
        assert_eq!(
            next_state(RuntimeState::Nominal, false, 5, T),
            RuntimeState::Nominal
        );
    }

    #[test]
    fn degraded_transitions_to_recovery_when_healthy() {
        // First good tick (streak=1) below threshold 3 → dwell in Recovery.
        assert_eq!(
            next_state(RuntimeState::Degraded, false, 1, T),
            RuntimeState::Recovery
        );
    }

    #[test]
    fn degraded_stays_degraded_when_still_degraded() {
        assert_eq!(
            next_state(RuntimeState::Degraded, true, 0, T),
            RuntimeState::Degraded
        );
    }

    #[test]
    fn recovery_transitions_to_nominal_when_confirmed_healthy() {
        // Streak has reached the threshold → confirmed recovery.
        assert_eq!(
            next_state(RuntimeState::Recovery, false, T, T),
            RuntimeState::Nominal
        );
    }

    #[test]
    fn recovery_returns_to_degraded_when_flapping() {
        assert_eq!(
            next_state(RuntimeState::Recovery, true, 0, T),
            RuntimeState::Degraded
        );
    }

    #[test]
    fn emergency_stop_is_sticky() {
        assert_eq!(
            next_state(RuntimeState::EmergencyStop, false, T, T),
            RuntimeState::EmergencyStop
        );
        assert_eq!(
            next_state(RuntimeState::EmergencyStop, true, 0, T),
            RuntimeState::EmergencyStop
        );
    }

    #[test]
    fn initializing_transitions_unconditionally_to_warmup() {
        assert_eq!(
            next_state(RuntimeState::Initializing, false, 1, T),
            RuntimeState::Warmup
        );
        assert_eq!(
            next_state(RuntimeState::Initializing, true, 0, T),
            RuntimeState::Warmup
        );
    }

    // ── CHANGE 2: recovery hysteresis (pure next_state) ──────────────────────

    #[test]
    fn recovery_dwells_until_streak_reaches_threshold() {
        // threshold 3: streaks 1 and 2 stay in Recovery; streak 3 promotes.
        assert_eq!(
            next_state(RuntimeState::Degraded, false, 1, 3),
            RuntimeState::Recovery
        );
        assert_eq!(
            next_state(RuntimeState::Recovery, false, 2, 3),
            RuntimeState::Recovery
        );
        assert_eq!(
            next_state(RuntimeState::Recovery, false, 3, 3),
            RuntimeState::Nominal
        );
    }

    #[test]
    fn threshold_one_recovers_in_a_single_good_tick() {
        // A threshold of 1 collapses to direct Degraded → Nominal recovery.
        assert_eq!(
            next_state(RuntimeState::Degraded, false, 1, 1),
            RuntimeState::Nominal
        );
    }

    #[test]
    fn threshold_two_reproduces_prior_two_tick_behavior() {
        // Prior behavior: Degraded → Recovery → Nominal (two good ticks).
        assert_eq!(
            next_state(RuntimeState::Degraded, false, 1, 2),
            RuntimeState::Recovery
        );
        assert_eq!(
            next_state(RuntimeState::Recovery, false, 2, 2),
            RuntimeState::Nominal
        );
    }

    #[test]
    fn degrade_is_immediate_regardless_of_streak() {
        // A degraded tick drops to Degraded from any recovering state, even with
        // a large prior streak (the degrade path is never gated).
        assert_eq!(
            next_state(RuntimeState::Nominal, true, 99, 3),
            RuntimeState::Degraded
        );
        assert_eq!(
            next_state(RuntimeState::Recovery, true, 99, 3),
            RuntimeState::Degraded
        );
        assert_eq!(
            next_state(RuntimeState::Degraded, true, 99, 3),
            RuntimeState::Degraded
        );
    }

    #[test]
    fn zero_threshold_is_clamped_to_one() {
        // A 0 threshold must not allow recovery with no confirmation; clamped to 1.
        assert_eq!(
            next_state(RuntimeState::Degraded, false, 1, 0),
            RuntimeState::Nominal
        );
    }

    #[test]
    fn set_state_for_test_overrides_initial_warmup_state() {
        use crate::backend::{
            BackendError, InferenceBackend, ModelHandle, PrecisionMode, TensorBatch,
        };
        use crate::sensor::{SensorFrame, SensorStream};
        use std::collections::HashMap;
        use std::sync::Arc;

        struct FastBackend;
        impl InferenceBackend for FastBackend {
            fn load_model(&self, _: &str) -> Result<ModelHandle, BackendError> {
                Ok(ModelHandle {
                    model_id: "fast".into(),
                    input_shapes: HashMap::new(),
                    output_shapes: HashMap::new(),
                    expected_precision: PrecisionMode::FP32,
                })
            }
            fn run(
                &self,
                _: &ModelHandle,
                _: &TensorBatch,
            ) -> Result<TensorBatch<'static>, BackendError> {
                Ok(TensorBatch {
                    named_tensors: HashMap::new(),
                    metadata: HashMap::new(),
                })
            }
        }

        struct EmptyStream;
        impl SensorStream for EmptyStream {
            fn next_frame(&mut self) -> Option<SensorFrame> {
                None
            }
        }

        let backend = Arc::new(FastBackend);
        let model = backend.load_model("").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut control = ControlLoop::new(backend, model, EmptyStream, tx, 10.0);

        assert_eq!(
            control.state(),
            RuntimeState::Warmup,
            "initial state should be Warmup"
        );
        control.set_state_for_test(RuntimeState::Degraded);
        assert_eq!(
            control.state(),
            RuntimeState::Degraded,
            "set_state_for_test must override Warmup"
        );
    }

    // ── PARK-005 clock tests ─────────────────────────────────────────────────

    /// MockClock controls tick firing without any wall-clock sleep (ADL-004).
    ///
    /// Verifies that at 50ms intervals:
    ///  - first tick fires at t=0
    ///  - tick does NOT fire at 40ms (interval not elapsed)
    ///  - tick DOES fire at 50ms (exactly one interval elapsed)
    ///  - four advances of 50ms each produce exactly four fired ticks
    #[tokio::test]
    async fn test_mock_clock_tick_count() {
        use crate::backend::{
            BackendError, InferenceBackend, ModelHandle, PrecisionMode, TensorBatch,
        };
        use crate::clock::MockClock;
        use crate::sensor::{SensorFrame, SensorStream};
        use std::collections::HashMap;

        struct FastBackend2;
        impl InferenceBackend for FastBackend2 {
            fn load_model(&self, _: &str) -> Result<ModelHandle, BackendError> {
                Ok(ModelHandle {
                    model_id: "fast2".into(),
                    input_shapes: HashMap::new(),
                    output_shapes: HashMap::new(),
                    expected_precision: PrecisionMode::FP32,
                })
            }
            fn run(
                &self,
                _: &ModelHandle,
                _: &TensorBatch,
            ) -> Result<TensorBatch<'static>, BackendError> {
                Ok(TensorBatch {
                    named_tensors: HashMap::new(),
                    metadata: HashMap::new(),
                })
            }
        }

        struct InfiniteStream {
            next_id: u64,
        }
        impl SensorStream for InfiniteStream {
            fn next_frame(&mut self) -> Option<SensorFrame> {
                self.next_id += 1;
                Some(SensorFrame::new(
                    self.next_id,
                    TensorBatch {
                        named_tensors: HashMap::new(),
                        metadata: HashMap::new(),
                    },
                ))
            }
        }

        let mock = MockClock::new(0);
        let backend = Arc::new(FastBackend2);
        let model = backend.load_model("").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(32);

        let mut control = ControlLoop::new(backend, model, InfiniteStream { next_id: 0 }, tx, 20.0)
            .with_clock(Arc::new(mock.clone()))
            .with_tick_interval_ms(50);

        // t=0: first tick always fires (last_tick_ms starts at 0).
        let r = control.tick().await.unwrap();
        assert!(r.is_some(), "first tick must fire at t=0");

        // t=40ms: interval not yet elapsed (40 < 50).
        mock.advance(40);
        let r = control.tick().await.unwrap();
        assert!(
            r.is_none(),
            "tick at 40ms must not fire (interval not elapsed)"
        );

        // t=50ms: exactly one interval; must fire.
        mock.advance(10);
        let r = control.tick().await.unwrap();
        assert!(r.is_some(), "tick at 50ms must fire (interval elapsed)");

        // Four more advances of 50ms each → exactly 4 fired ticks.
        let mut fired = 0usize;
        for _ in 0..4 {
            mock.advance(50);
            let r = control.tick().await.unwrap();
            if r.is_some() {
                fired += 1;
            }
        }
        assert_eq!(
            fired, 4,
            "four 50ms advances must produce exactly 4 fired ticks"
        );
    }

    /// WallClock is the default when no with_clock() call is made.
    /// Verifies no panic and that the first tick fires (returns Some).
    #[tokio::test]
    async fn test_runtime_clock_default_smoke() {
        use crate::backend::{
            BackendError, InferenceBackend, ModelHandle, PrecisionMode, TensorBatch,
        };
        use crate::sensor::{SensorFrame, SensorStream};
        use std::collections::HashMap;

        struct FastBackend3;
        impl InferenceBackend for FastBackend3 {
            fn load_model(&self, _: &str) -> Result<ModelHandle, BackendError> {
                Ok(ModelHandle {
                    model_id: "fast3".into(),
                    input_shapes: HashMap::new(),
                    output_shapes: HashMap::new(),
                    expected_precision: PrecisionMode::FP32,
                })
            }
            fn run(
                &self,
                _: &ModelHandle,
                _: &TensorBatch,
            ) -> Result<TensorBatch<'static>, BackendError> {
                Ok(TensorBatch {
                    named_tensors: HashMap::new(),
                    metadata: HashMap::new(),
                })
            }
        }

        struct OneFrameStream {
            done: bool,
        }
        impl SensorStream for OneFrameStream {
            fn next_frame(&mut self) -> Option<SensorFrame> {
                if self.done {
                    return None;
                }
                self.done = true;
                Some(SensorFrame::new(
                    1,
                    TensorBatch {
                        named_tensors: HashMap::new(),
                        metadata: HashMap::new(),
                    },
                ))
            }
        }

        let backend = Arc::new(FastBackend3);
        let model = backend.load_model("").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        // No with_clock() call — defaults to WallClock.
        let mut control =
            ControlLoop::new(backend, model, OneFrameStream { done: false }, tx, 20.0);

        // First tick always fires; WallClock returns a non-zero unix timestamp.
        let result = control.tick().await;
        assert!(result.is_ok(), "tick must not error: {:?}", result);
        let snapshot = result.unwrap();
        assert!(
            snapshot.is_some(),
            "first tick must fire with WallClock default"
        );
    }

    // ── CHANGE 1 + CHANGE 2: loop-driven fail-closed / hysteresis tests ──────
    //
    // These drive the real `ControlLoop::tick()` (not just the pure `next_state`)
    // to prove the error→EmergencyStop wiring and the streak management.

    use crate::backend::{
        BackendError, InferenceBackend as IB, ModelHandle as MH, PrecisionMode, TensorBatch,
        TensorStorage,
    };
    use crate::sensor::{SensorFrame, SensorStream as SS};
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex;

    fn empty_model(id: &str) -> MH {
        MH {
            model_id: id.into(),
            input_shapes: HashMap::new(),
            output_shapes: HashMap::new(),
            expected_precision: PrecisionMode::FP32,
        }
    }

    /// Backend whose degraded-ness is scripted per tick. For each `degrade` flag
    /// (front→back) it emits a NaN `cmd_vel_linear`, which the scheduler's parser
    /// rejects → `active_state_degraded == true` (a deterministic, timing-free
    /// lever — unlike frame age, which depends on the monotonic clock). A `false`
    /// flag emits finite zero velocities → healthy. Exhausted script → healthy.
    struct ScriptedBackend {
        degrade: Mutex<VecDeque<bool>>,
    }
    impl IB for ScriptedBackend {
        fn load_model(&self, _: &str) -> Result<MH, BackendError> {
            Ok(empty_model("scripted"))
        }
        fn run(&self, _: &MH, _: &TensorBatch) -> Result<TensorBatch<'static>, BackendError> {
            let degrade = self.degrade.lock().unwrap().pop_front().unwrap_or(false);
            let linear = if degrade { f32::NAN } else { 0.0_f32 };
            let mut map = HashMap::new();
            map.insert(
                "cmd_vel_linear".to_string(),
                TensorStorage::Owned(vec![linear]),
            );
            map.insert(
                "cmd_vel_angular".to_string(),
                TensorStorage::Owned(vec![0.0_f32]),
            );
            Ok(TensorBatch {
                named_tensors: map,
                metadata: HashMap::new(),
            })
        }
    }

    /// Backend whose inference always fails — forces `inner.tick()` to return Err.
    struct ErrBackend;
    impl IB for ErrBackend {
        fn load_model(&self, _: &str) -> Result<MH, BackendError> {
            Ok(empty_model("err"))
        }
        fn run(&self, _: &MH, _: &TensorBatch) -> Result<TensorBatch<'static>, BackendError> {
            Err(BackendError::ExecutionFailure(
                "forced inner-tick failure".into(),
            ))
        }
    }

    /// Always-fresh frame source (degraded-ness is driven by the backend, not the
    /// frame, so the frames just need to exist).
    struct FreshStream {
        id: u64,
    }
    impl SS for FreshStream {
        fn next_frame(&mut self) -> Option<SensorFrame> {
            self.id += 1;
            Some(SensorFrame::new(
                self.id,
                TensorBatch {
                    named_tensors: HashMap::new(),
                    metadata: HashMap::new(),
                },
            ))
        }
    }

    /// Builds a loop driven by a per-tick degrade script, and RETURNS the actuator
    /// receiver so the caller keeps it alive — otherwise the channel closes and
    /// the per-tick command flush would error (and, post-CHANGE-1, trip
    /// EmergencyStop), masking the hysteresis under test.
    fn loop_with(
        degrade_script: Vec<bool>,
        threshold: u32,
    ) -> (
        ControlLoop<ScriptedBackend, FreshStream>,
        mpsc::Receiver<ControlCommand>,
    ) {
        let backend = Arc::new(ScriptedBackend {
            degrade: Mutex::new(degrade_script.into()),
        });
        let model = backend.load_model("").unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let control = ControlLoop::new(backend, model, FreshStream { id: 0 }, tx, 10.0)
            .with_tick_interval_ms(0) // every tick() fires
            .with_recovery_confirm_ticks(threshold);
        (control, rx)
    }

    /// CHANGE 1: a propagated inner-tick error drives the runtime to
    /// EmergencyStop before returning Err (state consistency, fail-closed).
    #[tokio::test]
    async fn inner_tick_error_drives_emergency_stop() {
        let backend = Arc::new(ErrBackend);
        let model = backend.load_model("").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut control = ControlLoop::new(backend, model, FreshStream { id: 0 }, tx, 10.0)
            .with_tick_interval_ms(0);

        let r = control.tick().await;
        assert!(
            r.is_err(),
            "a failing inner tick must propagate as Err, got {r:?}"
        );
        assert_eq!(
            control.state(),
            RuntimeState::EmergencyStop,
            "a propagated inner-tick error must set EmergencyStop (matching the sensor path)"
        );
    }

    /// CHANGE 2 (a + b): recovery requires exactly `threshold` consecutive good
    /// ticks — one good tick does NOT recover when threshold > 1.
    #[tokio::test]
    async fn loop_recovery_requires_threshold_consecutive_good_ticks() {
        let (mut control, _rx) = loop_with(vec![], 3); // all healthy ticks
        control.set_state_for_test(RuntimeState::Degraded);

        // (a) one good tick must not recover at threshold 3.
        control.tick().await.unwrap();
        assert_eq!(
            control.state(),
            RuntimeState::Recovery,
            "1 good tick → confirming, not Nominal"
        );

        control.tick().await.unwrap();
        assert_ne!(
            control.state(),
            RuntimeState::Nominal,
            "2 good ticks still below threshold"
        );

        // (b) the 3rd consecutive good tick confirms recovery → Nominal.
        control.tick().await.unwrap();
        assert_eq!(
            control.state(),
            RuntimeState::Nominal,
            "exactly 3 consecutive good ticks recover"
        );
    }

    /// CHANGE 2 (c): a degraded tick mid-streak resets the counter, so recovery
    /// must restart from zero (no early promotion to Nominal).
    #[tokio::test]
    async fn loop_degraded_tick_mid_streak_resets_recovery() {
        // healthy, healthy, DEGRADED, then healthy… (exhausted → healthy).
        let (mut control, _rx) = loop_with(vec![false, false, true], 3);
        control.set_state_for_test(RuntimeState::Degraded);

        control.tick().await.unwrap(); // healthy → Recovery (streak 1)
        control.tick().await.unwrap(); // healthy → Recovery (streak 2)
        assert_eq!(control.state(), RuntimeState::Recovery);

        control.tick().await.unwrap(); // DEGRADED → Degraded (streak reset 0)
        assert_eq!(
            control.state(),
            RuntimeState::Degraded,
            "mid-streak degrade drops back to Degraded"
        );

        control.tick().await.unwrap(); // healthy → Recovery (streak 1)
        assert_ne!(
            control.state(),
            RuntimeState::Nominal,
            "counter reset: must not recover immediately"
        );
        control.tick().await.unwrap(); // healthy → Recovery (streak 2)
        assert_ne!(
            control.state(),
            RuntimeState::Nominal,
            "still below threshold after reset"
        );
        control.tick().await.unwrap(); // healthy → Nominal (streak 3) — proves restart from 0
        assert_eq!(
            control.state(),
            RuntimeState::Nominal,
            "3 fresh ticks after the reset recover"
        );
    }

    /// CHANGE 2 (d): the degrade direction is unchanged — a single degraded tick
    /// drops to Degraded immediately (never gated by the streak).
    #[tokio::test]
    async fn loop_degrade_is_immediate_on_first_degraded_tick() {
        let (mut control, _rx) = loop_with(vec![true], 3); // first tick degraded
        control.set_state_for_test(RuntimeState::Nominal);

        control.tick().await.unwrap();
        assert_eq!(
            control.state(),
            RuntimeState::Degraded,
            "one degraded tick must drop to Degraded immediately (degrade path unchanged)"
        );
    }
}
