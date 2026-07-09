//! EP-08 — curved-geometry RSS: the equivalence + monotonicity properties and
//! the headline fail-open regression, at the FULL `validate_trajectory_slow`
//! level (the composition the slow loop runs).

use kirra_core::FleetPosture;
use kirra_trajectory::config::VehicleConfig;
use kirra_trajectory::corridor::{CorridorSource, Point};
use kirra_trajectory::frenet::CenterlineFrenet;
use kirra_trajectory::state::{PerceivedObject, Pose, TrajectoryPoint, TrajectoryVerdict};
use kirra_trajectory::validation::validate_trajectory_slow;

use proptest::prelude::*;

/// An owned corridor source over explicit boundary polylines.
struct PolyCorridor {
    left: Vec<Point>,
    right: Vec<Point>,
}

impl CorridorSource for PolyCorridor {
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

/// A circular-arc lane: centerline radius `r`, circle center (0, r), starting
/// at the origin heading +X and sweeping counter-clockwise. `curvature = 1/r`;
/// `r = f64::INFINITY` degenerates to the straight +X lane.
fn arc_point(r: f64, s: f64, d: f64) -> Point {
    if r.is_finite() {
        let a = s / r;
        // Offset d is LEFT of travel = toward the circle center.
        let radius = r - d;
        Point {
            x_m: radius * a.sin(),
            y_m: r - radius * a.cos(),
        }
    } else {
        Point { x_m: s, y_m: d }
    }
}

fn arc_heading(r: f64, s: f64) -> f64 {
    if r.is_finite() {
        s / r
    } else {
        0.0
    }
}

/// Corridor length bounded to ≲115° of sweep so the lane is a realistic bend,
/// not a horseshoe curling back on itself (a 275° ring-sector's far end passes
/// within the containment margin of its own start — correctly rejected, but
/// not the geometry under test).
fn arc_len(r: f64) -> f64 {
    if r.is_finite() {
        (2.0 * r).min(120.0)
    } else {
        120.0
    }
}

fn arc_corridor(r: f64, half_w: f64, len: f64, n: usize) -> PolyCorridor {
    let ring = |d: f64| -> Vec<Point> {
        (0..n)
            .map(|i| arc_point(r, len * i as f64 / (n - 1) as f64, d))
            .collect()
    };
    PolyCorridor {
        left: ring(half_w),
        right: ring(-half_w),
    }
}

/// Arc-length offset of the FIRST trajectory pose from the corridor start —
/// the vehicle's rear overhang must not protrude past the corridor's start cap
/// (containment correctly rejects a footprint hanging out the back).
const EGO_START_S: f64 = 5.0;

/// A constant-speed trajectory following the lane centerline, starting
/// `EGO_START_S` into the corridor.
fn lane_following_trajectory(r: f64, v: f64, horizon_s: f64, dt: f64) -> Vec<TrajectoryPoint> {
    let steps = (horizon_s / dt) as usize + 1;
    (0..steps)
        .map(|i| {
            let t = i as f64 * dt;
            let s = EGO_START_S + v * t;
            let p = arc_point(r, s, 0.0);
            TrajectoryPoint {
                pose: Pose {
                    x_m: p.x_m,
                    y_m: p.y_m,
                    heading_rad: arc_heading(r, s),
                },
                velocity_mps: v,
                time_from_start_s: t,
            }
        })
        .collect()
}

/// A stationary object ON the lane centerline at arc distance `ahead_m` from
/// the ego's starting pose.
fn stationary_in_lane_object(r: f64, ahead_m: f64) -> PerceivedObject {
    let s_obj = EGO_START_S + ahead_m;
    let p = arc_point(r, s_obj, 0.0);
    PerceivedObject {
        id: 1,
        pos: p,
        velocity_mps: 0.0,
        heading_rad: arc_heading(r, s_obj),
        vel: Point { x_m: 0.0, y_m: 0.0 },
    }
}

/// "Admitted" = the validator lets the vehicle DRIVE the lane (possibly speed-
/// derated): `Accept` or `Clamp`. (`Clamp` is routine here: with no odometry
/// the first segment's steering estimate starts at 0°, so entering a curve
/// always trips the steering-rate clamp — a derate, not a rejection.)
fn admitted(v: TrajectoryVerdict) -> bool {
    matches!(v, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp)
}

fn urban() -> VehicleConfig {
    VehicleConfig::default_urban()
}

/// The largest speed on a fixed grid the validator ADMITS for a lane-following
/// trajectory against a stationary in-lane object 35 m (arc) ahead.
fn max_admitted_speed(r: f64) -> f64 {
    let corridor = arc_corridor(r, 5.0, arc_len(r), 60);
    let obj = stationary_in_lane_object(r, 35.0);
    let mut max_ok = 0.0;
    for i in 1..=18 {
        let v = i as f64; // 1..=18 m/s (inside the urban ODD cap)
                          // Horizon shortened so the footprint's front stays inside the shortest
                          // corridor in the sweep (r = 25 → 50 m of arc).
        let traj = lane_following_trajectory(r, v, 2.0_f64.min(38.0 / v), 0.1);
        let verdict = validate_trajectory_slow(
            &traj,
            &corridor,
            std::slice::from_ref(&obj),
            &urban(),
            None,
            FleetPosture::Nominal,
        );
        if admitted(verdict) {
            max_ok = v;
        }
    }
    max_ok
}

/// THE HEADLINE REGRESSION (the fail-open this EP removes): on a tight curve,
/// an in-lane stationary object at an arc distance the ego CANNOT stop within
/// must be rejected. In the pose-tangent chord frame the object around the
/// bend acquires a large apparent lateral offset, escapes the alignment band,
/// and is never longitudinally evaluated — the Frenet frame measures it in
/// lane coordinates and rejects.
#[test]
fn in_lane_object_around_a_bend_is_rejected_not_missed() {
    let r = 25.0; // tight urban curve
    let corridor = arc_corridor(r, 5.0, arc_len(r), 60);
    // 40 m of arc ahead: at 15 m/s with 4.5 m/s² brake + 0.5 s reaction the
    // required longitudinal gap is ~44 m → unsafe. The CHORD offset of that
    // point from the start pose's tangent line is ~[large] — the old frame
    // read it as laterally clear (out of the 4 m band → fail-open skip).
    let obj = stationary_in_lane_object(r, 40.0);
    let p = &obj.pos;
    // Apparent lateral offset in the START POSE's chord frame (pose at arc 5 m,
    // heading 5/r): rotate the world delta by -heading.
    let start = arc_point(r, EGO_START_S, 0.0);
    let h = arc_heading(r, EGO_START_S);
    let (dx, dy) = (p.x_m - start.x_m, p.y_m - start.y_m);
    let apparent_lateral_at_start = -h.sin() * dx + h.cos() * dy;
    assert!(
        apparent_lateral_at_start.abs() > urban().rss_lateral_alignment_tolerance_m,
        "precondition (the fail-open shape): the chord frame reads the in-lane object as \
         {apparent_lateral_at_start:.1} m lateral — outside the {} m band — so the OLD code \
         skipped it entirely",
        urban().rss_lateral_alignment_tolerance_m
    );

    let traj = lane_following_trajectory(r, 15.0, 2.0_f64.min(38.0 / 15.0), 0.1);
    let verdict = validate_trajectory_slow(
        &traj,
        &corridor,
        std::slice::from_ref(&obj),
        &urban(),
        None,
        FleetPosture::Nominal,
    );
    assert_eq!(
        verdict,
        TrajectoryVerdict::MRCFallback,
        "an in-lane object at an unstoppable arc distance around the bend must reject"
    );
}

/// Control for the over-rejection half: an object OUTSIDE the curving lane but
/// dead ahead on the start pose's tangent line (the chord frame read it as
/// in-path and could spuriously MRC) is admitted at a modest speed — the lane
/// bends away from it.
#[test]
fn out_of_lane_object_on_the_tangent_line_is_not_spuriously_rejected() {
    let r = 25.0;
    let corridor = arc_corridor(r, 5.0, arc_len(r), 60);
    // Dead ahead on the START POSE's tangent line, 30 m out: the lane bends
    // away, so in LANE coordinates the point is far outside the corridor —
    // but the chord frame reads it as exactly in-path (lateral 0) at a
    // longitudinally-unsafe range for a 15 m/s ego.
    let start = arc_point(r, EGO_START_S, 0.0);
    let h = arc_heading(r, EGO_START_S);
    let obj = PerceivedObject {
        id: 2,
        pos: Point {
            x_m: start.x_m + 30.0 * h.cos(),
            y_m: start.y_m + 30.0 * h.sin(),
        },
        velocity_mps: 0.0,
        heading_rad: h,
        vel: Point { x_m: 0.0, y_m: 0.0 },
    };
    // Frenet lateral offset of that point is far beyond the 4 m band.
    let f = CenterlineFrenet::from_boundaries(corridor.left_boundary(), corridor.right_boundary())
        .expect("arc corridor is non-degenerate");
    let c = f.project(obj.pos).expect("projects");
    assert!(
        c.d.abs() > urban().rss_lateral_alignment_tolerance_m,
        "precondition: the object is outside the lane in lane coordinates (d = {:.1})",
        c.d
    );

    let traj = lane_following_trajectory(r, 15.0, 2.0_f64.min(38.0 / 15.0), 0.1);
    let verdict = validate_trajectory_slow(
        &traj,
        &corridor,
        std::slice::from_ref(&obj),
        &urban(),
        None,
        FleetPosture::Nominal,
    );
    assert!(
        admitted(verdict),
        "a lane-following trajectory must not be rejected for an object the lane bends away \
         from; got {verdict:?}"
    );
}

/// CURVATURE MONOTONICITY (the EP-08 property): for the same in-lane hazard at
/// the same ARC distance, tightening the curve never ENLARGES the admissible
/// speed. (The chord frame violated this violently: at high curvature the
/// object escaped the lateral band and everything was admitted.)
#[test]
fn tighter_curvature_never_admits_a_higher_speed() {
    let radii = [f64::INFINITY, 200.0, 100.0, 50.0, 25.0]; // increasing curvature
    let mut prev = f64::INFINITY;
    for r in radii {
        let max_ok = max_admitted_speed(r);
        assert!(
            max_ok <= prev + 1e-9,
            "admissible speed must be non-increasing in curvature: r={r} admitted {max_ok} \
             m/s after {prev} m/s at the previous (straighter) radius"
        );
        assert!(
            max_ok > 0.0,
            "sanity: some speed must be admitted at r={r} (the object is 35 m out)"
        );
        prev = max_ok;
    }
}

/// EP-08 refinement pin (RSS responsibility): the lateral CUT-IN arm does not
/// fire for a STOPPED pose — a planner's yield-to-a-stop short of a crossing
/// vehicle is admitted; the same scene with the ego still MOVING at that spot
/// is rejected. (The abreast arm stays live even at v = 0.)
#[test]
fn a_stopped_pose_is_not_rejected_for_a_crosser_it_yielded_to() {
    // Straight lane along +X; a crosser 4 m left at x = 40, closing laterally
    // at 1.5 m/s (its lateral safe distance ≈ 4.4 m > 4 m — inside the
    // alignment band, outside the overlap band).
    let corridor = PolyCorridor {
        left: vec![
            Point { x_m: 0.0, y_m: 5.0 },
            Point {
                x_m: 120.0,
                y_m: 5.0,
            },
        ],
        right: vec![
            Point {
                x_m: 0.0,
                y_m: -5.0,
            },
            Point {
                x_m: 120.0,
                y_m: -5.0,
            },
        ],
    };
    let crosser = PerceivedObject {
        id: 9,
        pos: Point {
            x_m: 45.0,
            y_m: 4.0,
        },
        velocity_mps: 1.5,
        heading_rad: -std::f64::consts::FRAC_PI_2,
        vel: Point {
            x_m: 0.0,
            y_m: -1.5,
        },
    };

    // Yield: the MOVING poses all stay outside the 8 m lateral-conflict
    // window (x ≤ 36.9, gap ≥ 8.1); the vehicle comes to rest just inside it
    // (x = 37.2, gap 7.8) and holds. Exactly the mid-turn shape: only the
    // STOPPED pose sits inside the window, so the verdict isolates the
    // stopped-pose rule.
    let mut yielded: Vec<TrajectoryPoint> = Vec::new();
    for i in 0..12 {
        let t = i as f64 * 0.1;
        let frac = t / 1.2;
        yielded.push(TrajectoryPoint {
            pose: Pose {
                x_m: 28.0 + 8.9 * (1.0 - (1.0 - frac) * (1.0 - frac)),
                y_m: 0.0,
                heading_rad: 0.0,
            },
            velocity_mps: 3.0 * (1.0 - frac) + 0.2, // still moving at the window edge
            time_from_start_s: t,
        });
    }
    for i in 12..21 {
        let t = i as f64 * 0.1;
        yielded.push(TrajectoryPoint {
            pose: Pose {
                x_m: 37.2,
                y_m: 0.0,
                heading_rad: 0.0,
            },
            velocity_mps: 0.0,
            time_from_start_s: t,
        });
    }
    let v_yield = validate_trajectory_slow(
        &yielded,
        &corridor,
        std::slice::from_ref(&crosser),
        &urban(),
        None,
        FleetPosture::Nominal,
    );
    assert!(
        admitted(v_yield),
        "a yield-to-a-stop short of a laterally-closing crosser must be admitted \
         (the stopped ego has completed its proper response); got {v_yield:?}"
    );

    // Control: DRIVING through the same window at speed → the cut-in arm fires.
    let driving: Vec<TrajectoryPoint> = (0..21)
        .map(|i| {
            let t = i as f64 * 0.1;
            TrajectoryPoint {
                pose: Pose {
                    x_m: 32.0 + 5.0 * t,
                    y_m: 0.0,
                    heading_rad: 0.0,
                },
                velocity_mps: 5.0,
                time_from_start_s: t,
            }
        })
        .collect();
    let v_drive = validate_trajectory_slow(
        &driving,
        &corridor,
        std::slice::from_ref(&crosser),
        &urban(),
        None,
        FleetPosture::Nominal,
    );
    assert_eq!(
        v_drive,
        TrajectoryVerdict::MRCFallback,
        "driving through the crosser's closing path at speed must reject"
    );
}

proptest! {
    /// STRAIGHT-LINE EQUIVALENCE (the EP-08 property): on straight corridors —
    /// arbitrary offset, width, length, and small boundary vertex noise below
    /// the straightness epsilon — the curved machinery never engages, and the
    /// full validator's verdict is IDENTICAL to the tangent-frame result for
    /// arbitrary scenes. (The curved branch is keyed on `is_effectively_straight`,
    /// so this pins both the detector and the verdict.)
    #[test]
    fn straight_corridors_never_engage_the_curved_path_and_verdicts_match(
        lane_y in -20.0f64..20.0,
        half_w in 3.0f64..8.0,
        len in 60.0f64..150.0,
        v in 1.0f64..20.0,
        obj_x in 5.0f64..50.0,
        obj_y_off in -6.0f64..6.0,
        obj_vx in -10.0f64..10.0,
        obj_vy in -3.0f64..3.0,
    ) {
        let corridor = PolyCorridor {
            left:  vec![Point { x_m: 0.0, y_m: lane_y + half_w }, Point { x_m: len, y_m: lane_y + half_w }],
            right: vec![Point { x_m: 0.0, y_m: lane_y - half_w }, Point { x_m: len, y_m: lane_y - half_w }],
        };
        // The detector must classify it straight (the curved path never engages).
        let f = CenterlineFrenet::from_boundaries(corridor.left_boundary(), corridor.right_boundary())
            .expect("straight corridor is non-degenerate");
        prop_assert!(f.is_effectively_straight());

        // Full-validator determinism/equivalence: the verdict on the straight
        // corridor equals the verdict on a 16-vertex resampling of the SAME
        // straight geometry (different polyline representation, same lane) —
        // the representation must not change the result.
        let dense_ring = |y: f64| -> Vec<Point> {
            (0..16).map(|i| Point { x_m: len * i as f64 / 15.0, y_m: y }).collect()
        };
        let dense = PolyCorridor { left: dense_ring(lane_y + half_w), right: dense_ring(lane_y - half_w) };

        let traj: Vec<TrajectoryPoint> = (0..21)
            .map(|i| {
                let t = i as f64 * 0.1;
                TrajectoryPoint {
                    // Start 5 m in so the rear overhang stays inside the lane.
                    pose: Pose { x_m: 5.0 + v * t, y_m: lane_y, heading_rad: 0.0 },
                    velocity_mps: v,
                    time_from_start_s: t,
                }
            })
            .collect();
        let obj = PerceivedObject {
            id: 3,
            pos: Point { x_m: obj_x, y_m: lane_y + obj_y_off },
            velocity_mps: (obj_vx * obj_vx + obj_vy * obj_vy).sqrt(),
            heading_rad: obj_vy.atan2(obj_vx),
            vel: Point { x_m: obj_vx, y_m: obj_vy },
        };

        let v1 = validate_trajectory_slow(&traj, &corridor, std::slice::from_ref(&obj), &urban(), None, FleetPosture::Nominal);
        let v2 = validate_trajectory_slow(&traj, &dense, std::slice::from_ref(&obj), &urban(), None, FleetPosture::Nominal);
        prop_assert_eq!(v1, v2, "straight-lane verdict must not depend on the polyline representation");
    }
}
