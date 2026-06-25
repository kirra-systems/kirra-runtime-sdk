//! **A tiny HTTP perception sidecar wrapping the REAL Taj (ADR-0015 Phase A).** Lets the
//! Python ROS 2 `cmd_vel` path derive a perception speed cap from raw lidar without
//! reimplementing Taj: POST a `LaserScan`, get back the geometric corridor's health plus an
//! **assured-clear-distance (ACD) speed cap** — the speed from which the robot can still stop
//! within the clear distance ahead (RSS Rule 4 / the ADR-0014 "lidar safety buffer").
//!
//! It is a perception PRODUCER, not the safety authority — Taj tightens the envelope, the
//! KIRRA governor still bounds the result. The cap composes BEFORE the governor on the
//! cmd_vel path (the doer's proposal is derated; the governor disposes). A hand-rolled
//! `std::net` HTTP/1.1 server keeps it dependency-free, mirroring `planner_service`.
//!
//!   cargo run -p kirra-mick --example taj_service          # listens on 127.0.0.1:8101
//!
//!   POST /perception  {"angle_min_rad":..,"angle_increment_rad":..,"range_min_m":..,
//!                      "range_max_m":..,"ranges":[..],"stamp_ms":..,
//!                      "forward_extent_m":20.0,        # optional Taj horizon
//!                      "decel_mps2":1.5,"margin_m":0.4,"lane_half_m":0.6,  # optional ACD params
//!                      "confidence_floor":0.5}         # optional health floor
//!     → {"healthy":true,"confidence":0.95,"age_ms":0,"clear_distance_m":..,
//!        "nearest_object_m":..,"object_count":..,"speed_cap_mps":..}
//!   GET /health  → {"status":"ok"}
//!
//! Fail-closed: perception below the confidence floor → `healthy:false` and `speed_cap_mps:0.0`
//! (the MRC floor — the consumer holds). An empty / all-no-return scan reads as an unhealthy
//! corridor, so the cap is 0, never an unbounded pass.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

use kirra_core::corridor::CorridorSource;
use kirra_taj::{LaserScan, TajConfig, TajPhaseA};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct PerceptionRequest {
    angle_min_rad: f64,
    angle_increment_rad: f64,
    range_min_m: f64,
    range_max_m: f64,
    ranges: Vec<f32>,
    #[serde(default)]
    stamp_ms: u64,
    #[serde(default = "default_extent")]
    forward_extent_m: f64,
    #[serde(default = "default_decel")]
    decel_mps2: f64,
    #[serde(default = "default_margin")]
    margin_m: f64,
    #[serde(default = "default_lane_half")]
    lane_half_m: f64,
    #[serde(default = "default_floor")]
    confidence_floor: f32,
}
fn default_extent() -> f64 { 20.0 }
fn default_decel() -> f64 { 1.5 }
fn default_margin() -> f64 { 0.4 }
fn default_lane_half() -> f64 { 0.6 }
fn default_floor() -> f32 { 0.5 }

#[derive(Serialize)]
struct ObjOut { id: u64, x: f64, y: f64, vx: f64, vy: f64 }

#[derive(Serialize)]
struct PerceptionResponse {
    healthy: bool,
    confidence: f32,
    age_ms: u64,
    clear_distance_m: f64,
    nearest_object_m: Option<f64>,
    object_count: usize,
    speed_cap_mps: f64,
    // The corridor geometry + objects, in the SAME shapes the Occy planner endpoint
    // (`planner_service` POST /plan) consumes, so the doer bridge can pass them straight
    // through: left/right boundary polylines as [[x,y],..] and objects as {id,x,y,vx,vy}.
    left: Vec<[f64; 2]>,
    right: Vec<[f64; 2]>,
    objects: Vec<ObjOut>,
}

/// The corridor's straight-ahead reach: the smaller of the two boundary polylines'
/// furthest forward point. Taj clips this at a dead-ahead obstacle, so it already
/// encodes the clear distance for the centre of the lane.
fn corridor_reach(corr: &impl CorridorSource) -> f64 {
    let far = |pts: &[kirra_core::corridor::Point]| pts.iter().map(|p| p.x_m).fold(0.0_f64, f64::max);
    far(corr.left_boundary()).min(far(corr.right_boundary()))
}

fn handle_perception(req: &PerceptionRequest) -> PerceptionResponse {
    let scan = LaserScan {
        angle_min_rad: req.angle_min_rad,
        angle_increment_rad: req.angle_increment_rad,
        range_min_m: req.range_min_m,
        range_max_m: req.range_max_m,
        ranges: req.ranges.clone(),
        stamp_ms: req.stamp_ms,
    };
    // Process at the scan's own stamp → age 0; wall-clock staleness is the consumer's job
    // (the ROS node times the cap topic), keeping this service stateless.
    let taj = TajPhaseA::new(TajConfig { forward_extent_m: req.forward_extent_m, ..Default::default() });
    let perception = taj.process(&scan, req.stamp_ms);

    let confidence = perception.corridor.confidence();
    let age_ms = perception.corridor.age_ms();
    let healthy = confidence >= req.confidence_floor;

    // The nearest IN-LANE object (|y| within half a lane), as a discrete clear-distance
    // bound that complements the corridor reach (a Phase-B object need not clip the corridor).
    let nearest_object_m = perception
        .objects
        .iter()
        .filter(|o| o.pos.y_m.abs() <= req.lane_half_m && o.pos.x_m > 0.0)
        .map(|o| o.pos.x_m)
        .fold(f64::INFINITY, f64::min);
    let nearest_object_m = nearest_object_m.is_finite().then_some(nearest_object_m);

    // Clear distance = the tighter of the corridor reach and the nearest in-lane object.
    let clear = corridor_reach(&perception.corridor)
        .min(nearest_object_m.unwrap_or(f64::INFINITY))
        .max(0.0);

    // ACD cap: the speed from which a `decel_mps2` brake still stops within (clear - margin).
    // Unhealthy perception → 0.0 (MRC floor): never trust an empty/low-confidence corridor.
    let speed_cap_mps = if healthy {
        (2.0 * req.decel_mps2 * (clear - req.margin_m).max(0.0)).sqrt()
    } else {
        0.0
    };

    let to_poly = |pts: &[kirra_core::corridor::Point]| -> Vec<[f64; 2]> {
        pts.iter().map(|p| [p.x_m, p.y_m]).collect()
    };
    let left = to_poly(perception.corridor.left_boundary());
    let right = to_poly(perception.corridor.right_boundary());
    let objects = perception
        .objects
        .iter()
        .map(|o| ObjOut { id: o.id, x: o.pos.x_m, y: o.pos.y_m, vx: o.vel.x_m, vy: o.vel.y_m })
        .collect();

    PerceptionResponse {
        healthy,
        confidence,
        age_ms,
        clear_distance_m: clear,
        nearest_object_m,
        object_count: perception.objects.len(),
        speed_cap_mps,
        left,
        right,
        objects,
    }
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let msg = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = stream.write_all(msg.as_bytes());
}

fn serve(mut stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() { return; }
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() { break; }
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
    if method == "POST" && path == "/perception" {
        let mut body = vec![0u8; content_length];
        if reader.read_exact(&mut body).is_err() {
            respond(&mut stream, "400 Bad Request", "{\"error\":\"short body\"}");
            return;
        }
        match serde_json::from_slice::<PerceptionRequest>(&body) {
            Ok(req) => {
                let resp = handle_perception(&req);
                respond(&mut stream, "200 OK", &serde_json::to_string(&resp).unwrap());
            }
            Err(e) => respond(&mut stream, "400 Bad Request", &format!("{{\"error\":\"{e}\"}}")),
        }
        return;
    }
    respond(&mut stream, "404 Not Found", "{\"error\":\"unknown route\"}");
}

fn main() {
    let addr = std::env::var("KIRRA_TAJ_ADDR").unwrap_or_else(|_| "127.0.0.1:8101".to_string());
    let listener = TcpListener::bind(&addr).unwrap_or_else(|e| { eprintln!("taj_service: bind {addr}: {e}"); std::process::exit(1); });
    println!("Taj perception service on http://{addr}  (POST /perception, GET /health)");
    for stream in listener.incoming() {
        match stream {
            Ok(s) => serve(s),
            Err(e) => eprintln!("taj_service: accept error: {e}"),
        }
    }
}
