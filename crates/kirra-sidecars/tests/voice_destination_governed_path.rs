//! **The live voice-to-destination path, end to end through the governed
//! seam** — the integration proof for the typed-destination integration.
//!
//! What this pins, in ONE chain and without touching actuation:
//!
//! ```text
//!   typed DestinationRef  (what the voice router is allowed to say)
//!     → DestinationService  (the trusted resolver + the frame-explicit latch)
//!     → MickIntent::GoTo    (the ordinary governed intent — the #1293 conversion)
//!     → handle_plan_with_destinations  (Occy + the KIRRA slow-loop checker)
//! ```
//!
//! The load-bearing negatives are here too: an unresolved / ambiguous / stale
//! / unsupported destination produces NO grounded target, and a plan request
//! carrying that same unresolved destination produces NO MOTION — an empty
//! trajectory behind a stable `DEST_*` code, never a fallback goal.
//!
//! Nothing in this file (or its crate) can reach a motor: `kirra-sidecars` is
//! fenced by `ci/check_mick_actuation_fence.py`.

use kirra_planner::MickIntent;
use kirra_sidecars::destination::{
    DestinationResolver, ObjectPolicy, PlaceRegistry, RouteRegistry,
};
use kirra_sidecars::destination_service::{
    DestinationRequest, DestinationService, GroundedFrame, TargetReq,
    ROUTE_RESOLUTION_FIRST_WAYPOINT_ONLY,
};
use kirra_sidecars::planner::{
    handle_plan_with_destinations, EgoReq, PerceptionFrameIdReq, PlanRequest, Xy,
};

/// A calibrated lab: one place well inside the corridor the plan request
/// supplies, and one saved route.
const PLACES: &str = r#"{
    "version": 1, "map_id": "r2-lab-01",
    "places": [
        {"id":"kitchen","name":"Kitchen","aliases":["the kitchen"],
         "x_m":40.0,"y_m":0.0,"heading_rad":0.0}
    ]}"#;
const ROUTES: &str = r#"{
    "version": 1, "map_id": "r2-lab-01",
    "routes": [
        {"id":"route-a","name":"Route A","aliases":["route alpha"],
         "waypoints":[{"x_m":20.0,"y_m":0.0},{"x_m":40.0,"y_m":1.0}]}
    ]}"#;

fn resolver() -> DestinationResolver {
    DestinationResolver {
        places: PlaceRegistry::from_json_str(PLACES).expect("places"),
        routes: RouteRegistry::from_json_str(ROUTES).expect("routes"),
        object_policy: ObjectPolicy::default(),
    }
}

/// The typed request a voice router would send — kind + the operator's words.
fn voice_says(kind: &str, query: &str) -> DestinationRequest {
    DestinationRequest {
        destination: serde_json::json!({"kind": kind, "query": query}),
        ..Default::default()
    }
}

fn plan_request(destination: Option<serde_json::Value>) -> PlanRequest {
    PlanRequest {
        perception_frame_id: Some(PerceptionFrameIdReq {
            scan_sequence: 7,
            camera_sequence: Some(11),
            tracker_generation: 3,
            profile_digest: "a".repeat(64),
            evidence_digest: "b".repeat(64),
        }),
        ego: EgoReq {
            x: 2.0,
            y: 0.0,
            heading: 0.0,
            speed: 1.0,
        },
        goal: Xy { x: 40.0, y: 0.0 },
        cruise: 3.0,
        left: vec![[-5.0, 5.0], [100.0, 5.0]],
        right: vec![[-5.0, -5.0], [100.0, -5.0]],
        objects: vec![],
        pedestrians: vec![],
        predicted_vrus: vec![],
        vehicle: None,
        intent: None,
        object_goal: None,
        destination,
        targets: vec![],
        targets_stamp_ms: None,
        now_ms: None,
    }
}

/// THE headline chain: "drive to the kitchen" resolves to a trusted registry
/// pose, becomes an ordinary governed `GoTo`, and the SAME typed destination
/// plans + passes the checker at the planner seam.
#[test]
fn a_spoken_named_place_grounds_through_the_resolver_and_plans() {
    let mut svc = DestinationService::new(resolver());

    // 1. The voice layer's whole contribution: a kind and a name.
    let accepted = svc
        .handle(&voice_says("named_place", "the kitchen"), 1_000)
        .expect("a calibrated place resolves");

    // 2. The TRUSTED resolver — not the caller — supplied the coordinates,
    //    and the frame is named so nothing downstream can misread them.
    assert_eq!(accepted.destination_id, "kitchen");
    assert_eq!(accepted.frame, GroundedFrame::Map("r2-lab-01".into()));
    assert_eq!(
        accepted.to_goto(),
        MickIntent::GoTo {
            x_m: 40.0,
            y_m: 0.0
        },
        "the #1293 conversion: an ordinary governed GoTo"
    );

    // 3. The same typed destination at the planner seam: Occy grounds it and
    //    the KIRRA slow-loop checker bounds it. A drivable corridor admits it.
    let resp = handle_plan_with_destinations(
        &plan_request(Some(
            serde_json::json!({"kind":"named_place","query":"the kitchen"}),
        )),
        &resolver(),
    )
    .expect("the grounded destination plans");
    assert!(
        resp.admitted && !resp.trajectory.is_empty(),
        "an admitted plan carries its trajectory: {} / {}",
        resp.verdict,
        resp.trajectory.len()
    );
    let echo = resp.destination.expect("provenance is echoed");
    assert_eq!(echo.destination_id, "kitchen");
    assert_eq!(echo.source, "place_registry");
    // The proposal is ADVISORY — authority stays with the release-token
    // chokepoint and the verifying consumer, in other processes.
    assert!(resp.advisory);
}

/// A saved route grounds to its FIRST waypoint only, and says so at every
/// layer — the latch, the admission body, and the planner echo.
#[test]
fn a_spoken_saved_route_is_marked_first_waypoint_only_everywhere() {
    let mut svc = DestinationService::new(resolver());
    let accepted = svc
        .handle(&voice_says("saved_route", "route alpha"), 1_000)
        .expect("a calibrated route resolves");
    assert_eq!(
        accepted.route_resolution,
        ROUTE_RESOLUTION_FIRST_WAYPOINT_ONLY
    );
    assert_eq!(
        accepted.goal_pose.x_m, 20.0,
        "the FIRST waypoint is the goal"
    );
    assert!(accepted
        .to_post_wire()
        .contains(ROUTE_RESOLUTION_FIRST_WAYPOINT_ONLY));

    let resp = handle_plan_with_destinations(
        &plan_request(Some(
            serde_json::json!({"kind":"saved_route","query":"route alpha"}),
        )),
        &resolver(),
    )
    .expect("the route's first waypoint plans");
    let echo = resp.destination.expect("provenance is echoed");
    assert_eq!(
        echo.route_waypoints.len(),
        2,
        "the whole route rides the echo"
    );
    assert_eq!(echo.source, "route_registry");
}

/// A tracked object supplied by a PERCEPTION-AWARE caller resolves to a
/// standoff goal. The voice router supplies no targets, which is why the
/// voice path gets `Stale` (proved below) rather than a guess.
#[test]
fn a_tracked_object_from_a_perception_aware_caller_grounds_with_a_standoff() {
    let mut svc = DestinationService::new(resolver());
    let req = DestinationRequest {
        destination: serde_json::json!({"kind":"tracked_object","class":"cup","color":"red"}),
        targets: vec![TargetReq {
            label: "red cup".into(),
            x: 4.0,
            y: 0.0,
            confidence: 0.9,
        }],
        targets_stamp_ms: Some(1_000),
        ego: None,
    };
    let accepted = svc.handle(&req, 1_000).expect("a fresh sighting resolves");
    assert_eq!(accepted.frame, GroundedFrame::EgoRelative);
    assert!(
        (accepted.goal_pose.x_m - 3.25).abs() < 1e-9,
        "0.75 m standoff short of a 4 m cup: {}",
        accepted.goal_pose.x_m
    );
}

/// EVERY refusal produces no grounded target AND no motion at the planner
/// seam — the fail-closed half of the live path, in one table.
#[test]
fn every_unresolved_destination_yields_no_target_and_no_motion() {
    let mut svc = DestinationService::new(resolver());
    let cases: [(serde_json::Value, &str); 4] = [
        (
            serde_json::json!({"kind":"named_place","query":"garage"}),
            "DEST_NOT_FOUND",
        ),
        (
            serde_json::json!({"kind":"saved_route","query":"route zulu"}),
            "DEST_NOT_FOUND",
        ),
        (
            serde_json::json!({"kind":"address","query":"125 Main Street"}),
            "DEST_UNSUPPORTED_ADDRESS",
        ),
        (
            // No camera frame → STALE. "The detector did not look" is never
            // "it isn't there" — this is the voice path's object outcome.
            serde_json::json!({"kind":"tracked_object","class":"cup","color":"red"}),
            "DEST_STALE",
        ),
    ];

    for (i, (dest, code)) in cases.iter().enumerate() {
        // (a) the resolver seam: a refusal, and NOTHING latched.
        let refusal = svc
            .handle(
                &DestinationRequest {
                    destination: dest.clone(),
                    ..Default::default()
                },
                10_000 + i as u64 * 5_000,
            )
            .expect_err("must refuse");
        assert_eq!(&refusal.code, code, "case {i}");
        assert!(
            !refusal.sentence.is_empty(),
            "case {i}: a refusal must be speakable"
        );
        assert!(svc.last().is_none(), "case {i}: a refusal must not latch");

        // (b) the planner seam: the SAME destination is refused to NO MOTION,
        //     never a fallback to the request's own goal.
        let rej = handle_plan_with_destinations(&plan_request(Some(dest.clone())), &resolver())
            .expect_err("case {i}: must refuse at the planner seam");
        assert_eq!(rej.code, *code, "case {i}");
        let wire: serde_json::Value = serde_json::from_str(&rej.to_json()).unwrap();
        assert_eq!(wire["kind"], "SafeStop", "case {i}");
        assert_eq!(
            wire["trajectory"].as_array().map(Vec::len),
            Some(0),
            "case {i}: no motion may ride an unresolved destination"
        );
    }
}

/// The core rule at the live seam: geometry offered through the language
/// channel is refused LOUDLY, at both the resolver and the planner.
#[test]
fn smuggled_coordinates_are_refused_at_every_door() {
    let smuggled = serde_json::json!({"kind":"named_place","query":"kitchen","x_m":9.0,"y_m":9.0});
    let mut svc = DestinationService::new(resolver());
    assert_eq!(
        svc.handle(
            &DestinationRequest {
                destination: smuggled.clone(),
                ..Default::default()
            },
            1_000
        )
        .expect_err("resolver must refuse")
        .code,
        "DEST_COORDINATES_FORBIDDEN"
    );
    assert!(svc.last().is_none());
    assert_eq!(
        handle_plan_with_destinations(&plan_request(Some(smuggled)), &resolver())
            .expect_err("planner seam must refuse")
            .code,
        "DEST_COORDINATES_FORBIDDEN"
    );
}

/// Apply-once: each accepted destination advances the sequence exactly once,
/// so a consumer polling the latch can never re-apply an old target — and a
/// refusal in between does not move the sequence at all.
#[test]
fn the_latch_sequence_advances_once_per_accepted_destination() {
    let mut svc = DestinationService::new(resolver());
    let first = svc
        .handle(&voice_says("named_place", "kitchen"), 1_000)
        .expect("resolves");
    assert_eq!(first.seq, 1);

    // A refusal between two acceptances must not advance anything.
    assert!(svc
        .handle(&voice_says("named_place", "garage"), 10_000)
        .is_err());
    assert_eq!(
        svc.last().unwrap().seq,
        1,
        "a refusal advanced the sequence"
    );

    let second = svc
        .handle(&voice_says("saved_route", "route alpha"), 20_000)
        .expect("resolves");
    assert_eq!(second.seq, 2);
    assert_eq!(svc.last().unwrap().destination_id, "route-a");
}
