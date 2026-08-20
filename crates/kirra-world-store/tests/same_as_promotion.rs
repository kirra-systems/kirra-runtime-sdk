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
    let promoted = promote_confirmed_same_as(&[decide(&candidate("a", "b"), Outcome::Promoted, 5)])
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
    let promoted = promote_confirmed_same_as(&[
        decide(&candidate("c", "d"), Outcome::Rejected, 5),
        decide(&candidate("e", "f"), Outcome::Unresolved, 6),
    ])
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
    let one = promote_confirmed_same_as(&[decide(&candidate("a", "b"), Outcome::Promoted, 5)])
        .expect("promotion");
    let other = promote_confirmed_same_as(&[decide(&candidate("b", "a"), Outcome::Promoted, 5)])
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

    let promoted = promote_confirmed_same_as(&[adj]).expect("promotion");
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
    let promoted = promote_confirmed_same_as(&[
        decide(&c, Outcome::Promoted, 90),
        decide(&c, Outcome::Promoted, 20),
    ])
    .expect("promotion");

    assert_eq!(promoted.len(), 1, "one pair, one merge: {promoted:?}");
    assert_eq!(
        promoted[0].at(),
        at(20),
        "the earliest promotion is when the identity began"
    );
}

/// **A pair promoted and then rejected is STILL promoted today.** Recorded, not
/// endorsed.
///
/// `confirmed_relations` filters `is_confirmed()` across every record and
/// applies no precedence, so one `Promoted` anywhere in the history confirms the
/// pair however many rejections follow. Box 2c consumes that verdict rather than
/// re-deriving it — whether a re-adjudicated pair stays confirmed is box 2b's
/// question, and answering it here would create a second answer that drifts.
///
/// This test exists so the behaviour cannot change by accident before someone
/// rules on it. If last-write-wins is the intent, this test is the one that
/// should fail first, and it should be changed deliberately in 2b.
#[test]
fn promoted_then_rejected_still_promotes_today_which_is_a_ruling_2b_owes() {
    let c = candidate("a", "b");
    let promoted = promote_confirmed_same_as(&[
        decide(&c, Outcome::Promoted, 10),
        decide(&c, Outcome::Rejected, 20),
    ])
    .expect("promotion");

    assert_eq!(
        promoted.len(),
        1,
        "today a later rejection does not un-confirm: {promoted:?}"
    );
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
    let err = promote_confirmed_same_as(&[
        decide_on(&c, Outcome::Promoted, 90, ClockDomain::System),
        decide_on(&c, Outcome::Promoted, 20, ClockDomain::Boundary),
    ])
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
    let promoted = promote_confirmed_same_as(&[
        decide_on(&c, Outcome::Promoted, 90, ClockDomain::Boundary),
        decide_on(&c, Outcome::Promoted, 20, ClockDomain::Boundary),
    ])
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

    let forward = promote_confirmed_same_as(&[ab.clone(), cd.clone()]).expect("promotion");
    let reversed = promote_confirmed_same_as(&[cd, ab]).expect("promotion");

    assert_eq!(forward.len(), 2, "two pairs, two merges: {forward:?}");
    assert_eq!(
        forward, reversed,
        "the merge list must not depend on the arrangement of the input"
    );
}
