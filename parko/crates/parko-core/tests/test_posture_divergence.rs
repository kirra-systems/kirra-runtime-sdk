// crates/parko-core/tests/test_posture_divergence.rs
//
// ControlLoop + KirraGovernor posture-driven enforcement.
//
// HISTORY / WHY THIS WAS REWRITTEN (verification-integrity fix): this target
// is gated `required-features = ["test-helpers"]`, and the default CI runs
// (`cargo test --workspace`) never build it — so it silently bit-rotted
// through THREE unrelated changes and stopped compiling AND stopped asserting
// current safety semantics:
//   1. `BackendCapabilities` lost `precision_modes`/`supports_zero_copy_inputs`/
//      `vendor_name` and `max_batch_size` became `Option` (compile break).
//   2. Issue #70 made Degraded a decel-to-stop-and-HOLD, not a 5 m/s crawl —
//      so the old "Degraded clamps 65→5.0" assertion tested a semantics that
//      was deliberately removed.
//   3. WS-0.1 (#770) made `KirraGovernor::new()` start `RssFeed::NeverFed`
//      (fail-closed) — so an UNFED governor now HOLDs at zero in every
//      posture, and the old "Nominal clamps 65→35.0" assertion is impossible.
//
// The rewrite pins the CURRENT invariants (and is now added to CI, below):
//   - unfed governor → hold at zero in every posture (#770 fail-closed);
//   - fed + Nominal → admits accel-limited forward motion (0 < v < input);
//   - fed + Degraded → HOLD at zero on first-tick re-initiation (#70);
//   - Degraded is strictly more restrictive than Nominal.
//
// Run: cargo test -p parko-core --features test-helpers

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;

use parko_kirra::KirraGovernor;
use parko_core::{
    backend::{
        BackendCapabilities, BackendError, InferenceBackend, ModelHandle,
        PrecisionMode, TensorBatch, TensorStorage,
    },
    commands::ControlCommand,
    control_loop::ControlLoop,
    runtime::RuntimeState,
    sensor::{SensorFrame, SensorStream},
};

/// The over-speed command the mock emits (m/s) — far above any envelope, so
/// every admitted result below is a genuine clamp, not a pass-through.
const OVERSPEED_MPS: f64 = 65.0;

/// Mock backend that emits a single dangerous over-speed command.
struct OverspeedBackend;

impl InferenceBackend for OverspeedBackend {
    fn load_model(&self, _path: &str) -> Result<ModelHandle, BackendError> {
        Ok(ModelHandle {
            model_id: "overspeed-mock".into(),
            input_shapes: HashMap::new(),
            output_shapes: HashMap::new(),
            expected_precision: PrecisionMode::FP32,
        })
    }

    fn run(
        &self,
        _model: &ModelHandle,
        _inputs: &TensorBatch,
    ) -> Result<TensorBatch<'static>, BackendError> {
        let mut named = HashMap::new();
        named.insert("cmd_vel_linear".into(), TensorStorage::Owned(vec![OVERSPEED_MPS as f32]));
        named.insert("cmd_vel_angular".into(), TensorStorage::Owned(vec![0.0]));
        Ok(TensorBatch {
            named_tensors: named,
            metadata: HashMap::new(),
        })
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_int8: false,
            supports_fp16: false,
            max_batch_size: Some(1),
        }
    }
}

/// Sensor stream that yields exactly one frame.
struct SingleFrameStream {
    frame: Option<SensorFrame>,
}

impl SensorStream for SingleFrameStream {
    fn next_frame(&mut self) -> Option<SensorFrame> {
        self.frame.take()
    }
}

/// Build a ControlLoop over the over-speed backend. `fed` selects whether the
/// governor's scene-RSS tier is satisfied: `true` declares external gating
/// (`with_external_rss_gate`, so the posture→kinematic tier is what's
/// exercised), `false` leaves it `NeverFed` (fail-closed — the #770 default).
fn make_loop(fed: bool) -> (
    ControlLoop<OverspeedBackend, SingleFrameStream>,
    mpsc::Receiver<ControlCommand>,
) {
    let backend = Arc::new(OverspeedBackend);
    let model = backend.load_model("").unwrap();
    let stream = SingleFrameStream {
        frame: Some(SensorFrame::new(
            1,
            TensorBatch {
                named_tensors: HashMap::new(),
                metadata: HashMap::new(),
            },
        )),
    };
    let governor = if fed {
        KirraGovernor::new().with_external_rss_gate()
    } else {
        KirraGovernor::new()
    };
    let (tx, rx) = mpsc::channel(8);
    let control = ControlLoop::new(backend, model, stream, tx, 20.0).with_governor(governor);
    (control, rx)
}

async fn tick_velocity(fed: bool, state: RuntimeState) -> f64 {
    let (mut control, _rx) = make_loop(fed);
    control.set_state_for_test(state);
    control
        .tick()
        .await
        .expect("tick should succeed")
        .expect("tick should fire")
        .active_command
        .linear_velocity
}

/// #770 fail-closed default: an UNFED `KirraGovernor::new()` (RssFeed::NeverFed)
/// must HOLD at zero in EVERY posture — never admit the over-speed command,
/// regardless of Nominal vs Degraded. This is the invariant WS-0.1 established
/// and the reason the old 35.0/5.0 assertions are impossible.
#[tokio::test]
async fn unfed_governor_holds_at_zero_in_every_posture() {
    let nominal = tick_velocity(false, RuntimeState::Nominal).await;
    let degraded = tick_velocity(false, RuntimeState::Degraded).await;
    assert_eq!(nominal, 0.0, "unfed governor must hold at zero even under Nominal (fail-closed)");
    assert_eq!(degraded, 0.0, "unfed governor must hold at zero under Degraded (fail-closed)");
}

/// Fed + Nominal: the scene-RSS tier is satisfied, so the posture→kinematic
/// tier governs. On the first tick (current speed 0) the from-zero
/// acceleration envelope binds, so the 65 m/s command is admitted only as a
/// small accel-limited forward motion: strictly between 0 and the input.
#[tokio::test]
async fn nominal_posture_admits_accel_limited_forward_motion() {
    let v = tick_velocity(true, RuntimeState::Nominal).await;
    assert!(
        v > 0.0 && v < OVERSPEED_MPS,
        "fed Nominal must admit clamped forward motion (0 < v < {OVERSPEED_MPS}); got {v}"
    );
}

/// Fed + Degraded: even with RSS satisfied, Degraded is decel-to-stop-and-HOLD
/// (#70). A first-tick command re-initiates motion from a stop, which is
/// denied → HOLD at zero. (The pre-#70 semantics — a 5 m/s crawl — are gone.)
#[tokio::test]
async fn degraded_posture_holds_at_zero_on_reinitiation() {
    let v = tick_velocity(true, RuntimeState::Degraded).await;
    assert_eq!(
        v, 0.0,
        "fed Degraded must HOLD at zero on first-tick re-initiation (#70 stop-and-hold); got {v}"
    );
}

/// The load-bearing ordering invariant, independent of exact numbers:
/// Degraded is strictly more restrictive than Nominal for the same input,
/// and both clamp the over-speed command.
#[tokio::test]
async fn degraded_clamp_is_more_restrictive_than_nominal() {
    let v_nominal = tick_velocity(true, RuntimeState::Nominal).await;
    let v_degraded = tick_velocity(true, RuntimeState::Degraded).await;
    assert!(
        v_degraded < v_nominal,
        "Degraded clamp ({v_degraded}) must be strictly more restrictive than Nominal ({v_nominal})"
    );
    assert!(
        v_nominal < OVERSPEED_MPS && v_degraded < OVERSPEED_MPS,
        "both postures must clamp the {OVERSPEED_MPS} m/s input"
    );
}

/// `set_state_for_test` overrides the initial Warmup state, and a Degraded
/// (fed) loop produces a more restrictive result than the Nominal envelope.
#[tokio::test]
async fn set_state_for_test_forces_degraded_behavior() {
    let (mut control, _rx) = make_loop(true);
    control.set_state_for_test(RuntimeState::Degraded);
    assert_eq!(
        control.state(),
        RuntimeState::Degraded,
        "set_state_for_test must override the initial Warmup state"
    );
    let snapshot = control.tick().await.expect("tick should succeed").expect("tick should fire");
    assert!(
        snapshot.active_command.linear_velocity < OVERSPEED_MPS,
        "Degraded state must clamp the over-speed command"
    );
}
