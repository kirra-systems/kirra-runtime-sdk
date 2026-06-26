// parko/crates/parko-ros2/src/tick_pipeline.rs
//
// One-tick driver for the Parko ROS 2 node. Owns the inference-loop
// drive + the staleness check + the command mapping. The ROS-side
// transport (`node.rs`) calls `run_pipeline_tick` per incoming sensor
// observation; the unit tests below exercise it with parko-core's
// `MockBackend`, no ROS involvement.
//
// Fail-closed paths:
//
//   1. Sensor input older than `sensor_staleness_budget_ms` → emit a
//      stopped `OutgoingTwist`, do NOT run inference. The inference
//      model has zero value on a stale frame and the staleness itself
//      is the safety signal.
//   2. `InferenceLoop::tick` returns `Err` (backend / runtime error)
//      → emit a stopped `OutgoingTwist`. parko-core's `tick` already
//      catches non-finite inference outputs internally and returns a
//      degraded snapshot rather than `Err` for those — but the
//      transport may still surface a `BackendError` (e.g. the model
//      handle is invalid). Either way: MRC.
//   3. `tick` returns `Ok(snapshot)` with `active_state_degraded=true`
//      → still publish the snapshot's command (the loop has already
//      sanitised it to a stopped command for the non-finite case;
//      otherwise the governor's clamp already applies).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parko_core::backend::InferenceBackend;
use parko_core::commands::ControlCommand;
use parko_core::safety::SafetyPosture;
use parko_core::scheduler::InferenceLoop;
use parko_core::sensor::SensorFrame;
use tokio::sync::Mutex;

use crate::command_mapping::{enforce_outgoing_twist, OutgoingTwist};
use crate::config::ParkoNodeConfig;

/// Errors surfaced by `run_pipeline_tick`. The node logs and continues
/// — every error path also produces an MRC `OutgoingTwist` in the
/// `TickOutcome` so the actuator side always has a safe command to
/// publish.
#[derive(Debug, Clone, PartialEq)]
pub enum TickError {
    /// Sensor frame timestamp is older than
    /// `ParkoNodeConfig::sensor_staleness_budget_ms` relative to wall
    /// clock at tick time. MRC.
    StaleSensorInput {
        frame_id:        u64,
        frame_age_ms:    u64,
        budget_ms:       u64,
    },
    /// `InferenceLoop::tick` returned `Err`. The string is the
    /// underlying error message; logged + audited.
    InferenceError(String),
    /// ADR-0029 Phase 3a — the governed command's lookahead left the
    /// ego-relative corridor (or the corridor was absent/stale). The tick
    /// MRCs (stopped twist). Set by `containment_gate::apply_containment_gate`.
    ContainmentBreach,
    /// ADR-0029 Phase 3b (object axis) — a perceived object made the proposed
    /// command RSS-unsafe (the §4 conjunction), or object perception was
    /// absent/stale. The tick MRCs (stopped twist). Set by
    /// `taj_objects::apply_object_rss_gate`.
    ObjectRssBreach,
}

/// What a single `run_pipeline_tick` produced. Always carries a safe
/// `OutgoingTwist` (MRC on any failure), plus an optional `TickError`
/// describing what went wrong so the node can log / audit it.
#[derive(Debug, Clone, PartialEq)]
pub struct TickOutcome {
    /// The twist to publish on the actuator topic. Always present:
    /// on success, the governed command; on error, `OutgoingTwist::stopped`.
    pub twist: OutgoingTwist,
    /// Set when the pipeline could not run a normal tick. `None` on
    /// the happy path.
    pub error: Option<TickError>,
    /// Was the parko-core scheduler in degraded mode this tick?
    /// (Carried for the audit ledger; on success only.)
    pub degraded: bool,
}

/// Drive one tick of the inference loop with the supplied sensor
/// frame and posture. Returns a `TickOutcome` describing what was
/// published.
///
/// The `loop_mutex` parameter is an `Arc<Mutex<InferenceLoop<_>>>`
/// because `InferenceLoop::tick` takes `&mut self` and the node
/// holds a long-lived loop instance across many ticks. The mutex is
/// uncontended in practice (one drain task drives the loop) but the
/// `&mut`-receiver shape requires interior mutability.
///
// SAFETY: SG8 SG9 | REQ: parko-ros2-tick-fail-closed | TEST: tick_with_finite_inference_publishes_governed_command,tick_with_stale_sensor_input_publishes_stopped_twist,tick_with_zero_inference_publishes_stopped_twist,tick_with_locked_out_posture_publishes_stopped_twist
pub async fn run_pipeline_tick<B>(
    config:      &ParkoNodeConfig,
    loop_mutex:  Arc<Mutex<InferenceLoop<B>>>,
    frame:       SensorFrame,
    posture:     SafetyPosture,
) -> TickOutcome
where
    B: InferenceBackend + 'static,
{
    run_pipeline_tick_inner(config, loop_mutex, frame, posture).await
}

/// **M2b** — tick driver that reads the effective posture from the
/// shared `PostureTracker` instead of taking a static parameter.
/// This is the variant the node binary calls so the parko-kirra
/// governor receives the live, fail-closed posture (pre-first-event
/// → Degraded; staleness → Degraded; LockedOut sticky).
///
/// Implementation: read `posture_state.current_safety_posture()`
/// ONCE per tick (the tracker resolves the FleetPosture at the
/// current wall-clock instant + bridges to SafetyPosture), then
/// dispatch into the shared `run_pipeline_tick_inner`.
///
// SAFETY: SG8 SG9 | REQ: parko-ros2-tick-posture-source-fail-closed | TEST: tick_with_no_posture_source_is_nominal,tick_with_source_pre_first_event_is_degraded,tick_with_source_after_locked_out_event_is_locked_out,tick_with_source_after_nominal_event_is_nominal
pub async fn run_pipeline_tick_with_posture_state<B>(
    config:        &ParkoNodeConfig,
    loop_mutex:    Arc<Mutex<InferenceLoop<B>>>,
    frame:         SensorFrame,
    posture_state: &crate::posture_state::ParkoPostureState,
) -> TickOutcome
where
    B: InferenceBackend + 'static,
{
    let posture = posture_state.current_safety_posture();
    run_pipeline_tick_inner(config, loop_mutex, frame, posture).await
}

pub(crate) async fn run_pipeline_tick_inner<B>(
    config:      &ParkoNodeConfig,
    loop_mutex:  Arc<Mutex<InferenceLoop<B>>>,
    frame:       SensorFrame,
    posture:     SafetyPosture,
) -> TickOutcome
where
    B: InferenceBackend + 'static,
{
    // Staleness check (priority 1 — even an error from the backend on a
    // stale frame would be misleading; the frame is the wrong artifact).
    let now_ms = current_time_ms();
    let frame_age_ms = now_ms.saturating_sub(frame.timestamp_ms);
    if frame_age_ms > config.sensor_staleness_budget_ms {
        return TickOutcome {
            twist: OutgoingTwist::stopped(now_ms),
            error: Some(TickError::StaleSensorInput {
                frame_id: frame.frame_id,
                frame_age_ms,
                budget_ms: config.sensor_staleness_budget_ms,
            }),
            degraded: false,
        };
    }

    // Drive the inference loop. The scheduler internally:
    //   - Sends the previously-validated command to actuator_tx (we
    //     also map it ourselves below to avoid a second hop — see the
    //     channel comment in `node.rs`).
    //   - Catches non-finite outputs and returns a `stopped`
    //     `PostureSnapshot` rather than propagating `Err`.
    //   - Applies the attached `SafetyGovernor` (the
    //     `GovernorComparator`) before stamping `active_command`.
    let tick_result = {
        let mut guard = loop_mutex.lock().await;
        // Close the divergence→posture loop: a governor that recommends a stricter posture (the
        // redundancy comparator escalating to Degraded / LockedOut on persistent disagreement)
        // ESCALATES the effective posture this tick. Escalation-only — it can make the posture
        // stricter than the source's verdict, never relax it. `Nominal` for a plain governor.
        let effective_posture = posture.escalate(guard.recommended_posture());
        guard.tick(frame, effective_posture).await
    };

    match tick_result {
        Ok(snapshot) => {
            // The snapshot's `active_command` is the post-governor
            // command; the mapping is a pure projection of axes plus
            // a finiteness defence-in-depth check.
            TickOutcome {
                twist:    enforce_outgoing_twist(&snapshot.active_command),
                error:    None,
                degraded: snapshot.active_state_degraded,
            }
        }
        Err(e) => TickOutcome {
            twist:    OutgoingTwist::stopped(now_ms),
            error:    Some(TickError::InferenceError(e)),
            degraded: false,
        },
    }
}

pub(crate) fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[allow(dead_code)]
fn _assert_control_command_is_publishable(cmd: &ControlCommand) {
    let _ = enforce_outgoing_twist(cmd);
}

// ---------------------------------------------------------------------------
// Tests — MockBackend lane (runs on stable; no ROS, no ORT, no model file).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tick_pipeline_tests {
    use super::*;
    use std::collections::HashMap;

    use parko_core::backend::{BackendDescriptor, TensorBatch};
    use parko_core::backends::mock::MockBackend;
    use parko_kirra::{GovernorComparator, KirraGovernor};
    use tokio::sync::mpsc;

    use crate::comparator_adapter::ComparatorAsGovernor;

    /// Build an InferenceLoop with the given mock inference output + a
    /// `GovernorComparator` (two `KirraGovernor::new()` instances).
    /// Returns the loop wrapped in the `Arc<Mutex<_>>` shape
    /// `run_pipeline_tick` expects, plus the actuator-rx half so tests
    /// can sanity-check what the scheduler forwarded internally.
    fn build_loop(
        linear_out: f32,
        angular_out: f32,
    ) -> (Arc<Mutex<InferenceLoop<MockBackend>>>, mpsc::Receiver<ControlCommand>) {
        let mut outputs: HashMap<String, Vec<f32>> = HashMap::new();
        outputs.insert("cmd_vel_linear".to_string(),  vec![linear_out]);
        outputs.insert("cmd_vel_angular".to_string(), vec![angular_out]);
        let backend = Arc::new(MockBackend::new(outputs, BackendDescriptor::Cpu));

        let model = backend.load_model("test.onnx").expect("mock model loads");

        let (tx, rx) = mpsc::channel::<ControlCommand>(8);
        let comparator = GovernorComparator::new(KirraGovernor::new(), KirraGovernor::new());

        let infer = InferenceLoop::new(backend, model, tx)
            .with_governor(ComparatorAsGovernor(comparator))
            .with_tick_period(0.05);
        (Arc::new(Mutex::new(infer)), rx)
    }

    /// Build an InferenceLoop with an arbitrary governor (for the divergence→posture test).
    fn build_loop_with<G: parko_core::safety::SafetyGovernor + 'static>(
        governor: G,
        linear_out: f32,
        angular_out: f32,
    ) -> Arc<Mutex<InferenceLoop<MockBackend>>> {
        let mut outputs: HashMap<String, Vec<f32>> = HashMap::new();
        outputs.insert("cmd_vel_linear".to_string(), vec![linear_out]);
        outputs.insert("cmd_vel_angular".to_string(), vec![angular_out]);
        let backend = Arc::new(MockBackend::new(outputs, BackendDescriptor::Cpu));
        let model = backend.load_model("test.onnx").expect("mock model loads");
        let (tx, _rx) = mpsc::channel::<ControlCommand>(8);
        let infer = InferenceLoop::new(backend, model, tx).with_governor(governor).with_tick_period(0.05);
        Arc::new(Mutex::new(infer))
    }

    /// A governor that always recommends a fixed posture and stops the command only at LockedOut
    /// (the relevant slice of a real governor's behaviour for the loop-closing test).
    struct RecommendsPosture(parko_core::safety::SafetyPosture);
    impl parko_core::safety::SafetyGovernor for RecommendsPosture {
        fn evaluate(
            &self,
            _proposed: &ControlCommand,
            _previous: Option<&ControlCommand>,
            _delta_time_s: f64,
            posture: SafetyPosture,
        ) -> parko_core::safety::EnforcementAction {
            if posture == SafetyPosture::LockedOut {
                parko_core::safety::EnforcementAction::Deny { reason: "locked out".into() }
            } else {
                parko_core::safety::EnforcementAction::Allow
            }
        }
        fn recommended_posture(&self) -> SafetyPosture {
            self.0
        }
    }

    fn make_frame(frame_id: u64, age_ms: u64) -> SensorFrame {
        let now = current_time_ms();
        SensorFrame {
            frame_id,
            timestamp_ms: now.saturating_sub(age_ms),
            payload: TensorBatch {
                named_tensors: HashMap::new(),
                metadata: HashMap::new(),
            },
        }
    }

    fn default_config() -> ParkoNodeConfig {
        ParkoNodeConfig::default()
    }

    // ---- Happy path ----------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn tick_with_finite_inference_publishes_governed_command() {
        // Inference says +0.1 m/s linear, +0.2 rad/s angular. On the
        // FIRST tick the kinematics-contract `current_velocity_mps`
        // is 0 (no previous), so the implied acceleration is
        // 0.1 / 0.05 = 2.0 m/s² — below the 2.5 m/s² accel ceiling,
        // so neither P2 (hard cap) nor P3 (accel) fires; the
        // governor returns `Allow` and the published twist mirrors
        // the model output. The angular axis is well within the
        // M1b PostureTracker / parko-kirra bounds.
        let (infer, _rx) = build_loop(0.1, 0.2);
        let frame = make_frame(1, 0);
        let outcome = run_pipeline_tick(&default_config(), infer, frame, SafetyPosture::Nominal).await;
        assert!(outcome.error.is_none(),
            "happy-path tick must not surface an error; got {:?}", outcome.error);
        assert!((outcome.twist.linear_x_mps - 0.1).abs() < 1e-4,
            "expected linear ~0.1 (within accel envelope), got {}",
            outcome.twist.linear_x_mps);
        assert!((outcome.twist.angular_z_rads - 0.2).abs() < 1e-4,
            "expected angular ~0.2, got {}", outcome.twist.angular_z_rads);
    }

    // ---- divergence→posture: the governor's recommendation escalates the tick ----

    #[tokio::test(start_paused = true)]
    async fn governor_recommendation_escalates_the_effective_posture() {
        // The CLOSED loop: a governor recommending LockedOut escalates the EFFECTIVE posture even
        // though the source posture is Nominal — so the LockedOut clamp fires and the tick
        // publishes a STOPPED twist. (Without the loop, evaluate(Nominal) would Allow the
        // 0.1 m/s command through.) This is the integrator step the #537 seam was built for.
        let infer = build_loop_with(RecommendsPosture(SafetyPosture::LockedOut), 0.1, 0.2);
        let outcome = run_pipeline_tick(&default_config(), infer, make_frame(1, 0), SafetyPosture::Nominal).await;
        assert!(
            outcome.twist.linear_x_mps.abs() < 1e-6,
            "a LockedOut RECOMMENDATION escalated the Nominal source → stopped twist, got linear {}",
            outcome.twist.linear_x_mps
        );

        // Control: a Nominal recommendation does NOT escalate — the command passes (the loop only
        // ESCALATES the posture, it never fabricates a stop).
        let infer2 = build_loop_with(RecommendsPosture(SafetyPosture::Nominal), 0.1, 0.2);
        let outcome2 = run_pipeline_tick(&default_config(), infer2, make_frame(2, 0), SafetyPosture::Nominal).await;
        assert!(
            outcome2.twist.linear_x_mps.abs() > 1e-6,
            "a Nominal recommendation leaves the command alone, got linear {}",
            outcome2.twist.linear_x_mps
        );
    }

    /// First-tick acceleration clamp invariant — also useful: the
    /// comparator + governor DOES clamp a request that exceeds the
    /// from-zero acceleration limit. The published twist is the
    /// clamped value, not the model output. This pins that the
    /// `with_governor` wiring actually ran.
    #[tokio::test(start_paused = true)]
    async fn tick_first_tick_above_accel_envelope_clamps() {
        // 1.0 m/s with current=0, dt=0.05 → 20 m/s² > 2.5 m/s² max.
        // P3 clamps to `max_accel * dt = 0.125 m/s`.
        let (infer, _rx) = build_loop(1.0, 0.0);
        let frame = make_frame(2, 0);
        let outcome = run_pipeline_tick(&default_config(), infer, frame, SafetyPosture::Nominal).await;
        assert!(outcome.error.is_none());
        assert!(outcome.twist.linear_x_mps > 0.0,
            "clamp must preserve motion direction; got {}", outcome.twist.linear_x_mps);
        assert!(outcome.twist.linear_x_mps < 0.2,
            "first-tick accel clamp must reduce 1.0 m/s well below model output; got {}",
            outcome.twist.linear_x_mps);
    }

    // ---- Fail-closed: stale sensor ------------------------------------

    #[tokio::test(start_paused = true)]
    async fn tick_with_stale_sensor_input_publishes_stopped_twist() {
        let (infer, _rx) = build_loop(2.0, 0.5);
        // Frame timestamp older than the default 200ms staleness budget.
        let frame = make_frame(7, 500);
        let outcome = run_pipeline_tick(&default_config(), infer, frame, SafetyPosture::Nominal).await;
        assert_eq!(outcome.twist.linear_x_mps,   0.0,
            "stale sensor must produce a stopped twist (linear=0)");
        assert_eq!(outcome.twist.angular_z_rads, 0.0,
            "stale sensor must produce a stopped twist (angular=0)");
        match outcome.error {
            Some(TickError::StaleSensorInput { frame_id, frame_age_ms, budget_ms }) => {
                assert_eq!(frame_id, 7);
                assert!(frame_age_ms >= 500,  "frame_age={frame_age_ms} should be ≥500ms");
                assert_eq!(budget_ms, 200);
            }
            other => panic!("expected StaleSensorInput error, got {other:?}"),
        }
    }

    // ---- Posture: LockedOut produces stop -----------------------------

    #[tokio::test(start_paused = true)]
    async fn tick_with_locked_out_posture_publishes_stopped_twist() {
        // Inference produces a forward command but the fleet is
        // LockedOut. The KirraGovernor (primary + shadow) returns Deny
        // → the scheduler stamps a stopped command → the published
        // twist is zero.
        let (infer, _rx) = build_loop(2.5, 0.4);
        let frame = make_frame(11, 0);
        let outcome = run_pipeline_tick(&default_config(), infer, frame, SafetyPosture::LockedOut).await;
        assert!(outcome.error.is_none(),
            "happy-path inference + LockedOut produces a deny inside the governor; \
             no TickError surfaces — got {:?}", outcome.error);
        assert_eq!(outcome.twist.linear_x_mps,   0.0,
            "LockedOut posture must produce a stopped twist (linear=0)");
        assert_eq!(outcome.twist.angular_z_rads, 0.0,
            "LockedOut posture must produce a stopped twist (angular=0)");
    }

    // ---- Posture: Degraded clamps -------------------------------------

    #[tokio::test(start_paused = true)]
    async fn tick_with_degraded_posture_clamps_to_mrc_velocity() {
        // 10 m/s requested but Degraded MRC cap = 5 m/s.
        let (infer, _rx) = build_loop(10.0, 0.2);
        let frame = make_frame(13, 0);
        let outcome = run_pipeline_tick(&default_config(), infer, frame, SafetyPosture::Degraded).await;
        assert!(outcome.twist.linear_x_mps <= 5.0 + 1e-9,
            "Degraded posture must clamp linear to MRC ceiling (≤5 m/s); got {}",
            outcome.twist.linear_x_mps);
        assert!(outcome.error.is_none());
    }

    // ---- Zero inference → stopped twist (the parse_inference / NaN path
    // already routes here too, since the scheduler stamps a stopped command
    // on parse failure; this exercises the happy-path "zero out") ----

    #[tokio::test(start_paused = true)]
    async fn tick_with_zero_inference_publishes_stopped_twist() {
        let (infer, _rx) = build_loop(0.0, 0.0);
        let frame = make_frame(17, 0);
        let outcome = run_pipeline_tick(&default_config(), infer, frame, SafetyPosture::Nominal).await;
        assert!(outcome.error.is_none());
        assert_eq!(outcome.twist.linear_x_mps,   0.0);
        assert_eq!(outcome.twist.angular_z_rads, 0.0);
    }

    // ---- Defence-in-depth: scheduler is meant to scrub NaN; if a NaN
    // ever leaked through, the OutgoingTwist mapping catches it. Use a
    // ControlCommand directly here since constructing a backend that
    // emits a NaN through MockBackend would be circular (the
    // parse_inference function would catch it before the mapping fires). ----

    #[test]
    fn defence_in_depth_nan_command_maps_to_stopped_twist_directly() {
        let cmd = ControlCommand {
            linear_velocity:  f64::NAN,
            angular_velocity: 0.1,
            timestamp_ms:     100,
        };
        let twist = enforce_outgoing_twist(&cmd);
        assert_eq!(twist, OutgoingTwist::stopped(100));
    }

    // =======================================================================
    // PART 3 — fault injection across the parko pipeline stage
    //   sensor → inference → ControlCommand → governor → gated cmd_vel.
    //
    // Each test injects a fault and asserts an EXPLICIT fail-closed outcome
    // (safe-stop / bounded / InferenceError), not merely "no panic". Mirrors
    // the comparator's injected-fault style. The stale-sensor, locked-out,
    // degraded-MRC, and posture-unfed→Degraded faults are covered by the
    // tests above and in the M2b section below; these add the inference-output
    // and backend-failure faults that were previously untested end-to-end.
    // =======================================================================

    /// A backend whose `run()` always fails — models an inference-engine error
    /// (ORT/OpenVINO dlopen/exec failure) at the pipeline's inference stage.
    #[derive(Debug)]
    struct FailingBackend;
    impl parko_core::backend::InferenceBackend for FailingBackend {
        fn load_model(
            &self,
            path: &str,
        ) -> Result<parko_core::backend::ModelHandle, parko_core::backend::BackendError> {
            Ok(parko_core::backend::ModelHandle {
                model_id: format!("failing::{path}"),
                input_shapes: HashMap::new(),
                output_shapes: HashMap::new(),
                expected_precision: parko_core::backend::PrecisionMode::FP32,
            })
        }
        fn run(
            &self,
            _model: &parko_core::backend::ModelHandle,
            _inputs: &TensorBatch,
        ) -> Result<TensorBatch<'static>, parko_core::backend::BackendError> {
            Err(parko_core::backend::BackendError::ExecutionFailure(
                "injected backend failure".to_string(),
            ))
        }
        fn descriptor(&self) -> BackendDescriptor {
            BackendDescriptor::Cpu
        }
    }

    fn build_loop_with_backend<B: parko_core::backend::InferenceBackend + 'static>(
        backend: Arc<B>,
    ) -> Arc<Mutex<InferenceLoop<B>>> {
        let model = backend.load_model("test.onnx").expect("mock model loads");
        let (tx, _rx) = mpsc::channel::<ControlCommand>(8);
        let comparator = GovernorComparator::new(KirraGovernor::new(), KirraGovernor::new());
        let infer = InferenceLoop::new(backend, model, tx)
            .with_governor(ComparatorAsGovernor(comparator))
            .with_tick_period(0.05);
        Arc::new(Mutex::new(infer))
    }

    /// FAULT: inference emits NaN linear velocity. ASSERT fail-closed — the
    /// gated cmd_vel is a safe stop; the NaN command never reaches the
    /// actuator. (Scheduler scrubs non-finite output; `enforce_outgoing_twist`
    /// is the defence-in-depth backstop.)
    #[tokio::test(start_paused = true)]
    async fn fault_nan_inference_output_publishes_stopped_twist() {
        let (infer, _rx) = build_loop(f32::NAN, 0.5);
        let frame = make_frame(101, 0);
        let outcome = run_pipeline_tick(&default_config(), infer, frame, SafetyPosture::Nominal).await;
        assert_eq!(outcome.twist.linear_x_mps, 0.0,
            "NaN inference must safe-stop (linear=0), got {}", outcome.twist.linear_x_mps);
        assert_eq!(outcome.twist.angular_z_rads, 0.0,
            "NaN inference must safe-stop (angular=0), got {}", outcome.twist.angular_z_rads);
        assert!(outcome.twist.linear_x_mps.is_finite() && outcome.twist.angular_z_rads.is_finite(),
            "the published command must be finite (no NaN leak to cmd_vel)");
    }

    /// FAULT: inference emits +∞ linear velocity. ASSERT fail-closed safe stop.
    #[tokio::test(start_paused = true)]
    async fn fault_inf_inference_output_publishes_stopped_twist() {
        let (infer, _rx) = build_loop(f32::INFINITY, 0.0);
        let frame = make_frame(102, 0);
        let outcome = run_pipeline_tick(&default_config(), infer, frame, SafetyPosture::Nominal).await;
        assert_eq!(outcome.twist.linear_x_mps, 0.0,
            "inf inference must safe-stop, got {}", outcome.twist.linear_x_mps);
        assert!(outcome.twist.linear_x_mps.is_finite(),
            "no inf leak to cmd_vel");
    }

    /// FAULT: inference emits a wildly out-of-range (but finite) 1000 m/s.
    /// ASSERT the governor BOUNDS it — a wrong-but-in-bounds command is never
    /// admitted as-is. The published value is the safe-envelope clamp, far
    /// below the requested 1000 m/s, finite and non-negative.
    #[tokio::test(start_paused = true)]
    async fn fault_out_of_range_inference_is_bounded_not_admitted() {
        let (infer, _rx) = build_loop(1000.0, 0.0);
        let frame = make_frame(103, 0);
        let outcome = run_pipeline_tick(&default_config(), infer, frame, SafetyPosture::Nominal).await;
        let v = outcome.twist.linear_x_mps;
        assert!(v.is_finite(), "published velocity must be finite, got {v}");
        assert!(v >= 0.0, "must not flip sign, got {v}");
        assert!(v < 1000.0,
            "1000 m/s must be CLAMPED by the governor envelope, never admitted as-is; got {v}");
    }

    /// FAULT: the inference backend itself errors. ASSERT fail-closed — the
    /// pipeline emits a stopped twist AND surfaces a structured InferenceError,
    /// never a fail-OPEN pass-through.
    #[tokio::test(start_paused = true)]
    async fn fault_backend_failure_fails_closed() {
        let infer = build_loop_with_backend(Arc::new(FailingBackend));
        let frame = make_frame(104, 0);
        let outcome = run_pipeline_tick(&default_config(), infer, frame, SafetyPosture::Nominal).await;
        assert_eq!(outcome.twist.linear_x_mps, 0.0,
            "backend failure must safe-stop (linear=0)");
        assert_eq!(outcome.twist.angular_z_rads, 0.0,
            "backend failure must safe-stop (angular=0)");
        match outcome.error {
            Some(TickError::InferenceError(_)) => {}
            other => panic!("backend failure must surface InferenceError (fail-closed), got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // M2b — `run_pipeline_tick_with_posture_state` exercises the shared
    // `PostureTracker` inside the tick. These tests mirror M1b's
    // adapter-side coverage, applied to parko-ros2's path.
    // -----------------------------------------------------------------------
    //
    // Together they show that the same `PostureTracker` instance has TWO
    // consumers — adapter (M1b) and parko-ros2 (M2b) — with identical
    // fail-closed behaviour. Duplicating the state machine is exactly
    // what M2b exists to prevent.

    use kirra_core::FleetPosture;
    use crate::posture_state::ParkoPostureState;

    /// **No source → Nominal default (preserves the M2 behaviour).**
    /// A node constructed without a posture source must publish the
    /// model output unmodified (subject to the kinematics envelope,
    /// not posture-driven MRC clamping). The 0.1 m/s command stays
    /// at 0.1 m/s — Nominal.
    #[tokio::test(start_paused = true)]
    async fn tick_with_no_posture_source_is_nominal() {
        let (infer, _rx) = build_loop(0.1, 0.2);
        let frame = make_frame(101, 0);
        let state = ParkoPostureState::no_source();
        let outcome = run_pipeline_tick_with_posture_state(
            &default_config(), infer, frame, &state).await;
        assert!(outcome.error.is_none(), "no-source path must not surface a TickError");
        assert!((outcome.twist.linear_x_mps - 0.1).abs() < 1e-4,
            "no-source default is Nominal — model output ~0.1 m/s passes through; got {}",
            outcome.twist.linear_x_mps);
    }

    /// **Source configured, no event yet → Degraded floor.**
    /// The operator's intent to govern is explicit (the source is
    /// configured) but the verifier hasn't confirmed posture yet. The
    /// tracker's pre-first-event seed is Degraded → the governor
    /// applies the MRC cap. A 10 m/s command (well under the 35 m/s
    /// nominal max but over the 5 m/s MRC cap) MUST be clamped.
    #[tokio::test(start_paused = true)]
    async fn tick_with_source_pre_first_event_is_degraded() {
        let (infer, _rx) = build_loop(10.0, 0.1);
        let frame = make_frame(102, 0);
        let state = ParkoPostureState::with_source();
        let outcome = run_pipeline_tick_with_posture_state(
            &default_config(), infer, frame, &state).await;
        assert!(outcome.twist.linear_x_mps <= 5.0 + 1e-6,
            "pre-first-event source must derate to Degraded (≤5 m/s MRC); got {}",
            outcome.twist.linear_x_mps);
    }

    /// **Source configured, LockedOut event observed → hard stop.**
    /// Mirrors M1b's `locked_out_dominates_*` invariant for the
    /// parko path. The kernel tracker's sticky-LockedOut latch
    /// holds; the governor returns Deny on `SafetyPosture::LockedOut`;
    /// the published twist is zero.
    #[tokio::test(start_paused = true)]
    async fn tick_with_source_after_locked_out_event_is_locked_out() {
        let (infer, _rx) = build_loop(2.5, 0.1);
        let frame = make_frame(103, 0);
        let state = ParkoPostureState::with_source();
        state.observe(FleetPosture::LockedOut);
        let outcome = run_pipeline_tick_with_posture_state(
            &default_config(), infer, frame, &state).await;
        assert_eq!(outcome.twist.linear_x_mps,   0.0,
            "LockedOut event must drive the tick to a stopped twist (linear=0)");
        assert_eq!(outcome.twist.angular_z_rads, 0.0,
            "LockedOut event must drive the tick to a stopped twist (angular=0)");
    }

    /// **Source configured, Nominal event observed → live posture.**
    /// Once the source confirms Nominal, the tracker exits its
    /// fail-closed seed; subsequent ticks use the full envelope.
    /// A 0.1 m/s request passes through unmodified.
    #[tokio::test(start_paused = true)]
    async fn tick_with_source_after_nominal_event_is_nominal() {
        let (infer, _rx) = build_loop(0.1, 0.2);
        let frame = make_frame(104, 0);
        let state = ParkoPostureState::with_source();
        state.observe(FleetPosture::Nominal);
        let outcome = run_pipeline_tick_with_posture_state(
            &default_config(), infer, frame, &state).await;
        assert!(outcome.error.is_none());
        assert!((outcome.twist.linear_x_mps - 0.1).abs() < 1e-4,
            "Nominal observation must release the Degraded seed; got {}",
            outcome.twist.linear_x_mps);
    }
}
