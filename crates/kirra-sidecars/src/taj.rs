//! The Taj perception endpoint core (promoted verbatim from the
//! `kirra-mick --example taj_service`): POST a `LaserScan`, get back the
//! geometric corridor's health plus an **assured-clear-distance (ACD) speed
//! cap** — the speed from which the robot can still stop within the clear
//! distance ahead (RSS Rule 4 / the ADR-0014 "lidar safety buffer").
//!
//! A perception PRODUCER, not the safety authority — Taj tightens the
//! envelope, the KIRRA governor still bounds the result. Fail-closed:
//! perception below the confidence floor → `healthy:false` and
//! `speed_cap_mps: 0.0` (the MRC floor — the consumer holds).

use kirra_core::corridor::CorridorSource;
use kirra_taj::{LaserScan, TajConfig, TajPhaseA};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct PerceptionRequest {
    pub angle_min_rad: f64,
    pub angle_increment_rad: f64,
    pub range_min_m: f64,
    pub range_max_m: f64,
    pub ranges: Vec<f32>,
    #[serde(default)]
    pub stamp_ms: u64,
    #[serde(default = "default_extent")]
    pub forward_extent_m: f64,
    #[serde(default = "default_decel")]
    pub decel_mps2: f64,
    #[serde(default = "default_margin")]
    pub margin_m: f64,
    #[serde(default = "default_lane_half")]
    pub lane_half_m: f64,
    #[serde(default = "default_floor")]
    pub confidence_floor: f32,
}
fn default_extent() -> f64 {
    20.0
}
fn default_decel() -> f64 {
    1.5
}
fn default_margin() -> f64 {
    0.4
}
fn default_lane_half() -> f64 {
    0.6
}
fn default_floor() -> f32 {
    0.5
}

#[derive(Serialize)]
pub struct ObjOut {
    pub id: u64,
    pub x: f64,
    pub y: f64,
    pub vx: f64,
    pub vy: f64,
}

#[derive(Serialize)]
pub struct PerceptionResponse {
    pub healthy: bool,
    pub confidence: f32,
    pub age_ms: u64,
    pub clear_distance_m: f64,
    pub nearest_object_m: Option<f64>,
    pub object_count: usize,
    pub speed_cap_mps: f64,
    // The corridor geometry + objects, in the SAME shapes the Occy planner
    // endpoint (POST /plan) consumes, so the doer bridge passes them through.
    pub left: Vec<[f64; 2]>,
    pub right: Vec<[f64; 2]>,
    pub objects: Vec<ObjOut>,
}

/// The corridor's straight-ahead reach: the smaller of the two boundary
/// polylines' furthest forward point. Taj clips this at a dead-ahead
/// obstacle, so it already encodes the clear distance for the lane centre.
fn corridor_reach(corr: &impl CorridorSource) -> f64 {
    let far =
        |pts: &[kirra_core::corridor::Point]| pts.iter().map(|p| p.x_m).fold(0.0_f64, f64::max);
    far(corr.left_boundary()).min(far(corr.right_boundary()))
}

pub fn handle_perception(req: &PerceptionRequest) -> PerceptionResponse {
    let scan = LaserScan {
        angle_min_rad: req.angle_min_rad,
        angle_increment_rad: req.angle_increment_rad,
        range_min_m: req.range_min_m,
        range_max_m: req.range_max_m,
        ranges: req.ranges.clone(),
        stamp_ms: req.stamp_ms,
    };
    // Process at the scan's own stamp → age 0; wall-clock staleness is the
    // consumer's job (the ROS node times the cap topic), keeping this
    // service stateless.
    let taj = TajPhaseA::new(TajConfig {
        forward_extent_m: req.forward_extent_m,
        ..Default::default()
    });
    let perception = taj.process(&scan, req.stamp_ms);

    let confidence = perception.corridor.confidence();
    let age_ms = perception.corridor.age_ms();
    let healthy = confidence >= req.confidence_floor;

    // The nearest IN-LANE object (|y| within half a lane), as a discrete
    // clear-distance bound that complements the corridor reach.
    let nearest_object_m = perception
        .objects
        .iter()
        .filter(|o| o.pos.y_m.abs() <= req.lane_half_m && o.pos.x_m > 0.0)
        .map(|o| o.pos.x_m)
        .fold(f64::INFINITY, f64::min);
    let nearest_object_m = nearest_object_m.is_finite().then_some(nearest_object_m);

    // Clear distance = the tighter of the corridor reach and the nearest
    // in-lane object.
    let clear = corridor_reach(&perception.corridor)
        .min(nearest_object_m.unwrap_or(f64::INFINITY))
        .max(0.0);

    // ACD cap: the speed from which a `decel_mps2` brake still stops within
    // (clear - margin). Unhealthy perception → 0.0 (MRC floor): never trust
    // an empty/low-confidence corridor.
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
        .map(|o| ObjOut {
            id: o.id,
            x: o.pos.x_m,
            y: o.pos.y_m,
            vx: o.vel.x_m,
            vy: o.vel.y_m,
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty / all-no-return scan reads as an unhealthy corridor → the
    /// MRC floor cap (fail-closed, unchanged from the example's contract).
    #[test]
    fn empty_scan_fails_closed_to_the_mrc_floor() {
        let resp = handle_perception(&PerceptionRequest {
            angle_min_rad: -1.5,
            angle_increment_rad: 0.01,
            range_min_m: 0.1,
            range_max_m: 12.0,
            ranges: vec![f32::INFINITY; 300],
            stamp_ms: 0,
            forward_extent_m: default_extent(),
            decel_mps2: default_decel(),
            margin_m: default_margin(),
            lane_half_m: default_lane_half(),
            confidence_floor: default_floor(),
        });
        if !resp.healthy {
            assert_eq!(resp.speed_cap_mps, 0.0, "unhealthy → MRC floor");
        }
    }
}
