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
use crate::scene_vetoes::{
    apply_commit_zone_gate, apply_occlusion_gate, apply_water_gate, StampedScene,
};
use crate::taj_objects::{
    apply_object_rss_gate, courier_rss_params, object_snapshot_to_vanished_scene, ObjectSnapshot,
};
use crate::tick_pipeline::{current_time_ms, TickError};
use parko_core::commit_zone::{CommitZoneCfg, CommitZoneScene};
use parko_core::rss::OcclusionScene;
use parko_core::water::{WaterScene, WaterVetoConfig};
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
    // EP-05: the OOD input-shift monitor (`KIRRA_OOD_ENABLED`). Resolved FIRST —
    // fail-closed: an armed gate whose calibration baseline is missing or
    // unloadable aborts startup here, before any task spawns, rather than
    // running with a monitor that silently isn't watching.
    let drain_ood = crate::ood_feed::ood_feed_from_env()?;
    match &drain_ood {
        Some(_) => tracing::info!(
            "parko-ros2: OOD input-shift monitor ARMED (corridor confidence → PSI window; \
             distribution drift escalates the tick posture, escalation-only)"
        ),
        None => tracing::info!(
            gate = crate::ood_feed::KIRRA_OOD_ENABLED_ENV,
            "parko-ros2: OOD input-shift monitor OFF — input-distribution drift is not watched"
        ),
    }

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
        // #309: the SG6 vanished-object detector is fed an AgentScene per tick
        // when armed (`vanished_detection_enabled`) AND object perception is
        // available (lidar + platform_profile → the Taj object snapshot the scene
        // is sourced from). Otherwise it stays unfed — reduced coverage, stated
        // loudly. (Arming the latching auto-immobilizer happens in the bin's
        // `build_node_clearance` via `with_vanished_detection`, gated the same way.)
        if config.vanished_detection_enabled
            && config.lidar_topic.is_some()
            && config.platform_profile.is_some()
        {
            tracing::info!(
                "parko-ros2: SG6 vanished-object detection ARMED (#309) — the node sources an \
                 AgentScene from Taj objects each tick; a close agent that VANISHES between \
                 frames latches the clearance loop (operator grant required to clear)."
            );
        } else if config.vanished_detection_enabled {
            tracing::warn!(
                "parko-ros2: SG6 vanished-object detection REQUESTED but object perception is \
                 not configured (needs lidar_topic + platform_profile) — REDUCED coverage; the \
                 detector stays unfed (#309)."
            );
        } else {
            tracing::warn!(
                "parko-ros2: SG6 vanished-object detection NOT enabled — REDUCED detection \
                 coverage; set vanished_detection_enabled (with lidar + platform_profile) to arm (#309)."
            );
        }
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
            // Populate the object slot when EITHER object-axis consumer needs it:
            // the SG1 object-RSS gate or the SG6 vanished-object detector (#309).
            let store_objects = config.needs_object_snapshot();
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

    // WS-0.1 scene-veto channels (occlusion / water / commit-zone). Each slot
    // is where a future producer subscription publishes its latest stamped
    // scene (the `latest_objects` pattern). The gates are ARMED by config;
    // while armed, an empty/stale slot fails CLOSED inside the gate (stop) —
    // the enabled-but-silent rule. Not armed → the gate is never called.
    let latest_occlusion: Arc<StdMutex<Option<StampedScene<OcclusionScene>>>> =
        Arc::new(StdMutex::new(None));
    let latest_water: Arc<StdMutex<Option<StampedScene<WaterScene>>>> =
        Arc::new(StdMutex::new(None));
    let latest_commit_zone: Arc<StdMutex<Option<StampedScene<CommitZoneScene>>>> =
        Arc::new(StdMutex::new(None));
    // Occlusion needs the RSS params → armed only with a platform_profile
    // (same precedent as the object gate). Enabled without a profile → warn.
    let drain_occlusion_params = config
        .platform_profile
        .as_ref()
        .filter(|_| config.occlusion_gate_enabled)
        .map(courier_rss_params);
    if config.occlusion_gate_enabled {
        match &drain_occlusion_params {
            Some(_) => tracing::warn!(
                "parko-ros2: WS-0.1 occlusion gate ARMED — a missing/stale sightline scene \
                 fails closed to a STOP; ensure a producer feeds the occlusion slot"
            ),
            None => tracing::warn!(
                "parko-ros2: occlusion_gate_enabled but no platform_profile (no RSS params) — \
                 occlusion gate NOT armed"
            ),
        }
    }
    if config.water_gate_enabled {
        tracing::warn!(
            "parko-ros2: WS-0.1 SG4 water gate ARMED — a missing/stale water scene fails \
                 closed to a STOP; ensure a producer feeds the water slot"
        );
    }
    if config.commit_zone_gate_enabled {
        tracing::warn!(
            "parko-ros2: WS-0.1 SG5 commit-zone gate ARMED — a missing/stale zone scene fails \
                 closed to a STOP; ensure a producer feeds the commit-zone slot"
        );
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
    // #309: SG6 vanished-object scene sourcing — armed under the SAME condition as
    // the bin's `with_vanished_detection` (enabled + object perception present),
    // so the per-tick scene is built only when the detector exists to consume it.
    let drain_vanished_armed = config.vanished_detection_enabled
        && config.platform_profile.is_some()
        && config.lidar_topic.is_some();
    let drain_occlusion = Arc::clone(&latest_occlusion);
    let drain_water = Arc::clone(&latest_water);
    let drain_commit_zone = Arc::clone(&latest_commit_zone);
    let drain_water_armed = config.water_gate_enabled;
    let drain_commit_zone_armed = config.commit_zone_gate_enabled;

    let drain_task = tokio::spawn(async move {
        use futures::StreamExt;
        // The node-owned clearance gate (Phase-B). Owned by the single drain task
        // so the per-tick `&mut` borrow needs no extra locking.
        let mut clearance = clearance;
        // EP-05: the OOD feed is likewise owned by the single drain task
        // (per-tick `&mut` observe/assess, no lock).
        let mut drain_ood = drain_ood;
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

            // #309: source the SG6 vanished-object scene from the latest Taj
            // objects (the SAME snapshot the object-RSS gate uses below). A
            // missing/stale snapshot → `AgentScene::Absent` (a gap; the detector
            // never fabricates a latch). Only when armed; otherwise `None` (the
            // detector stays unfed, byte-identical to pre-#309).
            //
            // Build the scene while holding the lock BRIEFLY (over an `&` borrow)
            // rather than cloning the whole `ObjectSnapshot` — `object_snapshot_to_
            // vanished_scene` returns an OWNED `AgentScene`, so no snapshot copy is
            // needed (Copilot PR #716). The guard drops at the end of the closure,
            // well before the `.await` below (no std-mutex held across await).
            let vanished_scene = drain_vanished_armed.then(|| {
                let now = current_time_ms();
                let guard = drain_objects.lock().ok();
                let snap: Option<&ObjectSnapshot> = guard.as_ref().and_then(|g| g.as_ref());
                object_snapshot_to_vanished_scene(snap, drain_config.corridor_max_age_ms, now)
            });

            // EP-05: feed the freshest corridor confidence into the OOD window
            // and fold the assessment into THIS tick's posture (escalation-only —
            // a stable window never relaxes the source). No corridor snapshot →
            // no sample this tick (an under-filled window is a no-op inside the
            // monitor; a PERSISTENTLY absent corridor is the containment gate's
            // fail-closed concern, never fabricated OOD evidence).
            let tick_posture = match drain_ood.as_mut() {
                Some(feed) => {
                    let conf = drain_corridor
                        .lock()
                        .ok()
                        .and_then(|g| g.as_ref().map(|snap| f64::from(snap.confidence())));
                    if let Some(c) = conf {
                        feed.observe(c);
                    }
                    let (effective, assessment) = feed.escalate(posture);
                    if effective != posture {
                        tracing::warn!(
                            psi = assessment.psi,
                            reason = ?assessment.reason,
                            source = ?posture,
                            effective = ?effective,
                            "parko-ros2: OOD monitor escalated the tick posture \
                             (input-distribution drift)"
                        );
                    }
                    effective
                }
                None => posture,
            };

            let cleared = run_pipeline_tick_with_clearance(
                &drain_config,
                Arc::clone(&drain_infer),
                frame,
                tick_posture,
                clearance.as_mut(),
                &impact_inputs,
                vanished_scene.as_ref(),
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

            // WS-0.1 scene-veto gates (occlusion / water / commit-zone),
            // composed after the object gate on the same publication seam.
            // Armed-when-configured; inside an armed gate a missing/stale
            // scene fails closed (stop). The brief std-mutex locks release
            // before any `.await`.
            let outcome = match &drain_occlusion_params {
                Some(params) => apply_occlusion_gate(
                    outcome,
                    drain_occlusion.lock().ok().and_then(|g| g.clone()).as_ref(),
                    params,
                    drain_config.corridor_max_age_ms,
                    current_time_ms(),
                ),
                None => outcome,
            };
            let outcome = if drain_water_armed {
                apply_water_gate(
                    outcome,
                    drain_water.lock().ok().and_then(|g| g.clone()).as_ref(),
                    &WaterVetoConfig::default(),
                    drain_config.corridor_max_age_ms,
                    current_time_ms(),
                )
            } else {
                outcome
            };
            let outcome = if drain_commit_zone_armed {
                apply_commit_zone_gate(
                    outcome,
                    drain_commit_zone.lock().ok().and_then(|g| g.clone()).as_ref(),
                    &CommitZoneCfg::default(),
                    drain_config.corridor_max_age_ms,
                    current_time_ms(),
                )
            } else {
                outcome
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
                    TickError::OcclusionBreach =>
                        tracing::warn!(frame_id,
                            "parko-ros2: RSS rule-iv occlusion breach (command speed above the \
                             assured-clear-distance cap, or the sightline scene was absent/stale); \
                             publishing MRC"),
                    TickError::WaterVeto =>
                        tracing::warn!(frame_id,
                            "parko-ros2: SG4 water veto (untraversable signature, or the water \
                             scene was absent/stale); publishing MRC (stop short of water)"),
                    TickError::CommitZoneVeto =>
                        tracing::warn!(frame_id,
                            "parko-ros2: SG5 commit-zone veto (entry blocked, or the zone scene / \
                             map was absent/stale); publishing MRC (stop short of the zone)"),
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
