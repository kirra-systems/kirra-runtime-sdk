//! #1214 gate 1 — a cold-started Taj must be ACCEPTED by the real downstream
//! composition, not merely serialize a nonzero generation.
//!
//! The original defect lived in the seam between two components that were each
//! individually correct: Taj stamped a generation, the planner refused zero as
//! the invalid-frame sentinel, and nobody had run the two together from process
//! start. A unit test asserting "Taj emits 1" would not have caught it either,
//! because the number alone is not the claim — the claim is that the number is
//! one the next component will accept.
//!
//! So this drives the ACTUAL types across the seam: a real `PerceptionResponse`
//! from a freshly-constructed `TrackedPerceptionState`, its `frame_id` carried
//! into a real `PlanRequest`, judged by the real `handle_plan`.

use kirra_sidecars::planner::{handle_plan, EgoReq, PerceptionFrameIdReq, PlanRequest, Xy};
use kirra_sidecars::taj::{
    handle_perception_tracked_at, PerceptionRequest, PerceptionResponse, TrackedPerceptionState,
};

/// A scan with clear road ahead, shaped like the sidecar's own fixtures.
fn scan(seq: u32) -> PerceptionRequest {
    let ranges: Vec<f32> = (0..300)
        .map(|i| {
            let theta = -1.5 + f64::from(i) * 0.01;
            let base = if theta.abs() < 0.15 {
                6.0 / theta.cos()
            } else {
                8.0
            };
            f32::from_bits((base as f32).to_bits() + (seq % 7))
        })
        .collect();
    PerceptionRequest {
        ranges,
        angle_min_rad: -1.5,
        angle_increment_rad: 0.01,
        range_min_m: 0.05,
        range_max_m: 12.0,
        stamp_ms: 1_000 + u64::from(seq) * 100,
        forward_extent_m: 8.0,
        decel_mps2: 1.5,
        margin_m: 0.4,
        lane_half_m: 0.26,
        vehicle_width_m: 0.203,
        lateral_clearance_m: 0.15,
        confidence_floor: 0.5,
        publisher_count: Some(1),
        camera_armed: false,
        detections: vec![],
        camera_observations: vec![],
        camera_stamp_ms: None,
        camera_max_age_ms: 200,
        diagnostic_probe: false,
    }
}

fn plan_request_from(response: &PerceptionResponse) -> PlanRequest {
    PlanRequest {
        perception_frame_id: Some(PerceptionFrameIdReq {
            scan_sequence: response.frame_id.scan_sequence,
            camera_sequence: response.frame_id.camera_sequence,
            tracker_generation: response.frame_id.tracker_generation,
            profile_digest: response.frame_id.profile_digest.clone(),
            evidence_digest: response.frame_id.evidence_digest.clone(),
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
        destination: None,
        targets: vec![],
        targets_stamp_ms: None,
        now_ms: None,
    }
}

/// THE regression, at the seam where it actually lived: the very first frame a
/// cold-started sidecar produces must be plannable.
#[test]
fn the_first_frame_of_a_cold_started_sidecar_is_accepted_by_the_planner() {
    let mut state = TrackedPerceptionState::new();
    let first = handle_perception_tracked_at(&scan(1), &mut state, 0);

    let response = handle_plan(&plan_request_from(&first)).unwrap_or_else(|rejection| {
        panic!(
            "a cold-started sidecar's first frame was refused by the planner: \
             {} — {}. This is the #1214 defect: the evidence-binding path never \
             worked from process start.",
            rejection.code, rejection.detail
        )
    });

    assert_eq!(
        response
            .perception_frame_id
            .expect("echoed")
            .tracker_generation,
        first.frame_id.tracker_generation,
        "the planner must echo the identity it planned against, unchanged"
    );
}

/// …and stays accepted across the resets that advance the generation, so the fix
/// is not "the first frame happens to work".
#[test]
fn every_frame_across_resets_remains_plannable() {
    let mut state = TrackedPerceptionState::new();
    let mut now = 0;

    for round in 0..5 {
        for seq in 1..=3 {
            now += 100;
            let frame = handle_perception_tracked_at(&scan(round * 10 + seq), &mut state, now);
            handle_plan(&plan_request_from(&frame)).unwrap_or_else(|rejection| {
                panic!(
                    "round {round} seq {seq} refused: {} — {}",
                    rejection.code, rejection.detail
                )
            });
        }
        state.reset();
    }
}

/// Non-vacuity: the planner genuinely refuses the sentinel, so the tests above
/// are not passing because the check is absent.
#[test]
fn the_planner_still_refuses_a_zero_generation() {
    let mut state = TrackedPerceptionState::new();
    let first = handle_perception_tracked_at(&scan(1), &mut state, 0);

    let mut request = plan_request_from(&first);
    request
        .perception_frame_id
        .as_mut()
        .expect("present")
        .tracker_generation = 0;

    let rejection = handle_plan(&request)
        .expect_err("a zero generation is the invalid-frame sentinel and must be refused");
    assert_eq!(rejection.code, "INVALID_PERCEPTION_FRAME_ID");
}
