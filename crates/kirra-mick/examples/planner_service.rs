//! **A tiny HTTP planner endpoint wrapping the REAL Occy + KIRRA.** Lets a non-Rust client (e.g.
//! the CARLA Python harness) drive egos with the actual planner instead of a placeholder
//! controller: POST a world snapshot, get back Occy's KIRRA-validated trajectory.
//!
//! It deliberately lives in `kirra-mick` (the doer/demo side), NOT the safety-critical verifier
//! crate — this serves the DOER's proposal; the governor's enforcement is the separate verifier
//! service. A hand-rolled `std::net` HTTP/1.1 server keeps it dependency-free.
//!
//!   cargo run -p kirra-mick --example planner_service      # listens on 127.0.0.1:8100
//!
//!   POST /plan   {"ego":{"x":..,"y":..,"heading":..,"speed":..},
//!                 "goal":{"x":..,"y":..}, "cruise":10.0,
//!                 "left":[[x,y],..], "right":[[x,y],..],       # drivable corridor boundaries
//!                 "objects":[{"id":1,"x":..,"y":..,"vx":..,"vy":..}]}
//!     → {"kind":"Motion|SafeStop", "verdict":"Accept|Clamp|MRCFallback",
//!        "trajectory":[{"x":..,"y":..,"heading":..,"v":..,"t":..}, ..]}
//!   GET /health  → {"status":"ok"}

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

use kirra_planner::{
    plan_for_intent, EgoState, FleetPosture, GeometricPlanner, GeometricPlannerConfig, Goal,
    MickIntent, PlanInput, Pose, ProposalKind,
};
use kirra_trajectory::corridor::{CorridorSource, Point};
use kirra_trajectory::state::{PerceivedObject, TrajectoryVerdict};
use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct Xy {
    x: f64,
    y: f64,
}
#[derive(Deserialize)]
struct EgoReq {
    x: f64,
    y: f64,
    heading: f64,
    speed: f64,
}
#[derive(Deserialize)]
struct ObjReq {
    id: u64,
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
}
#[derive(Deserialize)]
struct PlanRequest {
    ego: EgoReq,
    goal: Xy,
    #[serde(default = "default_cruise")]
    cruise: f64,
    left: Vec<[f64; 2]>,
    right: Vec<[f64; 2]>,
    #[serde(default)]
    objects: Vec<ObjReq>,
    /// Optional vehicle footprint/kinematics for the CHECKER. Absent → the urban-car
    /// default (4.8 m). A small differential robot (e.g. a Rosmaster) MUST pass its own
    /// dimensions, or the car-sized footprint can't fit a robot-scale corridor and KIRRA
    /// MRCs every plan.
    #[serde(default)]
    vehicle: Option<VehicleReq>,
}
fn default_cruise() -> f64 {
    10.0
}

/// Vehicle profile for BOTH the checker (`VehicleConfig`) and the doer's lateral-clearance
/// target. `class` picks the base sibling profile (`robotaxi` default, or `courier` for a
/// small robot — see docs/CONTRACT_PROFILES.md); the remaining fields fine-tune it. All
/// optional; absent → the urban-car/robotaxi default, so the AV path is unchanged.
#[derive(Deserialize)]
struct VehicleReq {
    class: Option<String>,
    wheelbase_m: Option<f64>,
    half_length_m: Option<f64>,
    half_width_m: Option<f64>,
    max_speed_mps: Option<f64>,
    max_steering_deg: Option<f64>,
    /// Per-class RSS lateral-alignment band (m). Robotaxi 4.0; a small robot needs a
    /// tighter band to pass an obstacle a car couldn't (the checker side of the pass).
    rss_lateral_alignment_tolerance_m: Option<f64>,
    /// The DOER's lateral clearance target (m) — how much room Occy demands before it will
    /// PROPOSE a pass. Robot scale must drop this (default 4.5 is car-scale) or Occy never
    /// proposes the pass in the first place; the checker (`rss_...`) then admits it.
    lateral_clearance_target_m: Option<f64>,
}

/// Build the checker's `VehicleConfig` from the request: a base sibling profile selected by
/// `class`, then explicit overrides. Absent → `default_urban` (robotaxi), unchanged.
fn vehicle_config(req: &PlanRequest) -> VehicleConfig {
    // Select the base sibling profile via the single slow-loop class selector (mirrors the
    // fast-loop VehicleClass::from_str). Absent class → robotaxi (default_urban), unchanged.
    let mut v = match req.vehicle.as_ref().and_then(|o| o.class.as_deref()) {
        Some(class) => VehicleConfig::for_class(class),
        None => VehicleConfig::default_urban(),
    };
    if let Some(o) = &req.vehicle {
        if let Some(x) = o.wheelbase_m {
            v.wheelbase_m = x;
        }
        if let Some(x) = o.half_length_m {
            v.half_length_m = x;
        }
        if let Some(x) = o.half_width_m {
            v.half_width_m = x;
        }
        if let Some(x) = o.max_speed_mps {
            v.max_speed_mps = x;
        }
        if let Some(x) = o.max_steering_deg {
            v.max_steering_rad = x.to_radians();
        }
        if let Some(x) = o.rss_lateral_alignment_tolerance_m {
            v.rss_lateral_alignment_tolerance_m = x;
        }
    }
    v
}

/// The DOER's lateral-clearance target from the request (default keeps the planner default).
fn lateral_clearance_target(req: &PlanRequest) -> Option<f64> {
    req.vehicle
        .as_ref()
        .and_then(|o| o.lateral_clearance_target_m)
}

#[derive(Serialize)]
struct TrajPt {
    x: f64,
    y: f64,
    heading: f64,
    v: f64,
    t: f64,
}
#[derive(Serialize)]
struct PlanResponse {
    kind: String,
    verdict: String,
    trajectory: Vec<TrajPt>,
}

/// A `CorridorSource` straight off the request's boundary polylines.
struct ReqCorridor {
    left: Vec<Point>,
    right: Vec<Point>,
}
impl CorridorSource for ReqCorridor {
    fn left_boundary(&self) -> &[Point] {
        &self.left
    }
    fn right_boundary(&self) -> &[Point] {
        &self.right
    }
    fn confidence(&self) -> f32 {
        0.95
    }
    fn age_ms(&self) -> u64 {
        10
    }
}

fn pts(v: &[[f64; 2]]) -> Vec<Point> {
    v.iter()
        .map(|p| Point {
            x_m: p[0],
            y_m: p[1],
        })
        .collect()
}

fn handle_plan(req: &PlanRequest) -> PlanResponse {
    let corr = ReqCorridor {
        left: pts(&req.left),
        right: pts(&req.right),
    };
    let objects: Vec<PerceivedObject> = req
        .objects
        .iter()
        .map(|o| PerceivedObject {
            id: o.id,
            pos: Point { x_m: o.x, y_m: o.y },
            velocity_mps: o.vx.hypot(o.vy),
            heading_rad: o.vy.atan2(o.vx),
            vel: Point {
                x_m: o.vx,
                y_m: o.vy,
            },
        })
        .collect();

    let world = PlanInput {
        ego: EgoState {
            pose: Pose {
                x_m: req.ego.x,
                y_m: req.ego.y,
                heading_rad: req.ego.heading,
            },
            linear_x_mps: req.ego.speed,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        goal: Goal {
            target: Pose {
                x_m: req.goal.x,
                y_m: req.goal.y,
                heading_rad: req.ego.heading,
            },
        },
        map: &corr,
        objects: &objects,
        controls: &[],
        lane_boundaries: &[],
        motion: &[],
        predicted_paths: &[],
        cedes_to_ego_ids: &[],
        lane_change_to_m: None,
        no_overtake_ids: &[],
        drivable: None,
        posture: FleetPosture::Nominal,
        target_speed_mps: None,
        request_overtake: false,
        request_pull_over: false,
        lane_graph: None,
        signal_states: &[],
    };

    // The DOER: real Occy grounds the GoTo intent. A courier class selects the robot-scale
    // planner preset (stops ~1 m short, routes around with the courier clearance, small
    // footprint) so Occy PROPOSES robot-scale motion the car-scale default never would; the
    // checker's per-class profile then bounds it. `class` mirrors the VehicleConfig selector.
    let mut cfg = match req.vehicle.as_ref().and_then(|o| o.class.as_deref()) {
        Some("courier") | Some("robot") | Some("sidewalk") => GeometricPlannerConfig::courier(),
        _ => GeometricPlannerConfig::default(),
    };
    cfg.cruise_speed_mps = req.cruise;
    if let Some(ct) = lateral_clearance_target(req) {
        cfg.lateral_clearance_target_m = ct;
    }
    let plan = plan_for_intent(
        &mut GeometricPlanner::new(cfg),
        &MickIntent::GoTo {
            x_m: req.goal.x,
            y_m: req.goal.y,
        },
        &world,
    );
    // The CHECKER: KIRRA's verdict on the proposal (the client applies it / falls back accordingly).
    let verdict = validate_trajectory_slow(
        &plan.trajectory,
        &corr,
        &objects,
        &vehicle_config(req),
        None,
        FleetPosture::Nominal,
    );

    PlanResponse {
        kind: match plan.kind {
            ProposalKind::Motion => "Motion",
            ProposalKind::SafeStop => "SafeStop",
        }
        .to_string(),
        verdict: match verdict {
            TrajectoryVerdict::Accept => "Accept",
            TrajectoryVerdict::Clamp => "Clamp",
            TrajectoryVerdict::MRCFallback => "MRCFallback",
            other => {
                return PlanResponse {
                    kind: "SafeStop".into(),
                    verdict: format!("{other:?}"),
                    trajectory: vec![],
                }
            }
        }
        .to_string(),
        trajectory: plan
            .trajectory
            .iter()
            .map(|p| TrajPt {
                x: p.pose.x_m,
                y: p.pose.y_m,
                heading: p.pose.heading_rad,
                v: p.velocity_mps,
                t: p.time_from_start_s,
            })
            .collect(),
    }
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let msg = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = stream.write_all(msg.as_bytes());
}

fn serve(mut stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut parts = request_line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));

    if method == "GET" && path == "/health" {
        respond(&mut stream, "200 OK", "{\"status\":\"ok\"}");
        return;
    }
    if method == "POST" && path == "/plan" {
        let mut body = vec![0u8; content_length];
        if reader.read_exact(&mut body).is_err() {
            respond(&mut stream, "400 Bad Request", "{\"error\":\"short body\"}");
            return;
        }
        match serde_json::from_slice::<PlanRequest>(&body) {
            Ok(req) => {
                let resp = handle_plan(&req);
                respond(
                    &mut stream,
                    "200 OK",
                    &serde_json::to_string(&resp).unwrap(),
                );
            }
            Err(e) => respond(
                &mut stream,
                "400 Bad Request",
                &format!("{{\"error\":\"{e}\"}}"),
            ),
        }
        return;
    }
    respond(
        &mut stream,
        "404 Not Found",
        "{\"error\":\"unknown route\"}",
    );
}

fn main() {
    let addr = std::env::var("KIRRA_PLANNER_ADDR").unwrap_or_else(|_| "127.0.0.1:8100".to_string());
    let listener = TcpListener::bind(&addr).unwrap_or_else(|e| {
        eprintln!("planner_service: bind {addr}: {e}");
        std::process::exit(1);
    });
    println!("Occy planner service on http://{addr}  (POST /plan, GET /health)");
    for stream in listener.incoming() {
        match stream {
            Ok(s) => serve(s),
            Err(e) => eprintln!("planner_service: accept error: {e}"),
        }
    }
}
