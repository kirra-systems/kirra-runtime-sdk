//! **Box 5a: the relationship projection, over the whole production chain.**
//!
//! These tests live in the INGEST crate, not the store crate, and that is the
//! point of the box. The chain the audit before 5a traced —
//!
//! > real producer → sanctioned candidate write → persisted candidate →
//! > authorized adjudication → confirmed event → projection
//!
//! — is exercised end to end here: every relationship these tests assert about
//! was proposed by `run_ingest_pass` from real observations and promoted by an
//! `AdjudicationAuthority`. Nothing is hand-appended into `world_events` except
//! where a test is deliberately forging a row the production doors refuse to
//! write, and each of those says so.
//!
//! # The scope this suite is testing against
//!
//! > The relationship projection is authoritative over promoted identity
//! > decisions, not over continued retention of the candidate evidence that
//! > motivated them. If cited candidate evidence is compacted, the relationship
//! > remains valid as adjudicated, while its explanatory provenance may degrade.
//!
//! `a_promoted_relationship_survives_compaction_of_its_candidate` is that
//! sentence as a test.
//!
//! # Operator-authored promotions are the only production input today
//!
//! Every promotion below goes through `AdjudicationAuthority::new`, which
//! refuses every class but `Operator`. There is no automated adjudicator to
//! test because `KIRRA-WM-PROMOTION-001` v1 does not authorize one.

use kirra_world::observation::{ClockDomain, DomainInstant, SourceClass};
use kirra_world::reference::{EntityId, EventId, ObservationId};
use kirra_world::same_as_adjudication::{AdjudicationAuthority, Outcome};
use kirra_world::same_as_candidate::{CandidatePair, MatcherIdentity};
use kirra_world_ingest::{run_ingest_pass, ExactIdentifierRule};
use kirra_world_store::provenance_graph::{CitationResolution, DanglingReason};
use kirra_world_store::same_as_adjudication_record::SameAsAdjudicationRequest;
use kirra_world_store::{ClaimStatus, NewEvent, StoreError, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;
const SURVEY_LIMIT: usize = 1_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!("kirra-5a-{name}-{}-{n}.sqlite", std::process::id()));
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

fn pair(a: &str, b: &str) -> CandidatePair {
    CandidatePair::new(
        EntityId::new(a).expect("entity"),
        EntityId::new(b).expect("entity"),
    )
    .expect("pair")
}

/// One real sensor observation of an identifier.
fn observe(store: &mut WorldStore, n: &str, subject: &str, value: &str) {
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
}

/// Run the real matcher over whatever the store has observed.
fn propose(store: &mut WorldStore) -> usize {
    let rule = ExactIdentifierRule::new(
        "serial_number",
        MatcherIdentity::new("world-ingest", "exact-identifier", "1.0.0").expect("matcher"),
    )
    .expect("rule");
    let mut n = 0;
    run_ingest_pass(store, &rule, T0, SURVEY_LIMIT, move || {
        n += 1;
        (
            EventId::new(format!("cand-ev-{n}")).expect("id"),
            ObservationId::new(format!("cand-obs-{n}")).expect("id"),
        )
    })
    .expect("ingest pass")
    .proposed
}

fn operator() -> AdjudicationAuthority {
    AdjudicationAuthority::new(SourceClass::Operator, "console-operator").expect("authority")
}

/// Record one authorized decision about a persisted candidate.
fn decide(
    store: &mut WorldStore,
    tag: &str,
    candidate_observation_id: &str,
    outcome: Outcome,
    decided_ms: u64,
) {
    let event_id = EventId::new(format!("adj-ev-{tag}")).expect("id");
    let observation_id = ObservationId::new(format!("adj-obs-{tag}")).expect("id");
    store
        .adjudicate_same_as(&SameAsAdjudicationRequest {
            event_id: &event_id,
            observation_id: &observation_id,
            candidate_observation_id,
            cited: vec![ObservationId::new(candidate_observation_id).expect("id")],
            authority: operator(),
            outcome,
            decided_at: at(decided_ms),
            txn_time_ms: T0 + 10,
            source: "operator-console",
            source_version: "1.0.0",
        })
        .expect("an operator judging a persisted candidate");
}

/// Two tracks agreeing on one serial, and the candidate that agreement produced.
fn two_tracks_and_a_candidate(name: &str) -> WorldStore {
    let mut store = WorldStore::open(&tmp(name)).expect("open");
    observe(&mut store, "a", "track-a", "SN-1");
    observe(&mut store, "b", "track-b", "SN-1");
    assert_eq!(
        propose(&mut store),
        1,
        "the fixture must persist exactly one candidate"
    );
    store
}

// ---------------------------------------------------------------------------
// The positive case first. A suite of absences with nothing admitted proves
// only that the fold never writes anything.
// ---------------------------------------------------------------------------

/// **The whole chain, end to end.**
///
/// Observations a sensor wrote → a candidate the real matcher proposed → an
/// operator's promotion → a row in `relationships_projection`. No step is
/// simulated, and the pair on the row is the pair the evidence named.
#[test]
fn a_promoted_candidate_becomes_a_relationship() {
    let mut store = two_tracks_and_a_candidate("promote");
    decide(
        &mut store,
        "1",
        "cand-obs-1",
        Outcome::Promoted,
        T0 as u64 + 10,
    );
    store.fold_relationship_projection().expect("fold");

    let rows = store.load_relationship_projection().expect("load");
    assert_eq!(rows.len(), 1, "one promotion, one relationship");
    let r = rows
        .get(&("track-a".to_owned(), "track-b".to_owned()))
        .expect("the promoted pair, canonically keyed");
    assert_eq!(r.pair.low().as_str(), "track-a");
    assert_eq!(r.pair.high().as_str(), "track-b");
    assert_eq!(
        r.candidate_observation_id, "cand-obs-1",
        "the row cites the persisted candidate the operator judged"
    );
    assert_eq!(r.adjudicator, "console-operator");
    assert_eq!(r.decided_at, at(T0 as u64 + 10));
}

/// **The row names a real, confirmed adjudication event.**
///
/// `decided_generation` is not decoration: it is what an operator follows from
/// "these two are the same" back to the decision that said so. This resolves it
/// against the log and checks the row it lands on is the adjudication —
/// confirmed, operator-written, and about this pair.
#[test]
fn a_relationship_traces_back_to_its_confirmed_adjudication_event() {
    let mut store = two_tracks_and_a_candidate("trace");
    decide(
        &mut store,
        "1",
        "cand-obs-1",
        Outcome::Promoted,
        T0 as u64 + 10,
    );
    store.fold_relationship_projection().expect("fold");

    let rows = store.load_relationship_projection().expect("load");
    let r = rows
        .get(&("track-a".to_owned(), "track-b".to_owned()))
        .expect("the relationship");
    let event = store
        .claim_at_generation(r.decided_generation)
        .expect("read the log")
        .expect("decided_generation must name a retained event");
    assert_eq!(event.kind, "same_as_adjudication");
    assert_eq!(event.subject, "track-a");
    assert_eq!(event.object.as_deref(), Some("track-b"));
    assert_eq!(event.predicate.as_deref(), Some("same_as_adjudged"));
    // `ProjectedClaim` carries no writer class or claim status -- deliberately,
    // per `claim_at_generation`'s own docs -- so those two are asserted against
    // the row directly. They are the load-bearing half: a projection fed by an
    // unconfirmed row, or one written under an unauthorized class, would be
    // KIRRA-WM-PROMOTION-001 bypassed.
    let authorized = store
        .query_scalar_for_test(&format!(
            "SELECT COUNT(*) FROM world_events WHERE generation = {} \
             AND claim_status = 'confirmed' AND writer_class = 'operator'",
            r.decided_generation
        ))
        .expect("count");
    assert_eq!(
        authorized, 1,
        "the deciding event must be a CONFIRMED, operator-written row"
    );
}

/// **A proposal is not a relationship.**
///
/// The non-vacuity control for everything above: the same fixture, the same
/// fold, no adjudication — and no row. Without this, a fold that wrote a row
/// for every candidate would pass the positive test.
#[test]
fn an_unadjudicated_candidate_is_not_a_relationship() {
    let mut store = two_tracks_and_a_candidate("unjudged");
    store.fold_relationship_projection().expect("fold");
    assert!(
        store
            .load_relationship_projection()
            .expect("load")
            .is_empty(),
        "clustering proposes and never confirms (KIRRA-WM-CLUSTERING-001)"
    );
}

/// **A rejection creates nothing.**
#[test]
fn a_rejected_candidate_is_not_a_relationship() {
    let mut store = two_tracks_and_a_candidate("reject");
    decide(
        &mut store,
        "1",
        "cand-obs-1",
        Outcome::Rejected,
        T0 as u64 + 10,
    );
    store.fold_relationship_projection().expect("fold");
    assert!(
        store
            .load_relationship_projection()
            .expect("load")
            .is_empty(),
        "an operator's refusal must not read as an identity"
    );
}

// ---------------------------------------------------------------------------
// Supersession — KIRRA-WM-IDENTITY-FRESHNESS-001's "until changed by later
// adjudication".
// ---------------------------------------------------------------------------

/// **A later rejection withdraws a standing promotion.**
#[test]
fn a_later_rejection_withdraws_a_standing_promotion() {
    let mut store = two_tracks_and_a_candidate("withdraw-reject");
    decide(
        &mut store,
        "1",
        "cand-obs-1",
        Outcome::Promoted,
        T0 as u64 + 10,
    );
    store.fold_relationship_projection().expect("fold");
    assert_eq!(store.load_relationship_projection().expect("load").len(), 1);

    decide(
        &mut store,
        "2",
        "cand-obs-1",
        Outcome::Rejected,
        T0 as u64 + 20,
    );
    store.fold_relationship_projection().expect("fold again");
    assert!(
        store
            .load_relationship_projection()
            .expect("load")
            .is_empty(),
        "the newest authorized decision governs, and the table must stop \
         asserting an identity it revoked"
    );
}

/// **A later `Unresolved` withdraws too** — the choice box 5a made, pinned.
///
/// The alternative (abstain: leave the promotion standing) is defensible and
/// was not taken, because the log does not record which the operator meant and
/// abstaining errs toward continuing to assert an identity the most recent
/// authorized decision declined to affirm. This test is what makes changing
/// that a visible decision rather than a quiet edit.
#[test]
fn a_later_unresolved_withdraws_a_standing_promotion() {
    let mut store = two_tracks_and_a_candidate("withdraw-unresolved");
    decide(
        &mut store,
        "1",
        "cand-obs-1",
        Outcome::Promoted,
        T0 as u64 + 10,
    );
    store.fold_relationship_projection().expect("fold");
    assert_eq!(store.load_relationship_projection().expect("load").len(), 1);

    decide(
        &mut store,
        "2",
        "cand-obs-1",
        Outcome::Unresolved,
        T0 as u64 + 20,
    );
    store.fold_relationship_projection().expect("fold again");
    assert!(
        store
            .load_relationship_projection()
            .expect("load")
            .is_empty(),
        "an unresolved re-decision withdraws rather than abstains"
    );
}

/// **A re-promotion after a withdrawal restores the relationship**, and the row
/// names the NEW decision.
///
/// Without this the withdrawal tests are satisfied by a fold that removes and
/// never re-admits — a projection that decays in one direction only.
#[test]
fn a_re_promotion_restores_the_relationship_under_the_new_decision() {
    let mut store = two_tracks_and_a_candidate("re-promote");
    decide(
        &mut store,
        "1",
        "cand-obs-1",
        Outcome::Promoted,
        T0 as u64 + 10,
    );
    decide(
        &mut store,
        "2",
        "cand-obs-1",
        Outcome::Rejected,
        T0 as u64 + 20,
    );
    store.fold_relationship_projection().expect("fold");
    assert!(store
        .load_relationship_projection()
        .expect("load")
        .is_empty());

    decide(
        &mut store,
        "3",
        "cand-obs-1",
        Outcome::Promoted,
        T0 as u64 + 30,
    );
    store.fold_relationship_projection().expect("fold again");
    let rows = store.load_relationship_projection().expect("load");
    let r = rows
        .get(&("track-a".to_owned(), "track-b".to_owned()))
        .expect("restored");
    assert_eq!(
        r.decided_at,
        at(T0 as u64 + 30),
        "the row must name the decision that currently holds, not the first one"
    );
}

// ---------------------------------------------------------------------------
// KIRRA-WM-TRANSITIVITY-001
// ---------------------------------------------------------------------------

/// **Promotion never synthesises a transitive relation.**
///
/// `track-a = track-b` and `track-b = track-c` are each promoted from their own
/// evidence. `(track-a, track-c)` is never proposed and never promoted, and the
/// projection must not invent it. Resolution (box 2c) may traverse merges;
/// this layer may not emit the traversal as a recorded relationship.
#[test]
fn promotion_never_synthesises_a_transitive_relation() {
    let mut store = WorldStore::open(&tmp("transitive")).expect("open");
    observe(&mut store, "a", "track-a", "SN-1");
    observe(&mut store, "b1", "track-b", "SN-1");
    observe(&mut store, "b2", "track-b", "SN-2");
    observe(&mut store, "c", "track-c", "SN-2");
    // Two agreements, two pairs: (a,b) on SN-1 and (b,c) on SN-2. Nothing
    // observed a and c agreeing on anything.
    assert_eq!(propose(&mut store), 2);

    decide(
        &mut store,
        "1",
        "cand-obs-1",
        Outcome::Promoted,
        T0 as u64 + 10,
    );
    decide(
        &mut store,
        "2",
        "cand-obs-2",
        Outcome::Promoted,
        T0 as u64 + 11,
    );
    store.fold_relationship_projection().expect("fold");

    let rows = store.load_relationship_projection().expect("load");
    assert_eq!(
        rows.len(),
        2,
        "two promotions must yield exactly two relationships, not three"
    );
    assert!(rows.contains_key(&("track-a".to_owned(), "track-b".to_owned())));
    assert!(rows.contains_key(&("track-b".to_owned(), "track-c".to_owned())));
    assert!(
        !rows.contains_key(&("track-a".to_owned(), "track-c".to_owned())),
        "KIRRA-WM-TRANSITIVITY-001: the traversed relation is not evidence"
    );
}

// ---------------------------------------------------------------------------
// Determinism — ADR-0041's rebuild-equals-incremental.
// ---------------------------------------------------------------------------

/// **Folding in stages equals rebuilding from generation 0.**
///
/// The withdrawal falls ACROSS the checkpoint deliberately: an incremental fold
/// that seeded its accumulator from empty would not find the promotion it has
/// to remove, so the two would diverge exactly where a naive implementation is
/// wrong. Compared by digest, which covers `decided_generation` and the cited
/// candidate as well as membership.
#[test]
fn incremental_folding_equals_rebuild_from_zero() {
    let mut store = WorldStore::open(&tmp("determinism")).expect("open");
    observe(&mut store, "a", "track-a", "SN-1");
    observe(&mut store, "b1", "track-b", "SN-1");
    observe(&mut store, "b2", "track-b", "SN-2");
    observe(&mut store, "c", "track-c", "SN-2");
    assert_eq!(propose(&mut store), 2);

    decide(
        &mut store,
        "1",
        "cand-obs-1",
        Outcome::Promoted,
        T0 as u64 + 10,
    );
    decide(
        &mut store,
        "2",
        "cand-obs-2",
        Outcome::Promoted,
        T0 as u64 + 11,
    );
    // Checkpoint here, mid-history.
    store.fold_relationship_projection().expect("first fold");
    let mid = store
        .relationship_projection_generation()
        .expect("generation");
    assert!(mid > 0, "the first fold must have consumed something");

    // The tail: one withdrawal and one re-promotion, both after the checkpoint.
    decide(
        &mut store,
        "3",
        "cand-obs-1",
        Outcome::Rejected,
        T0 as u64 + 20,
    );
    decide(
        &mut store,
        "4",
        "cand-obs-2",
        Outcome::Promoted,
        T0 as u64 + 21,
    );
    store.fold_relationship_projection().expect("second fold");

    let incremental = store
        .relationship_projection_state_digest()
        .expect("digest");
    let incremental_rows = store.load_relationship_projection().expect("load");

    store.rebuild_relationship_projection().expect("rebuild");
    let rebuilt = store
        .relationship_projection_state_digest()
        .expect("digest");

    assert_eq!(
        incremental, rebuilt,
        "an incremental fold across a withdrawal must equal a rebuild from zero"
    );
    // Non-vacuous: the state being compared is not empty, and it is not
    // everything either — one pair was withdrawn.
    assert_eq!(incremental_rows.len(), 1);
    assert!(incremental_rows.contains_key(&("track-b".to_owned(), "track-c".to_owned())));
}

/// **A rebuild does not inherit a row the log does not justify.**
///
/// The negative control for the `DELETE FROM relationships_projection` in
/// `rebuild_relationship_projection`, which — unlike the entity rebuild's — is
/// load-bearing. A row forged directly into the projection table must be gone
/// after a rebuild, because a rebuildable view is a function of the log alone.
#[test]
fn a_rebuild_does_not_inherit_a_row_the_log_does_not_justify() {
    let mut store = two_tracks_and_a_candidate("rebuild-forged");
    decide(
        &mut store,
        "1",
        "cand-obs-1",
        Outcome::Promoted,
        T0 as u64 + 10,
    );
    store.fold_relationship_projection().expect("fold");

    // Forged: no adjudication in the log says anything about this pair. Written
    // with raw SQL precisely because no production door will write it.
    store
        .raw_execute_for_test(
            "INSERT INTO relationships_projection
                 (low, high, decided_generation, candidate_observation_id,
                  adjudicator, decided_at_ms, decided_at_domain)
             VALUES ('track-x', 'track-y', 1, 'cand-obs-1', 'nobody', 1, 'system')",
        )
        .expect("forge");
    assert_eq!(
        store.load_relationship_projection().expect("load").len(),
        2,
        "the forgery must actually be present, or this test proves nothing"
    );

    store.rebuild_relationship_projection().expect("rebuild");
    let rows = store.load_relationship_projection().expect("load");
    assert_eq!(rows.len(), 1);
    assert!(
        !rows.contains_key(&("track-x".to_owned(), "track-y".to_owned())),
        "a rebuild answers from the log, not from what the table already held"
    );
}

// ---------------------------------------------------------------------------
// The scope statement, as a test.
// ---------------------------------------------------------------------------

/// **A promoted relationship survives compaction of the candidate it cites,
/// and its provenance degrades rather than disappearing or reading as full.**
///
/// This is the sentence box 5a was scoped by. Three things are asserted and all
/// three matter:
///
/// 1. Before compaction the citation resolves — otherwise "it degraded" would
///    be indistinguishable from "it never resolved".
/// 2. After compaction the relationship is still there, unchanged. Identity is
///    adjudicated, and `KIRRA-WM-IDENTITY-FRESHNESS-001` makes a promoted
///    `same_as` `Timeless`; making retention of the proposal a precondition for
///    the identity holding would contradict that ruling.
/// 3. The citation now reads `Dangling { PossiblyCompacted }` — not `Resolved`,
///    and not `NeverVisible`. The last distinction is the one §11.3 forbids
///    collapsing: *nothing was ever recorded* and *what was recorded was
///    deleted* are different findings.
#[test]
fn a_promoted_relationship_survives_compaction_of_its_candidate() {
    let mut store = two_tracks_and_a_candidate("compaction");
    decide(
        &mut store,
        "1",
        "cand-obs-1",
        Outcome::Promoted,
        T0 as u64 + 10,
    );
    store.fold_relationship_projection().expect("fold");

    let key = pair("track-a", "track-b");
    // (1) Before: the cited candidate is in the log and resolves to one carrier.
    let before = store
        .relationship_provenance(&key)
        .expect("provenance")
        .expect("the relationship holds");
    let candidate_generation = match before {
        CitationResolution::Resolved { target_generation } => target_generation,
        other => panic!("the candidate must resolve before it is compacted: {other:?}"),
    };

    // Compact exactly the candidate's own generation. The adjudication cannot
    // be compacted with it -- `retention_class = "adjudication"` is protected --
    // which is what makes this scenario the realistic one rather than contrived.
    let outcome = store
        .compact_range(candidate_generation, candidate_generation, T0 + 100)
        .expect("compacting a raw candidate is permitted");
    assert!(
        outcome.removed > 0,
        "the compaction must actually have removed the candidate"
    );

    // (2) The relationship is untouched, including after a full rebuild -- so
    // this is not merely a stale row that a recomputation would drop.
    store.rebuild_relationship_projection().expect("rebuild");
    let rows = store.load_relationship_projection().expect("load");
    let r = rows
        .get(&("track-a".to_owned(), "track-b".to_owned()))
        .expect("the relationship remains valid as adjudicated");
    assert_eq!(r.candidate_observation_id, "cand-obs-1");

    // (3) The provenance degrades, and says which kind of nothing it is.
    match store
        .relationship_provenance(&key)
        .expect("provenance")
        .expect("the relationship still holds")
    {
        CitationResolution::Dangling {
            reason: DanglingReason::PossiblyCompacted { spans, truncated },
        } => {
            assert!(!truncated);
            assert!(
                spans.contains(&candidate_generation),
                "the answer must name the compaction citation to go and read"
            );
        }
        other => panic!(
            "compacted evidence must degrade to PossiblyCompacted, never to \
             Resolved and never to NeverVisible: {other:?}"
        ),
    }
}

/// **`relationship_provenance` distinguishes "no such relationship" from
/// "dangling evidence".**
///
/// `Ok(None)` and `Ok(Some(Dangling))` are different findings and a caller told
/// the wrong one looks in the wrong place — the same distinction
/// `AdjudicateError` draws between `NoSuchCandidate` and `NotACandidate`.
#[test]
fn provenance_of_a_pair_that_was_never_promoted_is_none() {
    let mut store = two_tracks_and_a_candidate("no-such");
    store.fold_relationship_projection().expect("fold");
    assert!(store
        .relationship_provenance(&pair("track-a", "track-b"))
        .expect("provenance")
        .is_none());
}

// ---------------------------------------------------------------------------
// Fail-closed reads.
// ---------------------------------------------------------------------------

/// **A projection row stored the wrong way round is refused, not repaired.**
///
/// `CandidatePair::new` canonicalises, so a loader that simply reconstructed
/// the pair would return a correct-looking relationship and hide that the row
/// disagrees with the fold's own convention — and the repaired and unrepaired
/// rows would then collide on one key while claiming different provenance.
#[test]
fn a_non_canonical_projection_row_is_refused() {
    let mut store = two_tracks_and_a_candidate("non-canonical");
    decide(
        &mut store,
        "1",
        "cand-obs-1",
        Outcome::Promoted,
        T0 as u64 + 10,
    );
    store.fold_relationship_projection().expect("fold");

    store
        .raw_execute_for_test(
            "UPDATE relationships_projection SET low = 'track-b', high = 'track-a'",
        )
        .expect("swap");

    match store.load_relationship_projection() {
        Err(StoreError::CorruptRelationshipProjectionRow { detail }) => {
            assert!(
                detail.contains("canonical"),
                "the refusal must name what disagreed: {detail}"
            );
        }
        other => panic!("a non-canonical row must be refused: {other:?}"),
    }
}

/// **A relationship promoted below the checkpoint survives a later, unrelated
/// fold.**
///
/// This test exists because it was MISSING and the gap was found by mutation,
/// not by review. Seeding `fold_relationship_range`'s accumulator from empty
/// instead of from the stored rows left all thirteen other tests green: every
/// pair in `incremental_folding_equals_rebuild_from_zero` is touched by the
/// tail, so an accumulator that had forgotten the earlier state still ended up
/// agreeing with one that had not.
///
/// Here `(track-a, track-b)` is promoted, folded, and then never mentioned
/// again while a different pair is decided. An unseeded fold does not merely
/// leave it stale — it DELETES it, because the withdrawal sweep sees a stored
/// key the accumulator no longer holds and reads that as a withdrawal.
#[test]
fn a_relationship_promoted_below_the_checkpoint_survives_a_later_unrelated_fold() {
    let mut store = WorldStore::open(&tmp("below-checkpoint")).expect("open");
    observe(&mut store, "a", "track-a", "SN-1");
    observe(&mut store, "b1", "track-b", "SN-1");
    observe(&mut store, "b2", "track-b", "SN-2");
    observe(&mut store, "c", "track-c", "SN-2");
    assert_eq!(propose(&mut store), 2);

    decide(
        &mut store,
        "1",
        "cand-obs-1",
        Outcome::Promoted,
        T0 as u64 + 10,
    );
    store.fold_relationship_projection().expect("first fold");
    assert_eq!(store.load_relationship_projection().expect("load").len(), 1);

    // A decision about a DIFFERENT pair, after the checkpoint. Nothing here
    // says anything about (track-a, track-b).
    decide(
        &mut store,
        "2",
        "cand-obs-2",
        Outcome::Promoted,
        T0 as u64 + 20,
    );
    store.fold_relationship_projection().expect("second fold");

    let rows = store.load_relationship_projection().expect("load");
    assert!(
        rows.contains_key(&("track-a".to_owned(), "track-b".to_owned())),
        "a relationship the tail never mentions must not be withdrawn by a fold"
    );
    assert!(rows.contains_key(&("track-b".to_owned(), "track-c".to_owned())));

    // And the checkpoint digest must describe that state, not the tail alone --
    // a fold that dropped the earlier pair from its accumulator would also
    // stamp a digest that omitted it, so the two would agree with each other
    // while both being wrong.
    let incremental = store
        .relationship_projection_state_digest()
        .expect("digest");
    store.rebuild_relationship_projection().expect("rebuild");
    assert_eq!(
        incremental,
        store
            .relationship_projection_state_digest()
            .expect("digest"),
        "incremental must equal rebuild across an untouched pair too"
    );
}

/// **The provenance pin is the DECIDING generation, and a later carrier of the
/// same observation id does not resurrect compacted evidence.**
///
/// `relationship_provenance` resolves at `decided_generation` rather than at
/// the head of the log, and this is the case that makes the difference visible.
/// After the judged candidate is compacted, a *different* event carrying the
/// same observation id is appended. Resolving at the head would find it and
/// report `Resolved`, quietly substituting an event the operator never saw for
/// the one their decision rested on. Pinned at the decision, it is still
/// `Dangling`.
///
/// Written after the pin's justification was already in a code comment — an
/// asserted reason with no control is the defect this box already hit once.
#[test]
fn a_later_carrier_of_the_same_id_does_not_resurrect_compacted_evidence() {
    let mut store = two_tracks_and_a_candidate("impostor");
    decide(
        &mut store,
        "1",
        "cand-obs-1",
        Outcome::Promoted,
        T0 as u64 + 10,
    );
    store.fold_relationship_projection().expect("fold");

    let key = pair("track-a", "track-b");
    let candidate_generation = match store
        .relationship_provenance(&key)
        .expect("provenance")
        .expect("holds")
    {
        CitationResolution::Resolved { target_generation } => target_generation,
        other => panic!("must resolve before compaction: {other:?}"),
    };
    store
        .compact_range(candidate_generation, candidate_generation, T0 + 100)
        .expect("compact the raw candidate");

    // A LATER event reusing the compacted observation's id. Not a forgery of
    // the projection -- an ordinary append, which is why the pin has to do the
    // work rather than the log preventing it.
    let event_id = EventId::new("impostor-ev").expect("id");
    let observation_id = ObservationId::new("cand-obs-1").expect("id");
    store
        .append(&NewEvent {
            event_id: &event_id,
            observation_id: &observation_id,
            txn_time_ms: T0 + 200,
            valid_from_ms: T0 + 200,
            valid_to_ms: None,
            source: "asset-tag-reader",
            source_version: "1.0.0",
            writer_class: WriterClass::Sensor,
            claim_status: ClaimStatus::Confirmed,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "observation",
            subject: "track-a",
            subject_ref: None,
            predicate: Some("serial_number"),
            object: Some("SN-1"),
            payload: "{}",
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append a later carrier of the same observation id");

    match store
        .relationship_provenance(&key)
        .expect("provenance")
        .expect("holds")
    {
        CitationResolution::Dangling { .. } => {}
        other => panic!(
            "an event appended after the decision must not stand in for the \
             evidence the decision rested on: {other:?}"
        ),
    }
}
