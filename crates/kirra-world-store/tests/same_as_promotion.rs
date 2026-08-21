//! **Tier 2 box 2c — a confirmed `same_as` changes what identity resolves to.**
//!
//! The acceptance bar here is BEHAVIOURAL, deliberately: box 2b
//! (`kirra_world::same_as_adjudication`) sat in the orphan baseline for weeks,
//! and its predecessor 2a had its own entry retired because 2b *named* its
//! types — while 2b itself did nothing. Module reachability had already been
//! mistaken for integration once in this exact chain.
//!
//! So the property under test is not "the promotion function is called". It is:
//!
//! > Given the same log, adding a confirmed `same_as` adjudication changes what
//! > `kirra_world::resolution::resolve` ANSWERS.
//!
//! Every test below is a pair: the same store, with and without the promotion.
//! A change that stopped promoting would keep compiling, keep passing an
//! is-it-wired check, and fail here.

use kirra_world::adjudication::{
    promote_confirmed_same_as, AdjudicationError, AssertIdentity, IdentityAdjudication,
    Justification,
};
use kirra_world::observation::{
    ClockDomain, Confidence, ConfidenceBasis, DomainInstant, SourceClass,
};
use kirra_world::reference::{EntityId, EventId, ObservationId};
use kirra_world::resolution::{resolve, ResolutionOutcome};
use kirra_world::same_as_adjudication::{AdjudicationAuthority, Outcome, SameAsAdjudication};
use kirra_world::same_as_candidate::{CandidatePair, MatcherIdentity, SameAsCandidate};
use kirra_world_store::adjudication_record::AdjudicationRow;
use kirra_world_store::{WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;
const OBS: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const OBS2: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB0";
/// The persisted candidate these decisions judge.
///
/// Box 2b made adjudication name the candidate OBSERVATION rather than take a
/// candidate value, so every record here carries one. These are pure-layer
/// promotion tests, so the id is a fixture — the store door is what proves a
/// real one exists (`kirra-world-ingest/tests/persisted_adjudication.rs`).
const CANDIDATE_OBS: &str = "01ARZ3NDEKTSV4RRFFQ69G5FC1";

fn eid(s: &str) -> EntityId {
    EntityId::new(s.to_string()).expect("entity id")
}
fn obs(s: &str) -> ObservationId {
    ObservationId::new(s.to_string()).expect("observation id")
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
    p.push(format!("kirra-2c-{name}-{}-{n}.sqlite", std::process::id()));
    for s in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(s);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

fn candidate(a: &str, b: &str) -> SameAsCandidate {
    SameAsCandidate::propose(
        CandidatePair::new(eid(a), eid(b)).expect("distinct"),
        MatcherIdentity::new("track-matcher", "siamese-v2", "2.3.1").expect("matcher"),
        Confidence::new(Some(0.9), ConfidenceBasis::ModelScore, None).expect("confidence"),
        vec![obs(OBS)],
    )
    .expect("candidate")
}

fn operator() -> AdjudicationAuthority {
    AdjudicationAuthority::new(SourceClass::Operator, "console-operator").expect("authority")
}

fn decide(c: &SameAsCandidate, outcome: Outcome, ms: u64) -> SameAsAdjudication {
    SameAsAdjudication::record(
        c.pair().clone(),
        obs(CANDIDATE_OBS),
        vec![obs(OBS)],
        operator(),
        outcome,
        at(ms),
    )
    .expect("adjudication")
}

/// `decide`, but the caller names the clock the decision was read from.
///
/// Separate from `decide` rather than a fourth parameter on it: every other test
/// here is about identity, where the domain is noise, and threading `System`
/// through six call sites would bury the one place the domain is the subject.
fn decide_on(
    c: &SameAsCandidate,
    outcome: Outcome,
    ms: u64,
    domain: ClockDomain,
) -> SameAsAdjudication {
    SameAsAdjudication::record(
        c.pair().clone(),
        obs(CANDIDATE_OBS),
        vec![obs(OBS)],
        operator(),
        outcome,
        DomainInstant { ms, domain },
    )
    .expect("adjudication")
}

/// Seed a store with the asserted entities, then whatever identity
/// adjudications the caller supplies, then fold.
fn store_with(name: &str, entities: &[&str], extra: &[IdentityAdjudication]) -> WorldStore {
    let path = tmp(name);
    let mut s = WorldStore::open(&path).expect("open");
    let mut all: Vec<IdentityAdjudication> = entities
        .iter()
        .map(|e| IdentityAdjudication::Assert(AssertIdentity::new(eid(e), just(), at(1))))
        .collect();
    all.extend(extra.iter().cloned());

    for (i, a) in all.iter().enumerate() {
        let event_id = EventId::new(format!("ev-{i}")).expect("event id");
        let observation_id = ObservationId::new(format!("obs-src-{i}")).expect("obs");
        s.append_adjudication(
            &AdjudicationRow {
                event_id: &event_id,
                observation_id: &observation_id,
                txn_time_ms: T0 + i as i64,
                valid_from_ms: T0 + i as i64,
                source: "operator-console",
                source_version: "1.0.0",
                writer_class: WriterClass::Operator,
            },
            a,
        )
        .expect("append");
    }
    s.fold_entity_projection().expect("fold");
    s
}

fn resolved(s: &WorldStore, id: &str) -> ResolutionOutcome {
    let view = s.identity_view().expect("identity view");
    resolve(&view, &eid(id))
}

// ---------------------------------------------------------------------------
// The acceptance proof
// ---------------------------------------------------------------------------

/// **The load-bearing test.** The same log, with and without the promotion,
/// resolves `b` to two different entities.
///
/// Without it, `b` is itself. With it, `b` is `a` — because the confirmed pair
/// promoted to a merge the projection folds and the resolver walks. Nothing
/// here asserts that a module is reachable.
#[test]
fn a_confirmed_same_as_changes_what_b_resolves_to() {
    let promoted = promote_confirmed_same_as(&numbered([decide(
        &candidate("a", "b"),
        Outcome::Promoted,
        5,
    )]))
    .expect("promotion");
    assert_eq!(promoted.len(), 1, "one confirmed pair, one merge");

    // WITHOUT the promotion: `b` is its own entity.
    let before = store_with("before", &["a", "b"], &[]);
    assert!(
        matches!(resolved(&before, "b"), ResolutionOutcome::Located { ref entity, .. } if entity == &eid("b")),
        "unpromoted, b must resolve to itself: {:?}",
        resolved(&before, "b")
    );

    // WITH it: `b` resolves to `a`.
    let after = store_with("after", &["a", "b"], &promoted);
    match resolved(&after, "b") {
        ResolutionOutcome::Located { entity, .. } => {
            assert_eq!(entity, eid("a"), "the confirmed pair must redirect b to a")
        }
        other => panic!("expected b to resolve to a, got {other:?}"),
    }
}

/// **Rejection is not promotion.** A pair the operator refused must leave
/// identity untouched — otherwise "promote" would mean "adjudicated at all",
/// and `Outcome` would be decoration.
#[test]
fn a_rejected_pair_does_not_move_identity() {
    let promoted = promote_confirmed_same_as(&numbered([
        decide(&candidate("c", "d"), Outcome::Rejected, 5),
        decide(&candidate("e", "f"), Outcome::Unresolved, 6),
    ]))
    .expect("promotion");
    assert!(
        promoted.is_empty(),
        "neither outcome promotes: {promoted:?}"
    );

    let s = store_with("rejected", &["c", "d"], &promoted);
    assert!(
        matches!(resolved(&s, "d"), ResolutionOutcome::Located { ref entity, .. } if entity == &eid("d")),
        "a rejected pair must leave d as itself: {:?}",
        resolved(&s, "d")
    );
}

/// **The merge direction is canonical, not adjudication order.**
///
/// The same pair adjudicated from either side promotes identically, because
/// `CandidatePair` is canonical by construction. Box 2c requires the same log to
/// yield the same identity with no dependence on tie-break order; this is that
/// requirement as a test rather than a comment.
#[test]
fn promotion_does_not_depend_on_which_side_was_named_first() {
    let one = promote_confirmed_same_as(&numbered([decide(
        &candidate("a", "b"),
        Outcome::Promoted,
        5,
    )]))
    .expect("promotion");
    let other = promote_confirmed_same_as(&numbered([decide(
        &candidate("b", "a"),
        Outcome::Promoted,
        5,
    )]))
    .expect("promotion");
    assert_eq!(
        one, other,
        "canonical pairing must make the order irrelevant"
    );
}

/// **Provenance is carried, not invented.** The merge cites what the
/// adjudication cited — the evidence a `Related` query and box 4b's provenance
/// walk will later have to stand on.
#[test]
fn the_promoted_merge_cites_the_adjudications_evidence() {
    let adj = SameAsAdjudication::record(
        candidate("a", "b").pair().clone(),
        obs(CANDIDATE_OBS),
        vec![obs(OBS), obs(OBS2)],
        operator(),
        Outcome::Promoted,
        at(5),
    )
    .expect("adjudication");

    let promoted = promote_confirmed_same_as(&numbered([adj])).expect("promotion");
    let IdentityAdjudication::Merge(m) = &promoted[0] else {
        panic!("expected a merge, got {:?}", promoted[0]);
    };
    let cited: Vec<&str> = m
        .justification()
        .observations()
        .iter()
        .map(kirra_world::reference::ObservationId::as_str)
        .collect();
    assert_eq!(
        cited,
        vec![OBS, OBS2],
        "the merge must carry the adjudication's own citations, in order"
    );
}

/// **Re-affirming a pair does not move when the identity began.**
///
/// Two promotions of one pair yield ONE merge, stamped with the earliest
/// decision. A later re-affirmation is corroboration, not a new beginning.
#[test]
fn re_affirming_a_pair_keeps_the_earliest_beginning() {
    let c = candidate("a", "b");
    let promoted = promote_confirmed_same_as(&numbered([
        decide(&c, Outcome::Promoted, 90),
        decide(&c, Outcome::Promoted, 20),
    ]))
    .expect("promotion");

    assert_eq!(promoted.len(), 1, "one pair, one merge: {promoted:?}");
    assert_eq!(
        promoted[0].at(),
        at(20),
        "the earliest promotion is when the identity began"
    );
}

/// **A pair promoted and then rejected is NOT promoted** — the ruling, at 2c.
///
/// This test used to assert the opposite, under the name
/// `promoted_then_rejected_still_promotes_today_which_is_a_ruling_2b_owes`. It
/// existed to record a behaviour nobody endorsed and to fail the moment someone
/// ruled, so the ruling could not be made by accident. It has now done exactly
/// that: `KIRRA-WM-ADJUDICATION-PRECEDENCE-001` adopted latest-decision-wins,
/// this test broke, and its expectation was changed deliberately rather than
/// deleted to make a build green.
///
/// 2c gets this for free — `promote_confirmed_same_as` asks
/// `promotions_in_effect` rather than filtering on `Outcome::Promoted` itself,
/// which is why one ruling moved both layers.
#[test]
fn a_promotion_the_operator_withdrew_does_not_reach_the_identity_path() {
    let c = candidate("a", "b");
    let promoted = promote_confirmed_same_as(&numbered([
        decide(&c, Outcome::Promoted, 10),
        decide(&c, Outcome::Rejected, 20),
    ]))
    .expect("promotion");

    assert!(
        promoted.is_empty(),
        "the newest authorized decision governs, so no merge is emitted: {promoted:?}"
    );
}

/// **A withdrawal RESETS when the identity began.**
///
/// The second half of the ruling, and the one a latest-decision-wins rule does
/// not settle on its own. `promote_confirmed_same_as` dates an identity from
/// the EARLIEST promotion in its current unbroken run — so re-affirming a pair
/// does not move its start date, while a rejection followed by a fresh
/// promotion does.
///
/// Without this, "earliest promotion wins" would reach back across a withdrawal
/// and date the identity from a decision the operator had already un-made.
#[test]
fn a_re_promotion_after_a_withdrawal_is_dated_from_the_new_run() {
    let c = candidate("a", "b");
    let promoted = promote_confirmed_same_as(&numbered([
        decide(&c, Outcome::Promoted, 10),
        decide(&c, Outcome::Rejected, 20),
        decide(&c, Outcome::Promoted, 30),
        decide(&c, Outcome::Promoted, 40),
    ]))
    .expect("promotion");

    assert_eq!(promoted.len(), 1);
    let IdentityAdjudication::Merge(merge) = &promoted[0] else {
        panic!("expected a merge, got {:?}", promoted[0]);
    };
    assert_eq!(
        merge.at(),
        at(30),
        "the identity began at the first promotion of the CURRENT run, not the \
         withdrawn one at 10 and not the re-affirmation at 40"
    );
}

/// Persist one candidate, then promote it and reject it, through the REAL doors.
///
/// Returns the generations of the two adjudications, which are the T1 and T2
/// coordinates the historical query is asked at. Taken from the store rather
/// than counted, because generations are not dense once compaction has run.
/// Attach generations in recorded order.
///
/// Every call reads as a HISTORY rather than a bag now, which is the visible
/// consequence of `KIRRA-WM-ADJUDICATION-PRECEDENCE-001`: the order decisions
/// were recorded in is part of the question, so a caller cannot ask without
/// saying what that order was.
fn numbered(
    records: impl IntoIterator<Item = SameAsAdjudication>,
) -> Vec<(i64, SameAsAdjudication)> {
    records
        .into_iter()
        .enumerate()
        .map(|(i, a)| (i as i64 + 1, a))
        .collect()
}

fn promote_then_reject(store: &mut WorldStore) -> (i64, i64) {
    use kirra_world::observation::{Confidence, ConfidenceBasis, SourceClass};
    use kirra_world::same_as_candidate::{MatcherIdentity, SameAsCandidate};
    use kirra_world_store::candidate_record::CandidateRow;
    use kirra_world_store::same_as_adjudication_record::SameAsAdjudicationRequest;

    let cand_event = EventId::new("cand-ev-1").expect("id");
    let cand_obs = ObservationId::new("cand-obs-1").expect("id");
    let proposal = SameAsCandidate::propose(
        CandidatePair::new(eid("a"), eid("b")).expect("distinct"),
        MatcherIdentity::new("world-ingest", "exact-identifier", "1.0.0").expect("matcher"),
        Confidence::new(None, ConfidenceBasis::Unspecified, None).expect("confidence"),
        vec![obs(OBS)],
    )
    .expect("candidate");
    store
        .append_same_as_candidate(
            &CandidateRow {
                event_id: &cand_event,
                observation_id: &cand_obs,
                txn_time_ms: 1,
                valid_from_ms: 1,
                source: "world-ingest",
                source_version: "1.0.0",
            },
            &proposal,
        )
        .expect("persist the candidate");

    let mut decide_through_the_door = |tag: &str, outcome: Outcome, ms: u64| -> i64 {
        let event_id = EventId::new(format!("adj-ev-{tag}")).expect("id");
        let observation_id = ObservationId::new(format!("adj-obs-{tag}")).expect("id");
        store
            .adjudicate_same_as(&SameAsAdjudicationRequest {
                event_id: &event_id,
                observation_id: &observation_id,
                candidate_observation_id: "cand-obs-1",
                cited: vec![obs(OBS)],
                authority: AdjudicationAuthority::new(SourceClass::Operator, "console-operator")
                    .expect("authority"),
                outcome,
                decided_at: at(ms),
                txn_time_ms: ms as i64,
                source: "operator-console",
                source_version: "1.0.0",
            })
            .expect("an operator judging a persisted candidate");
        store.head_generation_for_test().expect("head")
    };

    let t1 = decide_through_the_door("1", Outcome::Promoted, 10);
    let t2 = decide_through_the_door("2", Outcome::Rejected, 20);
    (t1, t2)
}

/// **THE CONFORMANCE PROOF — both precedence rules, one answer.**
///
/// `KIRRA-WM-ADJUDICATION-PRECEDENCE-001` replaced a contradiction. Until it
/// was ruled, two deterministic readings of one operator history disagreed:
///
/// | Rule | Promoted-then-rejected (before the ruling) |
/// |---|---|
/// | `confirmed_relations` (2c, pure) | still confirmed — one `Promoted` anywhere confirmed forever |
/// | `relationship_projection::fold_all` (5a) | withdrawn — the latest decision governs |
///
/// The ruling adopted the second reading and made both sides derive from it:
/// the EFFECT half is `leaves_pair_related`, called by both, so it cannot drift
/// at all. The ORDERING half cannot be shared — one side folds incrementally
/// and the other walks a whole history — so it is proven here instead, over a
/// corpus chosen so each row discriminates something.
///
/// This test replaces the pin that used to assert the two DISAGREED. That pin
/// did its job: it made whoever ruled confront both halves at once.
#[test]
fn both_precedence_rules_agree_on_every_history_in_the_corpus() {
    use kirra_world::same_as_adjudication::confirmed_relations;
    use kirra_world_store::relationship_projection;
    use kirra_world_store::same_as_adjudication_record::StoredAdjudication;

    // Each row is (label, the decisions in RECORDED order, does the pair end up
    // related). The `related` column IS the ruling, written as data.
    let corpus: &[(&str, &[Outcome], bool)] = &[
        ("promoted", &[Outcome::Promoted], true),
        ("rejected", &[Outcome::Rejected], false),
        ("unresolved", &[Outcome::Unresolved], false),
        // The historic disagreement, now agreed.
        (
            "promoted_then_rejected",
            &[Outcome::Promoted, Outcome::Rejected],
            false,
        ),
        (
            "promoted_then_unresolved",
            &[Outcome::Promoted, Outcome::Unresolved],
            false,
        ),
        // Order matters, and this row is what proves it: the same two decisions
        // as the row above, reversed, give the opposite answer. A rule that
        // ignored order entirely would answer both identically.
        (
            "rejected_then_promoted",
            &[Outcome::Rejected, Outcome::Promoted],
            true,
        ),
        (
            "promoted_rejected_repromoted",
            &[Outcome::Promoted, Outcome::Rejected, Outcome::Promoted],
            true,
        ),
        (
            "promoted_twice",
            &[Outcome::Promoted, Outcome::Promoted],
            true,
        ),
        (
            "promoted_rejected_then_unresolved",
            &[Outcome::Promoted, Outcome::Rejected, Outcome::Unresolved],
            false,
        ),
    ];

    let c = candidate("a", "b");
    for (label, outcomes, expected_related) in corpus {
        let history: Vec<SameAsAdjudication> = outcomes
            .iter()
            .enumerate()
            .map(|(i, o)| decide(&c, *o, 10 + i as u64))
            .collect();
        let numbered: Vec<(i64, &SameAsAdjudication)> = history
            .iter()
            .enumerate()
            .map(|(i, a)| (i as i64 + 1, a))
            .collect();

        // Side 1 -- the domain's whole-history walk.
        let domain_related = !confirmed_relations(numbered.iter().copied()).is_empty();

        // Side 2 -- the store's incremental fold, over the SAME order.
        let stored: Vec<StoredAdjudication> = history
            .iter()
            .map(|a| StoredAdjudication {
                pair: a.pair().clone(),
                outcome: a.outcome(),
                candidate_observation_id: a.candidate_observation_id().as_str().to_owned(),
                adjudicator: a.authority().adjudicator().to_owned(),
                decided_at: a.decided_at(),
            })
            .collect();
        let folded = relationship_projection::fold_all(
            stored.iter().enumerate().map(|(i, d)| (i as i64 + 1, d)),
        );
        let store_related = !folded.is_empty();

        // Both must match the RULING, not merely each other. Asserting only
        // agreement would pass if both sides were wrong in the same direction,
        // which is exactly what a shared helper makes easy to do.
        assert_eq!(
            domain_related, *expected_related,
            "{label}: the domain rule disagrees with the ruling"
        );
        assert_eq!(
            store_related, *expected_related,
            "{label}: the store fold disagrees with the ruling"
        );
        assert_eq!(
            domain_related, store_related,
            "{label}: the two rules disagree with each other"
        );
    }
}

/// **T1/T2 — the ruling changes the present without erasing the past.**
///
/// The acceptance shape the ruling was scoped to:
///
/// ```text
///   T1  Promote(A,B)          -> current: related
///   T2  Reject(A,B)           -> current: NOT related  (the ruled precedence)
///       query as of T1        -> STILL related         (history is intact)
/// ```
///
/// The third line is the one that makes the ruling honest rather than merely
/// decisive. Precedence reorders what holds NOW; a promotion that genuinely
/// held between T1 and T2 must still be visible at a coordinate inside that
/// window, or the ruling would be rewriting the record instead of reading it.
#[test]
fn precedence_governs_the_present_and_leaves_the_past_intact() {
    let path = tmp("t1-t2");
    let mut store = WorldStore::open(&path).expect("open");
    let (t1, t2) = promote_then_reject(&mut store);

    // --- the present, under the ruled precedence ------------------------
    store.fold_relationship_projection().expect("fold");
    assert!(
        store
            .load_relationship_projection()
            .expect("load")
            .is_empty(),
        "T2: the newest authorized decision governs, so the pair is withdrawn"
    );

    // --- the past, unchanged --------------------------------------------
    let at_t1 = store.relationships_in_effect_at(t1).expect("historical");
    assert_eq!(
        at_t1.len(),
        1,
        "as of T1 the promotion held, and the ruling must not erase that"
    );
    let at_t2 = store.relationships_in_effect_at(t2).expect("historical");
    assert!(
        at_t2.is_empty(),
        "as of T2 the rejection had landed, so the pair is withdrawn there too"
    );

    drop(store);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// The refusal branch, and the boundary of the determinism claim
// ---------------------------------------------------------------------------

/// **Two clocks cannot be ordered, so the promotion refuses instead of guessing.**
///
/// One pair confirmed twice, once off the system clock and once off the boundary
/// clock. "Earliest promotion wins" needs an ordering, and `DomainInstant`
/// refuses to supply one across domains — comparing raw milliseconds from two
/// clocks is not imprecise, it is meaningless. So this returns
/// `PromotionDomainsDiffer` rather than stamping the identity with whichever
/// number happened to be smaller.
///
/// Paired with `..._is_only_the_domains` below, which holds everything else
/// constant and swaps the domain back: without that twin, this test would still
/// pass if the promotion started refusing every re-affirmation for some
/// unrelated reason.
#[test]
fn a_cross_domain_re_affirmation_is_refused_rather_than_ordered() {
    let c = candidate("a", "b");
    let err = promote_confirmed_same_as(&numbered([
        decide_on(&c, Outcome::Promoted, 90, ClockDomain::System),
        decide_on(&c, Outcome::Promoted, 20, ClockDomain::Boundary),
    ]))
    .expect_err("two clock domains must not be ordered");

    let AdjudicationError::PromotionDomainsDiffer { pair } = err else {
        panic!("expected PromotionDomainsDiffer, got {err:?}");
    };
    assert_eq!(
        (pair.low().as_str(), pair.high().as_str()),
        ("a", "b"),
        "the refusal must name the pair it could not date"
    );
}

/// **The non-vacuity twin.** The same two decisions, same instants, same order —
/// only both now read off one clock — promote normally, at the earlier of the
/// two.
///
/// This is what makes the refusal above attributable to the DOMAIN rather than
/// to re-affirmation, to the 90-then-20 ordering, or to anything else the two
/// tests share.
#[test]
fn the_only_thing_that_refuses_it_is_only_the_domains() {
    let c = candidate("a", "b");
    let promoted = promote_confirmed_same_as(&numbered([
        decide_on(&c, Outcome::Promoted, 90, ClockDomain::Boundary),
        decide_on(&c, Outcome::Promoted, 20, ClockDomain::Boundary),
    ]))
    .expect("one clock domain orders fine");

    assert_eq!(promoted.len(), 1, "one pair, one merge: {promoted:?}");
    assert_eq!(
        promoted[0].at(),
        DomainInstant {
            ms: 20,
            domain: ClockDomain::Boundary
        },
        "the earliest promotion is still when the identity began"
    );
}

/// **Arrangement changes provenance order, never identity.**
///
/// The precise boundary of the determinism claim in `promote_confirmed_same_as`,
/// pinned so the corrected comment there is checkable rather than asserted.
/// Two pairs, fed in both orders:
///
/// * the merge LIST is identical — same pairs, same directions, same order,
///   because `confirmed_relations` yields them through a `BTreeSet`;
/// * so a caller asking "what does `b` resolve to" gets the same answer either
///   way, which is the half box 2c actually requires.
///
/// Citation order within a merge is the half that does follow arrangement, and
/// `Justification` preserves recorded order deliberately. That is why this test
/// asserts equality of the whole merge list here — with one citation per
/// adjudication there is nothing for arrangement to reorder — rather than
/// claiming a set-determinism the function does not have.
#[test]
fn feeding_the_same_adjudications_in_either_order_yields_the_same_merges() {
    let ab = decide(&candidate("a", "b"), Outcome::Promoted, 5);
    let cd = decide(&candidate("c", "d"), Outcome::Promoted, 7);

    let forward =
        promote_confirmed_same_as(&numbered([ab.clone(), cd.clone()])).expect("promotion");
    let reversed = promote_confirmed_same_as(&numbered([cd, ab])).expect("promotion");

    assert_eq!(forward.len(), 2, "two pairs, two merges: {forward:?}");
    assert_eq!(
        forward, reversed,
        "the merge list must not depend on the arrangement of the input"
    );
}
