//! **Tier 5 box 5c.1 — the command surface adds a door, not a decision.**
//!
//! `KIRRA-WM-TIER5-CQRS-001` allows this box to define a sanctioned command
//! surface over the EXISTING domain operations, and forbids one thing:
//!
//! > **No new semantics hidden inside command wrappers.**
//!
//! That is a property about what the wrapper DOES NOT do, and the usual way to
//! test such a property is to read the wrapper and agree it looks harmless.
//! This file does not do that. The control is DIFFERENTIAL: every command is
//! run against one store while the domain operation it wraps is run against a
//! second store from the identical starting state, and the two stores'
//! **chain digests** are compared.
//!
//! `head_chain()` covers the canonical bytes of every event, so the assertion
//! is not "both succeeded" but *"the log the command produced is
//! indistinguishable from the log the domain call produced"*. A wrapper that
//! set a different `writer_class`, stamped its own source, chose a different
//! `valid_from_ms`, or wrapped the wrong `IdentityAdjudication` variant would
//! keep compiling, keep passing a smoke test, and diverge here.
//!
//! Non-vacuity is not assumed: `a_divergent_wrapper_is_caught_by_the_digest`
//! performs the same comparison across a DELIBERATELY different write and
//! asserts the digests differ. Without it, every assertion above would also
//! pass if `head_chain` returned a constant.

use kirra_world::adjudication::{
    ForgetEntity, IdentityAdjudication, Justification, MergeEntities, RetirementReason,
};
use kirra_world::observation::{
    ClockDomain, Confidence, ConfidenceBasis, DomainInstant, SourceClass,
};
use kirra_world::reference::{EntityId, EventId, ObservationId};
use kirra_world::same_as_adjudication::{AdjudicationAuthority, Outcome};
use kirra_world::same_as_candidate::{CandidatePair, MatcherIdentity, SameAsCandidate};
use kirra_world_service::command::{
    AdjudicateSameAs, CommandEngine, ProposeSameAs, RecordForget, RecordMerge,
};
use kirra_world_store::adjudication_record::AdjudicationRow;
use kirra_world_store::candidate_record::CandidateRow;
use kirra_world_store::same_as_adjudication_record::SameAsAdjudicationRequest;
use kirra_world_store::{WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;
const OBS: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const EVT: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB0";
const CAND_OBS: &str = "01ARZ3NDEKTSV4RRFFQ69G5FC1";
const DECIDE_OBS: &str = "01ARZ3NDEKTSV4RRFFQ69G5FD2";
const DECIDE_EVT: &str = "01ARZ3NDEKTSV4RRFFQ69G5FE3";

fn eid(s: &str) -> EntityId {
    EntityId::new(s.to_string()).expect("entity id")
}
fn obs(s: &str) -> ObservationId {
    ObservationId::new(s.to_string()).expect("observation id")
}
fn evt(s: &str) -> EventId {
    EventId::new(s.to_string()).expect("event id")
}
fn just() -> Justification {
    Justification::new([obs(OBS)]).expect("justification")
}
fn at(ms: u64) -> DomainInstant {
    DomainInstant {
        ms,
        domain: ClockDomain::System,
    }
}

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-5c1-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    for s in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(s);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

fn store(name: &str) -> WorldStore {
    WorldStore::open(&tmp(name)).expect("store")
}

fn candidate() -> SameAsCandidate {
    SameAsCandidate::propose(
        CandidatePair::new(eid("track-a"), eid("track-b")).expect("distinct"),
        MatcherIdentity::new("track-matcher", "siamese-v2", "2.3.1").expect("matcher"),
        Confidence::new(Some(0.9), ConfidenceBasis::ModelScore, None).expect("confidence"),
        vec![obs(OBS)],
    )
    .expect("candidate")
}

fn candidate_row<'a>(event: &'a EventId, observation: &'a ObservationId) -> CandidateRow<'a> {
    CandidateRow {
        event_id: event,
        observation_id: observation,
        txn_time_ms: T0,
        valid_from_ms: T0,
        source: "track-matcher",
        source_version: "2.3.1",
    }
}

fn adjudication_row<'a>(event: &'a EventId, observation: &'a ObservationId) -> AdjudicationRow<'a> {
    AdjudicationRow {
        event_id: event,
        observation_id: observation,
        txn_time_ms: T0 + 10,
        valid_from_ms: T0 + 10,
        source: "console-operator",
        source_version: "1.0.0",
        writer_class: WriterClass::Operator,
    }
}

/// `KIRRA-WM-IDENTITY-AUTHORITY-001`: every identity adjudication names an
/// authorized adjudicator, so every fixture here does too.
fn who() -> AdjudicationAuthority {
    AdjudicationAuthority::new(SourceClass::Operator, "console-operator").expect("authority")
}

fn merge() -> MergeEntities {
    MergeEntities::new(
        [eid("track-a")],
        eid("track-b"),
        who(),
        just(),
        at(T0 as u64 + 10),
    )
    .expect("merge")
}

fn forget() -> ForgetEntity {
    ForgetEntity::new(
        eid("track-z"),
        RetirementReason::new("decommissioned").expect("reason"),
        who(),
        just(),
        at(T0 as u64 + 10),
    )
}

// ---------------------------------------------------------------------------
// The differential control, one per wrapped operation.
// ---------------------------------------------------------------------------

#[test]
fn propose_same_as_writes_exactly_what_the_domain_door_writes() {
    let (event, observation) = (evt(EVT), obs(CAND_OBS));

    let mut via_command = store("propose-cmd");
    let mut engine = CommandEngine::new(&mut via_command);
    engine
        .execute(ProposeSameAs {
            row: candidate_row(&event, &observation),
            candidate: candidate(),
        })
        .expect("command");

    let mut direct = store("propose-direct");
    direct
        .append_same_as_candidate(&candidate_row(&event, &observation), &candidate())
        .expect("direct");

    assert_eq!(
        via_command.head_chain().expect("chain"),
        direct.head_chain().expect("chain"),
        "the command wrote a different event than append_same_as_candidate — \
         the wrapper decided something the domain door does not"
    );
}

#[test]
fn record_merge_writes_exactly_what_append_adjudication_writes() {
    let (event, observation) = (evt(DECIDE_EVT), obs(DECIDE_OBS));

    let mut via_command = store("merge-cmd");
    let mut engine = CommandEngine::new(&mut via_command);
    engine
        .execute(RecordMerge {
            row: adjudication_row(&event, &observation),
            merge: merge(),
        })
        .expect("command");

    let mut direct = store("merge-direct");
    direct
        .append_adjudication(
            &adjudication_row(&event, &observation),
            &IdentityAdjudication::Merge(merge()),
        )
        .expect("direct");

    assert_eq!(
        via_command.head_chain().expect("chain"),
        direct.head_chain().expect("chain"),
        "RecordMerge and append_adjudication(Merge) produced different logs"
    );
}

#[test]
fn record_forget_writes_exactly_what_append_adjudication_writes() {
    let (event, observation) = (evt(DECIDE_EVT), obs(DECIDE_OBS));

    let mut via_command = store("forget-cmd");
    let mut engine = CommandEngine::new(&mut via_command);
    engine
        .execute(RecordForget {
            row: adjudication_row(&event, &observation),
            forget: forget(),
        })
        .expect("command");

    let mut direct = store("forget-direct");
    direct
        .append_adjudication(
            &adjudication_row(&event, &observation),
            &IdentityAdjudication::Forget(forget()),
        )
        .expect("direct");

    assert_eq!(
        via_command.head_chain().expect("chain"),
        direct.head_chain().expect("chain"),
        "RecordForget and append_adjudication(Forget) produced different logs"
    );
}

#[test]
fn adjudicate_same_as_writes_exactly_what_the_domain_door_writes() {
    let (cand_event, cand_obs) = (evt(EVT), obs(CAND_OBS));
    let (dec_event, dec_obs) = (evt(DECIDE_EVT), obs(DECIDE_OBS));

    // Both stores need the candidate present first: the adjudication names a
    // PERSISTED candidate and the store loads it.
    let seed = |s: &mut WorldStore| {
        s.append_same_as_candidate(&candidate_row(&cand_event, &cand_obs), &candidate())
            .expect("seed");
    };
    let request = || SameAsAdjudicationRequest {
        event_id: &dec_event,
        observation_id: &dec_obs,
        candidate_observation_id: CAND_OBS,
        cited: vec![obs(OBS)],
        authority: AdjudicationAuthority::new(SourceClass::Operator, "console-operator")
            .expect("authority"),
        outcome: Outcome::Promoted,
        decided_at: at(T0 as u64 + 10),
        txn_time_ms: T0 + 10,
        source: "console-operator",
        source_version: "1.0.0",
    };

    let mut via_command = store("adjudicate-cmd");
    seed(&mut via_command);
    let mut engine = CommandEngine::new(&mut via_command);
    engine
        .execute(AdjudicateSameAs { request: request() })
        .expect("command");

    let mut direct = store("adjudicate-direct");
    seed(&mut direct);
    direct.adjudicate_same_as(&request()).expect("direct");

    assert_eq!(
        via_command.head_chain().expect("chain"),
        direct.head_chain().expect("chain"),
        "AdjudicateSameAs and adjudicate_same_as produced different logs"
    );
}

// ---------------------------------------------------------------------------
// Non-vacuity. Without this, every assertion above would also pass against a
// `head_chain` that returned a constant.
// ---------------------------------------------------------------------------

#[test]
fn a_divergent_wrapper_is_caught_by_the_digest() {
    let (event, observation) = (evt(DECIDE_EVT), obs(DECIDE_OBS));

    let mut a = store("divergent-a");
    a.append_adjudication(
        &adjudication_row(&event, &observation),
        &IdentityAdjudication::Merge(merge()),
    )
    .expect("merge");

    // The SAME row, the SAME entities, the SAME justification and instant --
    // and a different verb. This is the smallest divergence a mis-wired
    // wrapper could produce: `RecordForget` dispatching into `Merge`.
    let mut b = store("divergent-b");
    b.append_adjudication(
        &adjudication_row(&event, &observation),
        &IdentityAdjudication::Forget(forget()),
    )
    .expect("forget");

    assert_ne!(
        a.head_chain().expect("chain"),
        b.head_chain().expect("chain"),
        "the digest cannot tell two different adjudications apart, so every \
         equality assertion in this file is vacuous"
    );
}

// ---------------------------------------------------------------------------
// The two scope fences, pinned so a later edit has to argue with them.
// ---------------------------------------------------------------------------

const COMMAND_SOURCE: &str = include_str!("../src/command.rs");

#[test]
fn no_command_can_record_an_assert_because_5d_is_unruled() {
    // `IdentityAdjudication` has four verbs; this surface wraps three. `Assert`
    // is operator teaching -- box 5d, BLOCKED ON RULING. A single wide
    // `RecordIdentityAdjudication` taking any variant would have carried it in
    // as a side effect and unblocked 5d by accident.
    //
    // Asserted against the source rather than the type system because the
    // absence of a variant is not something a type can state.
    let executable: String = COMMAND_SOURCE
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !executable.contains("IdentityAdjudication::Assert"),
        "a command now records an Assert. That is box 5d (operator teaching), \
         which is blocked on ruling what writer class an operator assertion \
         carries and whether it may outrank sensed evidence."
    );
}

/// **The adjudicator reaches the log, so both routes to identity carry one.**
///
/// This REPLACES `a_merge_command_carries_no_authority_and_that_is_the_open_
/// finding`, and the replacement is recorded rather than done quietly.
///
/// That fence was written to fail the day `KIRRA-WM-IDENTITY-AUTHORITY-001`
/// landed. **It did not fail.** It asserted `RecordMerge` had grown no
/// `authority` FIELD, and the ruling put the authority inside `MergeEntities`
/// instead — so the fence kept passing while the thing it guarded had already
/// changed underneath it. A guard written against one shape of a change,
/// satisfied by a different shape of the same change.
///
/// The replacement is behavioural for exactly that reason. Two merges identical
/// in every respect except WHO decided must produce different logs. That cannot
/// be satisfied by reshaping: it fails if the command drops the authority, if
/// the payload does not persist it, and if the encoding writes a constant.
#[test]
fn two_merges_differing_only_in_adjudicator_are_different_records() {
    let (event, observation) = (evt(DECIDE_EVT), obs(DECIDE_OBS));

    let decided_by = |adjudicator: &str| {
        MergeEntities::new(
            [eid("track-a")],
            eid("track-b"),
            AdjudicationAuthority::new(SourceClass::Operator, adjudicator).expect("authority"),
            just(),
            at(T0 as u64 + 10),
        )
        .expect("merge")
    };

    let mut yard = store("adjudicator-yard");
    CommandEngine::new(&mut yard)
        .execute(RecordMerge {
            row: adjudication_row(&event, &observation),
            merge: decided_by("yard-supervisor"),
        })
        .expect("command");

    let mut night = store("adjudicator-night");
    CommandEngine::new(&mut night)
        .execute(RecordMerge {
            row: adjudication_row(&event, &observation),
            merge: decided_by("night-shift-lead"),
        })
        .expect("command");

    assert_ne!(
        yard.head_chain().expect("chain"),
        night.head_chain().expect("chain"),
        "who adjudicated does not reach the log — the authority is being \
         dropped between the domain value and the stored record, which makes \
         KIRRA-WM-IDENTITY-AUTHORITY-001 a type signature and nothing more"
    );
}
