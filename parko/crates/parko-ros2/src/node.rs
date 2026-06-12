// parko/crates/parko-ros2/src/node.rs
//
// r2r-backed Parko ROS 2 node. Feature-gated on `ros2`. Default cargo
// builds (and CI) do not compile this file.
//
// Responsibility:
//   - r2r context + node init
//   - Subscribe to the configured sensor topic; per-message →
//     `SensorInputMapping::to_frame` → `run_pipeline_tick`.
//   - Publish each tick's `OutgoingTwist` to the configured actuator
//     topic.
//   - Honour shutdown signals from the binary (the `JoinHandle::abort`
//     path).
//
// What this file does NOT do:
//   - Carry safety logic. The fail-closed paths live in
//     `tick_pipeline.rs` and parko-kirra; this module is pure transport.
//   - Subscribe to the verifier's posture stream. M1b's PostureTracker
//     is the reusable mechanism — wiring it into this node is the
//     next-milestone deliverable. For M2 the node accepts a posture
//     parameter (CLI / env) and defaults to `Nominal`.

use std::sync::{Arc, Mutex as StdMutex};

use parko_core::backend::InferenceBackend;
use parko_core::safety::SafetyPosture;
use parko_core::scheduler::InferenceLoop;
use tokio::sync::Mutex;

use crate::clearance_gate::{
    run_pipeline_tick_with_clearance, ContactCell, ImpactInputs, NodeClearance,
};
use crate::command_mapping::OutgoingTwist;
use crate::config::ParkoNodeConfig;
use crate::imu_shim::imu_msg_to_sample;
use crate::sensor_mapping::{ImuSample, SensorInputMapping};
use crate::tick_pipeline::TickError;
use parko_kirra::clearance_delivery::DeliveryOutcome;

/// Run the Parko ROS 2 node. Owns the r2r context for the lifetime of
/// the returned future. Cancelling the future drops the node + the
/// subscriptions + the publisher.
///
/// `infer` is an `Arc<Mutex<InferenceLoop<B>>>` built by the binary;
/// the loop has its governor and tick period pre-attached. `mapping`
/// is the integrator's sensor → frame mapper. `posture` is the
/// **static** posture for now (M1b wiring is a follow-up; see the
/// module-level note).
///
/// `clearance` is the node-owned [`NodeClearance`] (#304 Phase-B): when `Some`,
/// every tick polls for a console-recorded operator grant (delivering it on this
/// node's own tick) and forces a stopped command while the loop is immobilized
/// — alongside the LockedOut stop path. `None` is the dev lane (delivery
/// disabled); the binary warns when it constructs `None`.
pub async fn run_node<B, M>(
    config:  Arc<ParkoNodeConfig>,
    infer:   Arc<Mutex<InferenceLoop<B>>>,
    mapping: Arc<M>,
    posture: SafetyPosture,
    clearance: Option<NodeClearance>,
    node_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    B: InferenceBackend + 'static,
    M: SensorInputMapping<Sample = Vec<f32>> + 'static,
{
    let ctx = r2r::Context::create()?;
    let mut node = r2r::Node::create(ctx, node_name, "parko")?;

    // --- Subscriptions ------------------------------------------------
    //
    // The sensor topic carries the integrator's observation message.
    // For M2 we expect a vector-of-f32 payload via a JSON-wrapped
    // sensor msg (project-local convention). Production integrators
    // swap this for their typed message + a corresponding
    // `SensorInputMapping<Sample = TheirMsg>`.
    let sensor_sub = node.subscribe_untyped(
        &config.sensor_topic,
        // The wire type is integrator-defined; we use a JSON-shaped
        // message so the M2 default works with any publisher that can
        // emit a JSON array of floats. Integrators replace this with
        // their own message + mapping.
        "std_msgs/msg/Float32MultiArray",
        r2r::QosProfile::default(),
    )?;

    // --- Publisher ----------------------------------------------------
    let cmd_pub = node.create_publisher_untyped(
        &config.command_topic,
        "geometry_msgs/msg/Twist",
        r2r::QosProfile::default(),
    )?;

    // --- SG6 impact-detection sources (#309) --------------------------
    //
    // Optional IMU (decel) + contact subscriptions feed the per-tick
    // `ImpactInputs`. Each runs a background drain task; the IMU task writes the
    // LATEST sample (a spike present at read time survives), the contact task is
    // STICKY-UNTIL-READ (#320) so a sub-tick pulse is not lost. A missing source
    // is REDUCED detection coverage, logged loudly — never a fabricated reading
    // (absent IMU → no decel; absent contact → `false`). Detection only matters
    // when the clearance gate is active.
    let detection_enabled = clearance.is_some();
    let latest_imu: Arc<StdMutex<Option<ImuSample>>> = Arc::new(StdMutex::new(None));
    // Contact is sticky-until-read so a sub-tick pulse is not lost — see
    // [`ContactCell`] (#320), the CI-tested semantics this transport just wires.
    let contact_state: Arc<ContactCell> = Arc::new(ContactCell::new());

    if detection_enabled {
        match &config.imu_topic {
            Some(topic) => {
                let imu_stream =
                    node.subscribe::<r2r::sensor_msgs::msg::Imu>(topic, r2r::QosProfile::default())?;
                let cell = Arc::clone(&latest_imu);
                tokio::spawn(async move {
                    use futures::StreamExt;
                    let mut s = imu_stream;
                    while let Some(msg) = s.next().await {
                        let sample = imu_msg_to_sample(&msg);
                        if let Ok(mut g) = cell.lock() {
                            *g = Some(sample);
                        }
                    }
                    tracing::warn!("parko-ros2: IMU stream closed — decel detection now inactive");
                });
                tracing::info!(topic = %topic,
                    "parko-ros2: SG6 decel detection ARMED (IMU → spike-magnitude latch)");
            }
            None => tracing::warn!(
                "parko-ros2: SG6 decel detection NOT wired (no imu_topic) — REDUCED detection \
                 coverage; the clearance loop will not latch on hard deceleration."
            ),
        }
        match &config.contact_topic {
            Some(topic) => {
                let contact_stream =
                    node.subscribe::<r2r::std_msgs::msg::Bool>(topic, r2r::QosProfile::default())?;
                let cell = Arc::clone(&contact_state);
                tokio::spawn(async move {
                    use futures::StreamExt;
                    let mut s = contact_stream;
                    while let Some(msg) = s.next().await {
                        cell.assert(msg.data); // sticky-until-read (#320)
                    }
                    tracing::warn!("parko-ros2: contact stream closed — contact detection now inactive");
                });
                tracing::info!(topic = %topic, "parko-ros2: SG6 contact detection ARMED");
            }
            None => tracing::warn!(
                "parko-ros2: SG6 contact detection NOT wired (no contact_topic) — REDUCED detection \
                 coverage; the clearance loop will not latch on a contact-sensor hit."
            ),
        }
        // The vanished-object trigger needs an AgentScene per tick; no scene
        // source flows through the M2 tick yet (the #309 remainder). The node
        // passes `scene = None`; the detector stays unfed.
        tracing::warn!(
            "parko-ros2: SG6 vanished-object detection NOT wired (no AgentScene source in the \
             tick) — REDUCED detection coverage; tracked as the remainder of #309."
        );
    }

    // --- Drain task: consume the sensor stream, tick, publish ---------
    let drain_config  = Arc::clone(&config);
    let drain_infer   = Arc::clone(&infer);
    let drain_mapping = Arc::clone(&mapping);
    let drain_imu     = Arc::clone(&latest_imu);
    let drain_contact = Arc::clone(&contact_state);

    let drain_task = tokio::spawn(async move {
        use futures::StreamExt;
        // The node-owned clearance gate (Phase-B). Owned by the single drain task
        // so the per-tick `&mut` borrow needs no extra locking.
        let mut clearance = clearance;
        let mut frame_id: u64 = 0;
        let mut stream = sensor_sub;
        while let Some(msg) = stream.next().await {
            let msg = match msg {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = ?e,
                        "parko-ros2 sensor stream error; sensor staleness will derate to stop");
                    continue;
                }
            };
            // Project-local JSON shape: { "data": [f32, ...], "stamp_ms": u64 }
            let sample_vec: Vec<f32> = msg.get("data")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|n| n.as_f64().map(|f| f as f32)).collect())
                .unwrap_or_default();
            let stamp_ms: u64 = msg.get("stamp_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            frame_id = frame_id.saturating_add(1);
            let frame = drain_mapping.to_frame(frame_id, stamp_ms, &sample_vec);

            // Assemble this tick's SG6 evidence from the latest sensor readings.
            // Absent sources read as their no-signal defaults (no fabrication).
            let impact_inputs = ImpactInputs {
                imu: drain_imu.lock().ok().and_then(|g| *g),
                contact: drain_contact.drain(), // sticky-until-read drain (#320)
            };

            let cleared = run_pipeline_tick_with_clearance(
                &drain_config,
                Arc::clone(&drain_infer),
                frame,
                posture,
                clearance.as_mut(),
                &impact_inputs,
                None, // AgentScene source deferred (#309 remainder)
            ).await;
            let outcome = cleared.tick;

            // Surface the per-tick clearance delivery (a console-recorded grant
            // arriving on this node's own tick). A `NoGrant` no-op is silent.
            match &cleared.delivery {
                Some(DeliveryOutcome::Cleared { operator_id, grant_rowid }) =>
                    tracing::info!(operator_id = %operator_id, grant_rowid,
                        "parko-ros2: operator clearance DELIVERED — loop cleared, motion released"),
                Some(DeliveryOutcome::Rejected { reason, grant_rowid }) =>
                    tracing::warn!(reason = %reason, grant_rowid,
                        "parko-ros2: clearance grant REJECTED at delivery (consumed, not retried) — \
                         operator must re-issue in the console"),
                Some(DeliveryOutcome::StoreError) =>
                    tracing::error!("parko-ros2: clearance store error — fail-closed (nothing cleared)"),
                Some(DeliveryOutcome::NoGrant) | None => {}
            }
            if cleared.vetoed {
                tracing::warn!(frame_id,
                    "parko-ros2: clearance veto ACTIVE (post-collision loop immobilized) — \
                     publishing stop regardless of posture until an operator grant is delivered");
            }

            if let Some(err) = &outcome.error {
                match err {
                    TickError::StaleSensorInput { frame_id, frame_age_ms, budget_ms } =>
                        tracing::warn!(frame_id, frame_age_ms, budget_ms,
                            "parko-ros2: sensor input stale; publishing MRC"),
                    TickError::InferenceError(msg) =>
                        tracing::error!(error = %msg,
                            "parko-ros2: inference error; publishing MRC"),
                }
            }

            // Publish the gated twist (always — happy path OR MRC OR clearance veto).
            if let Err(e) = publish_twist(&cmd_pub, outcome.twist) {
                tracing::warn!(error = ?e,
                    "parko-ros2: failed to publish OutgoingTwist; \
                     the next tick will retry");
            }
        }
        tracing::error!("parko-ros2: sensor subscription stream closed — \
                         tick loop exiting; the actuator will see staleness");
    });

    // Drive the r2r executor until the drain task ends or we're
    // cancelled by the binary's shutdown handler.
    let spin_task = tokio::task::spawn_blocking(move || {
        loop {
            node.spin_once(std::time::Duration::from_millis(50));
        }
    });

    tokio::select! {
        _ = drain_task => {}
        _ = spin_task  => {}
    }
    Ok(())
}

/// Map an `OutgoingTwist` to the geometry_msgs/Twist JSON envelope that
/// r2r's `create_publisher_untyped` expects, and publish it.
fn publish_twist(
    publisher: &r2r::PublisherUntyped,
    twist: OutgoingTwist,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use serde_json::json;
    let payload = json!({
        "linear":  { "x": twist.linear_x_mps,  "y": 0.0, "z": 0.0 },
        "angular": { "x": 0.0, "y": 0.0,        "z": twist.angular_z_rads },
    });
    publisher.publish(payload)
        .map_err(|e| Box::<dyn std::error::Error + Send + Sync>::from(format!("publish: {e:?}")))
}
