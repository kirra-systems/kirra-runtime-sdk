//! **The governed-loop demo, as a CI artifact** (#894 Part 3) — the thesis,
//! executable: *an LLM asked for something unsafe, the planner tried, the
//! checker refused, nothing reached the motors, and the system explained why
//! in a sentence.*
//!
//! End-to-end over the REAL seams, mirroring the shape of
//! `tier1_chokepoint.rs` (the mock serial chokepoint) and
//! `kirra_planner::tests::checker_catches_reacceleration_in_degraded` (the
//! independence-at-the-margin scenario):
//!
//!   LLM text ─▶ MickIntent::from_llm_json  (the ONE fail-closed parse)
//!            ─▶ plan_for_intent            (the real grounding seam, doer-generic)
//!            ─▶ validate_trajectory_slow_explained
//!                                          (the real checker + #893 narration)
//!            ─▶ governed release           (mints ONLY on an admitted verdict —
//!                                           the pinned deny-path-never-mints rule)
//!            ─▶ MotorConsumer + RecordingSerial (the ADR-0033 chokepoint)
//!
//! The DOER here is a *literal* doer — it obeys the intent verbatim (straight
//! at the goal / exactly the requested speed). That is the independence-test
//! pattern: the checker must be a real backstop for a doer that fails (or a
//! learned doer that was never safe to begin with), so the demo does not rest
//! on Occy's own good behavior. `plan_for_intent` is planner-generic by
//! design ("the same grounding holds for any doer behind the seam").
//!
//! 🔴 Verdict-core honesty: `TrajectoryVerdict` and the frozen kinematics
//! core are UNTOUCHED by this test (and by the loop it proves) — the
//! narration reason rides ALONGSIDE the verdict (#893), and the one-byte pin
//! (`kirra_core::trajectory::trajectory_verdict_stays_one_byte`) still gates.

use ed25519_dalek::SigningKey;
use kirra_actuation_consumer::{
    ConsumerConfig, FrameOutcome, MotorConsumer, MotorSerial, ReleaseToken, RosReleaseRefusal,
    RosTwistPayload, DEFAULT_MISSED_PERIODS,
};
use kirra_core::frame_integrity::FrameTrust;
use kirra_planner::{
    plan_for_intent, EgoState, FleetPosture, Goal, MickIntent, PlanInput, PlanOutput, Planner,
    Pose, TrajectoryPoint,
};
use kirra_release_token::ros_twist::issue_ros_release;
use kirra_trajectory::validation::{
    validate_trajectory_slow_explained, TrajectoryRefusalReason,
};
use kirra_trajectory::{MockCorridorSource, TrajectoryVerdict, VehicleConfig};

/// The demo governor key (test provenance only; a deployment provisions per
/// docs/safety/GOVERNOR_KEY_PROVISIONING.md — same fixture as tier1).
const DRILL_SEED: [u8; 32] = [42u8; 32];
const T_MINT: u64 = 10_000;
const T_VERIFY: u64 = 10_050;

/// verdicts.rs `EXPLAIN_UNKNOWN` marker text — the narration must be the
/// SPECIFIC sentence, never this generic fallback.
const EXPLAIN_UNKNOWN_MARKER: &str = "Unrecognized denial code";

#[derive(Default)]
struct RecordingSerial {
    writes: Vec<(f64, f64)>,
}
impl MotorSerial for RecordingSerial {
    type Error = std::convert::Infallible;
    fn write_twist(&mut self, linear: f64, angular: f64) -> Result<(), Self::Error> {
        self.writes.push((linear, angular));
        Ok(())
    }
}

fn consumer() -> MotorConsumer<RecordingSerial> {
    MotorConsumer::new(
        SigningKey::from_bytes(&DRILL_SEED).verifying_key(),
        ConsumerConfig {
            freshness_window_ms: 200,
            control_period_ms: 100,
            missed_periods: DEFAULT_MISSED_PERIODS,
            stop_decel_mps2: 1.0, // test fixture; deployments use the class MRC decel
        },
        RecordingSerial::default(),
    )
    .expect("valid drill config")
}

/// A LITERAL doer: drives straight at the goal at a constant speed (or ramps
/// to exactly the requested cruise speed). This is the misbehaving/naive doer
/// of the independence tests — it does not know the corridor exists. The
/// checker must catch it; nothing in the demo rests on the doer being good.
struct LiteralDoer {
    speed_mps: f64,
    dt_s: f64,
    steps: usize,
}
impl Planner for LiteralDoer {
    fn plan(&mut self, input: &PlanInput<'_>) -> PlanOutput {
        let ego = input.ego.pose;
        let (gx, gy) = (input.goal.target.x_m, input.goal.target.y_m);
        let heading = (gy - ego.y_m).atan2(gx - ego.x_m);
        // A requested cruise speed is honored LITERALLY — even off the
        // current speed, even in Degraded (the drift under test).
        let (v0, v1) = match input.target_speed_mps {
            Some(v) => (input.ego.linear_x_mps, v),
            None => (self.speed_mps, self.speed_mps),
        };
        let mut trajectory = Vec::with_capacity(self.steps + 1);
        let (mut x, mut y) = (ego.x_m, ego.y_m);
        for i in 0..=self.steps {
            let frac = i as f64 / self.steps.max(1) as f64;
            let v = v0 + (v1 - v0) * frac;
            trajectory.push(TrajectoryPoint {
                pose: Pose {
                    x_m: x,
                    y_m: y,
                    heading_rad: heading,
                },
                velocity_mps: v,
                time_from_start_s: i as f64 * self.dt_s,
            });
            x += v * self.dt_s * heading.cos();
            y += v * self.dt_s * heading.sin();
        }
        PlanOutput {
            trajectory,
            kind: kirra_planner::ProposalKind::Motion,
        }
    }
}

fn world<'a>(corr: &'a MockCorridorSource, posture: FleetPosture, speed: f64) -> PlanInput<'a> {
    PlanInput {
        ego: EgoState {
            pose: Pose {
                x_m: 10.0,
                y_m: 0.0,
                heading_rad: 0.0,
            },
            linear_x_mps: speed,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        goal: Goal {
            target: Pose {
                x_m: 60.0,
                y_m: 0.0,
                heading_rad: 0.0,
            },
        },
        map: corr,
        objects: &[],
        controls: &[],
        lane_boundaries: &[],
        motion: &[],
        predicted_paths: &[],
        cedes_to_ego_ids: &[],
        lane_change_to_m: None,
        no_overtake_ids: &[],
        drivable: None,
        posture,
        target_speed_mps: None,
        request_overtake: false,
        request_pull_over: false,
        lane_graph: None,
        signal_states: &[],
    }
}

/// The checker's verdict on a grounded proposal, with the #893 narration.
fn check(
    trajectory: &[TrajectoryPoint],
    corr: &MockCorridorSource,
    posture: FleetPosture,
) -> (TrajectoryVerdict, Option<TrajectoryRefusalReason>) {
    validate_trajectory_slow_explained(
        trajectory,
        corr,
        &[],
        &VehicleConfig::default_urban(),
        None,
        posture,
        None,
        None,
        None,
        None,
        FrameTrust::Trusted,
    )
}

/// The governed release stage: a token is minted ONLY for an ADMITTED verdict
/// — the deny path never mints (the pinned verifier invariant; tier1's case
/// (e) models exactly this). The refused proposal's bytes may still appear on
/// the bus (a rogue re-publisher), which is why the chokepoint, not this
/// function, is the enforcement.
fn governed_release(
    verdict: TrajectoryVerdict,
    payload: &RosTwistPayload,
    sk: &SigningKey,
) -> Option<ReleaseToken> {
    matches!(
        verdict,
        TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp
    )
    .then(|| issue_ros_release(payload, sk))
}

/// The first motion segment of a proposal as the twist the doer would emit.
fn first_twist(trajectory: &[TrajectoryPoint]) -> f64 {
    trajectory.get(1).map_or(0.0, |p| p.velocity_mps)
}

/// **The demo.** Mick (the LLM) asks to cut across the grass; the literal
/// doer tries; KIRRA refuses with the SPECIFIC corridor-departure sentence;
/// no bytes reach the serial seam. Then the positive control: an in-corridor
/// request is admitted, released, and becomes the ONLY write — proving the
/// refusal path isn't vacuous (a dead loop would also never write).
#[test]
fn mick_unsafe_intent_is_refused_narrated_and_nothing_reaches_the_motors() {
    let sk = SigningKey::from_bytes(&DRILL_SEED);
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let mut consumer = consumer();

    // ---- Phase 1: the unsafe request -----------------------------------
    // Raw LLM output, Gemma framing and all — through the ONE parse.
    let raw = "Sure! Cutting across to the kiosk now.\n```json\n\
               {\"intent\":\"go_to\",\"x_m\":30.0,\"y_m\":12.0}\n```";
    let intent = MickIntent::from_llm_json(raw).expect("well-formed intent parses");
    assert_eq!(
        intent,
        MickIntent::GoTo {
            x_m: 30.0,
            y_m: 12.0
        }
    );

    // The literal doer grounds it through the REAL seam (plan_for_intent):
    // straight at (30, 12) — out the side of the ±5 m corridor.
    let w = world(&corr, FleetPosture::Nominal, 2.0);
    let mut doer = LiteralDoer {
        speed_mps: 2.0,
        dt_s: 0.5,
        steps: 15,
    };
    let plan = plan_for_intent(&mut doer, &intent, &w);
    assert!(
        plan.trajectory.iter().any(|p| p.pose.y_m > 5.0),
        "the literal doer must actually leave the corridor for the demo to bite"
    );

    // The checker refuses — and NARRATES.
    let (verdict, reason) = check(&plan.trajectory, &corr, FleetPosture::Nominal);
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback);
    let reason = reason.expect("a refusal carries its reason (#893)");
    assert_eq!(reason, TrajectoryRefusalReason::ContainmentBreach);
    assert_eq!(reason.code(), "TRAJECTORY_CONTAINMENT_BREACH");
    let sentence = reason.explain();
    assert!(
        sentence.contains("corridor"),
        "the narration names the actual violation: {sentence}"
    );
    assert!(
        !sentence.contains(EXPLAIN_UNKNOWN_MARKER),
        "the narration must be the SPECIFIC explanation, not the generic fallback"
    );

    // Deny path never mints: the refused proposal goes to the bus token-less
    // (tier1 case (e)); the chokepoint refuses it and writes NOTHING.
    let refused_payload = RosTwistPayload {
        sequence: 1,
        issued_at_ms: T_MINT,
        linear_mps: first_twist(&plan.trajectory),
        angular_rad_s: 0.0,
    };
    assert!(
        governed_release(verdict, &refused_payload, &sk).is_none(),
        "a refused verdict must never mint a release token"
    );
    assert!(matches!(
        consumer.on_frame(&refused_payload.encode(), None, T_VERIFY),
        FrameOutcome::Refused(RosReleaseRefusal::NoToken)
    ));
    assert!(
        consumer.serial().writes.is_empty(),
        "the refused command must not reach the serial seam"
    );

    // ---- Phase 2: the positive control ----------------------------------
    // A sane request down the corridor is admitted end-to-end — the loop is
    // alive, and the ONLY write is the admitted, signed, enforced twist.
    let raw_ok = r#"{"intent":"go_to","x_m":60.0,"y_m":0.0}"#;
    let intent_ok = MickIntent::from_llm_json(raw_ok).expect("parses");
    let mut doer_ok = LiteralDoer {
        speed_mps: 2.0,
        dt_s: 0.5,
        steps: 15,
    };
    let plan_ok = plan_for_intent(&mut doer_ok, &intent_ok, &w);
    let (verdict_ok, reason_ok) = check(&plan_ok.trajectory, &corr, FleetPosture::Nominal);
    assert_eq!(verdict_ok, TrajectoryVerdict::Accept, "the control must admit");
    assert_eq!(reason_ok, None, "an admitted proposal carries no refusal reason");

    let admitted_payload = RosTwistPayload {
        sequence: 2,
        issued_at_ms: T_MINT,
        linear_mps: first_twist(&plan_ok.trajectory),
        angular_rad_s: 0.0,
    };
    let token = governed_release(verdict_ok, &admitted_payload, &sk)
        .expect("an admitted verdict mints the release");
    assert!(matches!(
        consumer.on_frame(&admitted_payload.encode(), Some(&token), T_VERIFY),
        FrameOutcome::Released { sequence: 2 }
    ));

    // THE assertion (the tier1 shape): exactly one write ever occurred, and
    // it is the ADMITTED twist — the refused intent contributed zero bytes.
    assert_eq!(
        consumer.serial().writes.as_slice(),
        &[(admitted_payload.linear_mps, 0.0)],
        "exactly one serial write: the admitted, signed, enforced twist"
    );

    // The demo's sentence, for the humans running -- --nocapture:
    println!(
        "MICK asked: go_to(30, 12) — across the corridor edge.\n\
         OCCY (literal doer) proposed it. KIRRA refused [{}]:\n  \"{}\"\n\
         MOTORS saw: {} write(s) from the refused intent, 1 from the admitted one.",
        reason.code(),
        sentence,
        0
    );
}

/// The Degraded margin scenario, LLM-authored — mirrors
/// `checker_catches_reacceleration_in_degraded`: Mick asks to SPEED UP while
/// the fleet is Degraded; the literal doer obeys (2.0 → 2.1 m/s, the subtle
/// 5% drift, not an obvious jump); the checker's #70 non-increasing gate
/// refuses with the Degraded-specific deny code; nothing reaches the motors.
#[test]
fn mick_reacceleration_request_in_degraded_is_refused_with_the_specific_reason() {
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let mut consumer = consumer();

    let raw = r#"{"intent":"cruise","target_speed_mps":2.1}"#;
    let intent = MickIntent::from_llm_json(raw).expect("parses");

    // Ego currently at 2.0 m/s, fleet Degraded. plan_for_intent hands the
    // requested speed through; the LITERAL doer ramps to it verbatim.
    let w = world(&corr, FleetPosture::Degraded, 2.0);
    let mut doer = LiteralDoer {
        speed_mps: 2.0,
        dt_s: 0.1,
        steps: 1,
    };
    let plan = plan_for_intent(&mut doer, &intent, &w);
    assert!(
        plan.trajectory.iter().any(|p| p.velocity_mps > 2.0),
        "the literal doer must actually re-accelerate for the margin to be tested"
    );

    let (verdict, reason) = check(&plan.trajectory, &corr, FleetPosture::Degraded);
    assert_eq!(
        verdict,
        TrajectoryVerdict::MRCFallback,
        "even a marginal re-acceleration in Degraded must hard-reject"
    );
    let reason = reason.expect("narrated");
    let TrajectoryRefusalReason::KinematicsDenied(code) = reason else {
        panic!("expected the per-pose kinematics denial, got {reason:?}");
    };
    assert!(
        code.reason().starts_with("DEGRADED"),
        "the carried deny code names the Degraded gate: {}",
        code.reason()
    );
    assert!(!reason.explain().contains(EXPLAIN_UNKNOWN_MARKER));

    // Refused → token-less on the bus → the chokepoint writes nothing.
    let payload = RosTwistPayload {
        sequence: 1,
        issued_at_ms: T_MINT,
        linear_mps: 2.1,
        angular_rad_s: 0.0,
    };
    assert!(matches!(
        consumer.on_frame(&payload.encode(), None, T_VERIFY),
        FrameOutcome::Refused(RosReleaseRefusal::NoToken)
    ));
    assert!(consumer.serial().writes.is_empty());
}

/// Part 2.4 at the actuation boundary: unparseable LLM output produces NO
/// intent, hence NO plan, NO frame — and the consumer's starve path is
/// SILENCE (never-released means nothing ever moves), not a default goal or
/// a "proceed cautiously" crawl.
#[test]
fn unparseable_llm_output_holds_no_frame_no_motion() {
    let err = MickIntent::from_llm_json("just floor it, trust me")
        .expect_err("garbage must fail the parse");
    assert_eq!(err, "MICK_JSON_PARSE_ERROR");
    // Non-finite and unknown-tag variants fail the same way (fail-closed,
    // pinned in kirra-planner; re-asserted here at the loop boundary).
    assert!(MickIntent::from_llm_json(r#"{"intent":"go_to","x_m":1e999,"y_m":0}"#).is_err());
    assert!(MickIntent::from_llm_json(r#"{"intent":"warp_speed"}"#).is_err());

    // With no intent there is nothing to ground or emit. The consumer,
    // never having released, stays SILENT through the liveness sweep.
    let mut consumer = consumer();
    for k in 0..40u64 {
        consumer.on_tick(T_VERIFY + k * 100);
    }
    assert!(
        consumer.serial().writes.is_empty(),
        "no intent → no frame → no motion: silence is the never-released safe state"
    );
    assert_eq!(consumer.release_count(), 0);
}
