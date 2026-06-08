// crates/kirra-ros2-adapter/tests/perception_mechanism_gate_ros2.rs
//
// PMON-004 sub-gate 1 — MECHANISM harness, LAYER 2 (ros2-gated; runs in a ROS 2
// environment, NOT in CI). This is the layer that exercises the CI-UNREACHABLE
// wiring: `parse_predicted_objects` (the r2r decode) and — via the documented
// node launch — the node slow-loop tick (node.rs:364-380).
//
// ====================================================================
// HOW TO RUN (this file does NOT run in CI — no ros2 job; the sandbox has no
// ROS and an older toolchain). Run it in a ROS 2 container / dev box with the
// integrator's Autoware messages on AMENT_PREFIX_PATH (Humble/Jazzy/Kilted),
// or on the R2's Orin when it arrives:
//
//   source /opt/ros/${ROS_DISTRO}/setup.bash
//   # autoware_perception_msgs (+ geometry_msgs, unique_identifier_msgs) must be
//   # discoverable so r2r generates the bindings.
//   cargo test -p kirra-ros2-adapter --features ros2 \
//       --test perception_mechanism_gate_ros2
//
// AUTO-TESTED HERE (a real ros2 cargo test): the `parse_predicted_objects`
// round-trip — build r2r `autoware_perception_msgs/PredictedObjects` from the
// shared fixtures, decode them, and assert the resulting `PerceivedObject`s match
// the fixture (id/pos/velocity-vector). This deterministically exercises the
// r2r message DECODE (half the CI-unreachable wiring) and reuses the same pure
// pipeline + expected caps as Layer 1.
//
// AUTOMATED HERE (a real ros2 cargo test, gated `#[ignore]` for the live node
// graph): the full slow-loop tick. `run_full_node_integration` spawns
// `run_adapter` over real DDS, publishes each scenario's PredictedObjects (+ a
// trajectory + odom) on the adapter's resolved `~/input/*` topics, and reads the
// slow loop's `TrajectoryVerdict` from the SHARED `AdaptorState` — asserting
// plausible→Accept and implausible/stale→Clamp with the derate enabled (and
// every scenario→Accept with it OFF, the negative control). Run it with
// `-- --ignored` on a ROS-sourced dev box (recipe in its doc comment). The node
// publishes no output topic yet (Phase 4), so the shared state slot — not a
// `~/output/control_cmd` message — is the observation point; the exact graded m/s
// cap stays a Layer-1 assertion.
// ====================================================================
//
// SCOPE (restated): the synthetic twists are values WE choose, so a green Layer 2
// says NOTHING about whether real Autoware emits absolute map-frame twist — that
// is sub-gate 2 (AWSIM). AOU-PERCEPTION-FRAME-001 stays OPEN;
// KIRRA_PERCEPTION_DERATE_ENABLED stays OFF. Governor boundary: drive INPUT
// (pre-formed PredictedObjects), observe OUTPUT — no perception built here.

#![cfg(feature = "ros2")]

mod common;

use common::*;
use kirra_ros2_adapter::parsing::parse_predicted_objects;

/// Build an r2r `autoware_perception_msgs/PredictedObjects` from the shared
/// fixtures — the INVERSE of `parse_predicted_objects` (parsing.rs §
/// parse_predicted_objects). Sets only the fields the parser reads; everything
/// else is `Default::default()` so it compiles against whatever the integrator's
/// Autoware version generates.
///
/// Field paths (must mirror parse_predicted_objects):
///   object_id.uuid                                  ← id big-endian in bytes 0..8
///   kinematics.initial_pose_with_covariance.pose.position.{x,y}  ← pos
///   kinematics.initial_twist_with_covariance.twist.linear.{x,y}  ← velocity vector
pub fn fixture_to_predicted_objects(
    fixtures: &[FixtureObj],
) -> r2r::autoware_perception_msgs::msg::PredictedObjects {
    use r2r::autoware_perception_msgs::msg::{PredictedObject, PredictedObjects};

    let objects = fixtures
        .iter()
        .map(|f| {
            let mut obj = PredictedObject::default();

            // object_id.uuid: parse folds the FIRST 8 bytes big-endian → u64,
            // so bytes 0..8 = id.to_be_bytes() round-trips the id exactly.
            let be = f.id.to_be_bytes();
            for (i, b) in be.iter().enumerate() {
                obj.object_id.uuid[i] = *b;
            }

            let pose = &mut obj.kinematics.initial_pose_with_covariance.pose;
            pose.position.x = f.x_m;
            pose.position.y = f.y_m;
            // Identity orientation (quat_to_yaw of (0,0,0,1) = 0); heading is not
            // part of the kinematic ceiling check.
            pose.orientation.w = 1.0;

            let twist = &mut obj.kinematics.initial_twist_with_covariance.twist;
            twist.linear.x = f.vx;
            twist.linear.y = f.vy;

            obj
        })
        .collect();

    PredictedObjects { objects, ..Default::default() }
}

// --- AUTO-TESTED: r2r decode round-trip (exercises parse_predicted_objects) ---

/// Build → decode → assert the decoded `PerceivedObject`s match the fixtures for
/// every mechanism scenario. This is the deterministic ros2 cargo test that
/// exercises the r2r message decode path.
#[test]
fn parse_predicted_objects_roundtrips_all_scenarios() {
    for fixtures in [scenario_b(), scenario_c1(), scenario_c2()] {
        let msg = fixture_to_predicted_objects(&fixtures);
        let decoded = parse_predicted_objects(&msg);
        let expected = perceived_vec(&fixtures);
        assert_eq!(decoded.len(), expected.len());
        for (d, e) in decoded.iter().zip(expected.iter()) {
            assert_eq!(d.id, e.id, "id round-trips through the uuid fold");
            assert!((d.pos.x_m - e.pos.x_m).abs() < 1e-9);
            assert!((d.pos.y_m - e.pos.y_m).abs() < 1e-9);
            // The PRESERVED velocity vector (PMON-003 §5) survives the decode.
            assert!((d.vel.x_m - e.vel.x_m).abs() < 1e-9, "vx preserved");
            assert!((d.vel.y_m - e.vel.y_m).abs() < 1e-9, "vy preserved");
            assert!((d.velocity_mps - e.velocity_mps).abs() < 1e-9);
        }
    }
}

/// End-to-end check that the DECODED objects, fed through the SAME pure pipeline
/// as Layer 1, produce the SAME expected caps — i.e. the r2r decode + the pure
/// mechanism agree. (Layer 1 asserts the caps from hand-built PerceivedObjects;
/// this asserts them from decoded r2r messages.)
#[test]
fn decoded_objects_produce_expected_caps() {
    let now = 1_000;
    let cases: [(Vec<FixtureObj>, Option<f64>); 3] = [
        (scenario_b(), Some(NOMINAL_CAP_MPS)),
        (scenario_c1(), Some(MRC_FLOOR_CAP_MPS)),
        (scenario_c2(), Some(C2_GRADED_CAP_MPS)),
    ];
    for (fixtures, expected) in cases {
        let msg = fixture_to_predicted_objects(&fixtures);
        let decoded = parse_predicted_objects(&msg);
        let cap = published_cap(&decoded, /*enabled*/ true, now, now);
        match (cap, expected) {
            (Some(c), Some(e)) => assert!((c - e).abs() < 1e-9, "decoded cap {c} != expected {e}"),
            (a, b) => assert_eq!(a, b),
        }
    }
}

// --- AUTOMATED: full node slow-loop tick over LIVE DDS (ros2; #[ignore]) ---

use std::sync::Arc;
use std::time::{Duration, Instant};

use kirra_ros2_adapter::corridor::{CorridorSource, MockCorridorSource};
use kirra_ros2_adapter::node::run_adapter;
use kirra_ros2_adapter::perception_ingest::perception_derate_enabled;
use kirra_ros2_adapter::state::{AdaptorState, TrajectoryVerdict};

/// Adapter node name + namespace the harness publishes INTO. `run_adapter`
/// creates the node in namespace `"kirra"`; the harness picks the name, so the
/// adapter's relative `~/input/*` topics resolve to `/kirra/<NODE_NAME>/input/*`
/// and the harness publishes on those fully-qualified names (G4).
const NODE_NAME: &str = "pmon004_subgate1";
const NS: &str = "kirra";

/// Commanded trajectory speed. Chosen so the perception cap is the ONLY actor:
///   < NOMINAL_CAP_MPS (22.35)  → plausible objects ⇒ Accept (no derate)
///   > C2_GRADED_CAP_MPS (16.7625) and > MRC_FLOOR_CAP_MPS (0.0)
///                              → implausible / stale ⇒ per-pose ClampLinear ⇒ Clamp
const TRAJ_SPEED_MPS: f64 = 20.0;

fn input_topic(leaf: &str) -> String {
    format!("/{NS}/{NODE_NAME}/input/{leaf}")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A 3-pose straight trajectory at x = 60, 62, 64 (inside the 0..100 × ±5 m mock
/// corridor → containment passes) and AHEAD of every fixture object (x ≤ 40), so
/// each object is behind ego (`dx_ego ≤ 0`) and RSS is skipped — leaving the
/// perception cap as the only thing that can change the verdict.
fn build_trajectory_msg() -> r2r::autoware_planning_msgs::msg::Trajectory {
    use r2r::autoware_planning_msgs::msg::{Trajectory, TrajectoryPoint};
    let points = (0..3)
        .map(|i| {
            let mut pt = TrajectoryPoint::default();
            pt.pose.position.x = 60.0 + 2.0 * i as f64;
            pt.pose.position.y = 0.0;
            pt.pose.orientation.w = 1.0; // identity → yaw 0
            pt.longitudinal_velocity_mps = TRAJ_SPEED_MPS as f32;
            pt.time_from_start.sec = 0;
            pt.time_from_start.nanosec = (i as u32) * 100_000_000; // 0, 0.1, 0.2 s
            pt
        })
        .collect();
    Trajectory { points, ..Default::default() }
}

/// Ego odom at the trajectory speed, zero yaw-rate (→ derived current steering 0,
/// so the per-pose kinematics isolate the velocity ceiling).
fn build_odom_msg() -> r2r::nav_msgs::msg::Odometry {
    let mut o = r2r::nav_msgs::msg::Odometry::default();
    o.twist.twist.linear.x = TRAJ_SPEED_MPS;
    o.twist.twist.angular.z = 0.0;
    o
}

/// Publish trajectory + odom (+ optional objects) at ~20 Hz and poll the SHARED
/// `AdaptorState` slot for `"ego"` until its fail-closed verdict equals `want`, or
/// fail after a bounded timeout. `settle_before_ms` lets scenario (d) wait past
/// the perception TTL so previously-published objects go stale.
#[allow(clippy::too_many_arguments)]
async fn drive_until(
    state: &Arc<AdaptorState>,
    traj_pub: &r2r::Publisher<r2r::autoware_planning_msgs::msg::Trajectory>,
    odom_pub: &r2r::Publisher<r2r::nav_msgs::msg::Odometry>,
    obj_pub: &r2r::Publisher<r2r::autoware_perception_msgs::msg::PredictedObjects>,
    objects: Option<&r2r::autoware_perception_msgs::msg::PredictedObjects>,
    want: TrajectoryVerdict,
    settle_before_ms: u64,
    label: &str,
) {
    let traj = build_trajectory_msg();
    let odom = build_odom_msg();
    if settle_before_ms > 0 {
        tokio::time::sleep(Duration::from_millis(settle_before_ms)).await;
    }
    // 8 s covers first-scenario DDS discovery + the slow-loop period; later
    // scenarios are warm.
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut last = TrajectoryVerdict::MRCFallback;
    while Instant::now() < deadline {
        let _ = traj_pub.publish(&traj);
        let _ = odom_pub.publish(&odom);
        if let Some(o) = objects {
            let _ = obj_pub.publish(o);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Read the slow loop's installed verdict for "ego" (the node's traj
        // drain stamps that asset id). `current_verdict` collapses a stale slot
        // to MRCFallback, so we publish fast enough (<200 ms) to keep it fresh.
        last = state.current_verdict("ego", now_ms());
        tracing::debug!(label, observed = ?last, t_ms = now_ms(), "drive_until poll");
        if last == want {
            return;
        }
    }
    panic!("scenario [{label}]: expected {want:?} within 8 s; last observed {last:?}");
}

/// FULL INTEGRATION — the node slow-loop tick end-to-end over LIVE DDS, AUTOMATED.
/// Spawns `run_adapter` (its own r2r node) sharing an `Arc<AdaptorState>` with the
/// harness, publishes each scenario's inputs on the adapter's resolved
/// `~/input/{objects,trajectory,odometry}` topics, and asserts the slow loop's
/// `TrajectoryVerdict` (read from the shared state) responds to the perception
/// derate. This exercises the CI-unreachable wiring:
///   subscriptions → drain tasks → slow loop → publish_perception_tick →
///   resolve_perception_cap → validate_trajectory_slow_capped → update_trajectory.
///
/// Still `#[ignore]` (needs a live ROS 2 graph; not for `cargo test`); run it on a
/// dev box with ROS sourced + the Autoware msgs discoverable:
///
///   source /opt/ros/${ROS_DISTRO}/setup.bash
///   # derate ON (asserts plausible→Accept, implausible/stale→Clamp):
///   KIRRA_PERCEPTION_DERATE_ENABLED=1 cargo test -p kirra-ros2-adapter \
///       --features ros2 --test perception_mechanism_gate_ros2 -- --ignored
///   # negative control, derate OFF (env unset → every scenario Accepts):
///   cargo test -p kirra-ros2-adapter --features ros2 \
///       --test perception_mechanism_gate_ros2 -- --ignored
///
/// SCOPE: this proves the derate MECHANISM is live through the node (plausible vs
/// implausible discrimination + the ON/OFF delta). The verdict is coarse
/// (Accept / Clamp / MRCFallback), so it does NOT pin the exact graded cap (c1's
/// 0.0 vs c2's 16.7625) — those exact m/s caps stay a Layer-1 assertion
/// (`decoded_objects_produce_expected_caps`). It also says NOTHING about whether
/// real Autoware emits absolute map-frame twist: that is sub-gate 2 (AWSIM),
/// AOU-PERCEPTION-FRAME-001 stays OPEN, and KIRRA_PERCEPTION_DERATE_ENABLED stays
/// OFF in production. Governor boundary: the harness only publishes and reads
/// shared state — it builds no perception and never touches the decision logic.
#[test]
#[ignore = "live ROS 2 node graph over real DDS; run via `-- --ignored` on a ROS-sourced dev box (see doc comment)"]
fn run_full_node_integration() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(run_full_node_integration_async());
    // run_adapter has no shutdown channel (Phase 4) and r2r's spin is a blocking
    // thread tokio abort can't cancel; force teardown instead of hanging at drop.
    rt.shutdown_timeout(std::time::Duration::from_millis(200));
}

async fn run_full_node_integration_async() {
    // Phase I observability: install a tracing subscriber so node.rs's existing
    // `trajectory_verdict` INFO install events + drain warn/error events become
    // visible. try_init avoids a panic on repeated runs; with_test_writer routes to
    // captured test output (shown with --nocapture). Default INFO; RUST_LOG overrides.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();
    // INV-4: the derate is enabled ONLY by the process env (the run recipe
    // exports it); we READ it here, never `set_var`. The node reads the same env
    // internally, so the harness expectation and the node behaviour agree.
    let derate_on = perception_derate_enabled();

    // 1) Spawn the adapter node, sharing the AdaptorState so we can observe the
    //    slow-loop verdict (the node publishes no output topic — Phase 4 — so the
    //    shared slot is the observation point).
    let state = AdaptorState::new();
    let corridor: Arc<dyn CorridorSource> =
        Arc::new(MockCorridorSource::straight_5m_half_width(100.0));
    let adapter_state = Arc::clone(&state);
    let adapter = tokio::spawn(async move {
        match run_adapter(adapter_state, corridor, NODE_NAME).await {
            Ok(()) => eprintln!("[harness] run_adapter returned Ok (it should spin forever)"),
            Err(e) => eprintln!("[harness] run_adapter ERROR at startup: {e:?}"),
        }
    });

    // 2) Harness publisher node (its own r2r context → real DDS to the adapter).
    let ctx = r2r::Context::create().expect("harness r2r context");
    let mut node = r2r::Node::create(ctx, "pmon004_harness", NS).expect("harness node");
    let traj_pub = node
        .create_publisher::<r2r::autoware_planning_msgs::msg::Trajectory>(
            &input_topic("trajectory"),
            r2r::QosProfile::default(),
        )
        .expect("trajectory publisher");
    let obj_pub = node
        .create_publisher::<r2r::autoware_perception_msgs::msg::PredictedObjects>(
            &input_topic("objects"),
            r2r::QosProfile::default(),
        )
        .expect("objects publisher");
    let odom_pub = node
        .create_publisher::<r2r::nav_msgs::msg::Odometry>(
            &input_topic("odometry"),
            r2r::QosProfile::default(),
        )
        .expect("odometry publisher");

    let objs_b = fixture_to_predicted_objects(&scenario_b());
    let objs_c1 = fixture_to_predicted_objects(&scenario_c1());
    let objs_c2 = fixture_to_predicted_objects(&scenario_c2());

    if derate_on {
        // (b) PLAUSIBLE → no derate → trajectory accepted at 20 m/s.
        drive_until(&state, &traj_pub, &odom_pub, &obj_pub, Some(&objs_b),
            TrajectoryVerdict::Accept, 0, "b plausible → Accept").await;
        // (c1) SINGLE IMPLAUSIBLE → MRC-floor cap (0.0) → per-pose clamp → Clamp.
        drive_until(&state, &traj_pub, &odom_pub, &obj_pub, Some(&objs_c1),
            TrajectoryVerdict::Clamp, 0, "c1 single-implausible → derated (Clamp)").await;
        // (c2) GRADED (1-of-10) → cap 16.7625 < 20 → per-pose clamp → Clamp.
        drive_until(&state, &traj_pub, &odom_pub, &obj_pub, Some(&objs_c2),
            TrajectoryVerdict::Clamp, 0, "c2 graded → derated (Clamp)").await;
        // (d) SILENT → objects go stale past the TTL → swept to the MRC-floor cap
        //     → Clamp. Settle > TTL_MS so the prior (c2) objects are stale, then
        //     publish NO objects.
        drive_until(&state, &traj_pub, &odom_pub, &obj_pub, None,
            TrajectoryVerdict::Clamp, TTL_MS + 200, "d silent → staleness → derated (Clamp)").await;
    } else {
        // NEGATIVE CONTROL (#159-style): derate OFF → the cap is never applied, so
        // every scenario — plausible OR implausible — accepts at 20 m/s. The
        // ON-vs-OFF delta is the evidence the mechanism is what changed the verdict.
        // Baseline: trajectory + odom only, NO objects. With derate OFF the perception
        // cap is never applied, so a clean in-corridor 20 m/s trajectory MUST Accept.
        // If THIS times out on MRCFallback, the failure is delivery/install (Branch A),
        // not perception — and the trace above says which.
        drive_until(&state, &traj_pub, &odom_pub, &obj_pub, None,
            TrajectoryVerdict::Accept, 0, "baseline: traj+odom only -> Accept").await;

        for (objs, label) in [
            (&objs_b, "b (derate OFF) → Accept"),
            (&objs_c1, "c1 (derate OFF) → Accept"),
            (&objs_c2, "c2 (derate OFF) → Accept"),
        ] {
            drive_until(&state, &traj_pub, &odom_pub, &obj_pub, Some(objs),
                TrajectoryVerdict::Accept, 0, label).await;
        }
    }

    // Clean shutdown: stop the adapter task (run_adapter spins forever — there is
    // no shutdown channel yet, Phase 4 — so abort is the clean exit), then drop
    // the harness node.
    adapter.abort();
}

