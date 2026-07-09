// crates/kirra-ros2-adapter/tests/perception_mechanism_gate.rs
//
// PMON-004 sub-gate 1 — MECHANISM harness, LAYER 1 (CI-testable, default
// features). Asserts the scenario fixtures' EXPECTED caps + gated outcomes
// against the REAL pure pipeline (perceived_to_tracked → ingest_perception_output
// → PerceptionCapPublisher::on_tick → resolve_perception_cap → apply_perception_cap
// → enforcement). This validates the scenario DESIGN — that the expected cap
// values are correct against the real guard logic — and computationally
// re-confirms the c1/c2 step-table finding (PMON-004 §3.1).
//
// WHAT THIS LAYER DOES / DOES NOT DO:
//   - DOES (in CI, now): machine-check scenarios (b)/(c1)/(c2)/(d)/(e) against
//     the real ingest + composition + enforcement + simulator.
//   - DOES NOT: build or decode r2r `PredictedObjects`, nor run the node
//     slow-loop tick — that CI-UNREACHABLE wiring (parse_predicted_objects +
//     node.rs:364-380) is exercised by LAYER 2 (perception_mechanism_gate_ros2.rs)
//     in a ROS 2 environment.
//   - DOES NOT verify the frame assumption — the synthetic twists are values WE
//     choose. That is sub-gate 2 (AWSIM); AOU-PERCEPTION-FRAME-001 stays OPEN and
//     KIRRA_PERCEPTION_DERATE_ENABLED stays OFF.
//
// Governor boundary: drives pre-formed objects IN, observes the gated command
// OUT — no tracker/associator/detector here.

mod common;

use common::*;
use kirra_core::kinematics_contract::EnforceAction;
use kirra_core::kinematics_contract::VehicleKinematicsContract;
use kirra_core::kinematics_sim::{apply_enforcement, VehicleState};
use kirra_core::perception_monitor::apply_perception_cap;

const NOW: u64 = 1_000;
const FRESH_TICK: u64 = 1_000; // now - tick = 0 ≤ ttl → fresh

// --- scenario (b): PLAUSIBLE → no derate, gated command unchanged vs baseline ---

#[test]
fn scenario_b_plausible_publishes_nominal_cap() {
    let objs = perceived_vec(&scenario_b());
    let cap = published_cap(&objs, /*enabled*/ true, FRESH_TICK, NOW);
    assert_eq!(
        cap,
        Some(NOMINAL_CAP_MPS),
        "all-plausible snapshot → nominal cap (no derate)"
    );
}

#[test]
fn scenario_b_gated_command_unchanged_vs_disabled_baseline() {
    // A command WITHIN the nominal ODD cap (20 < 22.35) so "no derate" is
    // observable as an identical Allow on both sides (#159-style delta = 0).
    let objs = perceived_vec(&scenario_b());
    let enabled_cap = published_cap(&objs, true, FRESH_TICK, NOW);
    let gated = gated_linear_mps(enabled_cap, 20.0);
    let baseline = baseline_linear_mps(20.0);
    assert_eq!(gated, baseline, "plausible: gated == disabled baseline");
    assert_eq!(
        gated,
        Some(20.0),
        "20 m/s within the nominal envelope → unchanged"
    );
}

// --- scenario (c1): SINGLE IMPLAUSIBLE → MRC floor → controlled stop ---

#[test]
fn scenario_c1_single_implausible_is_mrc_floor() {
    let objs = perceived_vec(&scenario_c1());
    let cap = published_cap(&objs, true, FRESH_TICK, NOW);
    // fraction 1/1 = 1.0 > 0.50 → table tail → MRC floor (0.0). The
    // conservative-by-design property: a single implausible track → full stop.
    assert_eq!(
        cap,
        Some(MRC_FLOOR_CAP_MPS),
        "single implausible object → MRC floor"
    );
    assert_eq!(
        gated_action(cap, 30.0),
        EnforceAction::ClampLinear(0.0),
        "MRC-floor cap → ClampLinear(0.0) controlled stop"
    );
}

// --- scenario (c2): MIXED 1-of-10 → graded cap 0.75 × 22.35 = 16.7625 ---

#[test]
fn scenario_c2_mixed_is_graded_cap() {
    let objs = perceived_vec(&scenario_c2());
    let cap = published_cap(&objs, true, FRESH_TICK, NOW);
    // fraction 0.10 → KIN_DERATE_TABLE (0.10, 0.75) → 0.75 × 22.35 = 16.7625.
    let expected = C2_GRADED_CAP_MPS;
    assert!(
        (cap.unwrap() - expected).abs() < 1e-9,
        "1-of-10 → graded cap {expected}"
    );
    // A command above the cap is clamped TO the cap; below it passes.
    assert_eq!(
        gated_action(cap, 30.0),
        EnforceAction::ClampLinear(expected)
    );
    assert_eq!(
        gated_linear_mps(cap, 10.0),
        Some(10.0),
        "10 < cap → unchanged"
    );
}

#[test]
fn c2_finding_single_vs_mixed_differ() {
    // Re-confirms the §3.1 finding directly: one implausible object alone is a
    // STOP, but the same implausible object diluted in a 10-object scene is a
    // graded slowdown. The mechanism keys on the implausible FRACTION.
    let c1 = published_cap(&perceived_vec(&scenario_c1()), true, FRESH_TICK, NOW);
    let c2 = published_cap(&perceived_vec(&scenario_c2()), true, FRESH_TICK, NOW);
    assert_eq!(c1, Some(0.0));
    assert!(c2.unwrap() > 0.0 && c2.unwrap() < NOMINAL_CAP_MPS);
}

// --- scenario (d): SILENT STREAM → MRC stop, integrated through kinematics_sim ---

#[test]
fn scenario_d_silent_stream_resolves_to_mrc() {
    // Publish a plausible snapshot at t=1000, then resolve at t=1601:
    // now - tick = 601 > ttl 500 → stale → MRC floor (state 3).
    let objs = perceived_vec(&scenario_b());
    let stale_cap = published_cap(&objs, true, 1_000, 1_601);
    assert_eq!(
        stale_cap,
        Some(MRC_FLOOR_CAP_MPS),
        "silent stream past ttl → MRC floor"
    );
}

#[test]
fn scenario_d_gated_stop_brings_vehicle_to_rest() {
    // A moving vehicle (10 m/s) under the stale → MRC cap: the gated command is
    // ClampLinear(0.0); integrating it through the bicycle model drives velocity
    // to 0 — the derate actually stops the (sim) vehicle.
    let objs = perceived_vec(&scenario_b());
    let stale_cap = published_cap(&objs, true, 1_000, 1_601); // Some(0.0)
    let base = VehicleKinematicsContract::nominal_reference_profile();
    let contract = apply_perception_cap(&base, stale_cap);

    let mut state = VehicleState::new(0.0, 0.0, 0.0, 10.0); // moving at 10 m/s
    let keep_going = steady_cmd(10.0); // planner still wants 10 m/s
    let gated = apply_enforcement(&keep_going, &contract).expect("derate-only: never denies");
    state = state.step(&gated, base.wheelbase_m);
    assert!(
        state.velocity_mps.abs() < 1e-9,
        "MRC derate → vehicle at rest, got {}",
        state.velocity_mps
    );
}

// --- scenario (e): DISABLED → no-op, verdict path byte-identical to baseline ---

#[test]
fn scenario_e_disabled_is_noop() {
    // Even with an implausible object present, the disabled flag → resolver None
    // → no cap → gated command identical to the no-perception baseline.
    let objs = perceived_vec(&scenario_c1()); // would be MRC if enabled
    let cap = published_cap(&objs, /*enabled*/ false, FRESH_TICK, NOW);
    assert_eq!(cap, None, "disabled monitor → no cap (state 1, no-op)");
    for v in [10.0, 20.0, 30.0, 40.0] {
        assert_eq!(
            gated_linear_mps(cap, v),
            baseline_linear_mps(v),
            "disabled: gated command byte-identical to baseline at {v} m/s"
        );
    }
}
