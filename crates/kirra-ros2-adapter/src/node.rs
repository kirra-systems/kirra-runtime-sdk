// crates/kirra-ros2-adapter/src/node.rs
//
// S131 Phase 1 — r2r-backed ROS 2 node skeleton.
//
// Feature-gated behind `ros2`. Default builds (including the safety
// kernel's CI) do not pull r2r and do not compile this module.
//
// Phase 1 scope (skeleton only):
//   - Create an `r2r::Node` and instantiate the five Autoware-shaped
//     subscriptions agreed in the design doc:
//        ~/input/trajectory   (autoware_planning_msgs/Trajectory)
//        ~/input/objects      (autoware_perception_msgs/PredictedObjects)
//        ~/input/map          (autoware_map_msgs/LaneletMapBin)
//        ~/input/odometry     (nav_msgs/Odometry)
//        ~/input/control_cmd  (autoware_control_msgs/Control)
//   - Wire the internal mpsc channels:
//        trajectory_tx / trajectory_rx → slow-loop validator task
//        control_tx    / control_rx    → fast-loop conformance task
//   - Spawn the slow- and fast-loop task stubs. They log receipt only.
//   - Bounded channels with drop-on-full + a loud `tracing::warn!`
//     (matches the audit-writer pattern from Pass B2: never block the
//     hot path; surface the drop loudly).
//
// Phase 2 will turn the slow loop into a real
// `validate_trajectory_containment` + `validate_vehicle_command` + RSS
// driver. Phase 3 turns the fast loop into the conformance check.
//
// IMPORTANT: each subscriber is currently registered with a String-typed
// placeholder message name. The exact r2r message-type imports require
// the integrator's Autoware install to be on the `AMENT_PREFIX_PATH` at
// build time; we deliberately keep them as untyped subscriptions here
// so the Phase-1 skeleton compiles on a stock cargo install. Phase 2
// swaps these for the typed forms once the integrator's package paths
// are pinned.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::corridor::CorridorSource;
use crate::state::{
    AdaptorState, TrajectoryPoint, TrajectoryVerdict,
    SUBSCRIPTION_STALENESS_TIMEOUT_MS,
};
use crate::validation::{
    check_command_conforms, validate_trajectory_slow_capped, ConformanceVerdict, IncomingControl,
};
// KIRRA-OCCY-PMON-003 slice-1: pure ingest orchestration (safety logic lives
// in `perception_ingest` + the kernel; this node only forwards to them).
use crate::perception_ingest::{perception_derate_enabled, publish_perception_tick};
use kirra_core::perception_monitor::{
    empty_perception_cap, resolve_perception_cap, KinematicPlausibilityContract,
    PerceptionCapPublisher,
};
// Learning-loop capture (Phase 1.5, docs/CAPTURE_PIPELINE_SPEC.md §3) — the
// slow-loop trajectory emit point. Reuses the SDK's Phase-1 machinery
// (bounded mpsc + spawn_blocking JSONL drain); DEFAULT OFF via
// `KIRRA_CAPTURE_ENABLED`. Additive only — never on / altering the verdict.
use kirra_core::capture::{
    capture_enabled, record_from_trajectory_verdict, spawn_capture_writer, CaptureRecord,
    PoseSnapshot, TrajectoryCaptureExt, TrajectoryDecision,
};

/// Read the subscription staleness timeout (ms) from
/// `KIRRA_SUBSCRIPTION_STALENESS_MS`, falling back to the constant
/// default. Phase 4: lets the integrator widen the window for slow
/// sensor pipelines without recompile.
fn subscription_staleness_timeout_ms() -> u64 {
    std::env::var("KIRRA_SUBSCRIPTION_STALENESS_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(SUBSCRIPTION_STALENESS_TIMEOUT_MS)
}

/// Capacity of the trajectory channel between the ROS subscription side
/// and the slow-loop validator. 4 is generous: the slow loop processes
/// at planning rate (~10 Hz); 4 buffered candidates = 400 ms of
/// backlog before drop-on-full fires.
pub const TRAJECTORY_CHANNEL_CAPACITY: usize = 4;

/// Capacity of the control-command channel between the ROS subscription
/// side and the fast-loop conformance task. 16 buffers ~160 ms at a
/// 100 Hz control rate; drop-on-full is the safe disposition (a missed
/// conformance check defaults to MRC via the staleness path).
pub const CONTROL_CHANNEL_CAPACITY: usize = 16;

/// Fast-loop conformance budget — see design §4 (per-cycle FTTI).
/// 200 µs = 2% of a 100 Hz control cycle (10 ms). This is separate from
/// the existing 100 µs SG9 timeout for the kernel's per-command verdict
/// path; the two loops have independent budgets because they fire at
/// different rates.
pub const FAST_LOOP_WCET_BUDGET_US: u128 = 200;

#[inline]
fn wall_clock_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Same as `wall_clock_ms` but with a name that's less likely to collide
/// inside a closure body that already has a `wall_clock_ms` shadow. The
/// stamping tasks call this on every received message; it's hot enough
/// that the call is inlined.
#[inline]
fn now_ms_wall() -> u64 { wall_clock_ms() }

/// Trajectory ingress payload. The subscription callback (Phase 2B —
/// when Lanelet2 wiring lands) deserializes
/// `autoware_planning_msgs::Trajectory` into this shape. Carries the
/// planner-published TrajectoryPoint sequence so the slow loop has
/// everything it needs without going back to the kernel for the bytes.
#[derive(Debug, Clone)]
pub struct IngressTrajectory {
    pub asset_id: String,
    pub trajectory_id: u64,
    pub points: Vec<TrajectoryPoint>,
}

/// Control-command ingress payload (envelope over the typed
/// `autoware_control_msgs::Control` map). The fast-loop task converts
/// this to `IncomingControl` for the conformance check.
#[derive(Debug, Clone)]
pub struct IngressControlCommand {
    pub asset_id: String,
    pub linear_velocity_mps: f64,
    pub steering_angle_rad: f64,
    /// Wall-clock ms when the command was received.
    pub stamp_ms: u64,
}

/// Outgoing control command — the gated command (pass-through on
/// Accept, MRC on Reject/no-trajectory). Phase 4 replaces with a typed
/// `autoware_control_msgs::Control` publisher.
#[derive(Debug, Clone)]
pub struct OutgoingControlCommand {
    pub asset_id: String,
    pub linear_velocity_mps: f64,
    pub steering_angle_rad: f64,
    pub accel_mps2: f64,
}

/// Returns the MRC command: zero velocity, neutral steering, max-decel
/// brake ramp. The integrator's vehicle interface honours the
/// `accel_mps2 = -config.max_decel_mps2` as a service-brake demand
/// (separate from any hardware emergency-brake interlock).
pub fn mrc_command(asset_id: impl Into<String>, max_decel_mps2: f64) -> OutgoingControlCommand {
    OutgoingControlCommand {
        asset_id: asset_id.into(),
        linear_velocity_mps: 0.0,
        steering_angle_rad: 0.0,
        accel_mps2: -max_decel_mps2,
    }
}

#[inline]
fn cmd_to_output(asset_id: &str, cmd: &IncomingControl) -> OutgoingControlCommand {
    OutgoingControlCommand {
        asset_id: asset_id.to_string(),
        linear_velocity_mps: cmd.velocity_mps,
        steering_angle_rad: cmd.steering_rad,
        // Pass-through accel: Phase 3 carries 0 (the integrator's
        // existing accel-from-velocity controller computes this on the
        // vehicle side). Phase 4 may carry the planner's commanded
        // accel through if `autoware_control_msgs::Control` has it.
        accel_mps2: 0.0,
    }
}

/// Run the adapter node. Owns the r2r context for the lifetime of the
/// returned future; cancelling the future drops the node and all
/// subscriptions.
///
/// The function returns once the spin loop exits (typically driven by a
/// shutdown signal handled by the caller). On Phase 1 it just spins
/// forever; Phase 4 wires in a shutdown channel.
pub async fn run_adapter(
    state: Arc<AdaptorState>,
    corridor: Arc<dyn CorridorSource>,
    node_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ctx = r2r::Context::create()?;
    let mut node = r2r::Node::create(ctx, node_name, "kirra")?;

    let (trajectory_tx, trajectory_rx) =
        mpsc::channel::<IngressTrajectory>(TRAJECTORY_CHANNEL_CAPACITY);
    let (control_tx, control_rx) =
        mpsc::channel::<IngressControlCommand>(CONTROL_CHANNEL_CAPACITY);

    // ----- Subscriptions ------------------------------------------------
    //
    // Phase 4c — typed subscriptions. r2r auto-generates bindings for the
    // three Autoware message types from the integrator's sourced ROS env
    // (AMENT_PREFIX_PATH discovery at build time). Each subscription
    // returns a `Stream<Item = T>` of the typed message, which the drain
    // task hands to `crate::parsing::*` for the kernel-shape conversion.
    //
    // Map + control_cmd remain untyped (the map is a one-shot binary
    // blob handed to lanelet2_bridge; the control command's parser is
    // Phase 5+ scope and is currently logged-only via the
    // `IngressControlCommand` channel).
    let traj_stream = node.subscribe::<r2r::autoware_planning_msgs::msg::Trajectory>(
        "~/input/trajectory",
        r2r::QosProfile::default(),
    )?;
    let obj_stream = node.subscribe::<r2r::autoware_perception_msgs::msg::PredictedObjects>(
        "~/input/objects",
        r2r::QosProfile::default(),
    )?;
    let _map_sub = node.subscribe_untyped(
        "~/input/map",
        "autoware_map_msgs/msg/LaneletMapBin",
        r2r::QosProfile::default(),
    )?;
    let odom_stream = node.subscribe::<r2r::nav_msgs::msg::Odometry>(
        "~/input/odometry",
        r2r::QosProfile::default(),
    )?;
    let _ctrl_sub = node.subscribe_untyped(
        "~/input/control_cmd",
        "autoware_control_msgs/msg/Control",
        r2r::QosProfile::default(),
    )?;

    tracing::info!(
        node = node_name,
        traj_cap = TRAJECTORY_CHANNEL_CAPACITY,
        ctrl_cap = CONTROL_CHANNEL_CAPACITY,
        "kirra-ros2-adapter: subscriptions registered"
    );

    // ----- Subscription-stamping tasks (SG9 liveness) -------------------
    //
    // For each REQUIRED subscription (trajectory / objects / odometry),
    // spawn a task that drains the stream and stamps the matching
    // `last_*_ms` slot on AdaptorState on every received message. This
    // is what makes `AdaptorState::any_subscription_stale` actually
    // observe liveness in production — without these stamping tasks,
    // the AtomicU64 slots stay at the cold-start sentinel `0` and the
    // fast loop publishes MRC every cycle (the safe direction, but
    // useless behavior).
    //
    // Phase 4c — typed parsing + forwarding. Each drain task:
    //   1. Stamps the SG9 liveness slot via `state.touch_*(now)`.
    //   2. Calls `crate::parsing::parse_*` to convert the typed r2r
    //      message into the kernel-shape envelope.
    //   3. Forwards the result onward:
    //        - Trajectory  → trajectory_tx channel (wrapped in
    //                        `IngressTrajectory` with the constant
    //                        `asset_id = "ego"` and a monotonic
    //                        `trajectory_id`).
    //        - Objects     → `state.update_objects(...)` write-replace.
    //        - Odometry    → `state.update_odom(...)` write-replace.
    use futures::StreamExt;

    // Per-node asset id. Phase 4c is single-asset (one Governor instance
    // per vehicle); Phase 5+ may multi-multiplex but that's out of scope.
    let asset_id_str: &'static str = "ego";

    let traj_state = Arc::clone(&state);
    let traj_tx_clone = trajectory_tx.clone();
    tokio::spawn(async move {
        let mut s = traj_stream;
        let mut traj_seq: u64 = 0;
        while let Some(msg) = s.next().await {
            let now = now_ms_wall();
            // Phase I observability (integration-harness §I.1a): a per-callback
            // entry event PROVES delivery (vs staleness) for the A-vs-B split.
            tracing::info!(
                target: "kirra::ingress",
                topic = "trajectory",
                stamp_ms = now,
                "subscription_callback"
            );
            traj_state.touch_trajectory(now);
            tracing::debug!(points = msg.points.len(), "trajectory_msg_received");
            let parsed = crate::parsing::parse_trajectory(&msg, now);
            if parsed.points.is_empty() {
                tracing::debug!("trajectory message had zero points — skipping forward");
                continue;
            }
            traj_seq = traj_seq.wrapping_add(1);
            let envelope = IngressTrajectory {
                asset_id: asset_id_str.to_string(),
                trajectory_id: traj_seq,
                points: parsed.points,
            };
            match traj_tx_clone.try_send(envelope) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(dropped)) => {
                    tracing::warn!(
                        asset_id = %dropped.asset_id,
                        trajectory_id = dropped.trajectory_id,
                        "trajectory channel FULL — dropping candidate (slow loop is behind)"
                    );
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    tracing::error!(
                        "trajectory channel CLOSED — slow loop is gone; adapter must restart"
                    );
                    return;
                }
            }
        }
        tracing::error!("trajectory subscription stream closed — staleness will fire fleet-wide");
    });

    let obj_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut s = obj_stream;
        while let Some(msg) = s.next().await {
            let now = now_ms_wall();
            tracing::info!(
                target: "kirra::ingress",
                topic = "objects",
                stamp_ms = now,
                "subscription_callback"
            );
            obj_state.touch_objects(now);
            let parsed = crate::parsing::parse_predicted_objects(&msg);
            obj_state.update_objects(parsed);
        }
        tracing::error!("objects subscription stream closed — staleness will fire fleet-wide");
    });

    let odom_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut s = odom_stream;
        while let Some(msg) = s.next().await {
            let now = now_ms_wall();
            tracing::info!(
                target: "kirra::ingress",
                topic = "odometry",
                stamp_ms = now,
                "subscription_callback"
            );
            odom_state.touch_odom(now);
            let parsed = crate::parsing::parse_odom(&msg);
            odom_state.update_odom(parsed);
        }
        tracing::error!("odometry subscription stream closed — staleness will fire fleet-wide");
    });

    // ----- Slow loop (Phase 2A; posture-aware as of M1) ----------------
    //
    // For each candidate trajectory:
    //   1) Snapshot the perception cache (read-and-clone; do NOT hold
    //      the RwLock across the validation).
    //   2) Snapshot the current fleet posture from AdaptorState.
    //   3) Run validate_trajectory_slow with that posture — Nominal /
    //      Degraded select the effective kinematics contract;
    //      LockedOut short-circuits to MRCFallback.
    //   4) update_trajectory(asset_id, ..., verdict, now_ms) — installs
    //      on Accept/Clamp, removes on MRC.
    //   5) Log WCET for the cycle (warns if > 10 ms — the per-trajectory
    //      budget from the design §3).
    //
    // M1b (live posture source): `AdaptorState::current_posture()`
    // resolves via the fail-closed `PostureTracker` state machine.
    // When the binary is built with `KIRRA_POSTURE_STREAM_URL` set,
    // the SSE subscriber in `crate::posture_source` feeds the tracker
    // from the verifier's `/system/posture/stream` endpoint
    // (pre-first-event seed = Degraded; staleness derates to
    // Degraded; LockedOut is sticky-toward-safe). When the env var
    // is unset the tracker stays in `nominal_default_no_source` mode
    // and `current_posture()` returns Nominal forever — preserving
    // the M1-era behaviour for verifier-less deployments.
    let slow_state = Arc::clone(&state);
    let slow_corridor = Arc::clone(&corridor);
    // KIRRA-OCCY-PMON-003 slice-1 (D3a): adapter-local perception-derate cap.
    // The publisher runs the Track-C kinematic guard at perception-tick rate
    // and publishes a speed cap; the slow loop reads it O(1) and composes it
    // into the per-pose verdict. DEFAULT OFF via `KIRRA_PERCEPTION_DERATE_ENABLED`
    // — `resolve_perception_cap(false, ..)` returns `None` → pure no-op.
    let perception_cache = empty_perception_cap();
    let perception_publisher = PerceptionCapPublisher::new(
        perception_cache.clone(),
        KinematicPlausibilityContract::urban_reference(),
        subscription_staleness_timeout_ms(), // ttl reuses the subscription staleness budget
    );
    // Learning-loop capture — the slow-loop trajectory emit point (Phase 1.5).
    // DEFAULT OFF: only when `KIRRA_CAPTURE_ENABLED` is truthy do we spawn the
    // SDK's bounded JSONL writer; otherwise `capture_tx` is `None` and the emit
    // below is a pure no-op. The writer takes no state (the SDK refactored the
    // unused `Arc<AppState>` out), so the adapter spawns its own. The seq is an
    // adapter-local monotonic decision counter (the slow loop's analogue of the
    // gateway's `capture_decision_seq`).
    let capture_tx: Option<mpsc::Sender<CaptureRecord>> =
        capture_enabled().then(spawn_capture_writer);
    let capture_seq = Arc::new(std::sync::atomic::AtomicU64::new(0));
    tokio::spawn(async move {
        let mut rx = trajectory_rx;
        while let Some(traj) = rx.recv().await {
            let start = std::time::Instant::now();
            let objects = slow_state.snapshot_objects();
            let odom = slow_state.snapshot_odom();
            let posture = slow_state.current_posture();

            // Track-C ingest tick (pure orchestration in `perception_ingest`):
            // publish the cap stamped with the OBJECTS' freshness timestamp, so
            // the cap ages with the object stream and `resolve_perception_cap`
            // fails closed (state-3 MRC) when objects go silent. If objects are
            // stale/never-seen, sweep an MRC-floor cap proactively.
            let now_wall = now_ms_wall();
            let objects_ms =
                slow_state.last_objects_ms.load(std::sync::atomic::Ordering::Relaxed);
            let objects_fresh = objects_ms != 0
                && now_wall.saturating_sub(objects_ms) <= subscription_staleness_timeout_ms();
            if objects_fresh {
                publish_perception_tick(&perception_publisher, &objects, objects_ms);
            } else {
                perception_publisher.sweep_staleness(now_wall);
            }
            let effective_perception_cap = resolve_perception_cap(
                perception_derate_enabled(),
                &perception_cache,
                now_wall,
            );

            let verdict = validate_trajectory_slow_capped(
                &traj.points,
                slow_corridor.as_ref(),
                &objects,
                &slow_state.config,
                odom.as_ref(),
                posture.clone(),
                effective_perception_cap,
                // Occlusion / assured-clear-distance bound (RSS Rule 4): perception
                // does not yet supply a visibility range → None (no-op), mirroring
                // the perception-derate cap's pre-wiring state.
                None,
                // Multi-modal predictive RSS: prediction does not yet supply per-object
                // modes here → None (no-op); the snapshot RSS remains the bound.
                None,
            );
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            slow_state.update_trajectory(
                traj.asset_id.clone(),
                traj.trajectory_id,
                traj.points.clone(),
                verdict,
                now_ms,
            );
            let elapsed_us = start.elapsed().as_micros();
            // Phase I observability (integration-harness §I.1b): carry posture +
            // per-input freshness ages on the verdict span so a derate caused by
            // stale inputs (Branch B) is distinguishable from non-delivery
            // (Branch A) directly from the logs. A never-seen slot (last_*_ms == 0)
            // reports u64::MAX rather than a misleadingly-huge "age since epoch".
            let now_fresh = now_ms_wall();
            let age_of =
                |t: u64| if t == 0 { u64::MAX } else { now_fresh.saturating_sub(t) };
            let traj_age_ms =
                age_of(slow_state.last_trajectory_ms.load(std::sync::atomic::Ordering::Relaxed));
            let objects_age_ms =
                age_of(slow_state.last_objects_ms.load(std::sync::atomic::Ordering::Relaxed));
            let odom_age_ms =
                age_of(slow_state.last_odom_ms.load(std::sync::atomic::Ordering::Relaxed));
            tracing::info!(
                asset_id = %traj.asset_id,
                trajectory_id = traj.trajectory_id,
                verdict = ?verdict,
                posture = ?posture,
                traj_age_ms,
                objects_age_ms,
                odom_age_ms,
                elapsed_us = elapsed_us,
                "trajectory_verdict"
            );
            if elapsed_us > 10_000 {
                tracing::warn!(
                    asset_id = %traj.asset_id,
                    elapsed_us = elapsed_us,
                    "trajectory_validation_wcet_exceeded"
                );
            }

            // Learning-loop capture (Phase 1.5) — emit the slow-loop verdict as
            // a BOUNDED record AFTER the WCET measurement (so it never counts
            // against the slow-loop budget) and beside update_trajectory. The
            // record is O(1): counts + endpoint poses + join keys, NEVER a fresh
            // clone of the full points/objects vectors. Wait-free try_send;
            // drop-on-full/closed with a loud log — capture never blocks or
            // alters the verdict. No-op when capture is disabled (capture_tx None).
            if let Some(tx) = capture_tx.as_ref() {
                let decision = match verdict {
                    TrajectoryVerdict::Accept => TrajectoryDecision::Accept,
                    TrajectoryVerdict::Clamp => TrajectoryDecision::Clamp,
                    TrajectoryVerdict::MRCFallback | TrajectoryVerdict::Pending => {
                        TrajectoryDecision::MrcFallback
                    }
                };
                let pose_snap = |p: &TrajectoryPoint| PoseSnapshot {
                    x_m: p.pose.x_m,
                    y_m: p.pose.y_m,
                    heading_rad: p.pose.heading_rad,
                };
                let ext = TrajectoryCaptureExt {
                    asset_id: traj.asset_id.clone(),
                    trajectory_id: traj.trajectory_id,
                    objects_ms,
                    point_count: traj.points.len(),
                    object_count: objects.len(),
                    first_pose: traj.points.first().map(pose_snap),
                    last_pose: traj.points.last().map(pose_snap),
                    target_speed_mps: traj.points.last().map(|p| p.velocity_mps),
                };
                let rec = record_from_trajectory_verdict(
                    capture_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                    now_ms,
                    decision,
                    posture,
                    ext,
                    perception_derate_enabled(),
                );
                match tx.try_send(rec) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => tracing::warn!(
                        asset_id = %traj.asset_id,
                        "capture channel FULL — dropping slow-loop record (off the verdict path)"
                    ),
                    Err(mpsc::error::TrySendError::Closed(_)) => tracing::warn!(
                        "capture channel CLOSED — capture writer gone; slow-loop record dropped"
                    ),
                }
            }
        }
    });

    // ----- Fast loop (Phase 3) -----------------------------------------
    //
    // For each incoming control command from vehicle_cmd_gate's output:
    //   1. Snapshot the per-asset AcceptedTrajectory (clone — do NOT
    //      hold the DashMap shard lock across the conformance check).
    //   2. Snapshot the latest ego odometry (read-and-clone).
    //   3. Call check_command_conforms.
    //   4. On Accept: publish the pass-through command.
    //      On MRCFallback OR no trajectory installed: publish the MRC
    //      command (zero velocity, neutral steering, max-decel brake).
    //   5. Log WCET; warn if elapsed > 200 µs (the fast-loop budget per
    //      design §4 — 2% of a 100 Hz control cycle).
    //
    // Publication: Phase 3 emits `OutgoingControlCommand` on an
    // internal channel `fast_loop_out_rx`. Phase 4 (or the integrator's
    // glue) wires this to a `~/output/control_cmd` r2r publisher; the
    // separation keeps the conformance check stays ROS-free.
    let (fast_loop_out_tx, mut fast_loop_out_rx) =
        mpsc::channel::<OutgoingControlCommand>(CONTROL_CHANNEL_CAPACITY);
    let fast_state = Arc::clone(&state);
    let staleness_timeout_ms = subscription_staleness_timeout_ms();
    tokio::spawn(async move {
        let mut rx = control_rx;
        while let Some(in_cmd) = rx.recv().await {
            let start = std::time::Instant::now();
            let now_ms = wall_clock_ms();
            let cmd = IncomingControl {
                velocity_mps: in_cmd.linear_velocity_mps,
                steering_rad: in_cmd.steering_angle_rad,
                stamp_ms:     in_cmd.stamp_ms,
            };
            // SAFETY: SG9 | REQ: subscription-liveness | TEST: test_stale_subscription_mrcs
            // Subscription staleness check (SG9) — adapter's own
            // fail-closed path. If any of the three required upstream
            // subscriptions (trajectory / objects / odometry) hasn't
            // delivered a message within the configured window, MRC
            // regardless of any other state. Done BEFORE the
            // conformance check so the upstream-dropout case fails
            // closed even if a stale AcceptedTrajectory + a clean
            // command would otherwise pass.
            if fast_state.any_subscription_stale(now_ms, staleness_timeout_ms) {
                let out = mrc_command(
                    in_cmd.asset_id.clone(),
                    fast_state.config.max_decel_mps2,
                );
                if let Err(e) = fast_loop_out_tx.try_send(out) {
                    tracing::error!(
                        asset_id = %in_cmd.asset_id,
                        error = ?e,
                        "fast-loop output channel send failed (staleness path)",
                    );
                }
                tracing::warn!(
                    asset_id = %in_cmd.asset_id,
                    timeout_ms = staleness_timeout_ms,
                    "subscription_staleness_mrc"
                );
                continue;
            }
            let odom = fast_state.snapshot_odom().unwrap_or_default();
            let traj = fast_state.snapshot(&in_cmd.asset_id);
            let verdict = match traj.as_ref() {
                Some(t) => check_command_conforms(
                    &cmd, t, &odom, &fast_state.config, now_ms,
                ),
                None => ConformanceVerdict::MRCFallback,
            };
            let out = match verdict {
                ConformanceVerdict::Accept => cmd_to_output(&in_cmd.asset_id, &cmd),
                ConformanceVerdict::MRCFallback =>
                    mrc_command(in_cmd.asset_id.clone(), fast_state.config.max_decel_mps2),
            };
            if let Err(e) = fast_loop_out_tx.try_send(out) {
                tracing::error!(
                    asset_id = %in_cmd.asset_id,
                    error = ?e,
                    "fast-loop output channel send failed — downstream publisher missing or full",
                );
            }
            let elapsed_us = start.elapsed().as_micros();
            tracing::debug!(
                asset_id = %in_cmd.asset_id,
                verdict = ?verdict,
                elapsed_us = elapsed_us,
                "fast_loop_verdict"
            );
            if elapsed_us > FAST_LOOP_WCET_BUDGET_US {
                tracing::warn!(
                    asset_id = %in_cmd.asset_id,
                    elapsed_us = elapsed_us,
                    "fast_loop_wcet_exceeded"
                );
            }
        }
    });

    // ----- Output publisher (Phase 3 placeholder) ----------------------
    //
    // For Phase 3 we drain the fast-loop output channel and log each
    // command. Phase 4 replaces this with an r2r publisher to
    // ~/output/control_cmd.
    tokio::spawn(async move {
        while let Some(out) = fast_loop_out_rx.recv().await {
            tracing::debug!(
                asset_id = %out.asset_id,
                v = out.linear_velocity_mps,
                delta = out.steering_angle_rad,
                accel = out.accel_mps2,
                "fast-loop output (Phase 3 placeholder: would publish on ~/output/control_cmd)"
            );
        }
    });

    // ----- Drop-on-full helpers (used by the subscription callbacks
    //       once Phase 2 fills them in). Kept here so Phase 1 fixes the
    //       drop-on-full policy in one place.
    #[allow(dead_code)]
    fn try_publish_trajectory(tx: &mpsc::Sender<IngressTrajectory>, item: IngressTrajectory) {
        match tx.try_send(item) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(dropped)) => {
                tracing::warn!(
                    asset_id = %dropped.asset_id,
                    trajectory_id = dropped.trajectory_id,
                    "trajectory channel FULL — dropping candidate (slow loop is behind)"
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::error!(
                    "trajectory channel CLOSED — slow loop is gone; adapter must restart"
                );
            }
        }
    }

    #[allow(dead_code)]
    fn try_publish_control(tx: &mpsc::Sender<IngressControlCommand>, item: IngressControlCommand) {
        match tx.try_send(item) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(
                    "control channel FULL — dropping command (fast loop is behind; \
                     staleness will collapse the next read to MRC)"
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::error!(
                    "control channel CLOSED — fast loop is gone; adapter must restart"
                );
            }
        }
    }

    // The trajectory_tx / control_tx senders are kept alive by the
    // subscription callbacks once Phase 2 wires the typed deserializers
    // in. For Phase 1 the senders just need to outlive their channel
    // halves so the tasks above don't see Closed immediately.
    let _ = (trajectory_tx, control_tx);

    // ----- Spin loop ----------------------------------------------------
    //
    // r2r's spin model is to drive the node's executor on a regular
    // tick. Phase 4 wires in a shutdown channel; Phase 1 just loops.
    loop {
        node.spin_once(Duration::from_millis(10));
    }
}
