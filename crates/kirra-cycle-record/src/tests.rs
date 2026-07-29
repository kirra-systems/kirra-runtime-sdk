//! Joiner, ring and trigger tests.
//!
//! The doctrine under test throughout: incomplete context is CLASSIFIED, never
//! guessed, and nothing is silently dropped — an eviction, a duplicate, a
//! chain disagreement all leave visible marks.

use super::*;

fn digest(tag: u8) -> String {
    format!("{:02x}", tag).repeat(32)
}

fn perception(scan: u64) -> PerceptionEvent {
    PerceptionEvent {
        scan_sequence: scan,
        camera_sequence: Some(scan * 10),
        tracker_generation: 1,
        evidence_digest: digest(0xaa),
        raw_speed_cap_mps: 0.9,
        stabilized_speed_cap_mps: 0.8,
        object_count: 3,
        camera_healthy: Some(true),
        t_wall_ms: 1_000 + scan,
        stage_duration_ms: 12,
    }
}

fn proposal(scan: u64) -> ProposalEvent {
    ProposalEvent {
        scan_sequence: scan,
        proposal_digest: digest(0xbb),
        evidence_digest: digest(0xaa),
        verdict: "Accept".to_string(),
        admitted: true,
        reason_code: None,
        t_wall_ms: 1_020 + scan,
        stage_duration_ms: 25,
    }
}

fn release(scan: u64, seq: u64) -> ReleaseEvent {
    ReleaseEvent {
        scan_sequence: scan,
        proposal_digest: digest(0xbb),
        evidence_digest: digest(0xaa),
        release_sequence: seq,
        release_nonce: seq * 7,
        linear_mps: 0.4,
        angular_rad_s: 0.0,
        expires_at_ms: 2_000 + scan,
        t_wall_ms: 1_040 + scan,
        stage_duration_ms: 8,
        gateway_capture_ref: None,
    }
}

fn actuation(seq: u64) -> ActuationEvent {
    ActuationEvent {
        release_sequence: seq,
        release_nonce: seq * 7,
        outcome: ActuationOutcome::Actuated,
        issued_linear_mps: Some(0.4),
        issued_angular_rad_s: Some(0.0),
        t_wall_ms: 1_050,
    }
}

fn full_cycle(scan: u64, seq: u64) -> Vec<StageEvent> {
    vec![
        StageEvent::Perception(perception(scan)),
        StageEvent::Proposal(proposal(scan)),
        StageEvent::Release(release(scan, seq)),
        StageEvent::Actuation(actuation(seq)),
    ]
}

// ---------------------------------------------------------------------------
// Joining
// ---------------------------------------------------------------------------

#[test]
fn a_full_cycle_joins_complete_with_no_anomalies() {
    let recs = join_cycles(&full_cycle(7, 100));
    assert_eq!(recs.len(), 1);
    let rec = &recs[0];
    assert_eq!(rec.scan_sequence, 7);
    assert_eq!(rec.completeness, Completeness::Complete);
    assert!(rec.anomalies.is_empty(), "{:#?}", rec.anomalies);
    assert!(rec.perception.is_some() && rec.proposal.is_some());
    assert!(rec.release.is_some() && rec.actuation.is_some());
}

/// Order independence: the joiner is deterministic over arrival order, because
/// per-process JSONL files concatenated in any order must yield the same
/// artifact.
#[test]
fn joining_is_arrival_order_independent() {
    let mut events = full_cycle(7, 100);
    let forward = join_cycles(&events);
    events.reverse();
    let reversed = join_cycles(&events);
    assert_eq!(forward, reversed);
}

/// The legitimate partial: a refused proposal has no release and no actuation.
/// That is a finding, not a join failure — classified with the missing stages
/// NAMED.
#[test]
fn a_refused_proposal_joins_partial_with_the_missing_stages_named() {
    let mut refused = proposal(9);
    refused.verdict = "MRCFallback".to_string();
    refused.admitted = false;
    refused.reason_code = Some("TRAJECTORY_CORRIDOR_BREACH".to_string());

    let recs = join_cycles(&[
        StageEvent::Perception(perception(9)),
        StageEvent::Proposal(refused),
    ]);
    assert_eq!(recs.len(), 1);
    assert_eq!(
        recs[0].completeness,
        Completeness::Partial {
            missing: vec![Stage::Release, Stage::Actuation]
        }
    );
    assert!(recs[0].anomalies.is_empty(), "a refusal is not an anomaly");
}

/// An evidence-digest disagreement between stages is the exact fault the #1214
/// chain exists to catch. The joiner RECORDS it on the record — repairing it
/// would destroy the evidence, refusing to emit would hide it.
#[test]
fn a_chain_disagreement_is_recorded_never_repaired() {
    let mut p = perception(4);
    p.evidence_digest = digest(0x11); // disagrees with the proposal's 0xaa

    let recs = join_cycles(&[StageEvent::Perception(p), StageEvent::Proposal(proposal(4))]);
    assert_eq!(recs.len(), 1);
    let rec = &recs[0];
    assert!(
        rec.anomalies.iter().any(|a| a.field == "evidence_digest"),
        "expected an evidence_digest anomaly; got {:#?}",
        rec.anomalies
    );
    // Both stages are still present, unrepaired.
    assert_eq!(
        rec.perception.as_ref().unwrap().evidence_digest,
        digest(0x11)
    );
    assert_eq!(rec.proposal.as_ref().unwrap().evidence_digest, digest(0xaa));
}

#[test]
fn a_release_token_digest_mismatch_is_recorded() {
    let mut r = release(4, 50);
    r.proposal_digest = digest(0x22);

    let recs = join_cycles(&[StageEvent::Proposal(proposal(4)), StageEvent::Release(r)]);
    assert!(
        recs[0]
            .anomalies
            .iter()
            .any(|a| a.field == "proposal_digest"),
        "{:#?}",
        recs[0].anomalies
    );
}

/// Duplicates are first-wins with the duplication RECORDED. Silent overwrite
/// would let a later (possibly attacker-shaped, possibly buggy) event replace
/// the one closest in time to the stage it describes.
#[test]
fn a_duplicate_stage_is_kept_first_and_recorded() {
    let mut second = perception(3);
    second.raw_speed_cap_mps = 99.0;

    let recs = join_cycles(&[
        StageEvent::Perception(perception(3)),
        StageEvent::Perception(second),
    ]);
    let rec = &recs[0];
    assert_eq!(
        rec.perception.as_ref().unwrap().raw_speed_cap_mps,
        0.9,
        "first wins"
    );
    assert!(
        rec.anomalies.iter().any(|a| a.field == "perception"),
        "the duplication must be visible: {:#?}",
        rec.anomalies
    );
}

/// The liveness ramp: an actuation with no witnessed release. The consumer
/// acted (a decel step) precisely BECAUSE no release arrived, so demanding a
/// release to join against would erase the most safety-relevant record shape.
#[test]
fn a_liveness_ramp_actuation_becomes_its_own_honest_record() {
    let ramp = ActuationEvent {
        release_sequence: 0,
        release_nonce: 0,
        outcome: ActuationOutcome::LivenessRamp,
        issued_linear_mps: Some(0.1),
        issued_angular_rad_s: Some(0.0),
        t_wall_ms: 5_000,
    };
    let recs = join_cycles(&[StageEvent::Actuation(ramp)]);
    assert_eq!(recs.len(), 1);
    let rec = &recs[0];
    assert_eq!(rec.scan_sequence, 0, "no scan is claimed — honest");
    assert!(rec.actuation.is_some());
    assert!(
        rec.anomalies.iter().any(|a| a.field == "release"),
        "the missing release is stated: {:#?}",
        rec.anomalies
    );
    assert_eq!(rec.triggers(), vec![CaptureTrigger::LivenessRamp]);
}

#[test]
fn multiple_cycles_join_independently_and_in_scan_order() {
    let mut events = full_cycle(20, 200);
    events.extend(full_cycle(10, 100));
    let recs = join_cycles(&events);
    assert_eq!(recs.len(), 2);
    assert_eq!(recs[0].scan_sequence, 10);
    assert_eq!(recs[1].scan_sequence, 20);
    assert!(recs
        .iter()
        .all(|r| r.completeness == Completeness::Complete));
}

// ---------------------------------------------------------------------------
// Triggers
// ---------------------------------------------------------------------------

#[test]
fn a_deny_verdict_and_mrc_entry_are_detected_as_triggers() {
    let mut refused = proposal(9);
    refused.verdict = "MRCFallback".to_string();
    refused.admitted = false;

    let recs = join_cycles(&[StageEvent::Proposal(refused)]);
    let triggers = recs[0].triggers();
    assert!(triggers.contains(&CaptureTrigger::DenyVerdict));
    assert!(triggers.contains(&CaptureTrigger::MrcEntry));
}

#[test]
fn a_token_rejection_is_detected_as_a_trigger() {
    let refused = ActuationEvent {
        release_sequence: 100,
        release_nonce: 700,
        outcome: ActuationOutcome::Refused {
            reason: "SIGNATURE_INVALID".to_string(),
        },
        issued_linear_mps: None,
        issued_angular_rad_s: None,
        t_wall_ms: 1_050,
    };
    let mut events = full_cycle(7, 100);
    events.pop(); // replace the actuated outcome with the refusal
    events.push(StageEvent::Actuation(refused));

    let recs = join_cycles(&events);
    assert!(recs[0].triggers().contains(&CaptureTrigger::TokenRejection));
    // A refusal issued nothing — the record must not claim otherwise.
    assert!(recs[0]
        .actuation
        .as_ref()
        .unwrap()
        .issued_linear_mps
        .is_none());
}

#[test]
fn an_unremarkable_cycle_raises_no_triggers() {
    let recs = join_cycles(&full_cycle(7, 100));
    assert!(recs[0].triggers().is_empty());
}

// ---------------------------------------------------------------------------
// The ring
// ---------------------------------------------------------------------------

#[test]
fn the_ring_is_bounded_and_eviction_is_counted_not_silent() {
    let mut ring = CycleRing::new();
    for scan in 0..(RING_CAPACITY as u64 + 10) {
        assert!(ring
            .push(StageEvent::Perception(perception(scan)))
            .is_none());
    }
    assert_eq!(ring.len(), RING_CAPACITY, "the bound holds");
    assert_eq!(ring.evicted(), 10, "eviction is observable");
}

#[test]
fn a_trigger_seals_after_the_post_event_window() {
    let mut ring = CycleRing::new();
    for scan in 0..50 {
        ring.push(StageEvent::Perception(perception(scan)));
    }
    ring.trigger(CaptureTrigger::DenyVerdict);

    let mut capture = None;
    for scan in 50..(50 + POST_TRIGGER_EVENTS as u64 + 5) {
        if let Some(c) = ring.push(StageEvent::Perception(perception(scan))) {
            capture = Some((scan, c));
            break;
        }
    }
    let (sealed_at, capture) = capture.expect("the window must close");
    assert_eq!(
        sealed_at,
        50 + POST_TRIGGER_EVENTS as u64 - 1,
        "sealed exactly when the post-event budget was spent"
    );
    assert_eq!(capture.trigger, CaptureTrigger::DenyVerdict);
    assert_eq!(capture.events_evicted_before_capture, 0);
    // The capture contains joined cycles from the whole retained window.
    assert!(!capture.cycles.is_empty());
}

/// The earlier trigger wins: its pre-event window is the one closest to the
/// cause, and the later trigger's events are inside the same window anyway.
#[test]
fn an_earlier_pending_trigger_is_not_displaced_by_a_later_one() {
    let mut ring = CycleRing::new();
    ring.push(StageEvent::Perception(perception(1)));
    ring.trigger(CaptureTrigger::StaleFrame);
    ring.trigger(CaptureTrigger::DenyVerdict); // later; must not displace

    let mut sealed = None;
    for scan in 2..(2 + POST_TRIGGER_EVENTS as u64 + 2) {
        if let Some(c) = ring.push(StageEvent::Perception(perception(scan))) {
            sealed = Some(c);
            break;
        }
    }
    assert_eq!(sealed.expect("seals").trigger, CaptureTrigger::StaleFrame);
}

#[test]
fn seal_now_cuts_the_window_short_rather_than_losing_the_capture() {
    let mut ring = CycleRing::new();
    ring.push(StageEvent::Perception(perception(1)));
    ring.trigger(CaptureTrigger::DeadlineMiss);
    let capture = ring.seal_now().expect("pending capture sealed on shutdown");
    assert_eq!(capture.trigger, CaptureTrigger::DeadlineMiss);
    assert!(ring.seal_now().is_none(), "sealing is one-shot");
}

/// The ring bound and the eviction counter compose: a pre-event window longer
/// than the ring is STATED on the capture, per the no-silent-caps rule.
#[test]
fn a_capture_after_eviction_states_what_was_lost() {
    let mut ring = CycleRing::new();
    for scan in 0..(RING_CAPACITY as u64 + 100) {
        ring.push(StageEvent::Perception(perception(scan)));
    }
    ring.trigger(CaptureTrigger::TokenRejection);
    let capture = ring.seal_now().expect("seals");
    assert_eq!(capture.events_evicted_before_capture, 100);
}

// ---------------------------------------------------------------------------
// JSONL wire format
// ---------------------------------------------------------------------------

#[test]
fn events_round_trip_through_jsonl() {
    let events = full_cycle(7, 100);
    let jsonl: String = events
        .iter()
        .map(|e| serde_json::to_string(e).expect("serializes"))
        .collect::<Vec<_>>()
        .join("\n");

    let (parsed, errors) = parse_events_jsonl(&jsonl);
    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(parsed, events);

    // And the joined artifact itself round-trips.
    let recs = join_cycles(&parsed);
    let json = serde_json::to_string(&recs).expect("serializes");
    let back: Vec<JoinedCycleRecord> = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(back, recs);
}

/// Malformed lines are returned WITH LINE NUMBERS, never dropped. A recorder
/// that silently skips evidence is the failure this crate exists to prevent.
#[test]
fn malformed_jsonl_lines_are_reported_with_line_numbers_not_dropped() {
    let good = serde_json::to_string(&StageEvent::Perception(perception(1))).unwrap();
    let jsonl = format!("{good}\nnot json at all\n\n{good}");
    let (parsed, errors) = parse_events_jsonl(&jsonl);
    assert_eq!(parsed.len(), 2);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, 2, "the line number points at the bad line");
}

/// The stage tag makes the wire format self-describing, so a mixed stream from
/// multiple witnesses needs no per-file framing.
#[test]
fn the_stage_tag_discriminates_the_wire_format() {
    let json = serde_json::to_string(&StageEvent::Release(release(1, 10))).unwrap();
    assert!(json.contains("\"stage\":\"release\""), "{json}");
}

// ---------------------------------------------------------------------------
// Chain-consistency replay (option 1)
// ---------------------------------------------------------------------------

mod replay {
    use super::*;
    use crate::consistency::{verify_artifact, verify_record, ConsistencyFinding};

    /// The acceptance property: replay reproduces the classification and every
    /// finding deterministically, without inventing missing stage data.
    #[test]
    fn a_clean_artifact_verifies_clean_and_deterministically() {
        let recs = join_cycles(&full_cycle(7, 100));
        let s1 = verify_artifact(&recs);
        let s2 = verify_artifact(&recs);
        assert_eq!(s1, s2, "replay is deterministic");
        assert!(s1.is_consistent(), "{:#?}", s1.findings);
        assert_eq!((s1.records, s1.complete, s1.partial), (1, 1, 0));
    }

    /// A legitimate partial (refusal → no downstream) verifies CLEAN: absence
    /// caused by an upstream refusal is the expected chronology, not a finding.
    #[test]
    fn a_refusal_partial_is_consistent_not_a_finding() {
        let mut refused = proposal(9);
        refused.admitted = false;
        refused.verdict = "MRCFallback".to_string();
        let recs = join_cycles(&[
            StageEvent::Perception(perception(9)),
            StageEvent::Proposal(refused),
        ]);
        assert!(verify_artifact(&recs).is_consistent());
    }

    /// The single highest-value cross-stage check: the released command must
    /// respect the stabilized cap perception reported. No single stream can
    /// see this — it spans three processes.
    #[test]
    fn a_release_faster_than_the_stabilized_cap_is_found() {
        let mut r = release(7, 100);
        r.linear_mps = 2.0; // cap is 0.8
        let recs = join_cycles(&[
            StageEvent::Perception(perception(7)),
            StageEvent::Proposal(proposal(7)),
            StageEvent::Release(r),
        ]);
        let findings = verify_record(&recs[0]);
        assert!(
            findings.iter().any(|f| matches!(
                f,
                ConsistencyFinding::ReleasedCommandExceedsStabilizedCap { .. }
            )),
            "{findings:#?}"
        );
    }

    /// Admission gates release — a release for an unadmitted proposal is the
    /// chain's core violation.
    #[test]
    fn a_release_without_admission_is_found() {
        let mut refused = proposal(7);
        refused.admitted = false;
        refused.verdict = "MRCFallback".to_string();
        let recs = join_cycles(&[
            StageEvent::Proposal(refused),
            StageEvent::Release(release(7, 100)),
        ]);
        let findings = verify_record(&recs[0]);
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, ConsistencyFinding::ReleaseWithoutAdmission { .. })),
            "{findings:#?}"
        );
    }

    /// "Actuated what was authorized" admits no epsilon — bit compare.
    #[test]
    fn an_actuation_differing_from_the_release_is_found() {
        let mut a = actuation(100);
        a.issued_linear_mps = Some(0.4000001);
        let mut events = full_cycle(7, 100);
        events.pop();
        events.push(StageEvent::Actuation(a));
        let recs = join_cycles(&events);
        let findings = verify_record(&recs[0]);
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, ConsistencyFinding::ActuationDiffersFromRelease { .. })),
            "{findings:#?}"
        );
    }

    /// A refusal that claims issued_* values has invented data.
    #[test]
    fn a_refusal_claiming_an_issued_command_is_found() {
        let a = ActuationEvent {
            release_sequence: 100,
            release_nonce: 700,
            outcome: ActuationOutcome::Refused {
                reason: "SIG".to_string(),
            },
            issued_linear_mps: Some(0.4),
            issued_angular_rad_s: None,
            t_wall_ms: 1_050,
        };
        let mut events = full_cycle(7, 100);
        events.pop();
        events.push(StageEvent::Actuation(a));
        let recs = join_cycles(&events);
        assert!(verify_record(&recs[0])
            .iter()
            .any(|f| matches!(f, ConsistencyFinding::RefusalClaimsIssuedCommand)));
    }

    /// Tamper detection: an artifact whose recorded completeness disagrees
    /// with its own embedded stages is detected, not trusted.
    #[test]
    fn a_hand_edited_completeness_claim_is_detected() {
        let mut recs = join_cycles(&full_cycle(7, 100));
        recs[0].completeness = Completeness::Partial {
            missing: vec![Stage::Release],
        };
        let findings = verify_record(&recs[0]);
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, ConsistencyFinding::CompletenessMisstated { .. })),
            "{findings:#?}"
        );
    }

    /// Tamper detection: stripping a derivable chain anomaly from the artifact
    /// is detected (the recorded set may be larger — join-time-only classes —
    /// but never smaller than what the embedded stages prove).
    #[test]
    fn a_stripped_chain_anomaly_is_detected() {
        let mut p = perception(4);
        p.evidence_digest = "11".repeat(32);
        let mut recs = join_cycles(&[StageEvent::Perception(p), StageEvent::Proposal(proposal(4))]);
        recs[0].anomalies.clear(); // the hand edit
        assert!(verify_record(&recs[0])
            .iter()
            .any(|f| matches!(f, ConsistencyFinding::AnomalySetMisstated { .. })));
    }

    /// Downstream without upstream: an actuation with no release on a GENUINE
    /// scan is a chronology violation; on the sentinel record (liveness ramp)
    /// it is the expected shape and NOT a finding.
    #[test]
    fn downstream_without_upstream_is_found_on_genuine_scans_only() {
        // Genuine scan: release present, proposal absent.
        let recs = join_cycles(&[StageEvent::Release(release(7, 100))]);
        assert!(verify_record(&recs[0])
            .iter()
            .any(|f| matches!(f, ConsistencyFinding::DownstreamWithoutUpstream { .. })));

        // Sentinel record: the liveness ramp's orphan actuation verifies clean.
        let ramp = ActuationEvent {
            release_sequence: 0,
            release_nonce: 0,
            outcome: ActuationOutcome::LivenessRamp,
            issued_linear_mps: Some(0.1),
            issued_angular_rad_s: Some(0.0),
            t_wall_ms: 5_000,
        };
        let recs = join_cycles(&[StageEvent::Actuation(ramp)]);
        assert_eq!(recs[0].scan_sequence, RESERVED_UNBOUND_SCAN);
        assert!(
            verify_record(&recs[0]).is_empty(),
            "the ramp's expected absence is not a finding"
        );
    }

    /// The sentinel is RESERVED in the schema: a producer emitting a genuine
    /// stage event with scan 0 is recorded as an anomaly at join time.
    #[test]
    fn a_genuine_event_claiming_the_sentinel_is_an_anomaly() {
        let recs = join_cycles(&[StageEvent::Perception(perception(0))]);
        assert!(recs
            .iter()
            .any(|r| r.anomalies.iter().any(|a| a.field == "scan_sequence")));
    }

    /// Option-3 composition, additive: a present ref must bind THIS cycle's
    /// digest; an absent ref weakens nothing.
    #[test]
    fn a_capture_ref_binding_the_wrong_digest_is_found_and_absence_is_clean() {
        let mut r = release(7, 100);
        r.gateway_capture_ref = Some(CaptureRecordRef {
            decision_seq: 42,
            proposal_digest: "22".repeat(32), // not this cycle's 0xbb
            source: "captures/session.jsonl".to_string(),
        });
        let mut events = full_cycle(7, 100);
        events[2] = StageEvent::Release(r);
        let recs = join_cycles(&events);
        assert!(verify_record(&recs[0])
            .iter()
            .any(|f| matches!(f, ConsistencyFinding::CaptureRefDigestMismatch { .. })));

        // Absence: the plain full cycle (no ref) is consistent.
        let plain = join_cycles(&full_cycle(7, 100));
        assert!(verify_artifact(&plain).is_consistent());
    }

    /// An artifact round-trips through JSON with the ref present — the
    /// additive field does not break older readers' shape.
    #[test]
    fn the_capture_ref_is_additive_over_the_wire() {
        // A pre-ref artifact (no field) parses into the new schema.
        let old_json = serde_json::to_string(&join_cycles(&full_cycle(7, 100))).unwrap();
        let back: Vec<JoinedCycleRecord> = serde_json::from_str(&old_json).unwrap();
        assert!(back[0]
            .release
            .as_ref()
            .unwrap()
            .gateway_capture_ref
            .is_none());
    }
}
