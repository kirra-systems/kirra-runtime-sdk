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

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
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
use crate::containment_gate::{apply_containment_gate, CONTAINMENT_HORIZON_S, CONTAINMENT_STEP_S};
use crate::imu_shim::imu_msg_to_sample;
use crate::sensor_mapping::{ImuSample, SensorInputMapping};
use crate::taj_corridor::{laserscan_msg_to_taj, CorridorSnapshot, EGO_REAR_COVER_M};
use crate::taj_objects::{apply_object_rss_gate, courier_rss_params, ObjectSnapshot};
use crate::tick_pipeline::{current_time_ms, TickError};
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
    // #324: arrival time (ms) of the LATEST IMU sample, 0 = none yet. The drain
    // stamps the gate's staleness watchdog from this; a silent IMU stops advancing
    // it, so the watchdog goes stale and the gate forces the MRC.
    let imu_arrival: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    // Contact is sticky-until-read so a sub-tick pulse is not lost — see
    // [`ContactCell`] (#320), the CI-tested semantics this transport just wires.
    let contact_state: Arc<ContactCell> = Arc::new(ContactCell::new());

    if detection_enabled {
        match &config.imu_topic {
            Some(topic) => {
                let imu_stream =
                    node.subscribe::<r2r::sensor_msgs::msg::Imu>(topic, r2r::QosProfile::default())?;
                let cell = Arc::clone(&latest_imu);
                let arrival = Arc::clone(&imu_arrival);
                tokio::spawn(async move {
                    use futures::StreamExt;
                    let mut s = imu_stream;
                    while let Some(msg) = s.next().await {
                        let sample = imu_msg_to_sample(&msg);
                        if let Ok(mut g) = cell.lock() {
                            *g = Some(sample);
                        }
                        // #324: record freshness so the watchdog can detect a stall.
                        arrival.store(current_time_ms(), AtomicOrdering::Release);
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

    // --- ADR-0029 Phase 3b: live SG2 containment (lidar → Taj corridor) ---
    // Armed only when BOTH a lidar topic and a platform_profile (for the
    // footprint) are configured. A background task runs Taj Phase-A on each
    // scan and stores the latest ego-relative corridor; the drain loop gates
    // the governed command against it. A missing/stale/low-confidence corridor
    // fails closed INSIDE the gate (MRC). Opt-in: no lidar_topic → no gate.
    let latest_corridor: Arc<StdMutex<Option<CorridorSnapshot>>> = Arc::new(StdMutex::new(None));
    // ADR-0029 Phase 3b (object axis): the latest perceived-object snapshot, fed
    // from the SAME lidar/Taj task as the corridor. Armed only when the operator
    // opts in (`object_rss_enabled`) AND lidar + footprint are present.
    let latest_objects: Arc<StdMutex<Option<ObjectSnapshot>>> = Arc::new(StdMutex::new(None));
    let gate_footprint = config.platform_profile.as_ref().map(|p| p.footprint());
    match (&config.lidar_topic, gate_footprint.is_some()) {
        (Some(topic), true) => {
            let scan_stream = node
                .subscribe::<r2r::sensor_msgs::msg::LaserScan>(topic, r2r::QosProfile::default())?;
            let cell = Arc::clone(&latest_corridor);
            let obj_cell = Arc::clone(&latest_objects);
            let store_objects = config.object_rss_enabled;
            // The TEMPORAL tracker (not the single-frame `TajPhaseA`): `track`
            // wraps `phase_a.process`, so the CORRIDOR is byte-identical, but it
            // also associates objects frame-to-frame and estimates each object's
            // ground velocity — what the RSS object gate needs to bound MOVING
            // objects (a first-sighting object is velocity-0 → treated static,
            // conservative). Single drain owner → the `&mut self` needs no lock.
            let mut taj = kirra_taj::TajTracker::new(kirra_taj::TajConfig::default());
            tokio::spawn(async move {
                use futures::StreamExt;
                let mut s = scan_stream;
                while let Some(msg) = s.next().await {
                    let scan = laserscan_msg_to_taj(&msg);
                    // ONE Phase-A pass (inside `track`) feeds both seams: the
                    // corridor (with ego rear cover) and — when armed — the
                    // velocity-carrying perceived objects.
                    let perception = taj.track(&scan, current_time_ms());
                    let snap = CorridorSnapshot::from_taj(&perception.corridor)
                        .with_ego_rear_cover(EGO_REAR_COVER_M);
                    if let Ok(mut g) = cell.lock() {
                        *g = Some(snap);
                    }
                    if store_objects {
                        let obj_snap =
                            ObjectSnapshot::from_objects(&perception.objects, perception.stamp_ms);
                        if let Ok(mut g) = obj_cell.lock() {
                            *g = Some(obj_snap);
                        }
                    }
                }
                tracing::warn!(
                    "parko-ros2: lidar stream closed — containment corridor + object perception now \
                     unfed; a stale corridor/object snapshot fails closed (MRC) inside the gates"
                );
            });
            tracing::info!(topic = %topic, object_rss = config.object_rss_enabled,
                "parko-ros2: SG2 containment gate ARMED (lidar → Taj Phase-A corridor); \
                 SG1 object-RSS gate armed iff object_rss");
        }
        (Some(_), false) => tracing::warn!(
            "parko-ros2: lidar_topic set but no platform_profile (no footprint) — SG2 containment \
             gate NOT armed"
        ),
        (None, _) => tracing::info!(
            "parko-ros2: SG2 containment gate not configured (no lidar_topic) — REDUCED coverage; \
             the drivable-space check is inactive"
        ),
    }

    // --- Drain task: consume the sensor stream, tick, publish ---------
    let drain_config  = Arc::clone(&config);
    let drain_infer   = Arc::clone(&infer);
    let drain_mapping = Arc::clone(&mapping);
    let drain_imu     = Arc::clone(&latest_imu);
    let drain_imu_arrival = Arc::clone(&imu_arrival);
    let drain_contact = Arc::clone(&contact_state);
    let drain_corridor = Arc::clone(&latest_corridor);
    let drain_footprint = gate_footprint;
    // Object-RSS gate: armed (Some params) only when opted in AND lidar +
    // footprint are configured — otherwise the slot would never be fed and the
    // gate would MRC forever. `None` → byte-identical (gate skipped).
    let drain_objects = Arc::clone(&latest_objects);
    let drain_object_params = config
        .platform_profile
        .as_ref()
        .filter(|_| config.object_rss_enabled && config.lidar_topic.is_some())
        .map(courier_rss_params);

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

            // #324: stamp the IMU staleness watchdog from the latest arrival time. A
            // no-op when no IMU is configured (watchdog unarmed). When the IMU stream
            // stalls, `arrival` stops advancing and the watchdog forces the MRC.
            if let Some(c) = clearance.as_mut() {
                let arrived = drain_imu_arrival.load(AtomicOrdering::Acquire);
                if arrived != 0 {
                    c.stamp_imu(arrived);
                }
            }

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

            // ADR-0029 Phase 3b: gate the governed command against the live
            // ego-relative corridor. No corridor configured/available → no-op
            // (the command passes through). A breach / stale / low-confidence
            // corridor → MRC (stopped twist + TickError::ContainmentBreach).
            let outcome = match (drain_footprint, drain_corridor.lock().ok().and_then(|g| g.clone())) {
                (Some(fp), Some(snap)) => apply_containment_gate(
                    outcome,
                    &fp,
                    &snap.to_corridor(
                        drain_config.corridor_min_confidence,
                        drain_config.corridor_max_age_ms,
                    ),
                    CONTAINMENT_HORIZON_S,
                    CONTAINMENT_STEP_S,
                ),
                _ => outcome,
            };

            // ADR-0029 Phase 3b (object axis): bound the governed command against
            // Taj's perceived objects via RSS. Armed only when `drain_object_params`
            // is set; a missing/stale object snapshot fails closed (MRC) INSIDE the
            // gate. No params → skipped (byte-identical).
            let outcome = match &drain_object_params {
                Some(params) => apply_object_rss_gate(
                    outcome,
                    drain_objects.lock().ok().and_then(|g| g.clone()).as_ref(),
                    params,
                    drain_config.corridor_max_age_ms,
                    current_time_ms(),
                ),
                None => outcome,
            };

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
                    TickError::ContainmentBreach =>
                        tracing::warn!(frame_id,
                            "parko-ros2: SG2 containment breach (command lookahead left the \
                             ego corridor, or the corridor was stale/low-confidence); publishing MRC"),
                    TickError::ObjectRssBreach =>
                        tracing::warn!(frame_id,
                            "parko-ros2: SG1 object-RSS breach (a perceived object made the command \
                             unsafe, or object perception was absent/stale); publishing MRC"),
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
