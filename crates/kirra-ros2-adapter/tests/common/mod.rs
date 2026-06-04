// crates/kirra-ros2-adapter/tests/common/mod.rs
//
// Shared fixtures + helpers for the PMON-004 sub-gate-1 MECHANISM harness
// (KIRRA-OCCY-PMON-004). Used by BOTH layers so the synthetic objects and their
// expected caps are a single source of truth:
//   - tests/perception_mechanism_gate.rs       (Layer 1, default features, CI)
//   - tests/perception_mechanism_gate_ros2.rs  (Layer 2, #![cfg(feature="ros2")])
//
// This module is NOT ros2-gated and adds NO perception logic — it only defines
// pre-formed object fixtures (id/pos/velocity-vector WE choose) and reuses the
// real pure pipeline (perceived_to_tracked → ingest_perception_output →
// PerceptionCapPublisher::on_tick → resolve_perception_cap → apply_perception_cap)
// + the kernel simulator. Drive INPUT, observe OUTPUT — governor boundary held.
//
// SCOPE: these are values WE pick, so a passing mechanism test says NOTHING
// about whether real Autoware emits absolute map-frame twist. That is sub-gate 2
// (AWSIM) and AOU-PERCEPTION-FRAME-001 — which stays OPEN. Sub-gate 1 green does
// NOT enable KIRRA_PERCEPTION_DERATE_ENABLED.

#![allow(dead_code)] // each test binary uses a subset of these helpers

use kirra_ros2_adapter::corridor::Point;
use kirra_ros2_adapter::perception_ingest::publish_perception_tick;
use kirra_ros2_adapter::state::PerceivedObject;

use kirra_runtime_sdk::gateway::kinematics_contract::{
    EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract, URBAN_ODD_SPEED_CAP_MPS,
};
use kirra_runtime_sdk::gateway::perception_monitor::{
    apply_perception_cap, empty_perception_cap, resolve_perception_cap,
};

// We use `apply_enforcement` from kinematics_sim as the enforcement→command
// bridge so the gated outcome matches the production clamp logic exactly.
use kirra_runtime_sdk::kinematics_sim::apply_enforcement;

// The adapter publisher uses `KinematicPlausibilityContract::urban_reference()`
// (see node.rs): nominal cap = URBAN_ODD_SPEED_CAP_MPS (22.35), MRC floor = 0.0,
// ceiling V_OBJECT_MAX_MPS = 60. The publisher ttl reuses the subscription
// staleness budget (500 ms).
use kirra_runtime_sdk::gateway::perception_monitor::{
    KinematicPlausibilityContract, PerceptionCapPublisher,
};

/// Staleness/ttl budget the publisher uses (mirrors SUBSCRIPTION_STALENESS_TIMEOUT_MS).
pub const TTL_MS: u64 = 500;

/// Expected caps (confirmed against KIN_DERATE_TABLE in PMON-004 §3.1).
pub const NOMINAL_CAP_MPS: f64 = URBAN_ODD_SPEED_CAP_MPS; // 22.35 — no derate
pub const MRC_FLOOR_CAP_MPS: f64 = 0.0; // c1 single-implausible & d silent → stop
/// c2: 1-of-10 implausible → fraction 0.10 → table factor 0.75 → 0.75 × 22.35.
pub const C2_GRADED_CAP_MPS: f64 = 0.75 * URBAN_ODD_SPEED_CAP_MPS; // = 16.7625

/// Ground-frame velocity ceiling the guard checks against (for fixture clarity).
pub const V_OBJECT_MAX_MPS: f64 = 60.0;

/// A pre-formed synthetic object: chosen id + position + map-frame velocity
/// vector. NOT produced by any tracker — an input fixture only.
#[derive(Debug, Clone, Copy)]
pub struct FixtureObj {
    pub id: u64,
    pub x_m: f64,
    pub y_m: f64,
    pub vx: f64,
    pub vy: f64,
}

impl FixtureObj {
    /// Map to the adapter's `PerceivedObject` (what `parse_predicted_objects`
    /// produces on the ros2 side; Layer 1 builds it directly).
    pub fn perceived(&self) -> PerceivedObject {
        PerceivedObject {
            id: self.id,
            pos: Point { x_m: self.x_m, y_m: self.y_m },
            velocity_mps: (self.vx * self.vx + self.vy * self.vy).sqrt(),
            heading_rad: self.vy.atan2(self.vx),
            vel: Point { x_m: self.vx, y_m: self.vy },
        }
    }
}

pub fn perceived_vec(fixtures: &[FixtureObj]) -> Vec<PerceivedObject> {
    fixtures.iter().map(|f| f.perceived()).collect()
}

// --- Scenario fixtures --------------------------------------------------------

/// (b) PLAUSIBLE: 2 objects, both speed < 60 → no derate (nominal cap).
pub fn scenario_b() -> Vec<FixtureObj> {
    vec![
        FixtureObj { id: 1, x_m: 12.0, y_m: 1.0, vx: 5.0, vy: 0.0 },  // 5 m/s
        FixtureObj { id: 2, x_m: 20.0, y_m: -1.0, vx: 3.0, vy: 4.0 }, // 5 m/s
    ]
}

/// (c1) SINGLE IMPLAUSIBLE: 1 object > 60 → fraction 1.0 > 0.50 → MRC floor.
pub fn scenario_c1() -> Vec<FixtureObj> {
    vec![FixtureObj { id: 1, x_m: 30.0, y_m: 0.0, vx: 70.0, vy: 0.0 }] // 70 m/s
}

/// (c2) MIXED: 10 objects, exactly 1 over 60 → fraction 0.10 → 0.75 × nominal.
pub fn scenario_c2() -> Vec<FixtureObj> {
    let mut v: Vec<FixtureObj> = (0..9)
        .map(|i| FixtureObj { id: i as u64, x_m: 10.0 + i as f64, y_m: 0.0, vx: 5.0, vy: 0.0 })
        .collect();
    v.push(FixtureObj { id: 99, x_m: 40.0, y_m: 0.0, vx: 65.0, vy: 0.0 }); // 1 implausible
    v
}

// --- Pure-pipeline helpers (the real adapter + kernel functions) --------------

/// Run the real ingest tick over `objects` and resolve the cap — the exact path
/// the node slow loop runs: perceived_to_tracked → ingest_perception_output →
/// PerceptionCapPublisher::on_tick → resolve_perception_cap.
///
/// `tick_ms` is the objects' freshness stamp; `now_ms` is the resolve clock
/// (set `now_ms - tick_ms > TTL_MS` to drive staleness / scenario d).
pub fn published_cap(
    objects: &[PerceivedObject],
    enabled: bool,
    tick_ms: u64,
    now_ms: u64,
) -> Option<f64> {
    let cache = empty_perception_cap();
    let publisher =
        PerceptionCapPublisher::new(cache.clone(), KinematicPlausibilityContract::urban_reference(), TTL_MS);
    publish_perception_tick(&publisher, objects, tick_ms);
    resolve_perception_cap(enabled, &cache, now_ms)
}

/// The gated outcome for a single commanded velocity: compose the resolved cap
/// into the Nominal contract via `apply_perception_cap`, then run the production
/// enforcement bridge. Returns the post-enforcement linear velocity, or `None`
/// on a deny (not expected in these scenarios — the mechanism is derate-only).
pub fn gated_linear_mps(eff_cap: Option<f64>, commanded_v: f64) -> Option<f64> {
    let base = VehicleKinematicsContract::nominal_reference_profile(); // max 35, no ODD cap
    let contract = apply_perception_cap(&base, eff_cap);
    let cmd = steady_cmd(commanded_v);
    apply_enforcement(&cmd, &contract).map(|c| c.linear_velocity_mps)
}

/// The raw (governor-bypassed) outcome — no perception cap, plain Nominal
/// contract. The #159-style negative control: the enabled-vs-this delta is the
/// evidence for (b)/(e).
pub fn baseline_linear_mps(commanded_v: f64) -> Option<f64> {
    let base = VehicleKinematicsContract::nominal_reference_profile();
    let cmd = steady_cmd(commanded_v);
    apply_enforcement(&cmd, &base).map(|c| c.linear_velocity_mps)
}

/// A steady-state command (current == commanded, no steering) so the only thing
/// that can act is the P2 velocity ceiling (isolates the perception cap).
pub fn steady_cmd(v: f64) -> ProposedVehicleCommand {
    ProposedVehicleCommand {
        linear_velocity_mps: v,
        current_velocity_mps: v,
        delta_time_s: 0.1,
        steering_angle_deg: 0.0,
        current_steering_angle_deg: 0.0,
    }
}

/// Convenience: the EnforceAction for a commanded velocity under a perception cap
/// (used where the test wants to assert the action variant directly).
pub fn gated_action(eff_cap: Option<f64>, commanded_v: f64) -> EnforceAction {
    use kirra_runtime_sdk::gateway::kinematics_contract::validate_vehicle_command;
    let base = VehicleKinematicsContract::nominal_reference_profile();
    let contract = apply_perception_cap(&base, eff_cap);
    validate_vehicle_command(&steady_cmd(commanded_v), &contract)
}
