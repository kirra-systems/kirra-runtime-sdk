//! **Box 5b: `Related` through `QueryEngine`, over the real production chain.**
//!
//! The projection landed in 5a with no way for production code to ask it
//! anything through the sanctioned door. This closes that: a typed request, one
//! `QueryEngine::execute`, an answer.
//!
//! Every relationship asserted about here was proposed by the REAL matcher
//! (`run_ingest_pass`) and promoted by a real `AdjudicationAuthority`. Nothing
//! is hand-appended. A query proven only against fixtures would prove it works
//! on data no producer can create — the exact gap the pre-5a chain audit found
//! at the other end of this same chain.

use kirra_world::observation::{ClockDomain, DomainInstant, SourceClass};
use kirra_world::reference::{EventId, ObservationId};
use kirra_world::same_as_adjudication::{AdjudicationAuthority, Outcome};
use kirra_world::same_as_candidate::MatcherIdentity;
use kirra_world_ingest::{run_ingest_pass, ExactIdentifierRule};
use kirra_world_service::freshness::{FreshnessPolicy, FreshnessSource};
use kirra_world_service::query::{QueryEngine, Related};
use kirra_world_service::read_view::AskError;
use kirra_world_store::same_as_adjudication_record::SameAsAdjudicationRequest;
use kirra_world_store::{ClaimStatus, NewEvent, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;
const SURVEY_LIMIT: usize = 1_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-5b-q-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    for s in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(s);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

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

fn decide(store: &mut WorldStore, tag: &str, candidate: &str, outcome: Outcome, ms: u64) {
    let event_id = EventId::new(format!("adj-ev-{tag}")).expect("id");
    let observation_id = ObservationId::new(format!("adj-obs-{tag}")).expect("id");
    store
        .adjudicate_same_as(&SameAsAdjudicationRequest {
            event_id: &event_id,
            observation_id: &observation_id,
            candidate_observation_id: candidate,
            cited: vec![ObservationId::new(candidate).expect("id")],
            authority: AdjudicationAuthority::new(SourceClass::Operator, "console-operator")
                .expect("authority"),
            outcome,
            decided_at: DomainInstant {
                ms,
                domain: ClockDomain::System,
            },
            txn_time_ms: T0 + 10,
            source: "operator-console",
            source_version: "1.0.0",
        })
        .expect("an operator judging a persisted candidate");
}

/// Two tracks agreeing on a serial, the matcher's candidate, and one promotion.
fn store_with_a_promoted_pair(name: &str) -> WorldStore {
    let mut store = WorldStore::open(&tmp(name)).expect("open");
    observe(&mut store, "a", "track-a", "SN-1");
    observe(&mut store, "b", "track-b", "SN-1");
    assert_eq!(
        propose(&mut store),
        1,
        "the fixture must persist one candidate"
    );
    decide(
        &mut store,
        "1",
        "cand-obs-1",
        Outcome::Promoted,
        T0 as u64 + 10,
    );
    store.fold_relationship_projection().expect("fold");
    store
}

// ---------------------------------------------------------------------------
// The positive case first.
// ---------------------------------------------------------------------------

/// **The whole chain, asked through the sanctioned door.**
///
/// Sensor observations → the real matcher's candidate → an operator's promotion
/// → the projection → `QueryEngine::execute(Related { .. })`. The answer names
/// the other entity and the decision that put it there.
#[test]
fn a_promoted_relationship_is_reachable_through_the_query_engine() {
    let store = store_with_a_promoted_pair("reachable");
    let engine = QueryEngine::new(&store, FreshnessSource::Ruled);

    let answer = engine
        .execute(Related {
            entity: "track-a".to_string(),
        })
        .expect("a promoted pair must be askable");

    assert_eq!(answer.neighbours().len(), 1);
    let n = &answer.neighbours()[0];
    assert_eq!(n.other.as_str(), "track-b");
    assert_eq!(
        n.relationship.candidate_observation_id, "cand-obs-1",
        "the answer must carry WHICH evidence the decision rested on"
    );
    assert_eq!(n.relationship.adjudicator, "console-operator");
    assert!(!answer.is_truncated());
}

/// **Either side of the canonical pair reaches the other.**
///
/// The store-level twin of this lives in `related_read.rs`; this one proves the
/// property survives the whole query path, since a family method could easily
/// pass only the subject it was handed to the `low` column.
#[test]
fn the_query_finds_the_pair_from_either_side() {
    let store = store_with_a_promoted_pair("either");
    let engine = QueryEngine::new(&store, FreshnessSource::Ruled);

    for (asked, expected) in [("track-a", "track-b"), ("track-b", "track-a")] {
        let answer = engine
            .execute(Related {
                entity: asked.to_string(),
            })
            .expect("askable");
        assert_eq!(answer.neighbours().len(), 1, "asking about {asked}");
        assert_eq!(answer.neighbours()[0].other.as_str(), expected);
    }
}

/// **An entity related to nothing answers EMPTY, not an error.**
///
/// The non-vacuity control: without it, a family that errored on every lookup
/// would still pass the positive test only if it happened to succeed there.
#[test]
fn an_unrelated_entity_answers_empty() {
    let store = store_with_a_promoted_pair("unrelated");
    let engine = QueryEngine::new(&store, FreshnessSource::Ruled);

    let answer = engine
        .execute(Related {
            entity: "track-zzz".to_string(),
        })
        .expect("a question about an unrelated entity is still a question");
    assert!(answer.neighbours().is_empty());
}

/// **An unaskable question is REFUSED, not answered as unrelated.**
///
/// "That is not an entity id" and "that entity is related to nothing" are
/// different findings, and a caller told the second would conclude the entity
/// exists. The same distinction `AdjudicateError` draws between
/// `NoSuchCandidate` and `NotACandidate`.
#[test]
fn a_malformed_entity_is_refused_rather_than_answered_as_unrelated() {
    let store = store_with_a_promoted_pair("malformed");
    let engine = QueryEngine::new(&store, FreshnessSource::Ruled);

    match engine.execute(Related {
        entity: String::new(),
    }) {
        Err(AskError::MalformedEntity { entity, .. }) => assert_eq!(entity, ""),
        other => panic!("an empty entity id must be refused: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// WHICH precedence rule this family serves.
// ---------------------------------------------------------------------------

/// **A withdrawn promotion is no longer related.**
///
/// The load-bearing test of this box, because it is what makes the family's
/// source of truth OBSERVABLE rather than merely documented.
///
/// Two precedence rules exist today and they disagree:
///
/// * `confirmed_relations` (2c) applies none — one `Promoted` anywhere confirms
///   the pair forever, so it would still call this pair the same.
/// * the relationship projection (5a) lets the newest authorized decision
///   govern, so a later rejection withdraws it.
///
/// `Related` reads the projection. A future refactor that "simplified" it onto
/// `confirmed_relations` would keep every other test in this file green and
/// fail here — which is the whole reason this one exists rather than a comment
/// saying which function is called.
#[test]
fn a_withdrawn_promotion_is_no_longer_related() {
    let mut store = store_with_a_promoted_pair("withdrawn");
    {
        let engine = QueryEngine::new(&store, FreshnessSource::Ruled);
        assert_eq!(
            engine
                .execute(Related {
                    entity: "track-a".to_string()
                })
                .expect("askable")
                .neighbours()
                .len(),
            1,
            "the pair must be related BEFORE the rejection, or this proves nothing"
        );
    }

    decide(
        &mut store,
        "2",
        "cand-obs-1",
        Outcome::Rejected,
        T0 as u64 + 20,
    );
    store.fold_relationship_projection().expect("re-fold");

    let engine = QueryEngine::new(&store, FreshnessSource::Ruled);
    assert!(
        engine
            .execute(Related {
                entity: "track-a".to_string()
            })
            .expect("askable")
            .neighbours()
            .is_empty(),
        "the newest authorized decision governs — Related serves the projection, \
         not confirmed_relations"
    );
}

// ---------------------------------------------------------------------------
// Freshness and semantics.
// ---------------------------------------------------------------------------

/// **The ruled `Timeless` disposition is RESOLVED, not assumed.**
///
/// `KIRRA-WM-IDENTITY-FRESHNESS-001` ruled an adjudicated identity `Timeless`.
/// The family asks `resolve_policy` for that rather than hard-coding it, so
/// deleting the ruled row reds this family instead of leaving it quietly
/// serving a policy nothing states.
#[test]
fn the_ruled_timeless_policy_is_resolved_and_served() {
    let store = store_with_a_promoted_pair("policy");
    let engine = QueryEngine::new(&store, FreshnessSource::Ruled);
    let answer = engine
        .execute(Related {
            entity: "track-a".to_string(),
        })
        .expect("askable");
    assert_eq!(answer.policy(), FreshnessPolicy::Timeless);
}

/// **A caller who states a policy governs, exactly as elsewhere.**
///
/// The twin of the test above. Without it, a family that ignored the source
/// entirely and always returned `Timeless` would pass that one.
#[test]
fn a_caller_stated_policy_is_honoured() {
    let store = store_with_a_promoted_pair("caller-policy");
    let engine = QueryEngine::new(
        &store,
        FreshnessSource::Caller(FreshnessPolicy::Bounded { max_age_ms: 5_000 }),
    );
    let answer = engine
        .execute(Related {
            entity: "track-a".to_string(),
        })
        .expect("askable");
    assert_eq!(
        answer.policy(),
        FreshnessPolicy::Bounded { max_age_ms: 5_000 },
        "the freshness source must actually be consulted"
    );
}

/// **`Related` depends on exactly one rule.**
///
/// The three that are absent are the point: it never folds `world_current`,
/// never resolves identity, and has no claim to grade. A set that accumulated
/// rules the family does not use would make recorded answers refuse to replay
/// for reasons unrelated to what produced them.
#[test]
fn the_related_query_depends_on_exactly_one_rule() {
    let store = store_with_a_promoted_pair("semantics");
    let engine = QueryEngine::new(&store, FreshnessSource::Ruled);
    let answer = engine
        .execute(Related {
            entity: "track-a".to_string(),
        })
        .expect("askable");

    let rules: Vec<&str> = answer
        .semantics()
        .entries()
        .iter()
        .map(|r| r.rule.as_str())
        .collect();
    assert_eq!(rules, ["relationship_fold"], "got {rules:?}");
}
