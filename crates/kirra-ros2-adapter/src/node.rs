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
use crate::state::AdaptorState;

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

/// Stub payload for trajectory ingress. Phase 2 replaces this with a
/// typed `autoware_planning_msgs::Trajectory` map.
#[derive(Debug, Clone)]
pub struct IngressTrajectory {
    pub asset_id: String,
    pub trajectory_id: u64,
}

/// Stub payload for control-command ingress. Phase 2 replaces with a
/// typed `autoware_control_msgs::Control` map.
#[derive(Debug, Clone)]
pub struct IngressControlCommand {
    pub asset_id: String,
    pub linear_velocity_mps: f64,
    pub steering_angle_rad: f64,
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
    // r2r 0.9 takes string-typed message names so the integrator's
    // package layout is what binds them. The exact field shapes are
    // pinned in Phase 2 with the integrator's Autoware release tag.
    let _traj_sub = node.subscribe_untyped(
        "~/input/trajectory",
        "autoware_planning_msgs/msg/Trajectory",
        r2r::QosProfile::default(),
    )?;
    let _obj_sub = node.subscribe_untyped(
        "~/input/objects",
        "autoware_perception_msgs/msg/PredictedObjects",
        r2r::QosProfile::default(),
    )?;
    let _map_sub = node.subscribe_untyped(
        "~/input/map",
        "autoware_map_msgs/msg/LaneletMapBin",
        r2r::QosProfile::default(),
    )?;
    let _odom_sub = node.subscribe_untyped(
        "~/input/odometry",
        "nav_msgs/msg/Odometry",
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
        "kirra-ros2-adapter: subscriptions registered (Phase 1 skeleton)"
    );

    // ----- Slow loop ----------------------------------------------------
    //
    // Receives candidate trajectories, runs (in Phase 2) the full
    // validate_trajectory_containment + per-pose kinematics + RSS
    // pipeline, then on Accept calls state.install(...). Phase 1: log
    // receipt only.
    let slow_state = Arc::clone(&state);
    let slow_corridor = Arc::clone(&corridor);
    tokio::spawn(async move {
        let _ = slow_corridor.left_boundary(); // touch to avoid Phase-1
                                               // unused warnings
        let mut rx = trajectory_rx;
        while let Some(traj) = rx.recv().await {
            tracing::debug!(
                asset_id = %traj.asset_id,
                trajectory_id = traj.trajectory_id,
                "slow-loop received candidate trajectory (Phase 1 stub: no validation)"
            );
            // Phase 2: validate_trajectory_containment, validate_vehicle_command,
            // RSS over horizon, then slow_state.install(...) on Accept.
            let _ = slow_state.len();
        }
    });

    // ----- Fast loop ----------------------------------------------------
    //
    // Receives every outgoing control command and (in Phase 3) checks
    // it conforms to the per-asset AcceptedTrajectory. Phase 1: log
    // receipt only.
    let fast_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut rx = control_rx;
        while let Some(cmd) = rx.recv().await {
            tracing::debug!(
                asset_id = %cmd.asset_id,
                v = cmd.linear_velocity_mps,
                delta = cmd.steering_angle_rad,
                "fast-loop received control command (Phase 1 stub: no conformance check)"
            );
            // Phase 3: read fast_state.snapshot, compute nearest-point
            // conformance, publish gated command OR MRC.
            let _ = fast_state.len();
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
