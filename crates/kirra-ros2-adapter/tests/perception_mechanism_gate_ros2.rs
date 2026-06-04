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
// LAUNCH-DOCUMENTED HERE (not a cargo assertion — needs a live node graph): the
// full slow-loop tick. See `run_full_node_integration` (ignored) + the recipe in
// its doc comment: launch `kirra_ros2_adapter_node`, set
// KIRRA_PERCEPTION_DERATE_ENABLED=1, publish each scenario's PredictedObjects (+
// a trajectory) with `publish_fixture_objects`, and observe the emitted gated
// `~/output/control_cmd` matches the Layer-1 expected cap for that scenario.
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

// --- LAUNCH-DOCUMENTED: full node slow-loop tick (needs a live ROS 2 graph) ---

/// FULL INTEGRATION — the node slow-loop tick end-to-end. NOT a self-contained
/// cargo assertion: it needs a running `kirra_ros2_adapter_node`, a synthetic
/// publisher, and an observer on the output topic. Marked `#[ignore]`; run it as
/// a guided manual/launch procedure (or wire it into a ROS 2 launch_test):
///
///   1. Build + launch the adapter node (mock corridor; perception ON):
///        KIRRA_PERCEPTION_DERATE_ENABLED=1 \
///        cargo run -p kirra-ros2-adapter --features ros2 \
///            --bin kirra_ros2_adapter_node -- --corridor-source mock
///   2. Publish a steady trajectory on `~/input/trajectory` (≥ 2 poses, e.g.
///      a straight line at the test speed) at planning rate (~10 Hz).
///   3. For each scenario, publish `fixture_to_predicted_objects(&scenario_*())`
///      on `~/input/objects` at sensor rate. For scenario (d), STOP publishing
///      after a few cycles and wait > ttl (500 ms).
///   4. Observe the emitted gated command on `~/output/control_cmd` and assert
///      it matches the Layer-1 expectation for that scenario:
///        (b) unchanged vs the perception-OFF baseline run;
///        (c1) controlled stop (MRC);  (c2) clamped to 16.7625 m/s;
///        (d) controlled stop after the stream goes silent;
///        (e) re-run with the env var UNSET → byte-identical to the OFF baseline.
///   This is the only step that exercises the node slow-loop tick
///   (publish_perception_tick → resolve_perception_cap → validate_trajectory_slow_capped
///   inside run_adapter); the round-trip tests above cover only the decode.
#[test]
#[ignore = "full node graph + synthetic publisher + topic observer; run via the launch recipe in the doc comment"]
fn run_full_node_integration() {
    // Intentionally a placeholder for the launch-driven procedure above. A
    // future ROS 2 launch_test (or an r2r in-process publisher + a subscription
    // on ~/output/control_cmd around `run_adapter`) would automate steps 1-4.
}
