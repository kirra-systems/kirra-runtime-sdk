//! **Occlusion-aware speed bound at junctions** — RSS Rule 4 applied LATERALLY at a blind
//! junction. The forward assured-clear-distance bound (a stopped hazard in your own lane beyond
//! visibility) already lives in the checker; this is its junction sibling: a building / hedge /
//! parked car blocks the ego's view of CROSS traffic, so it must creep in slowly enough to stop
//! for a vehicle that could emerge from the unseen approach.
//!
//! The map flags the approach lane's assured-clear sight distance toward the conflict; Occy's
//! behavioral layer turns it into an approach speed cap = the most the ego can carry and still
//! brake within what it can see. The doer therefore drives an open junction at cruise and CREEPS
//! a blind one — and KIRRA still bounds the result. This pins the doer behavior end to end (map
//! → `derive_controls` → behavioral cap → Occy → KIRRA), and that an open-view junction is
//! unaffected.

use kirra_planner::{
    plan_for_intent, EgoState, FleetPosture, GeometricPlanner, Goal, Lane, LaneControl, LaneEdge,
    LaneGraph, LineType, MickIntent, Occluder, PlanInput, Pose, ProposalKind, TrajectoryVerdict,
};
use kirra_trajectory::corridor::{CorridorSource, MockCorridorSource};
use kirra_trajectory::{validate_trajectory_slow, VehicleConfig};

/// A single straight approach lane (0,0)→(40,0) east; the junction conflict sits at its
/// terminus (x=40). `sight` optionally flags it as occluded with that assured-clear distance.
fn approach(sight: Option<f64>) -> LaneGraph {
    let line = LineType::Solid;
    let mut g = LaneGraph::new()
        .with_lane(Lane::straight(1, 0.0, 0.0, 40.0, 3.0, line, line).with_edge(LaneEdge::Successor { to: 2 }))
        .with_lane(Lane::straight(2, 0.0, 40.0, 80.0, 3.0, line, line));
    if let Some(d) = sight {
        g = g.with_occluded_approach(1, d);
    }
    g
}

fn world<'a>(map: &'a dyn CorridorSource, g: &'a LaneGraph) -> PlanInput<'a> {
    PlanInput {
        // Start slow so the planner ACCELERATES up toward the binding speed target — then the
        // trajectory's peak speed reflects the cap (an open junction climbs to cruise; a blind
        // one is held at the assured-clear creep speed).
        ego: EgoState { pose: Pose { x_m: 8.0, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: 2.0, yaw_rate_rads: 0.0, stamp_ms: 0 },
        goal: Goal { target: Pose { x_m: 60.0, y_m: 0.0, heading_rad: 0.0 } },
        map,
        objects: &[],
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
        lane_graph: Some(g),
        signal_states: &[],
    }
}

fn peak_speed(p: &kirra_planner::PlanOutput) -> f64 {
    p.trajectory.iter().map(|t| t.velocity_mps).fold(0.0, f64::max)
}

#[test]
fn the_ego_creeps_into_a_blind_junction_but_cruises_an_open_one() {
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let intent = MickIntent::GoTo { x_m: 60.0, y_m: 0.0 };
    let cfg = VehicleConfig::default_urban();
    let admit = |p: &kirra_planner::PlanOutput| matches!(
        validate_trajectory_slow(&p.trajectory, &corr, &[], &cfg, None, FleetPosture::Nominal),
        TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp
    );

    // OPEN junction (no occlusion datum): the ego approaches at its normal cruise speed.
    let open_g = approach(None);
    let open = plan_for_intent(&mut GeometricPlanner::default(), &intent, &world(&corr, &open_g));
    assert_eq!(open.kind, ProposalKind::Motion);
    let open_peak = peak_speed(&open);
    assert!(open_peak > 6.0, "an open junction is taken at cruise, got peak {open_peak}");
    assert!(admit(&open), "KIRRA admits the open-junction approach");

    // BLIND junction: only ~5 m of assured-clear sight toward the conflict. The ego CREEPS — its
    // peak speed drops to the assured-clear-distance bound (~4 m/s), well below the open case —
    // so it can stop for cross-traffic emerging from the unseen approach. KIRRA admits the creep.
    let blind_g = approach(Some(5.0));
    let blind = plan_for_intent(&mut GeometricPlanner::default(), &intent, &world(&corr, &blind_g));
    assert_eq!(blind.kind, ProposalKind::Motion, "the ego still proceeds (creeps), it does not HOLD");
    let blind_peak = peak_speed(&blind);
    assert!(blind_peak < 5.0, "the blind approach is creep-capped to the assured-clear speed, got peak {blind_peak}");
    assert!(blind_peak < open_peak - 1.5, "the blind junction is taken markedly slower than the open one ({blind_peak} vs {open_peak})");
    assert!(admit(&blind), "KIRRA admits the occlusion-creep approach");

    // The blinder the corner, the slower the creep: 3 m of sight is slower still than 5 m.
    let blinder_g = approach(Some(3.0));
    let blinder = plan_for_intent(&mut GeometricPlanner::default(), &intent, &world(&corr, &blinder_g));
    assert!(peak_speed(&blinder) < blind_peak, "less sight → slower creep ({} vs {blind_peak})", peak_speed(&blinder));
}

#[test]
fn occlusion_creep_is_driven_by_map_occluder_geometry_not_a_hand_fed_datum() {
    // The same creep, but the sight distance is DERIVED from a corner building's footprint instead
    // of being hand-fed. A building just off the +y edge of the approach, ending 5 m before the
    // conflict line (x=40) ⇒ derived sight = 40 − 35 = 5 m — identical to the `Some(5.0)` blind
    // case above. This proves the geometry closes the loop to the existing consumer end to end:
    // map footprint → derive_occluded_approaches → derive_controls → creep cap → Occy → KIRRA.
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let intent = MickIntent::GoTo { x_m: 60.0, y_m: 0.0 };
    let cfg = VehicleConfig::default_urban();
    let admit = |p: &kirra_planner::PlanOutput| matches!(
        validate_trajectory_slow(&p.trajectory, &corr, &[], &cfg, None, FleetPosture::Nominal),
        TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp
    );

    // approach() lane 1 is centred at y=0 with half-width 3 m; a building at y∈[3.5, 9] off the +y
    // edge, x∈[10, 35], is a corner occluder whose junction edge (x=35) sits 5 m before the line.
    let building = Occluder::new(10.0, 35.0, 3.5, 9.0);
    let derived_g = approach(None).with_derived_occlusion(&[building]);
    assert_eq!(derived_g.sight_distance(1), Some(5.0), "the sight distance is derived from the footprint");

    let derived = plan_for_intent(&mut GeometricPlanner::default(), &intent, &world(&corr, &derived_g));
    assert_eq!(derived.kind, ProposalKind::Motion, "the ego creeps the geometry-derived blind junction");
    let derived_peak = peak_speed(&derived);

    // Byte-for-byte the same outcome as the hand-fed Some(5.0) datum: same cap, same creep.
    let handfed = plan_for_intent(&mut GeometricPlanner::default(), &intent, &world(&corr, &approach(Some(5.0))));
    assert!((derived_peak - peak_speed(&handfed)).abs() < 1e-9, "derived geometry == hand-fed datum");
    assert!(derived_peak < 5.0, "creep-capped to the assured-clear speed, got peak {derived_peak}");
    assert!(admit(&derived), "KIRRA admits the geometry-derived occlusion creep");

    // And an open junction (no footprint anywhere) is still taken at cruise — the derivation is
    // a no-op when nothing shadows the approach.
    let open_g = approach(None).with_derived_occlusion(&[Occluder::new(10.0, 35.0, 40.0, 50.0)]); // far away
    assert_eq!(open_g.sight_distance(1), None, "a distant footprint shadows nothing");
    let open = plan_for_intent(&mut GeometricPlanner::default(), &intent, &world(&corr, &open_g));
    assert!(peak_speed(&open) > 6.0, "open junction taken at cruise");
}

#[test]
fn occlusion_creep_composes_with_a_stop_sign_at_the_same_blind_junction() {
    // A blind approach that ALSO carries a STOP sign: the ego must both stop at the line AND
    // creep on the way in. Until it has stopped (satisfied), the stop line binds; the occlusion
    // cap shapes the approach speed regardless. Here the ego is moving, so the stop line is
    // active → it decelerates to the line, and never exceeds the creep cap getting there.
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let line = LineType::Solid;
    let g = LaneGraph::new()
        .with_lane(
            Lane::straight(1, 0.0, 0.0, 40.0, 3.0, line, line)
                .with_control(LaneControl::Stop)
                .with_edge(LaneEdge::Successor { to: 2 }),
        )
        .with_lane(Lane::straight(2, 0.0, 40.0, 80.0, 3.0, line, line))
        .with_occluded_approach(1, 5.0);

    let out = plan_for_intent(&mut GeometricPlanner::default(), &MickIntent::GoTo { x_m: 60.0, y_m: 0.0 }, &world(&corr, &g));
    // Stops at/before the line (x=40) — the stop sign binds.
    let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
    assert!(max_x <= 41.0, "stops at the stop line, got max_x {max_x}");
    // And never exceeds the occlusion creep cap (~4 m/s) on the way in.
    assert!(peak_speed(&out) < 5.0, "the blind+stop approach stays under the creep cap, got peak {}", peak_speed(&out));
    assert!(
        matches!(validate_trajectory_slow(&out.trajectory, &corr, &[], &VehicleConfig::default_urban(), None, FleetPosture::Nominal), TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "KIRRA admits the blind stop-controlled approach"
    );
}
