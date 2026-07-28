//! FDIT matrix for the simulated MCU (`kirra_r2cp::sim`).
//!
//! Each fault row asserts BOTH halves: the injected/violating case is refused
//! AND the same case without the fault is accepted. A refusal test that would
//! pass against a peer which refuses everything proves nothing, so every row
//! carries its own positive control.
//!
//! Scope note, restated because it is easy to lose: passing here is evidence
//! about the BRIDGE, not about the firmware. The sim mirrors the firmware's
//! protocol-visible rules; HIL is what tests the firmware.

use kirra_r2cp::sim::{
    encode_with_raw_flags, feed, inject, InjectedFault, MotionCommand, Outcome, Refusal,
    SafetyState, SimulatedMcu, MAX_VALID_FOR_US, MIN_VALID_FOR_US, MODE_HOLD_ZERO, MODE_STOP,
    MODE_TRACK,
};
use kirra_r2cp::{decode, encode, DecodeError, Frame, MessageType, FLAG_AUTH_TAG};

const VALID_FOR: u32 = 100_000; // 100 ms, inside the configured 20..1000 ms band

fn motion(seq: u32, t_us: u64, v: f32, k: f32) -> Frame {
    motion_mode(seq, t_us, v, k, MODE_TRACK, VALID_FOR)
}

fn motion_mode(seq: u32, t_us: u64, v: f32, k: f32, mode: u8, valid_for_us: u32) -> Frame {
    let cmd = MotionCommand {
        command_id: seq,
        valid_for_us,
        velocity_mps: v,
        curvature_per_m: k,
        acceleration_limit_mps2: 1.0,
        jerk_limit_mps3: 5.0,
        mode,
    };
    Frame::new(MessageType::MotionCommand, seq, t_us).with_payload(&cmd.encode())
}

fn ctrl(ty: MessageType, seq: u32) -> Frame {
    Frame::new(ty, seq, 0)
}

/// Drive the machine to `Active` using sequences 1 and 2.
fn armed_active() -> SimulatedMcu {
    let mut mcu = SimulatedMcu::new();
    assert_eq!(
        mcu.handle(&ctrl(MessageType::Arm, 1), 0),
        Outcome::StateChanged(SafetyState::Armed)
    );
    assert_eq!(
        mcu.handle(&ctrl(MessageType::Activate, 2), 0),
        Outcome::StateChanged(SafetyState::Active)
    );
    mcu
}

// ---------------------------------------------------------------- state gate

#[test]
fn motion_is_refused_outside_active_and_accepted_inside_it() {
    // Negative: Standby.
    let mut mcu = SimulatedMcu::new();
    assert_eq!(
        mcu.handle(&motion(1, 0, 0.4, 0.0), 0),
        Outcome::Refused(Refusal::NotActive)
    );

    // Negative: Armed is not enough either.
    let mut mcu = SimulatedMcu::new();
    mcu.handle(&ctrl(MessageType::Arm, 1), 0);
    assert_eq!(
        mcu.handle(&motion(2, 0, 0.4, 0.0), 0),
        Outcome::Refused(Refusal::NotActive)
    );

    // Positive control: the identical command in Active is honoured.
    let mut mcu = armed_active();
    assert_eq!(
        mcu.handle(&motion(3, 0, 0.4, 0.0), 0),
        Outcome::Accepted {
            velocity_mps: 0.4,
            curvature_per_m: 0.0
        }
    );
}

#[test]
fn disarm_from_active_requests_a_controlled_stop_not_a_jump_to_standby() {
    let mut mcu = armed_active();
    assert_eq!(
        mcu.handle(&ctrl(MessageType::Disarm, 3), 0),
        Outcome::StateChanged(SafetyState::ControlledStop)
    );
    assert_eq!(mcu.state, SafetyState::ControlledStop);

    // Positive control: from Armed (not moving) DISARM does go to Standby, so
    // the assertion above is about the Active path specifically.
    let mut mcu = SimulatedMcu::new();
    mcu.handle(&ctrl(MessageType::Arm, 1), 0);
    assert_eq!(
        mcu.handle(&ctrl(MessageType::Disarm, 2), 0),
        Outcome::StateChanged(SafetyState::Standby)
    );
}

// -------------------------------------------------------------------- replay

#[test]
fn sequence_less_than_or_equal_to_the_watermark_is_a_replay() {
    let mut mcu = armed_active(); // watermark now 2
    assert_eq!(
        mcu.handle(&motion(3, 0, 0.4, 0.0), 0),
        Outcome::Accepted {
            velocity_mps: 0.4,
            curvature_per_m: 0.0
        }
    );

    // EQUAL is a replay, not a harmless duplicate — same rule as the QNX judge.
    assert_eq!(
        mcu.handle(&motion(3, 0, 0.4, 0.0), 0),
        Outcome::Refused(Refusal::Replay)
    );
    // And strictly older.
    assert_eq!(
        mcu.handle(&motion(2, 0, 0.4, 0.0), 0),
        Outcome::Refused(Refusal::Replay)
    );
    // Positive control: the next sequence still works, so the watermark is a
    // watermark and not a latch.
    assert_eq!(
        mcu.handle(&motion(4, 0, 0.4, 0.0), 0),
        Outcome::Accepted {
            velocity_mps: 0.4,
            curvature_per_m: 0.0
        }
    );
}

#[test]
fn a_replayed_arm_cannot_move_the_machine() {
    let mut mcu = SimulatedMcu::new();
    mcu.handle(&ctrl(MessageType::Arm, 5), 0);
    mcu.handle(&ctrl(MessageType::Disarm, 6), 0);
    assert_eq!(mcu.state, SafetyState::Standby);

    // Replaying the captured ARM frame must not re-arm.
    assert_eq!(
        mcu.handle(&ctrl(MessageType::Arm, 5), 0),
        Outcome::Refused(Refusal::Replay)
    );
    assert_eq!(mcu.state, SafetyState::Standby);
}

#[test]
fn a_refused_frame_does_not_consume_a_sequence_number() {
    let mut mcu = SimulatedMcu::new(); // Standby: motion will be refused
    assert_eq!(
        mcu.handle(&motion(7, 0, 0.4, 0.0), 0),
        Outcome::Refused(Refusal::NotActive)
    );
    assert_eq!(
        mcu.last_accepted_sequence, None,
        "a refusal must not advance the watermark, or garbage could burn the space"
    );
    // The bridge may legitimately reuse 7 for the frame it should have sent.
    assert_eq!(
        mcu.handle(&ctrl(MessageType::Arm, 7), 0),
        Outcome::StateChanged(SafetyState::Armed)
    );
    assert_eq!(mcu.last_accepted_sequence, Some(7));
}

// ----------------------------------------------------------------- freshness

#[test]
fn a_command_older_than_its_own_validity_window_is_stale() {
    let mut mcu = armed_active();
    // age == valid_for is still fresh (boundary), age > valid_for is not.
    assert!(matches!(
        mcu.handle(&motion(3, 0, 0.4, 0.0), u64::from(VALID_FOR)),
        Outcome::Accepted { .. }
    ));
    assert_eq!(
        mcu.handle(&motion(4, 0, 0.4, 0.0), u64::from(VALID_FOR) + 1),
        Outcome::Refused(Refusal::Stale)
    );
}

#[test]
fn a_stale_command_cannot_feed_the_watchdog() {
    let mut mcu = armed_active();
    // One fresh command at t=0 arms the watchdog for VALID_FOR µs.
    assert!(matches!(
        mcu.handle(&motion(3, 0, 0.4, 0.0), 0),
        Outcome::Accepted { .. }
    ));

    // A stale command arriving inside the window must be refused AND must not
    // refresh liveness — otherwise a replayed-but-old stream keeps the robot
    // moving on evidence that is by definition out of date.
    let t = u64::from(VALID_FOR) / 2;
    assert_eq!(
        mcu.handle(&motion(4, 0, 0.4, 0.0), t + u64::from(VALID_FOR) + 1),
        Outcome::Refused(Refusal::Stale)
    );

    // The watchdog therefore still fires on the ORIGINAL command's window.
    assert_eq!(
        mcu.tick(u64::from(VALID_FOR) + 1),
        Some(SafetyState::ControlledStop)
    );
}

#[test]
fn the_watchdog_does_not_fire_while_commands_are_fresh() {
    let mut mcu = armed_active();
    let mut t = 0u64;
    for seq in 3..20u32 {
        assert!(matches!(
            mcu.handle(&motion(seq, t, 0.4, 0.0), t),
            Outcome::Accepted { .. }
        ));
        t += u64::from(VALID_FOR) / 2;
        assert_eq!(
            mcu.tick(t),
            None,
            "watchdog fired inside the command window"
        );
    }
    assert_eq!(mcu.state, SafetyState::Active);

    // Positive control: stop feeding it and it stops the robot.
    t += u64::from(VALID_FOR) * 2;
    assert_eq!(mcu.tick(t), Some(SafetyState::ControlledStop));
}

// ------------------------------------------------------------------ payloads

#[test]
fn non_finite_command_values_are_refused_not_clamped() {
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut mcu = armed_active();
        assert_eq!(
            mcu.handle(&motion(3, 0, bad, 0.0), 0),
            Outcome::Refused(Refusal::MalformedPayload),
            "non-finite velocity {bad} must be refused"
        );
        let mut mcu = armed_active();
        assert_eq!(
            mcu.handle(&motion(3, 0, 0.4, bad), 0),
            Outcome::Refused(Refusal::MalformedPayload),
            "non-finite curvature {bad} must be refused"
        );
    }
    // Positive control: finite values through the same path are accepted.
    let mut mcu = armed_active();
    assert!(matches!(
        mcu.handle(&motion(3, 0, 0.4, 0.1), 0),
        Outcome::Accepted { .. }
    ));
}

#[test]
fn validity_window_outside_the_configured_band_is_refused() {
    for bad in [0, MIN_VALID_FOR_US - 1, MAX_VALID_FOR_US + 1, u32::MAX] {
        let mut mcu = armed_active();
        assert_eq!(
            mcu.handle(&motion_mode(3, 0, 0.4, 0.0, MODE_TRACK, bad), 0),
            Outcome::Refused(Refusal::MalformedPayload),
            "valid_for_us={bad} is outside the configured band"
        );
    }
    // Positive control: both boundaries are INSIDE the band.
    for ok in [MIN_VALID_FOR_US, MAX_VALID_FOR_US] {
        let mut mcu = armed_active();
        assert!(
            matches!(
                mcu.handle(&motion_mode(3, 0, 0.4, 0.0, MODE_TRACK, ok), 0),
                Outcome::Accepted { .. }
            ),
            "valid_for_us={ok} is a legal boundary and must be accepted"
        );
    }
}

#[test]
fn a_short_payload_is_refused() {
    let mut mcu = armed_active();
    let short = Frame::new(MessageType::MotionCommand, 3, 0).with_payload(&[0u8; 27]);
    assert_eq!(
        mcu.handle(&short, 0),
        Outcome::Refused(Refusal::MalformedPayload)
    );
    // Positive control: 28 bytes of the same shape parse.
    assert!(matches!(
        mcu.handle(&motion(4, 0, 0.4, 0.0), 0),
        Outcome::Accepted { .. }
    ));
}

#[test]
fn stop_and_hold_zero_modes_command_no_motion_whatever_the_payload_says() {
    for mode in [MODE_STOP, MODE_HOLD_ZERO] {
        let mut mcu = armed_active();
        assert_eq!(
            mcu.handle(&motion_mode(3, 0, 9.9, 9.9, mode, VALID_FOR), 0),
            Outcome::Accepted {
                velocity_mps: 0.0,
                curvature_per_m: 0.0
            },
            "mode {mode} must zero the commanded motion"
        );
    }
    // Positive control: MODE_TRACK passes the same numbers through, so the
    // zeroing above is the mode's doing and not a blanket zero.
    let mut mcu = armed_active();
    assert_eq!(
        mcu.handle(&motion_mode(3, 0, 9.9, 9.9, MODE_TRACK, VALID_FOR), 0),
        Outcome::Accepted {
            velocity_mps: 9.9,
            curvature_per_m: 9.9
        }
    );
}

#[test]
fn motion_command_round_trips_through_its_own_encoding() {
    let cmd = MotionCommand {
        command_id: 42,
        valid_for_us: VALID_FOR,
        velocity_mps: -0.375,
        curvature_per_m: 0.125,
        acceleration_limit_mps2: 1.5,
        jerk_limit_mps3: 4.25,
        mode: MODE_TRACK,
    };
    assert_eq!(MotionCommand::parse(&cmd.encode()), Ok(cmd));
}

// -------------------------------------------------------------- latched fault

#[test]
fn a_latched_fault_refuses_motion_and_arming_until_physically_acknowledged() {
    let mut mcu = armed_active();
    mcu.raise_latched_fault();

    assert_eq!(
        mcu.handle(&motion(3, 0, 0.4, 0.0), 0),
        Outcome::Refused(Refusal::FaultLatched)
    );
    assert_eq!(
        mcu.handle(&ctrl(MessageType::Arm, 4), 0),
        Outcome::Refused(Refusal::FaultLatched)
    );

    // A network packet is not physical acknowledgement.
    assert_eq!(
        mcu.handle(&ctrl(MessageType::AcknowledgeFault, 5), 0),
        Outcome::Refused(Refusal::FaultLatched)
    );
    assert_eq!(mcu.state, SafetyState::FaultLatched);

    // Positive control: with the local input asserted, the same frame clears it.
    mcu.physical_ack_asserted = true;
    assert_eq!(
        mcu.handle(&ctrl(MessageType::AcknowledgeFault, 6), 0),
        Outcome::StateChanged(SafetyState::Standby)
    );
}

// ------------------------------------------------------------ message catalogue

#[test]
fn a_message_the_mcu_does_not_accept_from_the_host_is_refused() {
    // Telemetry types flow MCU→host; receiving one is a protocol error.
    for ty in [
        MessageType::Odometry,
        MessageType::RobotState,
        MessageType::Battery,
        MessageType::FaultEvent,
    ] {
        let mut mcu = armed_active();
        assert_eq!(
            mcu.handle(&ctrl(ty, 3), 0),
            Outcome::Refused(Refusal::UnexpectedType),
            "{ty:?} must not be accepted from the host"
        );
    }
    // Positive control: HELLO on the same path is ignored, not refused.
    let mut mcu = armed_active();
    assert_eq!(
        mcu.handle(&ctrl(MessageType::Hello, 3), 0),
        Outcome::Ignored
    );
}

// -------------------------------------------------------------- wire faults

#[test]
fn injected_wire_faults_are_refused_and_leave_the_machine_untouched() {
    let good = encode(&motion(3, 0, 0.4, 0.0)).expect("encodes");

    // Positive control FIRST, so the negatives below are known to be about the
    // fault and not about a frame that never worked.
    let mut mcu = armed_active();
    let out = feed(&mut mcu, &good, 0);
    assert_eq!(out.len(), 1);
    assert!(matches!(out[0], Ok(Outcome::Accepted { .. })));

    for fault in [
        InjectedFault::Truncate,
        InjectedFault::CorruptCrc,
        InjectedFault::CorruptPayload,
        InjectedFault::Oversize,
    ] {
        let mut mcu = armed_active();
        let before = (mcu.state, mcu.last_accepted_sequence, mcu.accepted_commands);
        let bytes = inject(&good, fault);
        for r in feed(&mut mcu, &bytes, 0) {
            assert!(
                r.is_err(),
                "{fault:?} must not decode into an outcome: {r:?}"
            );
        }
        assert_eq!(
            (mcu.state, mcu.last_accepted_sequence, mcu.accepted_commands),
            before,
            "{fault:?} changed MCU state"
        );
    }
}

#[test]
fn a_crc_bit_flip_is_reported_as_a_crc_mismatch() {
    // The generic fault loop above accepts any error; this pins the mechanism —
    // a flipped body bit must be caught by the CRC specifically, so the CRC is
    // demonstrably load-bearing rather than incidentally redundant.
    let good = encode(&motion(3, 0, 0.4, 0.0)).expect("encodes");
    let mut bad = good.clone();
    let idx = good.len() - 2; // last body byte, before the delimiter
    bad[idx] ^= 0x01;
    assert_ne!(
        bad[idx], 0,
        "must stay a legal COBS byte so the CRC is the gate"
    );
    assert_eq!(decode(&bad[..bad.len() - 1]), Err(DecodeError::CrcMismatch));
    assert!(decode(&good[..good.len() - 1]).is_ok());
}

#[test]
fn a_reserved_flag_bit_is_refused() {
    let frame = motion(3, 0, 0.4, 0.0);
    for reserved in [0x10u8, 0x20, 0x40, 0x80] {
        let bytes = encode_with_raw_flags(&frame, reserved).expect("encodes");
        let mut mcu = armed_active();
        for r in feed(&mut mcu, &bytes, 0) {
            assert_eq!(r, Err(DecodeError::InvalidFlags), "flag {reserved:#04x}");
        }
        assert_eq!(mcu.accepted_commands, 0);
    }
    // Positive control: the same builder with a DEFINED flag decodes fine, so
    // the refusals above are about the reserved bits, not about the builder.
    let bytes = encode_with_raw_flags(&frame, kirra_r2cp::FLAG_ACK_REQUIRED).expect("encodes");
    let mut mcu = armed_active();
    assert!(matches!(
        feed(&mut mcu, &bytes, 0)[0],
        Ok(Outcome::Accepted { .. })
    ));
}

#[test]
fn an_auth_tagged_frame_is_refused_by_the_unauthenticated_path() {
    let mut frame = motion(3, 0, 0.4, 0.0);
    frame.flags = FLAG_AUTH_TAG;
    let bytes = encode(&frame).expect("AUTH_TAG is a defined flag, so it encodes");
    let mut mcu = armed_active();
    for r in feed(&mut mcu, &bytes, 0) {
        assert_eq!(r, Err(DecodeError::AuthRequired));
    }
    assert_eq!(
        mcu.accepted_commands, 0,
        "a tagged frame must never actuate"
    );
}

#[test]
fn resynchronisation_after_a_corrupt_frame_costs_exactly_one_frame() {
    let mut mcu = armed_active();
    let mut stream = Vec::new();
    stream.extend_from_slice(&encode(&motion(3, 0, 0.1, 0.0)).unwrap());
    stream.extend_from_slice(&inject(
        &encode(&motion(4, 0, 0.2, 0.0)).unwrap(),
        InjectedFault::CorruptCrc,
    ));
    stream.extend_from_slice(&encode(&motion(5, 0, 0.3, 0.0)).unwrap());

    let out = feed(&mut mcu, &stream, 0);
    assert_eq!(out.len(), 3);
    assert!(matches!(out[0], Ok(Outcome::Accepted { .. })));
    assert!(out[1].is_err(), "the corrupt frame is lost");
    assert!(
        matches!(out[2], Ok(Outcome::Accepted { .. })),
        "the frame AFTER the corrupt one must survive — resync is bounded to one delimiter"
    );
    assert_eq!(mcu.accepted_commands, 2);
    assert_eq!(mcu.last_accepted_sequence, Some(5));
}

#[test]
fn an_unterminated_tail_is_held_back_rather_than_decoded_early() {
    let good = encode(&motion(3, 0, 0.4, 0.0)).unwrap();
    let mut stream = good.clone();
    stream.extend_from_slice(&good[..good.len() - 4]); // partial second frame

    let mut mcu = armed_active();
    let out = feed(&mut mcu, &stream, 0);
    assert_eq!(out.len(), 1, "only the terminated frame is a frame");
    assert!(matches!(out[0], Ok(Outcome::Accepted { .. })));
}
