// crates/parko-core/tests/test_posture_divergence.rs
//
// Verifies that ControlLoop with an KirraGovernor selects different
// kinematic contract profiles based on derived SafetyPosture, and that
// this produces materially different clamping behavior for the same input.
//
// Confirmed Kirra profile values (surveyed from
// kirra_runtime_sdk::gateway::kinematics_contract):
// - nominal_reference_profile().max_speed_mps == 35.0
// - mrc_fallback_profile().max_speed_mps == 5.0

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

/// Mock backend that emits a single dangerous over-speed command (65.0 m/s).
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
        named.insert("cmd_vel_linear".into(), TensorStorage::Owned(vec![65.0]));
        named.insert("cmd_vel_angular".into(), TensorStorage::Owned(vec![0.0]));
        Ok(TensorBatch {
            named_tensors: named,
            metadata: HashMap::new(),
        })
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            precision_modes: vec![PrecisionMode::FP32],
            supports_zero_copy_inputs: false,
            max_batch_size: 1,
            vendor_name: "overspeed-mock",
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

fn make_loop() -> (
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
    let (tx, rx) = mpsc::channel(8);
    let control = ControlLoop::new(backend, model, stream, tx, 20.0)
        .with_governor(Box::new(KirraGovernor::new()));
    (control, rx)
}

#[tokio::test]
async fn nominal_posture_clamps_to_nominal_profile_max_speed() {
    let (mut control, _rx) = make_loop();
    control.set_state_for_test(RuntimeState::Nominal);

    let snapshot = control.tick().await.expect("tick should succeed");

    assert_eq!(
        snapshot.active_command.linear_velocity, 35.0,
        "Nominal posture should clamp 65.0 m/s to nominal profile max_speed_mps (35.0)"
    );
}

#[tokio::test]
async fn degraded_posture_clamps_to_mrc_fallback_profile_max_speed() {
    let (mut control, _rx) = make_loop();
    control.set_state_for_test(RuntimeState::Degraded);

    let snapshot = control.tick().await.expect("tick should succeed");

    assert_eq!(
        snapshot.active_command.linear_velocity, 5.0,
        "Degraded posture should clamp 65.0 m/s to MRC fallback profile max_speed_mps (5.0)"
    );
}

#[tokio::test]
async fn degraded_clamp_is_more_restrictive_than_nominal_clamp() {
    let (mut control_nominal, _rx_n) = make_loop();
    control_nominal.set_state_for_test(RuntimeState::Nominal);
    let snap_nominal = control_nominal.tick().await.unwrap();

    let (mut control_degraded, _rx_d) = make_loop();
    control_degraded.set_state_for_test(RuntimeState::Degraded);
    let snap_degraded = control_degraded.tick().await.unwrap();

    let v_nominal = snap_nominal.active_command.linear_velocity;
    let v_degraded = snap_degraded.active_command.linear_velocity;

    assert!(
        v_degraded < v_nominal,
        "Degraded clamp ({}) must be strictly more restrictive than Nominal clamp ({})",
        v_degraded,
        v_nominal,
    );
    assert!(
        v_nominal < 65.0 && v_degraded < 65.0,
        "Both postures must clamp the 65.0 m/s input"
    );
}
