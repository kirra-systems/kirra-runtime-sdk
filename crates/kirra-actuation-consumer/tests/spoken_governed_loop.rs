//! **The governed loop, SPOKEN** — the KITT demo as a CI artifact: *you say
//! something unsafe, and the car tells you — out loud — why it refused.*
//!
//! Extends `mick_governed_loop.rs`'s shape with the speech shell wrapped
//! around the SAME loop:
//!
//!   WAV fixture ─▶ speech::speech_turn (validate → STT seam → transcript)
//!               ─▶ the EXISTING intent door: IntentService::handle_text
//!                  (crates/kirra-sidecars/src/mick.rs — LlmBrain::
//!                  decide_request → MickIntent::parse_llm_json, the ONE
//!                  fail-closed parse; the same function mick_service's
//!                  POST /intent calls)
//!               ─▶ plan_for_intent (literal doer) ─▶ the REAL checker
//!               ─▶ governed release (deny-never-mints) ─▶ MotorConsumer
//!                  chokepoint ─▶ RecordingSerial
//!   verdict reason ─▶ speech::narration_sentence (#893 table text VERBATIM)
//!                  ─▶ the Speaker sink (recorded in-test; Piper in the demo)
//!
//! 🔴 No-bypass, structurally exercised: the speech layer's ONLY loop-facing
//! act is handing the transcript `&str` to the publish closure — which here
//! IS the production door core. A garbled transcription fails exactly as
//! typed garbage does (422-class refusal, latch untouched, zero motion).
//!
//! CI vs manual (the Ollama/MockModel precedent, kirra-mick/src/lib.rs): CI
//! runs a byte-reproducible generated WAV through the REAL wav gate + a
//! scripted STT seam + a prompt-spying model; the live whisper.cpp / Piper /
//! microphone path is the manual demo (`speech_shell`,
//! docs/testing/SPEECH_KITT_DEMO.md) — external OS processes CI can't carry.
//!
//! Verdict-core honesty: `TrajectoryVerdict` and the frozen kinematics core
//! are UNTOUCHED by this test and by the speech layer (input transduction +
//! output rendering only).

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;
use kirra_actuation_consumer::{
    ConsumerConfig, FrameOutcome, MotorConsumer, MotorSerial, ReleaseToken, RosReleaseRefusal,
    RosTwistPayload, DEFAULT_MISSED_PERIODS,
};
use kirra_core::frame_integrity::FrameTrust;
use kirra_planner::{
    plan_for_intent, EgoState, FleetPosture, Goal, LlmBrain, MickIntent, ModelClient, ModelError,
    PlanInput, PlanOutput, Planner, Pose, TrajectoryPoint,
};
use kirra_release_token::ros_twist::issue_ros_release;
use kirra_sidecars::mick::{IntentRequest, IntentService};
use kirra_sidecars::speech::{
    narration_sentence, pcm16_wav_bytes, speech_turn, utterance_for, Speaker, SpeechTurn,
    Transcriber,
};
use kirra_trajectory::validation::{validate_trajectory_slow_explained, TrajectoryRefusalReason};
use kirra_trajectory::{MockCorridorSource, TrajectoryVerdict, VehicleConfig};

const DRILL_SEED: [u8; 32] = [42u8; 32];
const T_MINT: u64 = 10_000;
const T_VERIFY: u64 = 10_050;

/// verdicts.rs `EXPLAIN_UNKNOWN` marker text — the spoken narration must be
/// the SPECIFIC sentence, never this generic fallback.
const EXPLAIN_UNKNOWN_MARKER: &str = "Unrecognized denial code";

// ---- the actuation seam (tier1 shape) --------------------------------------

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

// ---- the speech seams -------------------------------------------------------

/// Scripted STT: stands in for the external whisper.cpp process (the
/// MockModel-vs-live-Ollama pattern). Records the WAV path it was handed so
/// the test proves the audio plumbing actually reached the seam.
struct ScriptedStt {
    transcript: &'static str,
    heard_wav: RefCell<Option<PathBuf>>,
}
impl ScriptedStt {
    fn new(transcript: &'static str) -> Self {
        Self {
            transcript,
            heard_wav: RefCell::new(None),
        }
    }
}
impl Transcriber for ScriptedStt {
    fn transcribe(&self, wav_path: &Path) -> Result<String, String> {
        *self.heard_wav.borrow_mut() = Some(wav_path.to_path_buf());
        Ok(self.transcript.to_string())
    }
}

/// The TTS sink, recording what would be piped to Piper.
#[derive(Default)]
struct RecordingSpeaker {
    lines: Vec<String>,
}
impl Speaker for RecordingSpeaker {
    fn speak(&mut self, text: &str) -> Result<(), String> {
        self.lines.push(text.to_string());
        Ok(())
    }
}

/// A prompt-spying model: fixed reply, but records the prompt — so the test
/// can prove the SPOKEN words actually reached the LLM through the door's
/// `decide_request` path (not some parallel channel).
struct SpyModel {
    reply: &'static str,
    last_prompt: RefCell<String>,
}
impl SpyModel {
    fn replying(reply: &'static str) -> Self {
        Self {
            reply,
            last_prompt: RefCell::new(String::new()),
        }
    }
}
impl ModelClient for SpyModel {
    fn complete(&self, prompt: &str) -> Result<String, ModelError> {
        *self.last_prompt.borrow_mut() = prompt.to_string();
        Ok(self.reply.to_string())
    }
}

/// A deterministic, byte-reproducible spoken-command fixture: 0.25 s of PCM16
/// audio at 16 kHz (the STT content in CI comes from the scripted seam; the
/// WAV exercises the REAL fail-closed audio gate + byte handoff).
fn fixture_wav(name: &str) -> PathBuf {
    let samples: Vec<i16> = (0..4_000)
        .map(|i| (f64::from(i) * 0.35).sin().mul_add(6000.0, 0.0) as i16)
        .collect();
    let path = std::env::temp_dir().join(format!("kirra_spoken_loop_{name}.wav"));
    std::fs::write(&path, pcm16_wav_bytes(16_000, &samples)).expect("write fixture wav");
    path
}

// ---- the doer + checker (mick_governed_loop shape) --------------------------

/// The LITERAL doer of the independence tests: obeys the intent verbatim —
/// the demo must not rest on Occy being good.
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
        let mut trajectory = Vec::with_capacity(self.steps + 1);
        let (mut x, mut y) = (ego.x_m, ego.y_m);
        for i in 0..=self.steps {
            trajectory.push(TrajectoryPoint {
                pose: Pose {
                    x_m: x,
                    y_m: y,
                    heading_rad: heading,
                },
                velocity_mps: self.speed_mps,
                time_from_start_s: i as f64 * self.dt_s,
            });
            x += self.speed_mps * self.dt_s * heading.cos();
            y += self.speed_mps * self.dt_s * heading.sin();
        }
        PlanOutput {
            trajectory,
            kind: kirra_planner::ProposalKind::Motion,
        }
    }
}

fn world(corr: &MockCorridorSource) -> PlanInput<'_> {
    PlanInput {
        ego: EgoState {
            pose: Pose {
                x_m: 10.0,
                y_m: 0.0,
                heading_rad: 0.0,
            },
            linear_x_mps: 2.0,
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
        posture: FleetPosture::Nominal,
        target_speed_mps: None,
        request_overtake: false,
        request_pull_over: false,
        lane_graph: None,
        signal_states: &[],
    }
}

fn check(
    trajectory: &[TrajectoryPoint],
    corr: &MockCorridorSource,
) -> (TrajectoryVerdict, Option<TrajectoryRefusalReason>) {
    validate_trajectory_slow_explained(
        trajectory,
        corr,
        &[],
        &VehicleConfig::default_urban(),
        None,
        FleetPosture::Nominal,
        None,
        None,
        None,
        None,
        FrameTrust::Trusted,
    )
}

/// Deny-never-mints (the pinned verifier invariant, tier1 case (e)).
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

/// One spoken turn through the REAL door core. The publish closure is the
/// SAME binding `mick_service`'s `POST /intent` route performs
/// (`src/bin/mick_service.rs`: body → `IntentRequest` →
/// `IntentService::handle_text`): text in, ack-or-refusal out. Nothing else
/// crosses.
fn spoken_turn<M: ModelClient>(
    stt: &ScriptedStt,
    wav: &Path,
    door: &mut IntentService<M>,
    now_ms: u64,
) -> SpeechTurn {
    let mut publish = |text: &str| -> Result<String, String> {
        let req = IntentRequest {
            text: text.to_string(),
            context: None,
        };
        match door.handle_text(&req, now_ms) {
            Ok((_, accepted)) => Ok(accepted.to_post_wire()),
            Err(code) => Err(code.to_string()),
        }
    };
    speech_turn(stt, wav, &mut publish).expect("a valid fixture WAV transcribes")
}

/// **The spoken demo.** A spoken unsafe request is refused by the checker,
/// nothing reaches the serial seam, and the SPECIFIC refusal sentence is
/// spoken. Then the positive control: a spoken safe request produces the one
/// and only actuation write — so "it refused" isn't vacuously true.
#[test]
fn spoken_unsafe_intent_is_refused_and_the_reason_is_spoken() {
    let sk = SigningKey::from_bytes(&DRILL_SEED);
    let corr = MockCorridorSource::straight_5m_half_width(100.0);
    let mut consumer = consumer();
    let mut speaker = RecordingSpeaker::default();

    // ---- Phase 1: the spoken unsafe request -----------------------------
    let wav = fixture_wav("unsafe");
    let stt = ScriptedStt::new("cut across the grass to the kiosk");
    let mut door = IntentService::new(LlmBrain::new(SpyModel::replying(
        r#"{"intent":"go_to","x_m":30.0,"y_m":12.0}"#,
    )));

    let turn = spoken_turn(&stt, &wav, &mut door, 10_000);
    // The audio plumbing really ran: the STT seam saw the fixture file.
    assert_eq!(stt.heard_wav.borrow().as_deref(), Some(wav.as_path()));
    let SpeechTurn::Published { transcript, .. } = &turn else {
        panic!("a parseable model reply must publish, got {turn:?}");
    };
    speaker.speak(&utterance_for(&turn)).unwrap();

    // 🔴 The no-bypass proof: the SPOKEN words reached the model through the
    // door's decide_request prompt — the same path typed text takes — and the
    // latched artifact re-parses through the ONE parser to the intent.
    assert!(
        door.last().is_some(),
        "the door latched an intent for the doer"
    );
    let latched = door.last().unwrap().intent_json.clone();
    let intent = MickIntent::from_llm_json(&latched).expect("the one parse accepts its own slice");
    assert_eq!(
        intent,
        MickIntent::GoTo {
            x_m: 30.0,
            y_m: 12.0
        }
    );
    assert!(
        transcript.contains("cut across the grass"),
        "the transcript is the spoken text"
    );

    // The literal doer grounds it; the checker refuses; the reason narrates.
    let w = world(&corr);
    let mut doer = LiteralDoer {
        speed_mps: 2.0,
        dt_s: 0.5,
        steps: 15,
    };
    let plan = plan_for_intent(&mut doer, &intent, &w);
    assert!(
        plan.trajectory.iter().any(|p| p.pose.y_m > 5.0),
        "the literal doer must actually leave the corridor"
    );
    let (verdict, reason) = check(&plan.trajectory, &corr);
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback);
    let reason = reason.expect("a refusal carries its reason (#893)");
    assert_eq!(reason, TrajectoryRefusalReason::ContainmentBreach);

    // Deny path never mints; the token-less frame dies at the chokepoint.
    let refused_payload = RosTwistPayload {
        sequence: 1,
        issued_at_ms: T_MINT,
        linear_mps: 2.0,
        angular_rad_s: 0.0,
    };
    assert!(governed_release(verdict, &refused_payload, &sk).is_none());
    assert!(matches!(
        consumer.on_frame(&refused_payload.encode(), None, T_VERIFY),
        FrameOutcome::Refused(RosReleaseRefusal::NoToken)
    ));
    assert!(
        consumer.serial().writes.is_empty(),
        "the spoken refused command must not reach the serial seam"
    );

    // The refusal is SPOKEN: the #893-shaped narration (deny code + the
    // reviewed table sentence, verbatim) goes to the TTS sink.
    let narration_wire = serde_json::json!({"last": {
        "at_ms": T_VERIFY,
        "action": "DenyBreach",
        "deny_code": reason.code(),
        "explanation": reason.explain(),
    }});
    let spoken = narration_sentence(&narration_wire);
    speaker.speak(&spoken).unwrap();
    assert!(
        spoken.contains("TRAJECTORY_CONTAINMENT_BREACH") && spoken.contains("corridor"),
        "the spoken reason must be the SPECIFIC explanation: {spoken}"
    );
    assert!(
        !spoken.contains(EXPLAIN_UNKNOWN_MARKER),
        "never the generic fallback"
    );
    assert!(
        spoken.contains(reason.explain()),
        "the table sentence is spoken VERBATIM, not paraphrased"
    );

    // ---- Phase 2: the positive control, spoken ---------------------------
    let wav_ok = fixture_wav("safe");
    let stt_ok = ScriptedStt::new("ease forward down the corridor");
    let mut door_ok = IntentService::new(LlmBrain::new(SpyModel::replying(
        r#"{"intent":"go_to","x_m":60.0,"y_m":0.0}"#,
    )));
    let turn_ok = spoken_turn(&stt_ok, &wav_ok, &mut door_ok, 20_000);
    assert!(matches!(turn_ok, SpeechTurn::Published { .. }));
    let intent_ok =
        MickIntent::from_llm_json(&door_ok.last().unwrap().intent_json).expect("parses");

    let mut doer_ok = LiteralDoer {
        speed_mps: 2.0,
        dt_s: 0.5,
        steps: 15,
    };
    let plan_ok = plan_for_intent(&mut doer_ok, &intent_ok, &w);
    let (verdict_ok, reason_ok) = check(&plan_ok.trajectory, &corr);
    assert_eq!(verdict_ok, TrajectoryVerdict::Accept, "the control admits");
    assert_eq!(reason_ok, None);

    let admitted = RosTwistPayload {
        sequence: 2,
        issued_at_ms: T_MINT,
        linear_mps: 2.0,
        angular_rad_s: 0.0,
    };
    let token = governed_release(verdict_ok, &admitted, &sk).expect("admitted verdict mints");
    assert!(matches!(
        consumer.on_frame(&admitted.encode(), Some(&token), T_VERIFY),
        FrameOutcome::Released { sequence: 2 }
    ));

    // THE assertion: exactly one write ever — the spoken ADMITTED command;
    // the spoken refused one contributed zero bytes.
    assert_eq!(
        consumer.serial().writes.as_slice(),
        &[(admitted.linear_mps, 0.0)]
    );

    // And the demo's transcript, for -- --nocapture:
    println!("YOU said: \"cut across the grass to the kiosk\"");
    for line in &speaker.lines {
        println!("CAR said: \"{line}\"");
    }
    let _ = std::fs::remove_file(&wav);
    let _ = std::fs::remove_file(&wav_ok);
}

/// A GARBLED transcription — noise the STT heard as words — dead-ends at the
/// same fail-closed parser typed garbage does: the door refuses, no intent
/// latches, nothing is planned, nothing reaches the motors, and the shell
/// says it is holding. (The prompt's hardest-to-verify case, verified.)
#[test]
fn garbled_transcription_fails_exactly_like_typed_garbage() {
    let wav = fixture_wav("garbled");
    let stt = ScriptedStt::new("krrshh uh the the wind noise");
    // The model, fed nonsense, replies prose — which parse_llm_json refuses.
    let mut door = IntentService::new(LlmBrain::new(SpyModel::replying("just floor it, trust me")));
    let mut speaker = RecordingSpeaker::default();

    let turn = spoken_turn(&stt, &wav, &mut door, 10_000);
    let SpeechTurn::Refused { error, .. } = &turn else {
        panic!("a garbled turn must be Refused, got {turn:?}");
    };
    assert_eq!(error, "MICK_JSON_PARSE_ERROR", "the ONE parser refused");
    assert!(
        door.last().is_none(),
        "no intent latched — the doer has nothing to ground"
    );
    speaker.speak(&utterance_for(&turn)).unwrap();
    assert!(
        speaker.lines[0].contains("Holding"),
        "the hold is spoken: {}",
        speaker.lines[0]
    );

    // Nothing to plan → nothing minted → the never-released consumer stays
    // silent through the liveness sweep.
    let mut consumer = consumer();
    for k in 0..40u64 {
        consumer.on_tick(T_VERIFY + k * 100);
    }
    assert!(consumer.serial().writes.is_empty());
    assert_eq!(consumer.release_count(), 0);
    let _ = std::fs::remove_file(&wav);
}

/// A corrupt/empty audio clip is refused BEFORE any STT or door call — the
/// first gate of the fail-closed chain.
#[test]
fn corrupt_audio_never_reaches_the_intent_door() {
    let bad = std::env::temp_dir().join("kirra_spoken_loop_corrupt.wav");
    std::fs::write(&bad, b"this is not audio").expect("write");
    let stt = ScriptedStt::new("should never be asked");
    let mut called = 0u32;
    let mut publish = |_t: &str| -> Result<String, String> {
        called += 1;
        Ok("{}".into())
    };
    let err = speech_turn(&stt, &bad, &mut publish).expect_err("garbage bytes are refused");
    assert!(err.contains("SPEECH_WAV"), "{err}");
    assert!(stt.heard_wav.borrow().is_none(), "STT was never invoked");
    assert_eq!(called, 0, "the intent door was never called");
    let _ = std::fs::remove_file(&bad);
}
