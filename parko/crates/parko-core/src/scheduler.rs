// crates/parko-core/src/scheduler.rs

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task;

use crate::backend::{BackendCapabilities, BackendDescriptor, InferenceBackend, ModelHandle, PrecisionMode, TensorBatch};
use crate::commands::ControlCommand;
use crate::sensor::SensorFrame;
use crate::telemetry::{CumulativeJitterEvaluator, PostureSnapshot, RuntimeTelemetry, ThermalState};
use crate::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};
use crate::audit::{AuditClient, FaultRecord, OverrideRecord};

/// The stable audit reason code for a governor override, or `None` for `Allow`
/// (the common no-op, which is not recorded).
fn override_reason(action: &EnforcementAction) -> Option<&'static str> {
    match action {
        EnforcementAction::Allow => None,
        EnforcementAction::ClampLinearVelocity(_) => Some("clamp_linear"),
        EnforcementAction::ClampAngularVelocity(_) => Some("clamp_angular"),
        EnforcementAction::ClampMotion { .. } => Some("clamp_motion"),
        EnforcementAction::Deny { .. } => Some("deny"),
    }
}

/// Thresholds for degraded-mode detection.
///
/// TODO: expose externally or load from config.
#[derive(Debug, Clone)]
pub struct DegradationThresholds {
    pub max_inference_latency_ms: u64,
    pub max_jitter_ms: f64,
    pub max_frame_age_ms: u64,
    pub max_linear_velocity_mps: f64,
}

impl Default for DegradationThresholds {
    fn default() -> Self {
        Self {
            max_inference_latency_ms: 150,
            max_jitter_ms: 25.0,
            max_frame_age_ms: 100,
            max_linear_velocity_mps: 1.5,
        }
    }
}

/// Inference loop with one-tick-delayed actuator publication.
///
/// NOTE: This contains a placeholder degraded-mode policy.
/// Real physical envelope enforcement belongs in the KirraGovernor.
pub struct InferenceLoop<B: InferenceBackend> {
    backend: Arc<B>,
    model: Arc<ModelHandle>,
    actuator_tx: mpsc::Sender<ControlCommand>,
    last_validated_command: Option<ControlCommand>,
    last_frame_id: Option<u64>,
    dropped_frame_counter: u64,
    jitter_tracker: CumulativeJitterEvaluator,
    thresholds: DegradationThresholds,
    cached_capabilities: BackendCapabilities,
    cached_descriptor: BackendDescriptor,
    governor: Option<Box<dyn SafetyGovernor>>,
    tick_period_s: f64,
    /// Optional SDK-free audit sink (L5). Default `None` → no records emitted
    /// (byte-identical behaviour). When set, the decision path reports governor
    /// overrides and non-finite faults through the `AuditClient` trait, keeping
    /// it independent of any concrete (e.g. SDK-backed) audit implementation.
    audit: Option<Arc<dyn AuditClient>>,
}

fn capabilities_precision(caps: &BackendCapabilities) -> PrecisionMode {
    if caps.supports_int8 {
        PrecisionMode::INT8
    } else if caps.supports_fp16 {
        PrecisionMode::FP16
    } else {
        PrecisionMode::FP32
    }
}

fn descriptor_vendor(d: &BackendDescriptor) -> &'static str {
    match d {
        BackendDescriptor::Cpu => "ort-cpu",
        BackendDescriptor::Cuda => "cuda",
        BackendDescriptor::TensorRT => "tensorrt",
        BackendDescriptor::QualcommQnn => "qnn",
        BackendDescriptor::TiTidl => "ti-tidl",
        BackendDescriptor::IntelOpenVino => "openvino",
        BackendDescriptor::AmdVitis => "amd-vitis",
    }
}

impl<B: InferenceBackend + 'static> InferenceLoop<B> {
    pub fn new(
        backend: Arc<B>,
        model: ModelHandle,
        actuator_tx: mpsc::Sender<ControlCommand>,
    ) -> Self {
        let cached_capabilities = backend.capabilities();
        let cached_descriptor = backend.descriptor();
        Self {
            backend,
            model: Arc::new(model),
            actuator_tx,
            last_validated_command: None,
            last_frame_id: None,
            dropped_frame_counter: 0,
            jitter_tracker: CumulativeJitterEvaluator::new(),
            thresholds: DegradationThresholds::default(),
            cached_capabilities,
            cached_descriptor,
            governor: None,
            tick_period_s: 0.05,
            audit: None,
        }
    }

    /// Attach an [`AuditClient`] so the decision path records governor overrides
    /// and non-finite faults to a caller-chosen audit sink. Default: none → no
    /// records emitted (byte-identical). The SDK-free trait keeps the decision
    /// path independent of the verifier crate (the concrete sink is injected).
    #[must_use]
    pub fn with_audit_client(mut self, audit: Arc<dyn AuditClient>) -> Self {
        self.audit = Some(audit);
        self
    }

    /// Attach a safety governor to this loop. The governor's evaluation
    /// takes precedence over the built-in degraded-mode clamp when a governor
    /// is present; the built-in clamp is only active when no governor is set.
    pub fn with_governor(mut self, governor: impl SafetyGovernor + 'static) -> Self {
        self.governor = Some(Box::new(governor));
        self
    }

    /// The posture the attached governor RECOMMENDS from its internal state (e.g. a redundancy
    /// comparator escalating to `Degraded` / `LockedOut` on persistent divergence); `Nominal`
    /// when no governor is attached. The tick driver reads this each cycle and escalates the
    /// effective posture with it — closing the divergence→posture loop so a governor
    /// disagreement actually drives the fleet posture, not just this tick's clamp.
    #[must_use]
    pub fn recommended_posture(&self) -> SafetyPosture {
        self.governor
            .as_ref()
            .map_or(SafetyPosture::Nominal, |g| g.recommended_posture())
    }

    /// Set the tick period (used for time-delta calculations passed to
    /// the safety governor). Defaults to 0.05 (20Hz).
    pub fn with_tick_period(mut self, tick_period_s: f64) -> Self {
        self.tick_period_s = tick_period_s;
        self
    }

    pub async fn tick(&mut self, current_frame: SensorFrame, posture: SafetyPosture) -> Result<PostureSnapshot, String> {
        let loop_start_ms = crate::sensor::current_time_ms();

        // Flush previously validated command (frame N-1).
        if let Some(ref cmd) = self.last_validated_command {
            self.actuator_tx
                .send(cmd.clone())
                .await
                .map_err(|e| format!("actuator channel closed: {}", e))?;
        }

        // Track dropped frames.
        if let Some(prev) = self.last_frame_id {
            if current_frame.frame_id > prev + 1 {
                let gap = current_frame.frame_id - prev - 1;
                self.dropped_frame_counter =
                    self.dropped_frame_counter.saturating_add(gap);
            }
        }
        self.last_frame_id = Some(current_frame.frame_id);

        // Frame age + payload size.
        let frame_age_ms = loop_start_ms.saturating_sub(current_frame.timestamp_ms);
        let tensor_payload_bytes = current_frame
            .payload
            .named_tensors
            .values()
            .map(|s| std::mem::size_of_val(s.as_slice()))
            .sum();

        let backend_ref = Arc::clone(&self.backend);
        let model_handle = Arc::clone(&self.model);
        let payload = current_frame.payload;

        // Inference on blocking thread.
        let (output_tensors, inference_latency_ms) = task::spawn_blocking(move || {
            let start = std::time::Instant::now();
            let out = backend_ref.run(&model_handle, &payload);
            let elapsed = start.elapsed().as_millis() as u64;
            (out, elapsed)
        })
        .await
        .map_err(|e| format!("inference worker join failure: {}", e))?;

        let processed_outputs =
            output_tensors.map_err(|e| format!("backend inference error: {}", e))?;

        // Jitter update.
        self.jitter_tracker.update(inference_latency_ms);
        let rolling_jitter_ms = self.jitter_tracker.std_dev_ms();

        // Thermal probe.
        let thermal_state_opt = self.probe_platform_thermals();

        // Parse inference outputs. Non-finite values (NaN, Inf) are caught by
        // parse_inference_to_command; treat them as a recoverable degraded
        // condition and return a safe stopped snapshot rather than propagating
        // the error and crashing the loop.
        let proposed_cmd = match self.parse_inference_to_command(&processed_outputs, loop_start_ms) {
            Ok(cmd) => cmd,
            Err(parse_err) => {
                // Non-finite (NaN/Inf) inference output → fail-closed stopped
                // command. Audit it as a decision-path fault before returning.
                if let Some(a) = &self.audit {
                    a.record_fault(FaultRecord {
                        tick_ms: loop_start_ms,
                        code: "nonfinite_command",
                        detail: parse_err,
                        posture,
                    });
                }
                let telemetry = RuntimeTelemetry {
                    inference_latency_ms,
                    rolling_jitter_ms,
                    dropped_frames: self.dropped_frame_counter,
                    thermal_state: thermal_state_opt.unwrap_or(ThermalState::Normal),
                    frame_age_ms,
                    tensor_payload_bytes,
                    backend_precision: capabilities_precision(&self.cached_capabilities),
                    backend_vendor: std::borrow::Cow::Borrowed(descriptor_vendor(&self.cached_descriptor)),
                };
                return Ok(PostureSnapshot {
                    frame_id: current_frame.frame_id,
                    active_command: ControlCommand::stopped(loop_start_ms),
                    telemetry,
                    active_state_degraded: true,
                });
            }
        };

        // If a safety governor is configured, evaluate the proposed command.
        // The governor's decision takes precedence over the built-in
        // degraded-mode clamp below; the built-in clamp remains as a fallback
        // for callers without a governor.
        let proposed_cmd = if let Some(ref governor) = self.governor {
            let action = governor.evaluate(
                &proposed_cmd,
                self.last_validated_command.as_ref(),
                self.tick_period_s,
                posture,
            );
            // Compute the audit inputs ONLY when a client is attached AND the
            // governor actually overrode (`override_reason` is `None` for Allow),
            // so the hot path is byte-identical when auditing is disabled — nothing
            // here runs if `self.audit` is `None`. Captured BEFORE the match
            // consumes `action`; the proposal fields are Copy reads.
            let audit_override = self.audit.as_ref().and_then(|client| {
                override_reason(&action).map(|reason| {
                    (
                        client,
                        reason,
                        proposed_cmd.linear_velocity,
                        proposed_cmd.angular_velocity,
                    )
                })
            });
            let commanded = match action {
                EnforcementAction::Allow => proposed_cmd,
                EnforcementAction::ClampLinearVelocity(v) => ControlCommand {
                    linear_velocity: v,
                    angular_velocity: proposed_cmd.angular_velocity,
                    timestamp_ms: proposed_cmd.timestamp_ms,
                },
                EnforcementAction::ClampAngularVelocity(v) => ControlCommand {
                    linear_velocity: proposed_cmd.linear_velocity,
                    angular_velocity: v,
                    timestamp_ms: proposed_cmd.timestamp_ms,
                },
                // Multi-axis safe envelope. Each `Some` axis overrides the
                // proposed value; each `None` axis is left at the proposed
                // value (unconstrained on this tick). This is NOT a stop —
                // a `Deny` is still required for a full hard stop.
                EnforcementAction::ClampMotion { linear, angular } => ControlCommand {
                    linear_velocity: linear.unwrap_or(proposed_cmd.linear_velocity),
                    angular_velocity: angular.unwrap_or(proposed_cmd.angular_velocity),
                    timestamp_ms: proposed_cmd.timestamp_ms,
                },
                EnforcementAction::Deny { reason: _ } => {
                    ControlCommand::stopped(proposed_cmd.timestamp_ms)
                }
            };
            // Audit the override (the governor changed the doer's command). Sparse
            // and safety-relevant; `Allow` is the common no-op and was already
            // filtered out above (so `audit_override` is `None` for it).
            if let Some((client, reason, ml_lin, ml_ang)) = audit_override {
                client.record_override(OverrideRecord {
                    tick_ms: loop_start_ms,
                    reason,
                    proposed_linear_mps: ml_lin,
                    proposed_angular_rps: ml_ang,
                    commanded_linear_mps: commanded.linear_velocity,
                    commanded_angular_rps: commanded.angular_velocity,
                    posture,
                });
            }
            commanded
        } else {
            proposed_cmd
        };

        // Degraded-mode detection.
        let mut degraded = false;
        let t = &self.thresholds;

        if inference_latency_ms > t.max_inference_latency_ms {
            degraded = true;
        }
        if rolling_jitter_ms > t.max_jitter_ms {
            degraded = true;
        }
        if frame_age_ms > t.max_frame_age_ms {
            degraded = true;
        }
        if matches!(thermal_state_opt, Some(ThermalState::Critical)) {
            degraded = true;
        }

        // Clamp-only degraded mode — skipped when a governor is attached
        // because the governor's decision already constrains the command.
        let sanitized_command = if degraded && self.governor.is_none() {
            // #693: clamp the MAGNITUDE. `.min(max)` bounded only the forward
            // direction, so a large REVERSE command (e.g. -65 m/s) passed through
            // unclamped. Bound both directions to ±max_linear_velocity_mps. Use
            // `.max(-bound).min(bound)` rather than `f64::clamp` so the degraded
            // fallback keeps the original `.min`'s NaN-tolerant, panic-free
            // behaviour (clamp panics if the bound is NaN). Real physical-envelope
            // enforcement is the attached KirraGovernor, used when present; this is
            // only the governorless fallback.
            let bound = t.max_linear_velocity_mps;
            let clamped_linear = proposed_cmd.linear_velocity.max(-bound).min(bound);
            ControlCommand {
                linear_velocity: clamped_linear,
                angular_velocity: proposed_cmd.angular_velocity,
                timestamp_ms: loop_start_ms,
            }
        } else {
            proposed_cmd
        };

        self.last_validated_command = Some(sanitized_command.clone());

        let telemetry = RuntimeTelemetry {
            inference_latency_ms,
            rolling_jitter_ms,
            dropped_frames: self.dropped_frame_counter,
            thermal_state: thermal_state_opt.unwrap_or(ThermalState::Normal),
            frame_age_ms,
            tensor_payload_bytes,
            backend_precision: capabilities_precision(&self.cached_capabilities),
            backend_vendor: std::borrow::Cow::Borrowed(descriptor_vendor(&self.cached_descriptor)),
        };

        Ok(PostureSnapshot {
            frame_id: current_frame.frame_id,
            active_command: sanitized_command,
            telemetry,
            active_state_degraded: degraded,
        })
    }

    fn parse_inference_to_command(
        &self,
        outputs: &TensorBatch,
        ts: u64,
    ) -> Result<ControlCommand, String> {
        let linear = outputs
            .named_tensors
            .get("cmd_vel_linear")
            .and_then(|v| v.as_slice().first())
            .copied()
            .unwrap_or(0.0) as f64;

        let angular = outputs
            .named_tensors
            .get("cmd_vel_angular")
            .and_then(|v| v.as_slice().first())
            .copied()
            .unwrap_or(0.0) as f64;

        if !linear.is_finite() || !angular.is_finite() {
            return Err(format!(
                "non-finite command values: linear={}, angular={}",
                linear, angular
            ));
        }

        Ok(ControlCommand {
            linear_velocity: linear,
            angular_velocity: angular,
            timestamp_ms: ts,
        })
    }

    fn probe_platform_thermals(&self) -> Option<ThermalState> {
        let content = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp").ok()?;
        let temp_raw = content.trim().parse::<i32>().ok()?;
        let temp_c = temp_raw / 1000;

        Some(if temp_c >= 80 {
            ThermalState::Critical
        } else if temp_c >= 65 {
            ThermalState::Warning
        } else {
            ThermalState::Normal
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use crate::backend::{BackendError, InferenceBackend, PrecisionMode, TensorStorage};
    use crate::sensor::SensorStream;
    use proptest::prelude::*;

    struct TestBackend;

    impl InferenceBackend for TestBackend {
        fn load_model(&self, _: &str) -> Result<ModelHandle, BackendError> {
            let mut inputs = HashMap::new();
            inputs.insert("image_input".to_string(), vec![1, 3, 224, 224]);
            Ok(ModelHandle {
                model_id: "test".to_string(),
                input_shapes: inputs,
                output_shapes: HashMap::new(),
                expected_precision: PrecisionMode::FP32,
            })
        }

        fn run(
            &self,
            _: &ModelHandle,
            _: &TensorBatch,
        ) -> Result<TensorBatch<'static>, BackendError> {
            // Force degraded mode by exceeding latency threshold.
            std::thread::sleep(std::time::Duration::from_millis(200));

            let mut map = HashMap::new();
            map.insert(
                "cmd_vel_linear".to_string(),
                TensorStorage::Owned(vec![65.0]),
            );
            map.insert(
                "cmd_vel_angular".to_string(),
                TensorStorage::Owned(vec![0.0]),
            );
            Ok(TensorBatch {
                named_tensors: map,
                metadata: HashMap::new(),
            })
        }

    }

    /// Like `TestBackend` but emits a large NEGATIVE (reverse) command — the
    /// geometry #693 is about. Also exceeds the latency threshold to force
    /// degraded mode so the built-in clamp activates.
    struct ReverseTestBackend;

    impl InferenceBackend for ReverseTestBackend {
        fn load_model(&self, _: &str) -> Result<ModelHandle, BackendError> {
            let mut inputs = HashMap::new();
            inputs.insert("image_input".to_string(), vec![1, 3, 224, 224]);
            Ok(ModelHandle {
                model_id: "reverse-test".to_string(),
                input_shapes: inputs,
                output_shapes: HashMap::new(),
                expected_precision: PrecisionMode::FP32,
            })
        }

        fn run(
            &self,
            _: &ModelHandle,
            _: &TensorBatch,
        ) -> Result<TensorBatch<'static>, BackendError> {
            std::thread::sleep(std::time::Duration::from_millis(200)); // force degraded
            let mut map = HashMap::new();
            map.insert("cmd_vel_linear".to_string(), TensorStorage::Owned(vec![-65.0]));
            map.insert("cmd_vel_angular".to_string(), TensorStorage::Owned(vec![0.0]));
            Ok(TensorBatch { named_tensors: map, metadata: HashMap::new() })
        }
    }

    struct SimpleStream {
        next_id: u64,
    }

    impl SensorStream for SimpleStream {
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

    use crate::safety::{EnforcementAction, SafetyGovernor, SafetyPosture};

    /// Test governor that always clamps linear velocity to 2.0 m/s.
    struct ClampToTwoGovernor;
    impl SafetyGovernor for ClampToTwoGovernor {
        fn evaluate(
            &self,
            proposed: &ControlCommand,
            _previous: Option<&ControlCommand>,
            _delta_time_s: f64,
            _posture: SafetyPosture,
        ) -> EnforcementAction {
            if proposed.linear_velocity > 2.0 {
                EnforcementAction::ClampLinearVelocity(2.0)
            } else {
                EnforcementAction::Allow
            }
        }
    }

    /// A governor that unconditionally stops the vehicle (linear = 0.0).
    /// Used to verify that an injected governor takes precedence over the
    /// built-in degraded-mode clamp — the clamp must not fire when a governor
    /// is present (ADL-002).
    struct ZeroGovernor;
    impl SafetyGovernor for ZeroGovernor {
        fn evaluate(
            &self,
            _proposed: &ControlCommand,
            _previous: Option<&ControlCommand>,
            _delta_time_s: f64,
            _posture: SafetyPosture,
        ) -> EnforcementAction {
            EnforcementAction::Deny {
                reason: "ZeroGovernor: all commands denied".to_string(),
            }
        }
    }

    /// PARK-001 acceptance: when a governor is injected, the built-in clamp
    /// must not fire. ZeroGovernor produces 0.0; the built-in ceiling (1.5)
    /// is above 0.0, so if both fired the result would be 1.5, not 0.0.
    #[tokio::test]
    async fn test_builtin_clamp_suppressed() {
        // TestBackend emits 65.0 m/s — above the 1.5 built-in clamp ceiling.
        let backend = Arc::new(TestBackend);
        let model = backend.load_model("").unwrap();
        let (tx, _rx) = mpsc::channel(4);

        let mut loop_engine = InferenceLoop::new(backend, model, tx)
            .with_governor(ZeroGovernor);
        let mut stream = SimpleStream { next_id: 0 };

        // Tick 1: fills last_validated_command; nothing flushed to actuator yet.
        let _ = loop_engine
            .tick(stream.next_frame().unwrap(), SafetyPosture::Nominal)
            .await
            .unwrap();
        // Tick 2: ZeroGovernor denies the 65.0 m/s command → stopped (0.0).
        let snapshot = loop_engine
            .tick(stream.next_frame().unwrap(), SafetyPosture::Nominal)
            .await
            .unwrap();

        assert_eq!(
            snapshot.active_command.linear_velocity, 0.0,
            "ZeroGovernor must override the 65.0 m/s command; \
             built-in clamp must be suppressed when a governor is present"
        );
    }

    /// PARK-001 acceptance: without a governor, the built-in degraded-mode
    /// clamp caps linear velocity at max_linear_velocity_mps (1.5 m/s).
    /// TestBackend deliberately exceeds the inference-latency threshold (200ms
    /// sleep vs 150ms limit), forcing degraded mode so the clamp activates.
    #[tokio::test]
    async fn test_no_governor_uses_builtin_clamp() {
        let backend = Arc::new(TestBackend);
        let model = backend.load_model("").unwrap();
        let (tx, _rx) = mpsc::channel(4);

        // No governor attached.
        let mut loop_engine = InferenceLoop::new(backend, model, tx);
        let mut stream = SimpleStream { next_id: 0 };

        // Tick 1: fills last_validated_command.
        let _ = loop_engine
            .tick(stream.next_frame().unwrap(), SafetyPosture::Nominal)
            .await
            .unwrap();
        // Tick 2: 200ms inference latency trips degraded mode; clamp fires.
        let snapshot = loop_engine
            .tick(stream.next_frame().unwrap(), SafetyPosture::Nominal)
            .await
            .unwrap();

        assert!(
            snapshot.active_state_degraded,
            "TestBackend's 200ms latency must trigger degraded mode"
        );
        assert_eq!(
            snapshot.active_command.linear_velocity,
            DegradationThresholds::default().max_linear_velocity_mps,
            "Built-in clamp must cap the 65.0 m/s command at max_linear_velocity_mps"
        );
    }

    /// #693: the governorless built-in clamp must bound REVERSE commands too.
    /// `.min(max)` only capped the forward direction, so a -65 m/s command passed
    /// through unclamped. The magnitude clamp bounds it to -max_linear_velocity_mps.
    #[tokio::test]
    async fn test_builtin_clamp_bounds_reverse_velocity() {
        let backend = Arc::new(ReverseTestBackend);
        let model = backend.load_model("").unwrap();
        let (tx, _rx) = mpsc::channel(4);
        let mut loop_engine = InferenceLoop::new(backend, model, tx); // no governor
        let mut stream = SimpleStream { next_id: 0 };

        let _ = loop_engine
            .tick(stream.next_frame().unwrap(), SafetyPosture::Nominal)
            .await
            .unwrap();
        let snapshot = loop_engine
            .tick(stream.next_frame().unwrap(), SafetyPosture::Nominal)
            .await
            .unwrap();

        assert!(snapshot.active_state_degraded, "200ms latency must trigger degraded mode");
        let bound = DegradationThresholds::default().max_linear_velocity_mps;
        assert_eq!(
            snapshot.active_command.linear_velocity, -bound,
            "the -65 m/s reverse command must be clamped to -max_linear_velocity_mps, not pass through"
        );
        assert!(
            snapshot.active_command.linear_velocity.abs() <= bound,
            "clamped reverse speed must be within the magnitude bound"
        );
    }

    #[tokio::test]
    async fn governor_clamps_command_before_degraded_logic() {
        let backend = Arc::new(TestBackend);
        let model = backend.load_model("").unwrap();
        let (tx, mut rx) = mpsc::channel(4);

        let mut loop_engine = InferenceLoop::new(backend, model, tx)
            .with_governor(ClampToTwoGovernor);
        let mut stream = SimpleStream { next_id: 0 };

        let _ = loop_engine.tick(stream.next_frame().unwrap(), SafetyPosture::Nominal).await.unwrap();
        let snapshot = loop_engine.tick(stream.next_frame().unwrap(), SafetyPosture::Nominal).await.unwrap();

        assert_eq!(snapshot.active_command.linear_velocity, 2.0);

        let flushed = rx.recv().await.unwrap();
        assert_eq!(flushed.linear_velocity, 2.0);
    }

    /// L5 — an attached AuditClient records a governor override (and nothing else)
    /// when the governor changes the doer's command.
    #[tokio::test]
    async fn audit_client_records_governor_override() {
        use crate::audit::MockAuditClient;
        let backend = Arc::new(ConfigurableBackend { linear: 10.0 });
        let model = backend.load_model("").unwrap();
        let (tx, _rx) = mpsc::channel(4);
        let mock = Arc::new(MockAuditClient::new());

        let mut loop_engine = InferenceLoop::new(backend, model, tx)
            .with_governor(ClampToTwoGovernor)
            .with_audit_client(mock.clone());
        let mut stream = SimpleStream { next_id: 0 };

        let snap = loop_engine
            .tick(stream.next_frame().unwrap(), SafetyPosture::Nominal)
            .await
            .unwrap();
        assert_eq!(snap.active_command.linear_velocity, 2.0, "governor clamps 10 → 2");

        let (decisions, overrides, faults, health) = mock.counts();
        assert_eq!(
            (decisions, faults, health),
            (0, 0, 0),
            "only an override should be recorded on a clamped tick"
        );
        assert_eq!(overrides, 1, "the governor override must be audited");
        let ov = &mock.overrides()[0];
        assert_eq!(ov.reason, "clamp_linear");
        assert_eq!(ov.proposed_linear_mps, 10.0);
        assert_eq!(ov.commanded_linear_mps, 2.0);
        assert_eq!(ov.posture, SafetyPosture::Nominal);
    }

    /// L5 — an attached AuditClient records a non-finite-inference fault and the
    /// loop still fail-closes to a stopped command.
    #[tokio::test]
    async fn audit_client_records_nonfinite_fault() {
        use crate::audit::MockAuditClient;
        let backend = Arc::new(ConfigurableBackend { linear: f32::NAN });
        let model = backend.load_model("").unwrap();
        let (tx, _rx) = mpsc::channel(4);
        let mock = Arc::new(MockAuditClient::new());

        let mut loop_engine =
            InferenceLoop::new(backend, model, tx).with_audit_client(mock.clone());
        let mut stream = SimpleStream { next_id: 0 };

        let snap = loop_engine
            .tick(stream.next_frame().unwrap(), SafetyPosture::Nominal)
            .await
            .unwrap();
        assert!(snap.active_state_degraded, "nonfinite output → degraded snapshot");
        assert_eq!(snap.active_command.linear_velocity, 0.0, "fail-closed stop");

        let (decisions, overrides, faults, health) = mock.counts();
        assert_eq!((decisions, overrides, health), (0, 0, 0));
        assert_eq!(faults, 1, "the non-finite fault must be audited");
        assert_eq!(mock.faults()[0].code, "nonfinite_command");
    }

    #[tokio::test]
    async fn degraded_mode_clamps_overspeed_commands() {
        let backend = Arc::new(TestBackend);
        let model = backend.load_model("").unwrap();
        let (tx, mut rx) = mpsc::channel(4);

        let mut loop_engine = InferenceLoop::new(backend, model, tx);
        let mut stream = SimpleStream { next_id: 0 };

        // First tick: fills last_validated_command, sends nothing yet.
        let _ = loop_engine.tick(stream.next_frame().unwrap(), SafetyPosture::Nominal).await.unwrap();

        // Second tick: sends previous command, computes a new clamped one.
        let snapshot = loop_engine.tick(stream.next_frame().unwrap(), SafetyPosture::Nominal).await.unwrap();

        assert!(snapshot.active_state_degraded, "expected degraded mode");
        assert!(snapshot.active_command.linear_velocity <= 1.5);
        assert_eq!(snapshot.active_command.linear_velocity, 1.5);

        let flushed = rx.recv().await.unwrap();
        assert!(flushed.linear_velocity <= 1.5);
    }

    // ── PARK-004 test helpers ────────────────────────────────────────────────

    /// Backend that returns a configurable linear velocity; no sleep.
    struct ConfigurableBackend {
        linear: f32,
    }

    impl InferenceBackend for ConfigurableBackend {
        fn load_model(&self, _: &str) -> Result<ModelHandle, BackendError> {
            let mut inputs = HashMap::new();
            inputs.insert("image_input".to_string(), vec![1, 3, 224, 224]);
            Ok(ModelHandle {
                model_id: "configurable".to_string(),
                input_shapes: inputs,
                output_shapes: HashMap::new(),
                expected_precision: PrecisionMode::FP32,
            })
        }

        fn run(&self, _: &ModelHandle, _: &TensorBatch) -> Result<TensorBatch<'static>, BackendError> {
            let mut map = HashMap::new();
            map.insert("cmd_vel_linear".to_string(), TensorStorage::Owned(vec![self.linear]));
            map.insert("cmd_vel_angular".to_string(), TensorStorage::Owned(vec![0.0_f32]));
            Ok(TensorBatch { named_tensors: map, metadata: HashMap::new() })
        }

    }

    /// Governor that records the proposed linear velocity and allows the command through.
    struct RecordingGovernor {
        recorded: Arc<Mutex<Option<f64>>>,
    }

    impl SafetyGovernor for RecordingGovernor {
        fn evaluate(
            &self,
            proposed: &ControlCommand,
            _previous: Option<&ControlCommand>,
            _delta_time_s: f64,
            _posture: SafetyPosture,
        ) -> EnforcementAction {
            *self.recorded.lock().unwrap() = Some(proposed.linear_velocity);
            EnforcementAction::Allow
        }
    }

    // ── PARK-004 proptest: NaN/Inf/subnormal model outputs ──────────────────

    proptest! {
        #[test]
        fn nan_or_inf_model_output_produces_stopped_command(
            val in prop_oneof![
                Just(f32::NAN),
                Just(f32::INFINITY),
                Just(f32::NEG_INFINITY),
            ]
        ) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let (linear_vel, degraded) = rt.block_on(async {
                let backend = Arc::new(ConfigurableBackend { linear: val });
                let model = backend.load_model("").unwrap();
                let (tx, _rx) = mpsc::channel(4);
                let mut loop_engine = InferenceLoop::new(backend, model, tx);
                let mut stream = SimpleStream { next_id: 0 };
                let snapshot = loop_engine
                    .tick(stream.next_frame().unwrap(), SafetyPosture::Nominal)
                    .await
                    .unwrap();
                (snapshot.active_command.linear_velocity, snapshot.active_state_degraded)
            });
            prop_assert_eq!(
                linear_vel, 0.0,
                "NaN/Inf model output must produce stopped command (0.0), got {} for input {}",
                linear_vel, val
            );
            prop_assert!(
                degraded,
                "NaN/Inf model output must set active_state_degraded=true, input={}",
                val
            );
        }

        #[test]
        fn subnormal_model_output_does_not_panic(
            val in prop::num::f32::SUBNORMAL
        ) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let backend = Arc::new(ConfigurableBackend { linear: val });
                let model = backend.load_model("").unwrap();
                let (tx, _rx) = mpsc::channel(4);
                let mut loop_engine = InferenceLoop::new(backend, model, tx);
                let mut stream = SimpleStream { next_id: 0 };
                // Must not panic.
                let _ = loop_engine
                    .tick(stream.next_frame().unwrap(), SafetyPosture::Nominal)
                    .await
                    .unwrap();
            });
        }
    }

    // ── PARK-004 unit test: finite value reaches governor unchanged ──────────

    /// A valid finite model output must pass through to the governor unmodified.
    #[tokio::test]
    async fn valid_input_reaches_governor_unchanged() {
        let recorded = Arc::new(Mutex::new(None::<f64>));
        let governor = RecordingGovernor { recorded: Arc::clone(&recorded) };

        let backend = Arc::new(ConfigurableBackend { linear: 3.0_f32 });
        let model = backend.load_model("").unwrap();
        let (tx, _rx) = mpsc::channel(4);

        let mut loop_engine = InferenceLoop::new(backend, model, tx)
            .with_governor(governor);
        let mut stream = SimpleStream { next_id: 0 };

        let _ = loop_engine
            .tick(stream.next_frame().unwrap(), SafetyPosture::Nominal)
            .await
            .unwrap();

        let received = recorded.lock().unwrap().expect("governor must have been called");
        assert_eq!(
            received, 3.0_f64,
            "governor must receive the exact proposed velocity from model output, got {}",
            received
        );
    }
}
