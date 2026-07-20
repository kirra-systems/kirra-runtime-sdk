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

use kirra_hv_carrier::PosixShmRegion;

use crate::contract_producer::{proposal_payload, ProposalSequencer};
pub use crate::control_ingress::IngressControlCommand;
use crate::control_ingress::{fail_closed_control_command, parse_control_command_json};
use crate::corridor::CorridorSource;
use crate::occlusion_channel::{resolve_occlusion_channel, OCCLUSION_CHANNEL_ENABLED_ENV};
use crate::perception_redundancy::{
    more_restrictive_cap, perception_redundancy_enabled, resolve_redundancy_cap,
    DivergenceEscalator, RedundancyConfig,
};
use crate::prediction::slow_loop_modes;
use crate::state::{
    AdaptorState, TrajectoryPoint, TrajectoryVerdict, SUBSCRIPTION_STALENESS_TIMEOUT_MS,
};
use crate::validation::{
    check_command_conforms, validate_trajectory_slow_with_envelope, ConformanceVerdict,
    IncomingControl,
};
use crate::vru_channel::{resolve_vru_channel, VRU_CHANNEL_ENABLED_ENV};
use kirra_trajectory::vru::{PedestrianScene, VruRssParams};

/// Horizon / step for the multi-modal predictive-RSS mode rollout in the slow loop (matches the
/// planner's prediction horizon; the checker time-matches each sample to a trajectory pose).
const SLOW_PRED_HORIZON_S: f64 = 3.0;
const SLOW_PRED_DT_S: f64 = 0.5;
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

/// VRU / pedestrian channel enable gate (#789 follow-up 1) — reads
/// `KIRRA_VRU_CHANNEL_ENABLED`. Adapter INTEGRATION glue (env I/O), kept out of
/// the pure checker crate so its mutation gate covers only the tested
/// `resolve_vru_channel` decision. Truthy = `1`/`true`/`yes` (case-insensitive);
/// unset/anything else = disarmed (byte-identical no-op), mirroring
/// `perception_redundancy_enabled`.
fn vru_channel_enabled() -> bool {
    std::env::var(VRU_CHANNEL_ENABLED_ENV)
        .map(|v| {
            let t = v.trim();
            t == "1" || t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

/// Occlusion / assured-clear-distance channel enable gate (S2, #1025) — reads
/// `KIRRA_OCCLUSION_CHANNEL_ENABLED`. Adapter INTEGRATION glue (env I/O), kept out
/// of the pure checker crate so its mutation gate covers only the tested
/// `resolve_occlusion_channel` decision. Same truthy grammar as
/// `vru_channel_enabled`; unset/anything else = disarmed (byte-identical no-op).
fn occlusion_channel_enabled() -> bool {
    std::env::var(OCCLUSION_CHANNEL_ENABLED_ENV)
        .map(|v| {
            let t = v.trim();
            t == "1" || t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
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

/// N1 — explicit SENSOR-DATA QoS for the high-rate safety-ingress streams
/// (trajectory / objects / odometry / control). The RMW default
/// (`QosProfile::default()` = Reliable + KeepLast(10)) BUFFERS up to ten samples,
/// so after a publisher stall the adapter would drain a backlog of STALE samples
/// before reaching the current one — the exact stale-drain hazard the
/// actuator-OUTPUT side already forbids, here on the safety INGRESS.
///
/// Sensor-data discipline instead:
///   * KeepLast(1) — keep ONLY the freshest sample; an older one is overwritten,
///     never queued, so the validator never sees a backlog after a stall.
///   * BestEffort — prioritize freshness over delivery-completeness AND maximise
///     QoS compatibility: a BestEffort SUBSCRIBER accepts both Reliable and
///     BestEffort publishers, so swapping this in never silently drops a
///     producer's connection. The monotonic-clock staleness watchdog
///     (`now_ms_fresh`) remains the authority on a genuine gap.
///
/// Deadline + Liveliness are intentionally NOT set here: a subscriber-REQUESTED
/// deadline/lease the publisher does not OFFER rejects the QoS match and would
/// blind the adapter — they are a publisher-coordinated follow-up. The latched
/// map (`~/input/map`) uses [`map_qos`] (TransientLocal, below) — it is not a
/// high-rate stream and needs durability, not freshness. The control feedback
/// keeps the sensor-data profile (a high-rate fresh-only stream).
#[inline]
fn ingress_sensor_qos() -> r2r::QosProfile {
    r2r::QosProfile::default().best_effort().keep_last(1)
}

/// M1 (#1040) — the LATCHED-map QoS for `~/input/map`. The map is published ONCE
/// as a latched `autoware_map_msgs/LaneletMapBin` blob, so a late-joining /
/// restarted adapter must receive that historical sample. The prior
/// `QosProfile::default()` is **Volatile**: a Volatile subscriber that joins
/// AFTER the one-shot publish matches the publisher silently but never receives
/// the blob — a silent-no-map hazard (the match succeeds; the sample just never
/// arrives). TransientLocal + Reliable + KeepLast(1) instead: the DDS durability
/// cache re-delivers the last latched blob to a late subscriber. A TransientLocal
/// SUBSCRIBER requires a TransientLocal-or-better PUBLISHER to match — the map
/// publisher latches by construction (that is why it is a one-shot), so this is
/// the correct, intended pairing (ADR-0036 / KIRRA-OCCY-MSGSYNC-001), not a
/// match-blinding request like a subscriber-only deadline would be.
///
/// NOTE: today `~/input/map` is a placeholder subscription — the slow loop uses
/// the injected `CorridorSource`, not this blob. When Phase-2 makes the map
/// load-bearing, ADD a "map received before first validation" gate that fails
/// closed (MRC) until the blob lands; the TransientLocal QoS here is the
/// necessary precondition for that gate (a Volatile late-join would never see it).
#[inline]
fn map_qos() -> r2r::QosProfile {
    r2r::QosProfile::default()
        .reliable()
        .transient_local()
        .keep_last(1)
}

/// Actuator-OUTPUT QoS for the gated control-command stream (`~/output/control_cmd`):
/// **Reliable + KeepLast(1)**. A gated command is control-critical, so it must not
/// be silently dropped (Reliable); but only the FRESHEST gated command is ever
/// relevant — an older queued command must never actuate after a newer verdict —
/// so the depth is 1, never a backlog. This is the output-side mirror of the N1
/// ingress discipline (freshness over buffering), and matches INV-10's
/// "never queue a stale actuator command" intent.
#[inline]
fn actuator_output_qos() -> r2r::QosProfile {
    r2r::QosProfile::default().reliable().keep_last(1)
}

#[inline]
fn wall_clock_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// MONOTONIC freshness clock (B4) — the staleness / age clock for the adapter's
/// subscription stamps, `AcceptedTrajectory::promoted_at_ms`, and the slow/fast
/// loop freshness comparisons. Delegates to the shared `state::monotonic_now_ms`
/// so EVERY freshness timestamp in the adapter shares ONE non-decreasing epoch;
/// a forward wall-clock / NTP step can no longer inflate an age and spuriously
/// trip staleness → fleet-wide MRC. Wall time (`wall_clock_ms`) is reserved for
/// audit/correlation record timestamps ONLY. Hot path — inlined.
#[inline]
fn now_ms_fresh() -> u64 {
    crate::state::monotonic_now_ms()
}

/// Nominal fast-loop control cycle (100 Hz → 10 ms) as seconds — the
/// `delta_time_s` stamped on a published cross-partition proposal's kinematic
/// command (ADR-0030 incr. 2).
const CONTROL_CYCLE_S: f64 = 0.01;

/// Freshness budget added to a published proposal's boundary-clock publication
/// time to form its `deadline_nanos` (2 control cycles). The governor rejects a
/// snapshot whose deadline has passed (HVCHAN §3); a host stand-in pending the
/// #274 FTTI budget.
const CONTRACT_DEADLINE_BUDGET_NS: u64 = 20_000_000;

/// Monotonic nanoseconds — a HOST stand-in for the boundary clock domain
/// (AOU-TIMESYNC-001), derived from the same monotonic ms source as the freshness
/// checks. The real boundary-clock primitive is QNX target work (#274/#278).
#[inline]
fn now_ns_fresh() -> u64 {
    now_ms_fresh().saturating_mul(1_000_000)
}

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

/// Outgoing control command — the gated command (pass-through on
/// Accept, MRC on Reject/no-trajectory). Published on `~/output/control_cmd`
/// as an UNTYPED `autoware_control_msgs/msg/Control` (mirroring the untyped
/// `~/input/control_cmd` subscription; see [`control_command_to_json`]).
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

/// Serialize an [`OutgoingControlCommand`] into the UNTYPED
/// `autoware_control_msgs/msg/Control` JSON shape r2r's `publish` expects (Phase 4).
///
/// We publish untyped — exactly as `~/input/control_cmd` is subscribed untyped —
/// so the adapter need not pull the integrator's typed `Control` binding at build
/// time (the AOU-MSG-TOOLCHAIN curated-interface discipline). `now_ms` is the
/// wall-clock header stamp (ROS `builtin_interfaces/Time`), split into sec/nanosec.
///
/// The exact field set of `Control` is **integrator-pinned** against their
/// Autoware version (`AOU-MSG-TOOLCHAIN`); the well-established fields are
/// populated here (`lateral.steering_tire_angle`, `longitudinal.velocity`,
/// `longitudinal.acceleration`). A field-set mismatch surfaces at runtime on the
/// untyped publish, never silently — same contract as the input subscription.
fn control_command_to_json(out: &OutgoingControlCommand, now_ms: u64) -> serde_json::Value {
    let stamp = serde_json::json!({
        "sec": (now_ms / 1000) as i32,
        "nanosec": ((now_ms % 1000) * 1_000_000) as u32,
    });
    serde_json::json!({
        "stamp": stamp.clone(),
        "lateral": {
            "stamp": stamp.clone(),
            "control_time": stamp.clone(),
            "steering_tire_angle": out.steering_angle_rad as f32,
            "steering_tire_rotation_rate": 0.0_f32,
            "is_defined_steering_tire_rotation_rate": false,
        },
        "longitudinal": {
            "stamp": stamp.clone(),
            "control_time": stamp,
            "velocity": out.linear_velocity_mps as f32,
            "acceleration": out.accel_mps2 as f32,
            "jerk": 0.0_f32,
            "is_defined_acceleration": true,
            "is_defined_jerk": false,
        },
    })
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
    let (control_tx, control_rx) = mpsc::channel::<IngressControlCommand>(CONTROL_CHANNEL_CAPACITY);

    // ----- Subscriptions ------------------------------------------------
    //
    // Phase 4c — typed subscriptions. r2r auto-generates bindings for the
    // three Autoware message types from the integrator's sourced ROS env
    // (AMENT_PREFIX_PATH discovery at build time). Each subscription
    // returns a `Stream<Item = T>` of the typed message, which the drain
    // task hands to `crate::parsing::*` for the kernel-shape conversion.
    //
    // Map + control_cmd remain untyped (the map is a one-shot binary
    // blob handed to lanelet2_bridge; the control command uses the pure
    // JSON parser in `control_ingress` so the fast-loop conformance gate
    // stays ROS-message-codegen neutral).
    let traj_stream = node.subscribe::<r2r::autoware_planning_msgs::msg::Trajectory>(
        "~/input/trajectory",
        ingress_sensor_qos(), // N1: KeepLast(1) + BestEffort — no stale backlog after a stall
    )?;
    let obj_stream = node.subscribe::<r2r::autoware_perception_msgs::msg::PredictedObjects>(
        "~/input/objects",
        ingress_sensor_qos(), // N1
    )?;
    // Redundant (channel-B) objects subscription — a SECOND, INDEPENDENT PredictedObjects topic
    // (e.g. a camera-only world model vs the primary radar+lidar) feeding the True-Redundancy
    // cross-check (gap #2b). Registered ONLY when the divergence monitor is enabled, so a
    // deployment without redundancy adds no subscription. Remap `~/input/objects_secondary` to
    // the redundant detector's topic in the launch file.
    let obj_b_stream = if perception_redundancy_enabled() {
        Some(
            node.subscribe::<r2r::autoware_perception_msgs::msg::PredictedObjects>(
                "~/input/objects_secondary",
                ingress_sensor_qos(), // N1
            )?,
        )
    } else {
        None
    };
    // VRU / pedestrian subscription (#789 follow-up 1) — a DEDICATED pedestrian
    // topic (a producer such as kirra-taj's `classify_pedestrians`) feeding the
    // omnidirectional reachable-set bound. Registered ONLY when the VRU gate is
    // enabled, so a deployment without a VRU source adds no subscription and the
    // checker's `pedestrians: None` no-op path stays byte-identical. Remap
    // `~/input/pedestrians` to the detector's topic in the launch file. When
    // enabled but the channel is silent/stale, the slow loop FAILS CLOSED (MRC
    // cap), never treats silence as "no pedestrians".
    let ped_stream = if vru_channel_enabled() {
        Some(
            node.subscribe::<r2r::autoware_perception_msgs::msg::PredictedObjects>(
                "~/input/pedestrians",
                ingress_sensor_qos(), // N1
            )?,
        )
    } else {
        None
    };
    // Occlusion / assured-clear-distance subscription (S2, #1025) — a scalar
    // sight-distance-ahead topic (metres; a producer such as kirra-taj / kirra-map
    // `sight_distance`) that ARMS the RSS Rule 4 limited-visibility bound. Same
    // opt-in discipline as the VRU channel: registered ONLY when the occlusion
    // gate is enabled, so a deployment without an occlusion source adds no
    // subscription and the checker's `visibility_range_m: None` no-op path stays
    // byte-identical. Remap `~/input/visibility` to the producer's topic in the
    // launch file. When enabled but the channel is silent/stale, the slow loop
    // FAILS CLOSED (MRC cap), never treats silence as "wide-open visibility".
    let vis_stream = if occlusion_channel_enabled() {
        Some(node.subscribe::<r2r::std_msgs::msg::Float64>(
            "~/input/visibility",
            ingress_sensor_qos(), // N1
        )?)
    } else {
        None
    };
    let _map_sub = node.subscribe_untyped(
        "~/input/map",
        "autoware_map_msgs/msg/LaneletMapBin",
        map_qos(), // M1 (#1040): TransientLocal — a late-joining adapter must
                   // receive the latched map blob, not match Volatile and get nothing.
    )?;
    let odom_stream = node.subscribe::<r2r::nav_msgs::msg::Odometry>(
        "~/input/odometry",
        ingress_sensor_qos(), // N1
    )?;
    let ctrl_stream = node.subscribe_untyped(
        "~/input/control_cmd",
        "autoware_control_msgs/msg/Control",
        ingress_sensor_qos(), // N1: control feedback is also a high-rate fresh-only stream
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
    //        - Control     → control_tx channel for the fast-loop gate.
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
            let now = now_ms_fresh();
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

    let ctrl_tx_clone = control_tx.clone();
    tokio::spawn(async move {
        let mut s = ctrl_stream;
        while let Some(item) = s.next().await {
            let received_fresh_ms = now_ms_fresh();
            let received_wall_ms = wall_clock_ms();
            tracing::info!(
                target: "kirra::ingress",
                topic = "control_cmd",
                stamp_ms = received_fresh_ms,
                "subscription_callback"
            );

            let envelope = match item {
                Ok(msg) => match parse_control_command_json(asset_id_str, &msg, received_wall_ms) {
                    Ok(cmd) => cmd,
                    Err(reason) => {
                        tracing::warn!(
                            reason = reason,
                            "control_cmd malformed — forwarding fail-closed command to fast loop"
                        );
                        fail_closed_control_command(asset_id_str, received_wall_ms)
                    }
                },
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "control_cmd untyped decode failed — forwarding fail-closed command to fast loop"
                    );
                    fail_closed_control_command(asset_id_str, received_wall_ms)
                }
            };

            match ctrl_tx_clone.try_send(envelope) {
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
                    return;
                }
            }
        }
        tracing::error!(
            "control_cmd subscription stream closed — fast loop will stop receiving commands"
        );
    });

    let obj_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut s = obj_stream;
        while let Some(msg) = s.next().await {
            let now = now_ms_fresh();
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

    // Redundant (channel-B) objects drain — only spawned when the monitor is enabled and the
    // subscription was registered above. Each message updates the secondary snapshot AND stamps
    // its freshness (one call); the slow loop cross-checks it against the primary channel. If
    // this stream closes (the redundant detector dies), channel B goes stale → the divergence
    // monitor fails closed (redundancy lost), the intended fail-safe.
    if let Some(obj_b_stream) = obj_b_stream {
        let obj_state_b = Arc::clone(&state);
        tokio::spawn(async move {
            let mut s = obj_b_stream;
            while let Some(msg) = s.next().await {
                let now = now_ms_fresh();
                tracing::info!(
                    target: "kirra::ingress",
                    topic = "objects_secondary",
                    stamp_ms = now,
                    "subscription_callback"
                );
                let parsed = crate::parsing::parse_predicted_objects(&msg);
                obj_state_b.update_objects_secondary(parsed, now);
            }
            tracing::error!("redundant objects subscription stream closed — divergence monitor will fail closed");
        });
    }

    // VRU / pedestrian drain — only spawned when the gate is enabled and the
    // subscription was registered above. Each message REPLACES the pedestrian
    // snapshot and stamps its freshness (`update_pedestrians`); the slow loop
    // reads it via `snapshot_pedestrians` with fail-closed staleness. If this
    // stream closes (the detector dies), the channel goes stale → the slow loop
    // fails closed (MRC), the intended fail-safe.
    if let Some(ped_stream) = ped_stream {
        let ped_state = Arc::clone(&state);
        tokio::spawn(async move {
            let mut s = ped_stream;
            while let Some(msg) = s.next().await {
                let now = now_ms_fresh();
                tracing::info!(
                    target: "kirra::ingress",
                    topic = "pedestrians",
                    stamp_ms = now,
                    "subscription_callback"
                );
                let parsed = crate::parsing::parse_pedestrians(&msg);
                ped_state.update_pedestrians(parsed, now);
            }
            tracing::error!(
                "pedestrian subscription stream closed — VRU channel will fail closed (MRC)"
            );
        });
    }

    // Occlusion / visibility drain (S2, #1025) — mirror of the VRU drain. Each
    // Float64 message REPLACES the assured-clear-distance snapshot and stamps its
    // freshness (`update_visibility`); the slow loop reads it via
    // `snapshot_visibility` with fail-closed staleness. Stream close (producer
    // dies) → the channel goes stale → the slow loop fails closed (MRC).
    if let Some(vis_stream) = vis_stream {
        let vis_state = Arc::clone(&state);
        tokio::spawn(async move {
            let mut s = vis_stream;
            while let Some(msg) = s.next().await {
                let now = now_ms_fresh();
                tracing::info!(
                    target: "kirra::ingress",
                    topic = "visibility",
                    stamp_ms = now,
                    "subscription_callback"
                );
                vis_state.update_visibility(msg.data, now);
            }
            tracing::error!(
                "visibility subscription stream closed — occlusion channel will fail closed (MRC)"
            );
        });
    }

    let odom_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut s = odom_stream;
        while let Some(msg) = s.next().await {
            let now = now_ms_fresh();
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
    // Sustained-divergence posture escalator — persists across slow-loop ticks (owned by this
    // task), so a divergence that PERSISTS escalates fleet posture, not just this tick's cap.
    let mut divergence_escalator = DivergenceEscalator::new();
    tokio::spawn(async move {
        let mut rx = trajectory_rx;
        while let Some(traj) = rx.recv().await {
            let start = std::time::Instant::now();
            // M7 (fail-closed): a POISONED objects cache returns `None`. Do NOT
            // validate against a phantom-empty object set (RSS would see no
            // obstacles and could pass an unsafe trajectory). Install an
            // MRCFallback verdict — which removes the accepted slot, so the fast
            // loop publishes MRC — and skip this candidate.
            let objects = match slow_state.snapshot_objects() {
                Some(o) => o,
                None => {
                    tracing::error!(
                        asset_id = %traj.asset_id,
                        trajectory_id = traj.trajectory_id,
                        "objects_cache POISONED — slow loop failing closed to MRCFallback"
                    );
                    slow_state.update_trajectory(
                        traj.asset_id.clone(),
                        traj.trajectory_id,
                        traj.points.clone(),
                        TrajectoryVerdict::MRCFallback,
                        None,
                        // No lateral envelope on the removal path — the MRC arm
                        // removes the slot and ignores it (S1, #1024).
                        None,
                        now_ms_fresh(),
                    );
                    continue;
                }
            };
            let odom = slow_state.snapshot_odom();
            let posture = slow_state.current_posture();

            // Track-C ingest tick (pure orchestration in `perception_ingest`):
            // publish the cap stamped with the OBJECTS' freshness timestamp, so
            // the cap ages with the object stream and `resolve_perception_cap`
            // fails closed (state-3 MRC) when objects go silent. If objects are
            // stale/never-seen, sweep an MRC-floor cap proactively.
            let now_mono = now_ms_fresh();
            let objects_ms = slow_state
                .last_objects_ms
                .load(std::sync::atomic::Ordering::Relaxed);
            let objects_fresh = objects_ms != 0
                && now_mono.saturating_sub(objects_ms) <= subscription_staleness_timeout_ms();
            if objects_fresh {
                publish_perception_tick(&perception_publisher, &objects, objects_ms);
            } else {
                perception_publisher.sweep_staleness(now_mono);
            }
            let effective_perception_cap =
                resolve_perception_cap(perception_derate_enabled(), &perception_cache, now_mono);

            // Perception-divergence assurance monitor (True-Redundancy analog, gap #2b) — now
            // LIVE: cross-check the primary perception channel against the optional redundant
            // channel B. A divergence (a phantom/missed object, or a speed mismatch), OR a
            // configured-but-silent channel B (redundancy LOST), maps to an MRC-floor cap that
            // composes into the SAME Track-C derate (`apply_perception_cap`) — a controlled stop
            // with no change to the WCET-critical per-pose checker. Disabled (no channel B
            // configured) → no-op, byte-identical prior behaviour.
            let objects_b = slow_state.snapshot_objects_secondary();
            let objects_b_ms = slow_state
                .last_objects_b_ms
                .load(std::sync::atomic::Ordering::Relaxed);
            let objects_b_fresh = objects_b_ms != 0
                && now_mono.saturating_sub(objects_b_ms) <= subscription_staleness_timeout_ms();
            let redundancy_cap = resolve_redundancy_cap(
                perception_redundancy_enabled(),
                &objects,
                &objects_b,
                objects_b_fresh,
                RedundancyConfig::default(),
            );
            // The more-restrictive of the Track-C derate cap and the divergence cap binds.
            let effective_perception_cap =
                more_restrictive_cap(effective_perception_cap, redundancy_cap);

            // VRU / pedestrian channel (#789 follow-up 1) — resolve the three-way
            // decision the checker's `Option` cannot express: DISARMED → the
            // no-op `None`; armed + FRESH → a live `PedestrianScene` (the
            // omnidirectional stopping bound now enforces "don't run over
            // pedestrians"); armed + SILENT/STALE → fail closed to an MRC-floor
            // cap (never a silent no-op). `snapshot_pedestrians` already applies
            // the fail-closed freshness; `resolve_vru_channel` disambiguates its
            // overloaded `None` against the enable gate.
            // Short-circuit: only READ the pedestrian snapshot when the channel is
            // armed. `snapshot_pedestrians` takes the RwLock and (on a never-seen
            // channel) logs a fail-closed error every tick — so evaluating it
            // eagerly on a DISARMED, default-off deployment would break the
            // byte-identical claim (lock + error spam). Disarmed → `None`, untouched.
            let vru_enabled = vru_channel_enabled();
            let vru = resolve_vru_channel(
                vru_enabled,
                if vru_enabled {
                    slow_state.snapshot_pedestrians(now_mono, subscription_staleness_timeout_ms())
                } else {
                    None
                },
            );
            let effective_perception_cap =
                more_restrictive_cap(effective_perception_cap, vru.perception_cap());
            let pedestrian_scene = vru.scene().map(|peds| PedestrianScene {
                pedestrians: peds,
                params: VruRssParams::default(),
                // F6 corridor-clip barriers are supplied only by a per-ODD map
                // profile (a further reviewed change); none is wired → pure disc.
                barriers: &[],
            });

            // Occlusion / assured-clear-distance channel (S2, #1025) — the sibling
            // of the VRU resolver, ARMING the RSS Rule 4 limited-visibility bound
            // that was previously fed a hardcoded `None` (dormant every tick).
            // DISARMED → the checker's `None` (occlusion gate skipped,
            // byte-identical); armed + FRESH + valid range → feed the gate the
            // observed sight distance; armed + SILENT/STALE/garbage → fail closed
            // to an MRC-floor cap (never drive blind). Same short-circuit as VRU:
            // only read the snapshot when armed, so a DISARMED default-off
            // deployment never takes the visibility lock or logs.
            let occlusion_enabled = occlusion_channel_enabled();
            let occlusion = resolve_occlusion_channel(
                occlusion_enabled,
                if occlusion_enabled {
                    slow_state.snapshot_visibility(now_mono, subscription_staleness_timeout_ms())
                } else {
                    None
                },
            );
            let effective_perception_cap =
                more_restrictive_cap(effective_perception_cap, occlusion.perception_cap());

            // Sustained-divergence → posture escalation (orthogonal to the per-tick MRC cap
            // above): a divergence (or lost redundant channel) that PERSISTS is a
            // perception-integrity fault that escalates the EFFECTIVE fleet posture — Degraded,
            // then LockedOut — so the whole stack degrades, not just this tick's speed. A
            // momentary blip leaves posture unchanged (the cap handled it); escalation-only, so
            // it can never relax the verifier-sourced base posture.
            divergence_escalator.observe(redundancy_cap == Some(0.0), now_mono);
            let posture = posture.escalate(divergence_escalator.recommended_posture(now_mono));

            // Multi-modal predictive RSS (gap #3) — roll the live objects into PredictedMode
            // hypotheses for the checker's predictive pass. The tracker's per-object yaw estimate
            // (the SAME CTRV estimate the planner consumes as MotionState) is read from the yaw
            // channel and, when FRESH, adds a CTRV turn-in mode alongside CV — genuinely
            // multi-modal. A stale / unconfigured yaw feed degrades to CV-only (the CV mode +
            // snapshot RSS still bound the object); it is dropped, not trusted, never a fault.
            let object_yaw_rates = slow_state.snapshot_object_yaw_rates();
            let object_yaw_ms = slow_state
                .last_object_yaw_ms
                .load(std::sync::atomic::Ordering::Relaxed);
            let object_yaw_fresh = object_yaw_ms != 0
                && now_mono.saturating_sub(object_yaw_ms) <= subscription_staleness_timeout_ms();
            let predicted_owned = slow_loop_modes(
                &objects,
                &object_yaw_rates,
                object_yaw_fresh,
                SLOW_PRED_HORIZON_S,
                SLOW_PRED_DT_S,
            );
            let predicted_modes: Vec<_> = predicted_owned.iter().map(|m| m.as_mode()).collect();

            // B1 fix: take the effective per-pose velocity envelope alongside
            // the verdict. On a `Clamp` it carries the checker's derated
            // ceiling to the fast loop's conformance gate; `None` on Accept.
            let (verdict, _reason, effective_ceiling) = validate_trajectory_slow_with_envelope(
                &traj.points,
                slow_corridor.as_ref(),
                &objects,
                &slow_state.config,
                odom.as_ref(),
                posture.clone(),
                effective_perception_cap,
                // Occlusion / assured-clear-distance bound (RSS Rule 4): S2 (#1025)
                // — ARMED from the occlusion channel resolved above. DISARMED →
                // `None` (gate skipped, byte-identical); armed + fresh → the live
                // sight distance so the checker refuses a trajectory that outruns
                // what the ego can see; armed + silent/garbage → `None` here but
                // the MRC-floor cap already folded into `effective_perception_cap`
                // stops the ego (never a silent no-op — see `resolve_occlusion_channel`).
                occlusion.visibility_range(),
                // Multi-modal predictive RSS (gap #3) — LIVE: the CV (and, when the tracker yaw
                // feed is fresh, CTRV) modes rolled from the live objects above. The checker
                // worst-cases over them, refusing a trajectory a predicted cut-in / turn-in
                // breaches even though the snapshot showed the object laterally clear.
                Some(&predicted_modes),
                // WS-2 pedestrian/VRU RSS (#789 follow-up 1) — LIVE: the scene
                // resolved from the `~/input/pedestrians` channel above. Armed +
                // fresh → the omnidirectional reachable-set bound refuses any
                // trajectory that comes within a pedestrian's grown stopping disc
                // (MRC). DISARMED → `None`, byte-identical no-op. Armed + silent
                // → `None` HERE but the MRC-floor cap already folded into
                // `effective_perception_cap` above stops the ego (never a silent
                // no-op — see `resolve_vru_channel`).
                pedestrian_scene.as_ref(),
                // Frame/localization integrity (S-FI1): the LIVE frame trust resolved from the
                // integrator's per-tick `update_frame_integrity` report. With no source wired
                // this returns `Trusted` — the AOU-LOCALIZATION-001 seam (byte-for-byte the
                // prior behaviour); once a source reports, the gate is live (a poor / non-finite
                // ε derates the 0.40 m → 0.75 m margin or refuses, a silent source fails closed),
                // and a sustained fault also escalates fleet posture → LockedOut (S-FI1d).
                slow_state.snapshot_frame_trust(now_mono),
            );
            // B4: the trajectory's `promoted_at_ms` (the fast loop's `is_stale`
            // anchor) uses the MONOTONIC freshness clock — the SAME `now_mono` the
            // slow-loop freshness checks use and the same clock the fast loop's
            // staleness reads — so a forward wall-clock step can never make a fresh
            // trajectory look stale. (The capture record below keeps a separate
            // wall timestamp for human/audit correlation.)
            // S1 fix (#1024): derive the posture-composed lateral envelope from
            // the SAME contract the slow loop enforced (via the shared
            // `to_posture_kinematics_contract` mapping) and attach it, so the fast
            // loop can bound the outgoing command's lateral acceleration — not
            // just its steering against the static rack limit.
            let lateral_envelope = crate::state::LateralEnvelope::from_contract(
                &slow_state
                    .config
                    .to_posture_kinematics_contract(posture.clone()),
            );
            slow_state.update_trajectory(
                traj.asset_id.clone(),
                traj.trajectory_id,
                traj.points.clone(),
                verdict,
                effective_ceiling,
                Some(lateral_envelope),
                now_mono,
            );
            let elapsed_us = start.elapsed().as_micros();
            // Phase I observability (integration-harness §I.1b): carry posture +
            // per-input freshness ages on the verdict span so a derate caused by
            // stale inputs (Branch B) is distinguishable from non-delivery
            // (Branch A) directly from the logs. A never-seen slot (last_*_ms == 0)
            // reports u64::MAX rather than a misleadingly-huge "age since epoch".
            let now_fresh = now_ms_fresh();
            let age_of = |t: u64| {
                if t == 0 {
                    u64::MAX
                } else {
                    now_fresh.saturating_sub(t)
                }
            };
            let traj_age_ms = age_of(
                slow_state
                    .last_trajectory_ms
                    .load(std::sync::atomic::Ordering::Relaxed),
            );
            let objects_age_ms = age_of(
                slow_state
                    .last_objects_ms
                    .load(std::sync::atomic::Ordering::Relaxed),
            );
            let odom_age_ms = age_of(
                slow_state
                    .last_odom_ms
                    .load(std::sync::atomic::Ordering::Relaxed),
            );
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
                    // Audit/correlation timestamp — wall-clock (human-readable),
                    // NOT the staleness clock. Computed only when capture is on.
                    wall_clock_ms(),
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
    // ADR-0030 Clause C (incr. 2) — OPT-IN guest producer. When
    // KIRRA_CONTRACT_SHM_NAME is set, additively publish each incoming proposal
    // across the frozen cross-partition contract for the L3.3 governor consumer to
    // bound. Unset → no contract publishing, byte-identical to prior behavior.
    // Best-effort: a publish issue NEVER affects the gated ~/output/control_cmd
    // path. (Host PosixShmRegion carrier; the QNX HvRegion binds the same seam.)
    let mut contract_writer: Option<PosixShmRegion> = None;
    let mut contract_seq: Option<ProposalSequencer> = None;
    if let Ok(name) = std::env::var("KIRRA_CONTRACT_SHM_NAME") {
        // OPEN an existing region — the guest never CREATES/OWNS it. The region is
        // provided by the platform (the QNX hypervisor for HvRegion; a host setup
        // step / the governor side for PosixShmRegion). If the guest owned it, a
        // node restart would shm_unlink the object out from under a long-lived
        // governor still mapped to it, silently disconnecting the reader from new
        // publishes. Absent region → log + don't publish (fail-safe; the gated
        // ~/output/control_cmd path is unaffected).
        match PosixShmRegion::open(&name) {
            Ok(w) => {
                tracing::info!(shm = %name, "contract producer: publishing proposals to the cross-partition channel");
                contract_writer = Some(w);
                contract_seq = Some(ProposalSequencer::new());
            }
            Err(e) => {
                tracing::warn!(shm = %name, error = %e, "contract producer: region not available; proposals NOT published (gated output unaffected)");
            }
        }
    }
    tokio::spawn(async move {
        let mut rx = control_rx;
        while let Some(in_cmd) = rx.recv().await {
            let start = std::time::Instant::now();
            // B4: freshness clock is MONOTONIC — must match the subscription
            // stamps + `promoted_at_ms` so `any_subscription_stale` / `is_stale`
            // measure a true elapsed age, immune to a wall-clock step.
            let now_ms = now_ms_fresh();
            let cmd = IncomingControl {
                velocity_mps: in_cmd.linear_velocity_mps,
                steering_rad: in_cmd.steering_angle_rad,
                stamp_ms: in_cmd.stamp_ms,
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
                let out = mrc_command(in_cmd.asset_id.clone(), fast_state.config.max_decel_mps2);
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
                Some(t) => check_command_conforms(&cmd, t, &odom, &fast_state.config, now_ms),
                None => ConformanceVerdict::MRCFallback,
            };
            let out = match verdict {
                ConformanceVerdict::Accept => cmd_to_output(&in_cmd.asset_id, &cmd),
                ConformanceVerdict::MRCFallback => {
                    mrc_command(in_cmd.asset_id.clone(), fast_state.config.max_decel_mps2)
                }
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

            // ADR-0030 Clause C — additively publish the DOER's proposal across
            // the frozen contract for the governor to bound (L3.3). Placed AFTER
            // the conformance WCET window above so it never perturbs that metric;
            // best-effort — it can't affect the gated ~/output/control_cmd path.
            if let (Some(writer), Some(sequencer)) =
                (contract_writer.as_ref(), contract_seq.as_mut())
            {
                // Current velocity from ego odom. The guest has NO steering
                // measurement (EgoOdom carries yaw rate, not steering angle), so
                // the steering-rate baseline is the desired angle (rate 0) — the
                // AUTHORITATIVE current state is governor-measured on the real path
                // (HVCHAN R-HV-3); this guest field is untrusted regardless.
                let payload = proposal_payload(
                    &in_cmd,
                    odom.linear_x_mps,
                    in_cmd.steering_angle_rad.to_degrees(),
                    CONTROL_CYCLE_S,
                );
                let pub_ns = now_ns_fresh();
                let _ = sequencer.publish_to(
                    writer,
                    &payload,
                    pub_ns,
                    pub_ns.saturating_add(CONTRACT_DEADLINE_BUDGET_NS),
                );
            }
        }
    });

    // ----- Output publisher (Phase 4 — the real output edge) -----------
    //
    // The gated fast-loop command is now PUBLISHED on `~/output/control_cmd`,
    // closing the doer→checker→actuator loop. Before this it was a
    // `tracing::debug!` placeholder, which is why end-to-end FTTI was not
    // measurable; with a real publish on every cycle, the loop-closure timing is.
    //
    // UNTYPED `autoware_control_msgs/msg/Control` — mirrors the untyped
    // `~/input/control_cmd` subscription, so the adapter pulls no typed `Control`
    // binding at build time (AOU-MSG-TOOLCHAIN). Reliable + KeepLast(1)
    // (`actuator_output_qos`): a gated command is never silently dropped, but only
    // the freshest one is relevant. The publisher is created from `node` (still
    // owned here — it moves into the spin loop below) and moved into the drain
    // task. Every drained verdict output (Accept pass-through or MRC) is
    // published; a publish failure is loud, never swallowed.
    let control_pub = node.create_publisher_untyped(
        "~/output/control_cmd",
        "autoware_control_msgs/msg/Control",
        actuator_output_qos(),
    )?;
    tokio::spawn(async move {
        while let Some(out) = fast_loop_out_rx.recv().await {
            let msg = control_command_to_json(&out, wall_clock_ms());
            if let Err(e) = control_pub.publish(msg) {
                tracing::error!(
                    asset_id = %out.asset_id,
                    error = %e,
                    "control_cmd publish FAILED — gated command did not reach the vehicle interface"
                );
            }
        }
    });

    // ----- Drop-on-full helpers (legacy local helpers kept for tests /
    //       future refactors). The live trajectory and control drains above
    //       enforce this same bounded, never-blocking policy inline.
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
                tracing::error!("control channel CLOSED — fast loop is gone; adapter must restart");
            }
        }
    }

    // The drain tasks own clones. Keep the original senders bound so the
    // compiler makes lifetime/ownership explicit at the end of setup.
    let _ = (trajectory_tx, control_tx);

    // ----- Spin loop ----------------------------------------------------
    //
    // r2r's spin model is to drive the node's executor on a regular tick.
    // `spin_once` is a SYNCHRONOUS, thread-blocking call — it parks the
    // calling thread on the rcl wait-set for up to its timeout (~10 ms here),
    // so spinning it directly on the async runtime stalls a tokio worker for
    // that whole window every tick. Move the spin loop onto a dedicated
    // blocking thread via `spawn_blocking` so it never occupies a worker;
    // `node` is owned and unused after this point, so it moves cleanly into
    // the closure. Phase 4 wires in a shutdown channel; Phase 1 just loops.
    tokio::task::spawn_blocking(move || loop {
        node.spin_once(Duration::from_millis(10));
    })
    .await
    .expect("r2r spin-loop blocking task panicked");

    Ok(())
}
