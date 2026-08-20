//! **Box 2b: adjudication judges persisted evidence, or refuses.**
//!
//! `KIRRA-WM-PROMOTION-001` puts confirmed identity behind an explicitly
//! authorized adjudicator ruling over *recorded* evidence. Until 2b the API took
//! a `&SameAsCandidate` — a value the caller built — so an adjudicator could
//! judge something that merely looked like evidence.
//!
//! # The load-bearing property is structural, so there is no test for it
//!
//! `SameAsAdjudicationRequest` carries a candidate `ObservationId` and **no
//! candidate value**. An in-memory `SameAsCandidate` with no persisted id cannot
//! be passed to `adjudicate_same_as` at all — not "is rejected by it". That is a
//! compile-time property, and the honest way to record it is to say so rather
//! than to write a test that cannot fail.
//!
//! What IS tested is every way a caller can name a persisted thing that turns
//! out not to be admissible evidence, which is where the runtime risk actually
//! lives.

use kirra_world::observation::{ClockDomain, DomainInstant, SourceClass};
use kirra_world::reference::{EventId, ObservationId};
use kirra_world::same_as_adjudication::{
    corroboration_count, AdjudicationAuthority, Outcome, SameAsAdjudication,
};
use kirra_world::same_as_candidate::MatcherIdentity;
use kirra_world_ingest::{run_ingest_pass, ExactIdentifierRule};
use kirra_world_store::same_as_adjudication_record::{AdjudicateError, SameAsAdjudicationRequest};
use kirra_world_store::{ClaimStatus, NewEvent, StoreError, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;
const SURVEY_LIMIT: usize = 1_000;
/// The observation id the ingest pass mints for its first proposal.
const CANDIDATE_OBS: &str = "cand-obs-1";

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!("kirra-2b-{name}-{}-{n}.sqlite", std::process::id()));
    for s in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(s);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

fn at(ms: u64) -> DomainInstant {
    DomainInstant {
        ms,
        domain: ClockDomain::System,
    }
}

fn observe(store: &mut WorldStore, n: &str, subject: &str, value: &str) -> String {
    let event_id = EventId::new(format!("ev-{n}")).expect("event id");
    let observation_id = ObservationId::new(format!("obs-{n}")).expect("observation id");
    store
        .append(&NewEvent {
            event_id: &event_id,
            observation_id: &observation_id,
            txn_time_ms: T0,
            valid_from_ms: T0,
            valid_to_ms: None,
            source: "asset-tag-reader",
            source_version: "1.0.0",
            writer_class: WriterClass::Sensor,
            claim_status: ClaimStatus::Confirmed,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "observation",
            subject,
            subject_ref: None,
            predicate: Some("serial_number"),
            object: Some(value),
            payload: "{}",
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append observation");
    observation_id.as_str().to_owned()
}

/// A store holding one real, persisted `same_as` candidate for `(track-a, track-b)`.
fn store_with_a_candidate(name: &str) -> (WorldStore, String, String) {
    let path = tmp(name);
    let mut store = WorldStore::open(&path).expect("open");
    let obs_a = observe(&mut store, "a", "track-a", "SN-1");
    let obs_b = observe(&mut store, "b", "track-b", "SN-1");

    let rule = ExactIdentifierRule::new(
        "serial_number",
        MatcherIdentity::new("world-ingest", "exact-identifier", "1.0.0").expect("matcher"),
    )
    .expect("rule");
    let mut n = 0;
    let report = run_ingest_pass(&mut store, &rule, T0, SURVEY_LIMIT, move || {
        n += 1;
        (
            EventId::new(format!("cand-ev-{n}")).expect("id"),
            ObservationId::new(format!("cand-obs-{n}")).expect("id"),
        )
    })
    .expect("pass");
    assert_eq!(
        report.proposed, 1,
        "fixture must persist exactly one candidate"
    );
    (store, obs_a, obs_b)
}

/// A payload that decodes as a perfectly good candidate.
///
/// Load-bearing for the adversarial fixtures below: with `"{}"` they were
/// refused for having no `matcher` field, so each case passed without its named
/// property ever being consulted. Mutating away the `writer_class` check left
/// them all green — which is how that was found.
const VALID_CANDIDATE_PAYLOAD: &str = r#"{"matcher":{"producer":"p","model_or_rule":"m","version":"1.0.0"},"confidence":{"score":null,"basis":"unspecified","calibration":null}}"#;

fn operator() -> AdjudicationAuthority {
    AdjudicationAuthority::new(SourceClass::Operator, "console-operator").expect("authority")
}

fn request<'a>(
    event_id: &'a EventId,
    observation_id: &'a ObservationId,
    candidate_observation_id: &'a str,
    cited: Vec<ObservationId>,
    authority: AdjudicationAuthority,
    outcome: Outcome,
) -> SameAsAdjudicationRequest<'a> {
    SameAsAdjudicationRequest {
        event_id,
        observation_id,
        candidate_observation_id,
        cited,
        authority,
        outcome,
        decided_at: at(T0 as u64 + 10),
        txn_time_ms: T0 + 10,
        source: "operator-console",
        source_version: "1.0.0",
    }
}

fn ids(n: &str) -> (EventId, ObservationId) {
    (
        EventId::new(format!("adj-ev-{n}")).expect("id"),
        ObservationId::new(format!("adj-obs-{n}")).expect("id"),
    )
}

// ---------------------------------------------------------------------------
// The positive case, first — everything below is a refusal, and a suite of
// refusals with no admitted case proves only that nothing works.
// ---------------------------------------------------------------------------

/// **A persisted, valid candidate adjudicates.**
#[test]
fn a_persisted_valid_candidate_can_be_adjudicated() {
    let (mut store, obs_a, _) = store_with_a_candidate("valid");
    let (e, o) = ids("ok");
    let decision = store
        .adjudicate_same_as(&request(
            &e,
            &o,
            CANDIDATE_OBS,
            vec![ObservationId::new(&obs_a).expect("id")],
            operator(),
            Outcome::Promoted,
        ))
        .expect("a real candidate, judged by an operator, is adjudicable");

    assert_eq!(decision.outcome(), Outcome::Promoted);
    assert_eq!(decision.pair().low().as_str(), "track-a");
    assert_eq!(decision.pair().high().as_str(), "track-b");
    assert_eq!(
        decision.candidate_observation_id().as_str(),
        CANDIDATE_OBS,
        "the decision must name the persisted candidate it judged"
    );
}

/// **The pair comes from the loaded evidence, not from the caller.**
///
/// There is no pair parameter, so a decision cannot be recorded against a pair
/// the candidate does not name. Asserted against the fixture's real entities so
/// the property is checked rather than merely arranged.
#[test]
fn the_decided_pair_is_the_one_the_evidence_names() {
    let (mut store, obs_a, _) = store_with_a_candidate("pair-from-evidence");
    let (e, o) = ids("pair");
    let decision = store
        .adjudicate_same_as(&request(
            &e,
            &o,
            CANDIDATE_OBS,
            vec![ObservationId::new(&obs_a).expect("id")],
            operator(),
            Outcome::Rejected,
        ))
        .expect("adjudicable");
    assert_eq!(
        (
            decision.pair().low().as_str(),
            decision.pair().high().as_str()
        ),
        ("track-a", "track-b")
    );
}

// ---------------------------------------------------------------------------
// The adversarial cases
// ---------------------------------------------------------------------------

/// **1. A nonexistent observation id is refused.**
#[test]
fn a_nonexistent_observation_id_is_refused() {
    let (mut store, obs_a, _) = store_with_a_candidate("nonexistent");
    let (e, o) = ids("none");
    let err = store
        .adjudicate_same_as(&request(
            &e,
            &o,
            "no-such-observation",
            vec![ObservationId::new(&obs_a).expect("id")],
            operator(),
            Outcome::Promoted,
        ))
        .expect_err("there is no such evidence");
    assert!(
        matches!(
            err,
            StoreError::Adjudicate(AdjudicateError::NoSuchCandidate { .. })
        ),
        "and the refusal must say the evidence is ABSENT, not malformed: {err:?}"
    );
}

/// **2. An id naming a CONFIRMED row rather than a candidate is refused.**
///
/// `obs-a` is the sensor observation the matcher computed FROM — real, relevant,
/// and not a proposal. Judging it would be judging the input as though it were
/// the candidate.
///
/// Note what this does and does not isolate. That row differs from a candidate
/// in several ways at once (status, class, predicate), so it proves the door
/// refuses it, not *which* check did. The single-property isolation for
/// `claim_status` is
/// `a_derivation_row_marked_confirmed_is_refused_by_the_decoder` below, and it
/// has to live at the decoder because the store cannot produce such a row at
/// all.
#[test]
fn an_observation_that_is_confirmed_rather_than_candidate_is_refused() {
    let (mut store, obs_a, _) = store_with_a_candidate("confirmed-row");
    let (e, o) = ids("conf");
    let err = store
        .adjudicate_same_as(&request(
            &e,
            &o,
            &obs_a,
            vec![ObservationId::new(&obs_a).expect("id")],
            operator(),
            Outcome::Promoted,
        ))
        .expect_err("a confirmed observation is not a candidate");
    assert!(
        matches!(
            err,
            StoreError::Adjudicate(AdjudicateError::NotACandidate { .. })
        ),
        "the row EXISTS, so this must not read as absence: {err:?}"
    );
}

/// **2a. `derivation` + `confirmed` is refused by the decoder — and cannot be
/// stored in the first place.**
///
/// This one is deliberately at the decode layer rather than through the door,
/// and the reason is a structural fact worth stating: schema **v8** installs a
/// trigger refusing `writer_class = 'derivation'` with
/// `claim_status = 'confirmed'`, so no such row can exist in any store. The
/// door therefore cannot be handed one, and a store-level test of this property
/// would be asserting against an unreachable state.
///
/// The decoder's `claim_status` check is still not dead: `decode_candidate` is
/// public and total, so it must refuse a row shape it may be handed by any
/// caller — including a future backend whose write path is not SQLite's.
#[test]
fn a_derivation_row_marked_confirmed_is_refused_by_the_decoder() {
    use kirra_world_store::candidate_record::{
        decode_candidate, CandidateDecodeError, StoredCandidateRow, CANDIDATE_KIND,
        CANDIDATE_PAYLOAD_SCHEMA, CANDIDATE_PREDICATE_TOKEN, CANDIDATE_STATUS_TOKEN,
        DERIVATION_TOKEN,
    };
    let row = |status: &'static str| StoredCandidateRow {
        writer_class: DERIVATION_TOKEN,
        claim_status: status,
        kind: CANDIDATE_KIND,
        predicate: Some(CANDIDATE_PREDICATE_TOKEN),
        subject: "track-a",
        object_id: Some("track-b"),
        payload: VALID_CANDIDATE_PAYLOAD,
        payload_schema: CANDIDATE_PAYLOAD_SCHEMA,
    };
    assert!(
        matches!(
            decode_candidate(&row("confirmed"), &["obs-a".to_owned()]),
            Err(CandidateDecodeError::NotACandidateRow { .. })
        ),
        "a confirmed row is not a matcher's proposal"
    );
    // Non-vacuity: the SAME row at candidate status decodes, so the refusal is
    // attributable to `claim_status` and to nothing else about the fixture.
    decode_candidate(&row(CANDIDATE_STATUS_TOKEN), &["obs-a".to_owned()])
        .expect("the identical row, as a candidate, decodes");
}

/// **3. An id naming the wrong predicate is refused.**
///
/// A candidate-status row that proposes something other than co-reference is
/// still not a `same_as` candidate.
#[test]
fn an_observation_with_the_wrong_predicate_is_refused() {
    let (mut store, obs_a, _) = store_with_a_candidate("wrong-predicate");
    let (e, o) = ids("pred");
    let other = ObservationId::new("other-candidate").expect("id");
    store
        .append(&NewEvent {
            event_id: &EventId::new("other-ev").expect("id"),
            observation_id: &other,
            txn_time_ms: T0,
            valid_from_ms: T0,
            valid_to_ms: None,
            source: "some-matcher",
            source_version: "1.0.0",
            writer_class: WriterClass::Derivation,
            claim_status: ClaimStatus::Candidate,
            provenance: &[obs_a.as_str()],
            frame_id: None,
            map_id: None,
            kind: "observation",
            subject: "track-a",
            subject_ref: None,
            predicate: Some("near"),
            object: Some("track-b"),
            payload: VALID_CANDIDATE_PAYLOAD,
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append");

    let err = store
        .adjudicate_same_as(&request(
            &e,
            &o,
            "other-candidate",
            vec![ObservationId::new(&obs_a).expect("id")],
            operator(),
            Outcome::Promoted,
        ))
        .expect_err("`near` is not `same_as`");
    assert!(matches!(
        err,
        StoreError::Adjudicate(AdjudicateError::NotACandidate { .. })
    ));
}

/// **4. An id naming a non-`Derivation` writer is refused.**
///
/// A `same_as` candidate written by an operator or a sensor is not a matcher's
/// proposal, whatever its predicate says.
#[test]
fn an_observation_from_a_non_derivation_writer_is_refused() {
    let (mut store, obs_a, _) = store_with_a_candidate("wrong-writer");
    let (e, o) = ids("writer");
    let hand = ObservationId::new("hand-written").expect("id");
    store
        .append(&NewEvent {
            event_id: &EventId::new("hand-ev").expect("id"),
            observation_id: &hand,
            txn_time_ms: T0,
            valid_from_ms: T0,
            valid_to_ms: None,
            source: "operator-console",
            source_version: "1.0.0",
            writer_class: WriterClass::Operator,
            claim_status: ClaimStatus::Candidate,
            provenance: &[obs_a.as_str()],
            frame_id: None,
            map_id: None,
            kind: "observation",
            subject: "track-a",
            subject_ref: None,
            predicate: Some("same_as"),
            object: Some("track-b"),
            payload: VALID_CANDIDATE_PAYLOAD,
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append");

    let err = store
        .adjudicate_same_as(&request(
            &e,
            &o,
            "hand-written",
            vec![ObservationId::new(&obs_a).expect("id")],
            operator(),
            Outcome::Promoted,
        ))
        .expect_err("only a derivation-class proposal is a candidate");
    assert!(matches!(
        err,
        StoreError::Adjudicate(AdjudicateError::NotACandidate { .. })
    ));
}

/// **5. An unauthorized adjudicator cannot be CONSTRUCTED, let alone reach the
/// door.**
///
/// This test changed shape while being written, and the change is the finding.
/// It began as "the door refuses a `Derivation`-class adjudicator" — and failed,
/// because `AdjudicationAuthority::new` refuses that class itself. The store had
/// a comparison against `Operator` that could only ever be `false`: a dead guard
/// that a reader would count as the enforcement and stop looking for the real
/// one. It has been removed, and this asserts what is actually true.
///
/// A `Derivation`-class "adjudicator" is a matcher confirming its own proposal —
/// the exact loop `KIRRA-WM-PROMOTION-001` exists to break — and it is
/// unrepresentable rather than rejected.
#[test]
fn an_unauthorized_adjudicator_cannot_be_constructed_at_all() {
    for class in [
        SourceClass::Derivation,
        SourceClass::Sensor,
        SourceClass::Network,
        SourceClass::Import,
        SourceClass::Configuration,
    ] {
        assert!(
            AdjudicationAuthority::new(class, "would-be-adjudicator").is_err(),
            "{class:?} must not be able to hold adjudication authority"
        );
    }
    // Non-vacuity: the one authorized class still constructs, so the loop above
    // is rejecting the CLASS and not every authority.
    AdjudicationAuthority::new(SourceClass::Operator, "console-operator")
        .expect("v1 authorizes the operator");
}

/// **6. The candidate survives a rejection, and an unresolved decision.**
///
/// Deleting the thing a judgement is about destroys the judgement's subject, so
/// the candidate must still be loadable afterwards — and still be a candidate,
/// not something the rejection mutated.
#[test]
fn the_candidate_remains_in_the_ledger_after_reject_and_unresolved() {
    for (name, outcome, n) in [
        ("rejected", Outcome::Rejected, "rej"),
        ("unresolved", Outcome::Unresolved, "unr"),
    ] {
        let (mut store, obs_a, _) = store_with_a_candidate(name);
        let (e, o) = ids(n);
        store
            .adjudicate_same_as(&request(
                &e,
                &o,
                CANDIDATE_OBS,
                vec![ObservationId::new(&obs_a).expect("id")],
                operator(),
                outcome,
            ))
            .expect("adjudicable");

        let still_there = store
            .load_same_as_candidate(CANDIDATE_OBS)
            .expect("load")
            .expect("the candidate must survive being judged");
        assert_eq!(still_there.pair().low().as_str(), "track-a");
        assert_eq!(still_there.pair().high().as_str(), "track-b");
    }
}

/// **7. Re-adjudicating one persisted pair cannot inflate corroboration.**
///
/// `Corroboration(n)` counts **distinct confirmed relations**, not adjudication
/// records — so three promotions of one pair are one corroboration. Asserted
/// here against decisions that all came through the persisted door, because the
/// pure-layer test cannot show that the store path preserves the property.
#[test]
fn re_adjudicating_the_same_persisted_pair_does_not_inflate_corroboration() {
    let (mut store, obs_a, _) = store_with_a_candidate("replay");
    let mut decisions: Vec<SameAsAdjudication> = Vec::new();
    for i in 0..3 {
        let (e, o) = ids(&format!("replay-{i}"));
        decisions.push(
            store
                .adjudicate_same_as(&request(
                    &e,
                    &o,
                    CANDIDATE_OBS,
                    vec![ObservationId::new(&obs_a).expect("id")],
                    operator(),
                    Outcome::Promoted,
                ))
                .expect("adjudicable"),
        );
    }

    assert_eq!(decisions.len(), 3, "three records were genuinely written");
    assert_eq!(
        corroboration_count(&decisions),
        1,
        "but one pair is one corroboration, however many times it is affirmed"
    );
}

/// **Nothing is written when an adjudication is refused.**
///
/// The order is load → validate → decide → append, so a refused attempt leaves
/// no trace. Without this, a refusal could still be appending a row a later
/// reader would count.
///
/// The refusal used here is a nonexistent candidate — a REACHABLE one. An
/// earlier version reached for an unauthorized authority, which cannot be
/// constructed, so the test would have been asserting against a call that never
/// happened.
#[test]
fn a_refused_adjudication_appends_nothing() {
    let (mut store, obs_a, _) = store_with_a_candidate("no-trace");
    let before = store.count().expect("count");

    let (e, o) = ids("refused");
    store
        .adjudicate_same_as(&request(
            &e,
            &o,
            "no-such-observation",
            vec![ObservationId::new(&obs_a).expect("id")],
            operator(),
            Outcome::Promoted,
        ))
        .expect_err("refused");

    assert_eq!(
        store.count().expect("count"),
        before,
        "a refused adjudication must not append"
    );

    // Non-vacuity: an ACCEPTED adjudication does append, so the equality above
    // is showing that the refusal wrote nothing rather than that this door
    // never writes.
    let (e2, o2) = ids("accepted");
    store
        .adjudicate_same_as(&request(
            &e2,
            &o2,
            CANDIDATE_OBS,
            vec![ObservationId::new(&obs_a).expect("id")],
            operator(),
            Outcome::Promoted,
        ))
        .expect("adjudicable");
    assert_eq!(store.count().expect("count"), before + 1);
}

// ---------------------------------------------------------------------------
// Review finding on #1466 — the provenance walk must reach the judged candidate
// ---------------------------------------------------------------------------

/// **A provenance walk from the decision reaches the candidate it judged.**
///
/// The adjudication row's `provenance` originally carried only `cited()`, so the
/// `candidate_observation_id` — the single most important citation, the thing
/// the decision is ABOUT — was reachable only by parsing the payload. Box 4a's
/// citation index is built from the `provenance` column, so a walk from a
/// decision could not reach the proposal it judged.
///
/// This asserts the walk, not the encoding: it reads the edges the store
/// actually indexed rather than the payload it wrote, which is the whole
/// difference the finding turned on.
#[test]
fn the_decision_cites_the_candidate_it_judged_in_walkable_provenance() {
    let (mut store, obs_a, _) = store_with_a_candidate("walkable");
    let (e, o) = ids("walk");
    store
        .adjudicate_same_as(&request(
            &e,
            &o,
            CANDIDATE_OBS,
            vec![ObservationId::new(&obs_a).expect("id")],
            operator(),
            Outcome::Promoted,
        ))
        .expect("adjudicable");

    // The decision is the newest row; take its generation from the row itself
    // rather than assuming generations are dense.
    let generation = store.head_generation_for_test().expect("head");
    let page = store
        .citations_of(generation, 16, None)
        .expect("citations of the decision");
    let cited: Vec<String> = page
        .edges
        .iter()
        .map(|edge| edge.cited_observation_id.clone())
        .collect();

    assert!(
        cited.contains(&CANDIDATE_OBS.to_owned()),
        "the walk must reach the judged candidate: {cited:?}"
    );
    assert!(
        cited.contains(&obs_a),
        "and still reach the operator's own citations: {cited:?}"
    );
}

/// **The candidate is cited once, even when the caller also names it.**
///
/// `provenance` is keyed `(generation, ordinal)`, so one observation appearing
/// twice would make a single piece of evidence look like two — the defect
/// `SameAsCandidate::propose` already refuses for support lists.
#[test]
fn naming_the_candidate_in_cited_as_well_does_not_double_count_it() {
    let (mut store, obs_a, _) = store_with_a_candidate("no-double");
    let (e, o) = ids("dup");
    store
        .adjudicate_same_as(&request(
            &e,
            &o,
            CANDIDATE_OBS,
            vec![
                ObservationId::new(CANDIDATE_OBS).expect("id"),
                ObservationId::new(&obs_a).expect("id"),
            ],
            operator(),
            Outcome::Promoted,
        ))
        .expect("adjudicable");

    let generation = store.head_generation_for_test().expect("head");
    let page = store.citations_of(generation, 16, None).expect("citations");
    let occurrences = page
        .edges
        .iter()
        .filter(|edge| edge.cited_observation_id == CANDIDATE_OBS)
        .count();
    assert_eq!(occurrences, 1, "one citation, not two: {:?}", page.edges);
}
