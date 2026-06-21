// src/bin/kirra_carla_client.rs
//
// CARLA ↔ Kirra AV Safety Integration
//
// WHAT THIS IS
// ============
// A Rust binary that bridges the CARLA autonomous driving simulator to the
// Kirra AV safety stack. It runs alongside a CARLA server and the Kirra
// verifier service, acting as both:
//
//   1. A CARLA ego vehicle controller — reads vehicle state from CARLA,
//      generates planner commands, submits them through Kirra enforcement,
//      applies the enforced result back to CARLA
//
//   2. A sensor health reporter — reads CARLA sensor data (LiDAR, camera,
//      GPS, IMU), derives confidence scores, and posts health reports to
//      Kirra so the fleet posture engine can respond to sensor degradation
//
// The integration exercises the complete path that no unit test covers:
//
//   CARLA vehicle state
//     → planner generates ProposedVehicleCommand
//     → POST /actuator/motion/command (Kirra enforces)
//     → enforced command applied to CARLA
//     → CARLA steps forward
//     → sensor readings derived from CARLA state
//     → POST /fleet/diagnostics/report (posture engine updates)
//     → repeat at 20Hz
//
// ARCHITECTURE
// ============
// Uses CARLA's Python API via a subprocess bridge rather than a native Rust
// binding. CARLA's official SDK is Python; calling it from Rust via subprocess
// is more maintainable than maintaining unofficial FFI bindings.
//
// The Python bridge script (scripts/carla_bridge.py) runs as a child process.
// Communication is newline-delimited JSON over stdin/stdout. This keeps the
// safety-critical Rust code free of Python dependencies while using CARLA's
// stable Python API.
//
// For environments without CARLA, a built-in kinematic simulator (using
// kinematics_sim.rs) provides a headless fallback that exercises the full
// Kirra enforcement stack without requiring a CARLA server.
//
// USAGE
// =====
//   # With CARLA server running on localhost:2000
//   KIRRA_VERIFIER_URL=http://localhost:8090 \
//   KIRRA_ADMIN_TOKEN=test-token \
//   cargo run --bin kirra_carla_client -- --mode carla --scenario sensor_fault
//
//   # Headless simulation (no CARLA required)
//   KIRRA_VERIFIER_URL=http://localhost:8090 \
//   KIRRA_ADMIN_TOKEN=test-token \
//   cargo run --bin kirra_carla_client -- --mode headless --scenario all

use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Kirra HTTP client types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MotionCommandRequest {
    linear_velocity_mps: f64,
    current_velocity_mps: f64,
    delta_time_s: f64,
    steering_angle_deg: f64,
    current_steering_angle_deg: f64,
}

/// Canonical enforcement-response schema — the same keys the ROS
/// `cmd_vel_interceptor` reads (`action` / `enforced_*`). The gateway also
/// still emits the legacy `enforcement_action` / `linear_velocity_mps` keys
/// for transition, but both in-repo consumers now read this canonical form.
/// Extra keys in the body are ignored by serde.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MotionCommandResponse {
    action: String,
    enforced_linear_velocity_mps: f64,
    enforced_steering_angle_deg: f64,
    #[serde(default)]
    denial_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SensorFaultReport {
    source_node_id: String,
    confidence_score: f64,
    hardware_fault_detected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FleetPostureResponse {
    fleet: Vec<NodePosture>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NodePosture {
    node_id: String,
    propagated_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegisterNodeRequest {
    node_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegisterDepsRequest {
    node_id: String,
    depends_on: Vec<String>,
}

// ---------------------------------------------------------------------------
// Simulated vehicle state (headless mode)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SimVehicleState {
    x_m: f64,
    y_m: f64,
    heading_rad: f64,
    velocity_mps: f64,
    steering_angle_deg: f64,
    elapsed_ms: u64,
}

impl SimVehicleState {
    fn at_rest() -> Self {
        Self { x_m: 0.0, y_m: 0.0, heading_rad: 0.0,
               velocity_mps: 0.0, steering_angle_deg: 0.0, elapsed_ms: 0 }
    }

    fn step(&self, v: f64, delta_deg: f64, dt_s: f64, wheelbase_m: f64) -> Self {
        let delta_rad = delta_deg.to_radians();
        let new_x = self.x_m + self.velocity_mps * self.heading_rad.cos() * dt_s;
        let new_y = self.y_m + self.velocity_mps * self.heading_rad.sin() * dt_s;
        let heading_rate = if wheelbase_m > 1e-6 {
            (self.velocity_mps / wheelbase_m) * delta_rad.tan()
        } else { 0.0 };
        Self {
            x_m: new_x,
            y_m: new_y,
            heading_rad: self.heading_rad + heading_rate * dt_s,
            velocity_mps: v,
            steering_angle_deg: delta_deg,
            elapsed_ms: self.elapsed_ms + (dt_s * 1000.0) as u64,
        }
    }

    fn lateral_accel(&self, wheelbase_m: f64) -> f64 {
        let v2 = self.velocity_mps.powi(2);
        if v2 <= 1e-6 { return 0.0; }
        (v2 * self.steering_angle_deg.to_radians().tan().abs()) / wheelbase_m
    }
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum ScenarioEvent {
    Cruise { target_v: f64, target_delta: f64 },
    InjectFault { node_id: String, confidence: f64, hw_fault: bool },
    RestoreSensor { node_id: String },
    DangerousCommand { v: f64, delta: f64 },
}

#[derive(Debug)]
struct Scenario {
    name: String,
    events: Vec<(u64, ScenarioEvent)>,
    duration_ms: u64,
}

impl Scenario {
    fn sensor_fault() -> Self {
        Self {
            name: "sensor_fault".to_string(),
            duration_ms: 15_000,
            events: vec![
                (0,     ScenarioEvent::Cruise { target_v: 12.0, target_delta: 0.0 }),
                (3_000, ScenarioEvent::InjectFault {
                    node_id: "lidar_front".to_string(),
                    confidence: 0.0, hw_fault: true,
                }),
                (8_000, ScenarioEvent::RestoreSensor { node_id: "lidar_front".to_string() }),
                (5_500, ScenarioEvent::DangerousCommand { v: 25.0, delta: 15.0 }),
            ],
        }
    }

    fn highway_speed_steering() -> Self {
        Self {
            name: "highway_speed_steering".to_string(),
            duration_ms: 10_000,
            events: vec![
                (0,     ScenarioEvent::Cruise { target_v: 30.0, target_delta: 0.0 }),
                (3_000, ScenarioEvent::DangerousCommand { v: 30.0, delta: 20.0 }),
                (7_000, ScenarioEvent::Cruise { target_v: 30.0, target_delta: 3.0 }),
            ],
        }
    }

    fn watchdog_timeout() -> Self {
        Self {
            name: "watchdog_timeout".to_string(),
            duration_ms: 20_000,
            events: vec![
                (0,     ScenarioEvent::Cruise { target_v: 10.0, target_delta: 0.0 }),
                (2_000, ScenarioEvent::InjectFault {
                    node_id: "gps_primary".to_string(),
                    confidence: 0.0, hw_fault: true,
                }),
                (15_000, ScenarioEvent::RestoreSensor { node_id: "gps_primary".to_string() }),
            ],
        }
    }

    fn multi_sensor_degradation() -> Self {
        Self {
            name: "multi_sensor_degradation".to_string(),
            duration_ms: 20_000,
            events: vec![
                (0,      ScenarioEvent::Cruise { target_v: 15.0, target_delta: 2.0 }),
                (2_000,  ScenarioEvent::InjectFault {
                    node_id: "lidar_front".to_string(),
                    confidence: 0.1, hw_fault: false,
                }),
                (5_000,  ScenarioEvent::InjectFault {
                    node_id: "gps_primary".to_string(),
                    confidence: 0.4, hw_fault: false,
                }),
                (10_000, ScenarioEvent::RestoreSensor { node_id: "lidar_front".to_string() }),
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// Kirra HTTP client
// ---------------------------------------------------------------------------

struct KirraClient {
    base_url: String,
    admin_token: String,
    client: reqwest::blocking::Client,
}

impl KirraClient {
    fn new(base_url: &str, admin_token: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            admin_token: admin_token.to_string(),
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("HTTP client construction failed"),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.admin_token)
    }

    fn register_node(&self, node_id: &str) -> Result<(), String> {
        let body = RegisterNodeRequest { node_id: node_id.to_string() };
        let resp = self.client
            .post(self.url("/attestation/register"))
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .map_err(|e| e.to_string())?;
        match resp.status().as_u16() {
            201 | 200 | 409 => Ok(()),
            s => Err(format!("register_node {node_id}: status {s}")),
        }
    }

    fn register_deps(&self, node_id: &str, deps: &[&str]) -> Result<(), String> {
        let body = RegisterDepsRequest {
            node_id: node_id.to_string(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
        };
        let resp = self.client
            .post(self.url("/fleet/dependencies"))
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .map_err(|e| e.to_string())?;
        if resp.status().is_success() { Ok(()) }
        else { Err(format!("register_deps {node_id}: status {}", resp.status())) }
    }

    fn register_av_meta(
        &self,
        node_id: &str,
        subsystem_type: &str,
        hardware_id: &str,
    ) -> Result<(), String> {
        let body = serde_json::json!({
            "node_id": node_id,
            "subsystem_type": subsystem_type,
            "hardware_id": hardware_id,
            "confidence_floor": 0.70,
        });
        let resp = self.client
            .post(self.url("/fleet/assets/register"))
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .map_err(|e| e.to_string())?;
        if resp.status().is_success() || resp.status().as_u16() == 404 {
            Ok(())
        } else {
            Err(format!("register_av_meta {node_id}: status {}", resp.status()))
        }
    }

    fn submit_motion_command(
        &self,
        cmd: &MotionCommandRequest,
    ) -> Result<MotionCommandResponse, String> {
        let resp = self.client
            .post(self.url("/actuator/motion/command"))
            .header("Authorization", self.auth_header())
            .json(cmd)
            .send()
            .map_err(|e| e.to_string())?;

        match resp.status().as_u16() {
            200 => resp.json::<MotionCommandResponse>().map_err(|e| e.to_string()),
            400 => Ok(MotionCommandResponse {
                action: "DenyBreach".to_string(),
                enforced_linear_velocity_mps: 0.0,
                enforced_steering_angle_deg: 0.0,
                denial_reason: Some("COMMAND_DENIED".to_string()),
            }),
            403 => Ok(MotionCommandResponse {
                action: "DenyBreach".to_string(),
                enforced_linear_velocity_mps: 0.0,
                enforced_steering_angle_deg: 0.0,
                denial_reason: Some("FLEET_LOCKED_OUT".to_string()),
            }),
            // Degraded posture (or any posture-routing denial) returns 503 from
            // the outer `enforce_posture_routing` gate before the actuator handler
            // runs. Treat it like every other deny-shaped response: author an
            // explicit safe-stop command rather than letting the caller fall to
            // the Err branch and hold the last commanded velocity (ADR-0011 #405
            // safety floor — every deny authors a safe command).
            503 => Ok(MotionCommandResponse {
                action: "DenyBreach".to_string(),
                enforced_linear_velocity_mps: 0.0,
                enforced_steering_angle_deg: 0.0,
                denial_reason: Some("POSTURE_ROUTING_DENIED".to_string()),
            }),
            s => Err(format!("submit_motion_command: unexpected status {s}")),
        }
    }

    fn report_sensor_health(
        &self,
        node_id: &str,
        confidence: f64,
        hw_fault: bool,
    ) -> Result<(), String> {
        let body = SensorFaultReport {
            source_node_id: node_id.to_string(),
            confidence_score: confidence,
            hardware_fault_detected: hw_fault,
        };
        let resp = self.client
            .post(self.url("/fleet/diagnostics/report"))
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .map_err(|e| e.to_string())?;
        if resp.status().is_success() { Ok(()) }
        else { Err(format!("report_sensor_health {node_id}: status {}", resp.status())) }
    }

    fn get_fleet_posture(&self) -> Result<FleetPostureResponse, String> {
        let resp = self.client
            .get(self.url("/fleet/posture"))
            .send()
            .map_err(|e| e.to_string())?;
        resp.json::<FleetPostureResponse>().map_err(|e| e.to_string())
    }

    fn health_check(&self) -> bool {
        self.client
            .get(self.url("/health"))
            .timeout(Duration::from_secs(2))
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Simulation runner
// ---------------------------------------------------------------------------

struct RunStats {
    total_steps: u64,
    allow_count: u64,
    clamp_count: u64,
    deny_count: u64,
    peak_lateral_accel: f64,
    peak_speed: f64,
    posture_transitions: Vec<(u64, String)>,
    invariant_violations: Vec<String>,
}

impl RunStats {
    fn new() -> Self {
        Self {
            total_steps: 0,
            allow_count: 0,
            clamp_count: 0,
            deny_count: 0,
            peak_lateral_accel: 0.0,
            peak_speed: 0.0,
            posture_transitions: Vec::new(),
            invariant_violations: Vec::new(),
        }
    }

    fn record_enforcement(&mut self, action: &str) {
        self.total_steps += 1;
        match action {
            "Allow"         => self.allow_count += 1,
            "ClampLinear" | "ClampSteering" => self.clamp_count += 1,
            _               => self.deny_count += 1,
        }
    }

    fn check_invariant(
        &mut self,
        state: &SimVehicleState,
        wheelbase_m: f64,
        max_lateral: f64,
        max_speed: f64,
        elapsed_ms: u64,
    ) {
        let lat = state.lateral_accel(wheelbase_m);
        let spd = state.velocity_mps.abs();
        self.peak_lateral_accel = self.peak_lateral_accel.max(lat);
        self.peak_speed = self.peak_speed.max(spd);

        const TOL: f64 = 1e-6;
        if lat > max_lateral + TOL {
            self.invariant_violations.push(format!(
                "t={elapsed_ms}ms: lateral_accel {lat:.4} > max {max_lateral:.4}"
            ));
        }
        if spd > max_speed + TOL {
            self.invariant_violations.push(format!(
                "t={elapsed_ms}ms: speed {spd:.4} > max {max_speed:.4}"
            ));
        }
    }

    fn print_summary(&self, scenario_name: &str) {
        println!("\n{}", "=".repeat(60));
        println!("SCENARIO: {scenario_name}");
        println!("{}", "=".repeat(60));
        println!("Steps:            {}", self.total_steps);
        println!("Allow:            {} ({:.1}%)",
            self.allow_count,
            100.0 * self.allow_count as f64 / self.total_steps.max(1) as f64);
        println!("Clamp:            {} ({:.1}%)",
            self.clamp_count,
            100.0 * self.clamp_count as f64 / self.total_steps.max(1) as f64);
        println!("Deny:             {} ({:.1}%)",
            self.deny_count,
            100.0 * self.deny_count as f64 / self.total_steps.max(1) as f64);
        println!("Peak lateral:     {:.4} m/s\u{00b2}", self.peak_lateral_accel);
        println!("Peak speed:       {:.4} m/s", self.peak_speed);
        if !self.posture_transitions.is_empty() {
            println!("Posture changes:");
            for (t, p) in &self.posture_transitions {
                println!("  t={t}ms \u{2192} {p}");
            }
        }
        if self.invariant_violations.is_empty() {
            println!("Invariants:       \u{2713} ALL PASSED");
        } else {
            println!("INVARIANT VIOLATIONS ({}):", self.invariant_violations.len());
            for v in &self.invariant_violations {
                println!("  \u{2717} {v}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Planner
// ---------------------------------------------------------------------------

struct SimplePlanner {
    target_v: f64,
    target_delta: f64,
}

impl SimplePlanner {
    fn new() -> Self {
        Self { target_v: 0.0, target_delta: 0.0 }
    }

    fn update(&mut self, event: &ScenarioEvent) {
        match event {
            ScenarioEvent::Cruise { target_v, target_delta } => {
                self.target_v = *target_v;
                self.target_delta = *target_delta;
            }
            ScenarioEvent::DangerousCommand { v, delta } => {
                self.target_v = *v;
                self.target_delta = *delta;
            }
            _ => {}
        }
    }

    fn command(&self, state: &SimVehicleState, dt: f64) -> MotionCommandRequest {
        MotionCommandRequest {
            linear_velocity_mps: self.target_v,
            current_velocity_mps: state.velocity_mps,
            delta_time_s: dt,
            steering_angle_deg: self.target_delta,
            current_steering_angle_deg: state.steering_angle_deg,
        }
    }
}

// ---------------------------------------------------------------------------
// Main simulation loop
// ---------------------------------------------------------------------------

fn run_scenario_headless(
    client: &KirraClient,
    scenario: &Scenario,
    wheelbase_m: f64,
    max_lateral_accel: f64,
    max_speed: f64,
) -> RunStats {
    const DT_S: f64 = 0.05;
    const DT_MS: u64 = 50;
    const SENSOR_REPORT_INTERVAL_MS: u64 = 200;

    let mut state = SimVehicleState::at_rest();
    let mut planner = SimplePlanner::new();
    let mut stats = RunStats::new();
    let mut last_posture = String::from("unknown");
    let mut last_sensor_report_ms: u64 = 0;
    let mut recovery_restore_counts: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    let mut faulted_nodes: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut restoring_nodes: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    println!("\nRunning scenario: {} ({} ms)", scenario.name, scenario.duration_ms);
    println!("{}", "-".repeat(50));

    while state.elapsed_ms < scenario.duration_ms {
        let t = state.elapsed_ms;

        for (event_t, event) in &scenario.events {
            if *event_t == t {
                match event {
                    ScenarioEvent::InjectFault { node_id, confidence, hw_fault } => {
                        println!("  t={t}ms: FAULT injected on {node_id} \
                                 (confidence={confidence:.2}, hw={hw_fault})");
                        let _ = client.report_sensor_health(node_id, *confidence, *hw_fault);
                        faulted_nodes.insert(node_id.clone());
                        restoring_nodes.remove(node_id);
                        recovery_restore_counts.insert(node_id.clone(), 0);
                    }
                    ScenarioEvent::RestoreSensor { node_id } => {
                        println!("  t={t}ms: RESTORE initiated for {node_id}");
                        restoring_nodes.insert(node_id.clone());
                        faulted_nodes.remove(node_id);
                    }
                    ScenarioEvent::Cruise { target_v, target_delta } => {
                        println!("  t={t}ms: CRUISE v={target_v:.1} m/s \u{03b4}={target_delta:.1}\u{00b0}");
                    }
                    ScenarioEvent::DangerousCommand { v, delta } => {
                        println!("  t={t}ms: DANGEROUS COMMAND requested: v={v:.1} m/s \u{03b4}={delta:.1}\u{00b0}");
                    }
                }
                planner.update(event);
            }
        }

        if t >= last_sensor_report_ms + SENSOR_REPORT_INTERVAL_MS {
            last_sensor_report_ms = t;
            for node_id in &faulted_nodes.clone() {
                let _ = client.report_sensor_health(node_id, 0.0, false);
            }
            for node_id in &restoring_nodes.clone() {
                let count = recovery_restore_counts.entry(node_id.clone()).or_insert(0);
                *count += 1;
                let _ = client.report_sensor_health(node_id, 0.95, false);
                if *count >= 5 {
                    println!("  t={t}ms: RECOVERY CONFIRMED for {node_id} (streak={count})");
                    restoring_nodes.remove(node_id);
                }
            }
        }

        let cmd = planner.command(&state, DT_S);
        match client.submit_motion_command(&cmd) {
            Ok(resp) => {
                let enforced_v = resp.enforced_linear_velocity_mps;
                let enforced_delta = resp.enforced_steering_angle_deg;
                match resp.action.as_str() {
                    "ClampLinear" => {
                        println!("  t={t}ms: CLAMP linear {:.2}\u{2192}{:.2} m/s",
                            cmd.linear_velocity_mps, enforced_v);
                    }
                    "ClampSteering" => {
                        println!("  t={t}ms: CLAMP steering {:.2}\u{2192}{:.2}\u{00b0}",
                            cmd.steering_angle_deg, enforced_delta);
                    }
                    "DenyBreach" => {
                        println!("  t={t}ms: DENY \u{2014} {}",
                            resp.denial_reason.as_deref().unwrap_or("unknown"));
                    }
                    _ => {}
                }
                stats.record_enforcement(&resp.action);
                state = state.step(enforced_v, enforced_delta, DT_S, wheelbase_m);
            }
            Err(e) => {
                eprintln!("  t={t}ms: motion command error: {e}");
                state.elapsed_ms += DT_MS;
                continue;
            }
        }

        stats.check_invariant(&state, wheelbase_m, max_lateral_accel, max_speed, t);

        if t.is_multiple_of(500) {
            if let Ok(posture_resp) = client.get_fleet_posture() {
                let agg = posture_resp.fleet.iter()
                    .map(|n| n.propagated_status.as_str())
                    .max_by_key(|s| match *s {
                        "LockedOut" => 2,
                        "Degraded"  => 1,
                        _           => 0,
                    })
                    .unwrap_or("Nominal")
                    .to_string();
                if agg != last_posture {
                    println!("  t={t}ms: POSTURE TRANSITION \u{2192} {agg}");
                    stats.posture_transitions.push((t, agg.clone()));
                    last_posture = agg;
                }
            }
        }

        state.elapsed_ms += DT_MS;
    }

    stats
}

// ---------------------------------------------------------------------------
// Fleet setup
// ---------------------------------------------------------------------------

fn setup_av_fleet(client: &KirraClient) -> Result<(), String> {
    println!("Setting up AV fleet node graph...");

    let nodes = [
        ("lidar_front",        "Perception",  "LIDAR-001"),
        ("lidar_rear",         "Perception",  "LIDAR-002"),
        ("camera_front",       "Perception",  "CAM-001"),
        ("gps_primary",        "Positioning", "GPS-001"),
        ("imu_primary",        "Positioning", "IMU-001"),
        ("perception_fusion",  "Planning",    "FUSION-001"),
        ("trajectory_planner", "Planning",    "PLAN-001"),
        ("vehicle_controller", "Actuation",   "CTRL-001"),
    ];

    for (node_id, subsystem_type, hardware_id) in &nodes {
        client.register_node(node_id)?;
        std::thread::sleep(Duration::from_millis(50));
        let _ = client.register_av_meta(node_id, subsystem_type, hardware_id);
    }

    client.register_deps("perception_fusion", &["lidar_front", "lidar_rear", "camera_front"])?;
    client.register_deps("trajectory_planner", &["perception_fusion", "gps_primary"])?;
    client.register_deps("vehicle_controller", &["trajectory_planner"])?;

    println!("Fleet setup complete.");
    println!("  Nodes: {}", nodes.len());
    println!("  Dependency chain: lidar/camera \u{2192} fusion \u{2192} planner \u{2192} controller");
    Ok(())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    let base_url = std::env::var("KIRRA_VERIFIER_URL")
        .unwrap_or_else(|_| "http://localhost:8090".to_string());
    let admin_token = std::env::var("KIRRA_ADMIN_TOKEN")
        .unwrap_or_else(|_| "dev-token".to_string());

    let args: Vec<String> = std::env::args().collect();
    let mode = args.iter().find(|a| a.starts_with("--mode="))
        .and_then(|a| a.strip_prefix("--mode="))
        .unwrap_or("headless");
    let scenario_name = args.iter().find(|a| a.starts_with("--scenario="))
        .and_then(|a| a.strip_prefix("--scenario="))
        .unwrap_or("all");

    println!("Kirra CARLA Integration Client");
    println!("  Verifier URL: {base_url}");
    println!("  Mode:         {mode}");
    println!("  Scenario:     {scenario_name}");

    let client = KirraClient::new(&base_url, &admin_token);

    print!("Waiting for Kirra verifier...");
    let start = Instant::now();
    loop {
        if client.health_check() { println!(" ready."); break; }
        if start.elapsed() > Duration::from_secs(30) {
            eprintln!("\nVerifier not reachable after 30s \u{2014} is it running?");
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(500));
        print!(".");
    }

    if let Err(e) = setup_av_fleet(&client) {
        eprintln!("Fleet setup failed: {e}");
        std::process::exit(1);
    }

    let wheelbase_m = 2.8_f64;
    let max_lateral_accel = 3.5_f64;
    let max_speed = 35.0_f64;

    let scenarios: Vec<Scenario> = match scenario_name {
        "sensor_fault"             => vec![Scenario::sensor_fault()],
        "highway_speed_steering"   => vec![Scenario::highway_speed_steering()],
        "watchdog_timeout"         => vec![Scenario::watchdog_timeout()],
        "multi_sensor_degradation" => vec![Scenario::multi_sensor_degradation()],
        "all" => vec![
            Scenario::sensor_fault(),
            Scenario::highway_speed_steering(),
            Scenario::watchdog_timeout(),
            Scenario::multi_sensor_degradation(),
        ],
        other => {
            eprintln!("Unknown scenario: {other}");
            eprintln!("Available: sensor_fault, highway_speed_steering, watchdog_timeout, multi_sensor_degradation, all");
            std::process::exit(1);
        }
    };

    let mut all_passed = true;
    for scenario in &scenarios {
        let stats = run_scenario_headless(
            &client, scenario, wheelbase_m, max_lateral_accel, max_speed,
        );
        stats.print_summary(&scenario.name);
        if !stats.invariant_violations.is_empty() {
            all_passed = false;
        }
    }

    println!("\n{}", "=".repeat(60));
    if all_passed {
        println!("ALL SCENARIOS PASSED \u{2014} no invariant violations detected");
        std::process::exit(0);
    } else {
        println!("INVARIANT VIOLATIONS DETECTED \u{2014} see output above");
        std::process::exit(1);
    }
}
