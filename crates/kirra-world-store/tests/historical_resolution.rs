//! **Tier 2 box 2d — resolving identity as of a past instant.** §6.3, §14.2.
//!
//! §6.3: *"A query at a past instant resolves identity as it was adjudicated
//! then — because identity is a projection like everything else."*
//!
//! The cheap wrong implementation of that sentence resolves against **current**
//! state and labels the answer with a timestamp. It passes any test that only
//! checks the answer's shape, and it is wrong in exactly the way that matters:
//! it reports what an id means now under the name of what it meant then. Every
//! test below is chosen to fail against that implementation.
//!
//! The load-bearing one is `a_query_between_two_merges_stops_at_the_first`: with
//! `a = b` recorded, then later `b = c`, a query at the instant between them
//! must answer `b` and must not reach `c`. Resolving current state answers `c`
//! whatever instant it is handed, so that single assertion separates a real
//! temporal restriction from a decorated present-tense one.
//!
//! What is deliberately NOT re-tested here: the resolver's own semantics. There
//! is one engine — `kirra_world::resolution::resolve` — and it is walked
//! exhaustively by its own unit tests. These tests check that the *graph handed
//! to it* is the historical one, plus the two places 2d could smuggle in a
//! second set of rules (`KIRRA-WM-CLUSTERING-001` candidates, and the
//! degradation contract).

use kirra_world::adjudication::{
    AssertIdentity, ForgetEntity, IdentityAdjudication, Justification, MergeEntities,
    RetirementReason, SplitEntity,
};
use kirra_world::observation::{ClockDomain, DomainInstant};
use kirra_world::resolution::{resolve, ResolutionOutcome};
use kirra_world_store::adjudication_record::AdjudicationRow;
use kirra_world_store::{
    ClaimStatus, EntityId, EventId, NewEvent, ObservationId, Resolution, WorldStore, WriterClass,
};

/// The instant the first adjudication is recorded at. Every `t(n)` below is an
/// offset from it, so a test names *when* rather than a bare epoch number.
const T0: i64 = 1_700_000_000_000;

/// Transaction time of the `n`th recorded adjudication.
fn t(n: i64) -> i64 {
    T0 + n
}

fn eid(s: &str) -> EntityId {
    EntityId::new(s).expect("entity id")
}

fn just() -> Justification {
    Justification::new([ObservationId::new("obs-1").expect("obs")]).expect("justification")
}

fn at() -> DomainInstant {
    DomainInstant {
        ms: 1,
        domain: ClockDomain::System,
    }
}

fn assert_id(e: &str) -> IdentityAdjudication {
    IdentityAdjudication::Assert(AssertIdentity::new(eid(e), just(), at()))
}

fn merge(sources: &[&str], into: &str) -> IdentityAdjudication {
    IdentityAdjudication::Merge(
        MergeEntities::new(
            sources.iter().map(|s| eid(s)).collect::<Vec<_>>(),
            eid(into),
            just(),
            at(),
        )
        .expect("merge"),
    )
}

fn partition(source: &str, into: &[&str]) -> IdentityAdjudication {
    IdentityAdjudication::Split(
        SplitEntity::partition(
            eid(source),
            into.iter().map(|s| eid(s)).collect::<Vec<_>>(),
            just(),
            at(),
        )
        .expect("partition"),
    )
}

fn forget(e: &str) -> IdentityAdjudication {
    IdentityAdjudication::Forget(ForgetEntity::new(
        eid(e),
        RetirementReason::new("superseded by adjudication").expect("reason"),
        just(),
        at(),
    ))
}

/// Remove the database AND its WAL sidecars — a surviving `-wal` beside a
/// recreated database of the same name is a recovery source, which can make a
/// later run see rows it never wrote.
fn clean(p: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
}

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-wm-2d-{}-{}.sqlite",
        name,
        std::process::id()
    ));
    clean(&p);
    p
}

/// Append adjudications, the `n`th recorded at transaction time `t(n)`.
///
/// One per millisecond so a test can name an instant strictly between any two
/// of them, which is what the box-2d question is made of.
fn seed(s: &mut WorldStore, adjudications: &[IdentityAdjudication]) {
    for (i, a) in adjudications.iter().enumerate() {
        let i = i64::try_from(i).expect("small test index");
        let event_id = EventId::new(format!("ev-{i}")).expect("event id");
        let observation_id = ObservationId::new(format!("obs-src-{i}")).expect("obs");
        s.append_adjudication(
            &AdjudicationRow {
                event_id: &event_id,
                observation_id: &observation_id,
                txn_time_ms: t(i),
                valid_from_ms: t(i),
                source: "operator-console",
                source_version: "1.0.0",
                writer_class: WriterClass::Operator,
            },
            a,
        )
        .expect("append");
    }
    // Fold the CURRENT projection before any historical query runs.
    //
    // Not tidiness -- it is what makes these tests able to fail. A historical
    // fold seeded from the stored projection instead of from empty starts at
    // today's state and applies an old prefix on top, which is the subtlest way
    // to get 2d wrong. On a store whose projection was never folded, that bug is
    // INVISIBLE: the stored rows are empty, so seeding from them and seeding
    // from empty are the same thing. Every test here would have passed against
    // it. Found by running that exact mutation as a control.
    s.rebuild_entity_projection()
        .expect("fold current projection");
}

fn open(name: &str) -> (WorldStore, std::path::PathBuf) {
    let path = tmp(name);
    let s = WorldStore::open(&path).expect("open");
    (s, path)
}

// ---------------------------------------------------------------------------
// The property the whole slice rests on
// ---------------------------------------------------------------------------

/// **`a = b`, later `b = c`; ask between them.**
///
/// The one test that cannot pass against "resolve current state and label it
/// historical". At `t(2)` the graph knows `a -> b` and nothing more, so `a`
/// resolves to `b`. Resolving today's graph answers `c` no matter which instant
/// it is handed — so this assertion, alone among the file, distinguishes a
/// temporal restriction on the GRAPH from a present-tense answer wearing a
/// timestamp.
///
/// The `hops` are asserted too, not just the entity: an implementation that
/// resolved to `c` and then walked back to `b` would get the entity right and
/// the path wrong, and `KIRRA-WM-TRANSITIVITY-001` requires the accepted path
/// to be the one that existed at the instant.
#[test]
fn a_query_between_two_merges_stops_at_the_first() {
    let (mut s, path) = open("between-merges");
    seed(
        &mut s,
        &[
            assert_id("a"),     // t(0)
            assert_id("b"),     // t(1)
            assert_id("c"),     // t(2)
            merge(&["a"], "b"), // t(3)
            merge(&["b"], "c"), // t(4)
        ],
    );

    // Between the two merges: `a` is `b`, and `c` is not yet in the picture.
    assert_eq!(
        s.resolve_at_whole_graph(&eid("a"), t(3))
            .expect("resolve")
            .outcome,
        ResolutionOutcome::Located {
            entity: eid("b"),
            hops: 1,
        },
        "at t(3) the graph holds a->b only; reaching c means current state was \
         resolved and merely labelled historical"
    );

    // After the second: the same id now means `c`, through two hops.
    assert_eq!(
        s.resolve_at_whole_graph(&eid("a"), t(4))
            .expect("resolve")
            .outcome,
        ResolutionOutcome::Located {
            entity: eid("c"),
            hops: 2,
        }
    );

    // ...and the earlier answer is unchanged by having asked the later one.
    assert_eq!(
        s.resolve_at_whole_graph(&eid("a"), t(3))
            .expect("resolve")
            .outcome,
        ResolutionOutcome::Located {
            entity: eid("b"),
            hops: 1,
        },
        "a historical answer must not depend on what has been queried since"
    );

    drop(s);
    clean(&path);
}

/// **A merge that exists today did not exist at `t`, and must not reach back.**
///
/// The same property from the other end: query an instant *before* any merge
/// was recorded and the id must resolve to itself.
#[test]
fn a_merge_recorded_later_does_not_affect_an_earlier_instant() {
    let (mut s, path) = open("later-merge");
    seed(
        &mut s,
        &[assert_id("a"), assert_id("b"), merge(&["a"], "b")],
    );

    assert_eq!(
        s.resolve_at_whole_graph(&eid("a"), t(1))
            .expect("resolve")
            .outcome,
        ResolutionOutcome::Located {
            entity: eid("a"),
            hops: 0,
        },
        "before the merge was recorded, `a` is still `a`"
    );
    assert_eq!(
        s.resolve_at_whole_graph(&eid("a"), t(2))
            .expect("resolve")
            .outcome,
        ResolutionOutcome::Located {
            entity: eid("b"),
            hops: 1,
        },
        "and after it, `b` -- so the assertion above is not vacuous"
    );

    drop(s);
    clean(&path);
}

/// **An entity asserted after `t` is absent at `t`, not present-and-live.**
///
/// `Unknown` is the resolver's "we looked and it is not there". A historical
/// view must produce it for an id the log had not yet met, rather than
/// inventing a `Located` from a row that exists today.
#[test]
fn an_entity_asserted_after_the_instant_is_unknown_at_it() {
    let (mut s, path) = open("not-yet-born");
    seed(&mut s, &[assert_id("a"), assert_id("b")]);

    assert_eq!(
        s.resolve_at_whole_graph(&eid("b"), t(0))
            .expect("resolve")
            .outcome,
        ResolutionOutcome::Unknown,
        "`b` is asserted at t(1); at t(0) the graph has never heard of it"
    );
    assert_eq!(
        s.resolve_at_whole_graph(&eid("b"), t(1))
            .expect("resolve")
            .outcome,
        ResolutionOutcome::Located {
            entity: eid("b"),
            hops: 0,
        }
    );

    drop(s);
    clean(&path);
}

// ---------------------------------------------------------------------------
// Later records cannot rewrite an earlier answer
// ---------------------------------------------------------------------------

/// **A split after `t` does not retroactively make the earlier answer
/// ambiguous.**
///
/// `a` merges into `b`; `b` is later partitioned. Asked before the partition,
/// `a` is `b` — a single located answer. Asked after, it is ambiguous between
/// the partition's products. The earlier answer must not acquire the later
/// ambiguity.
#[test]
fn a_split_after_the_instant_does_not_reach_back_into_the_earlier_answer() {
    let (mut s, path) = open("later-split");
    seed(
        &mut s,
        &[
            assert_id("a"),                // t(0)
            assert_id("b"),                // t(1)
            merge(&["a"], "b"),            // t(2)
            partition("b", &["b1", "b2"]), // t(3)
        ],
    );

    assert_eq!(
        s.resolve_at_whole_graph(&eid("a"), t(2))
            .expect("resolve")
            .outcome,
        ResolutionOutcome::Located {
            entity: eid("b"),
            hops: 1,
        },
        "before the partition, `a` is unambiguously `b`"
    );
    assert_eq!(
        s.resolve_at_whole_graph(&eid("a"), t(3))
            .expect("resolve")
            .outcome,
        ResolutionOutcome::Ambiguous {
            successors: vec![eid("b1"), eid("b2")],
        },
        "after it, the same id is ambiguous -- so the instant is what decides"
    );

    drop(s);
    clean(&path);
}

/// **A retirement after `t` does not retire the entity at `t`** — and, because
/// retirement is not a redirect, it does not change the answer afterwards
/// either. Both halves asserted, since the second is the one that would make
/// the first look load-bearing when it was not.
#[test]
fn a_forget_after_the_instant_changes_the_state_but_never_the_earlier_answer() {
    let (mut s, path) = open("later-forget");
    seed(&mut s, &[assert_id("a"), forget("a")]);

    let before = s.identity_view_at(t(0)).expect("view");
    let after = s.identity_view_at(t(1)).expect("view");

    assert_eq!(
        before.resolve_at(&eid("a")).outcome,
        ResolutionOutcome::Located {
            entity: eid("a"),
            hops: 0,
        }
    );
    // §6.3: ForgetEntity "is not deletion" -- a retired id still answers with
    // itself, so the OUTCOME is identical either side. What differs is the
    // recorded lifecycle, which is where the instant actually shows.
    assert_eq!(
        after.resolve_at(&eid("a")).outcome,
        ResolutionOutcome::Located {
            entity: eid("a"),
            hops: 0,
        },
        "retirement is not a redirect"
    );

    use kirra_world::entity::Lifecycle;
    assert!(
        !matches!(
            before.view().get(&eid("a")).map(|e| &e.lifecycle),
            Some(Lifecycle::Retired)
        ),
        "at t(0) the entity had not been retired"
    );
    assert!(
        matches!(
            after.view().get(&eid("a")).map(|e| &e.lifecycle),
            Some(Lifecycle::Retired)
        ),
        "at t(1) it had -- without which the assertion above proves nothing"
    );

    drop(s);
    clean(&path);
}

// ---------------------------------------------------------------------------
// Agreement with the present-tense resolver
// ---------------------------------------------------------------------------

/// **`resolve_at(head)` and `resolve()` agree, over every id in a graph that
/// uses all four verbs.**
///
/// The two paths fold the same events with the same reducer, so they must not
/// be able to disagree at the head — and this is what would catch the historical
/// fold accidentally seeding itself differently, or applying a different
/// predicate, in the one case where the answer is independently known.
///
/// Every id is checked rather than a chosen one: a disagreement on a single
/// entity is exactly what a hand-picked assertion would miss.
#[test]
fn resolve_at_the_head_agrees_with_the_present_tense_resolver() {
    let (mut s, path) = open("head-agrees");
    seed(
        &mut s,
        &[
            assert_id("a"),
            assert_id("b"),
            assert_id("c"),
            merge(&["a"], "b"),
            partition("b", &["b1", "b2"]),
            merge(&["b1"], "c"),
            forget("c"),
        ],
    );

    let current = s.identity_view().expect("current view");
    let historical = s.identity_view_at(t(6)).expect("historical view");

    let ids = ["a", "b", "c", "b1", "b2", "never-existed"];
    let mut agreed_on_something_interesting = false;
    for id in ids {
        let e = eid(id);
        let now = resolve(&current, &e);
        let then = historical.resolve_at(&e).outcome;
        assert_eq!(now, then, "disagreement on `{id}`");
        if matches!(now, ResolutionOutcome::Located { hops, .. } if hops > 0) {
            agreed_on_something_interesting = true;
        }
    }
    assert!(
        agreed_on_something_interesting,
        "every id resolved to itself with zero hops, so this agreed vacuously"
    );

    drop(s);
    clean(&path);
}

/// **An instant past the head is the head**, not an error and not an empty
/// graph. A caller asking "as of now" with a wall-clock reading slightly ahead
/// of the last recorded event must get the current answer.
#[test]
fn an_instant_after_every_record_answers_as_the_head_does() {
    let (mut s, path) = open("past-head");
    seed(
        &mut s,
        &[assert_id("a"), assert_id("b"), merge(&["a"], "b")],
    );

    let current = s.identity_view().expect("current view");
    assert_eq!(
        s.resolve_at_whole_graph(&eid("a"), t(10_000))
            .expect("resolve")
            .outcome,
        resolve(&current, &eid("a"))
    );

    drop(s);
    clean(&path);
}

/// **An instant before every record is an empty graph**, so every id is
/// `Unknown` — never a panic and never today's answer.
#[test]
fn an_instant_before_every_record_knows_nothing() {
    let (mut s, path) = open("before-all");
    seed(
        &mut s,
        &[assert_id("a"), assert_id("b"), merge(&["a"], "b")],
    );

    let view = s.identity_view_at(T0 - 1).expect("view");
    assert!(view.view().is_empty(), "no adjudication had been recorded");
    assert_eq!(
        view.resolve_at(&eid("a")).outcome,
        ResolutionOutcome::Unknown
    );

    drop(s);
    clean(&path);
}

// ---------------------------------------------------------------------------
// Contradiction: refused at the instant it arose, never resolved into a winner
// ---------------------------------------------------------------------------

/// **A contradictory historical graph refuses; it does not synthesize a
/// winner.**
///
/// `a` merges into `b`, then `a` merges into `c` — two individually valid
/// events, neither able to see the other. `KIRRA-WM-TRANSITIVITY-001` requires
/// the contradictory graph to fail rather than be repaired, and the fold's
/// poison is what carries that into the historical view exactly as it does the
/// current one.
///
/// The instant *before* the second merge is asserted as well, and that is the
/// half that matters: it proves the refusal is a property of the graph AT THE
/// INSTANT rather than a flag inherited from today's projection.
#[test]
fn a_contradiction_refuses_at_and_after_it_arose_but_not_before() {
    use kirra_world::resolution::RefusalReason;

    let (mut s, path) = open("contradiction");
    seed(
        &mut s,
        &[
            assert_id("a"),     // t(0)
            assert_id("b"),     // t(1)
            assert_id("c"),     // t(2)
            merge(&["a"], "b"), // t(3)
            merge(&["a"], "c"), // t(4) -- contradicts the above
        ],
    );

    assert_eq!(
        s.resolve_at_whole_graph(&eid("a"), t(3))
            .expect("resolve")
            .outcome,
        ResolutionOutcome::Located {
            entity: eid("b"),
            hops: 1,
        },
        "before the second merge the history is consistent and answers"
    );
    assert_eq!(
        s.resolve_at_whole_graph(&eid("a"), t(4))
            .expect("resolve")
            .outcome,
        ResolutionOutcome::Refused(RefusalReason::ContradictoryHistory { at: eid("a") }),
        "once both merges are visible the graph contradicts itself and must \
         refuse rather than pick one"
    );

    drop(s);
    clean(&path);
}

// ---------------------------------------------------------------------------
// KIRRA-WM-CLUSTERING-001 / -PROMOTION-001: candidates are not identity
// ---------------------------------------------------------------------------

/// **A candidate `same_as` observation never enters the historical confirmed
/// graph.**
///
/// The rulings bar a matcher from confirming identity. The historical view must
/// not be the back door: a `claim_status = 'candidate'` row is invisible to the
/// fold at every instant, exactly as it is to the present-tense one.
///
/// Written as a raw `append` rather than through `append_adjudication`, because
/// that helper writes `Confirmed` unconditionally — so the candidate row this
/// test needs cannot be produced by the adjudication door at all, which is
/// itself the enforcement. What is checked here is the READ side: that a
/// candidate row sitting in the log is not picked up by the historical fold.
#[test]
fn a_candidate_same_as_row_is_invisible_to_the_historical_fold() {
    let (mut s, path) = open("candidate-excluded");
    seed(&mut s, &[assert_id("a"), assert_id("b")]);

    // A candidate claim carrying an adjudication-shaped payload, written by a
    // matcher. The `kind` matches what the fold selects on, so only the
    // claim_status predicate keeps it out.
    let event_id = EventId::new("ev-candidate").expect("event id");
    let observation_id = ObservationId::new("obs-candidate").expect("obs");
    let payload = kirra_world_store::adjudication_record::encode_adjudication(&merge(&["a"], "b"));
    s.append(&NewEvent {
        event_id: &event_id,
        observation_id: &observation_id,
        txn_time_ms: t(2),
        valid_from_ms: t(2),
        valid_to_ms: None,
        source: "matcher",
        source_version: "0.1.0",
        writer_class: WriterClass::LlmCandidate,
        claim_status: ClaimStatus::Candidate,
        provenance: &["obs-1"],
        frame_id: None,
        map_id: None,
        kind: kirra_world_store::adjudication_record::ADJUDICATION_KIND,
        subject: "a",
        subject_ref: None,
        predicate: None,
        object: None,
        payload: &payload,
        payload_schema: kirra_world_store::adjudication_record::ADJUDICATION_PAYLOAD_SCHEMA,
        retention_class: "raw",
        trust: None,
    })
    .expect("append candidate");

    assert_eq!(
        s.resolve_at_whole_graph(&eid("a"), t(2))
            .expect("resolve")
            .outcome,
        ResolutionOutcome::Located {
            entity: eid("a"),
            hops: 0,
        },
        "a candidate merge must not redirect `a` at any instant"
    );
    assert_eq!(
        s.resolve_at_whole_graph(&eid("a"), t(10))
            .expect("resolve")
            .outcome,
        ResolutionOutcome::Located {
            entity: eid("a"),
            hops: 0,
        },
        "nor later -- a candidate does not become identity by ageing"
    );

    drop(s);
    clean(&path);
}

// ---------------------------------------------------------------------------
// The degradation contract
// ---------------------------------------------------------------------------

/// **The answer carries its resolution, and it is `Full` because adjudications
/// are protected from compaction.**
///
/// Asserted through the compaction predicate rather than by comparing the field
/// to a literal: what makes identity immune to compaction is that
/// `is_protected` holds for the class adjudications are written with, and if
/// that ever stops being true this assertion is the one that should change.
#[test]
fn a_historical_answer_carries_a_resolution_and_it_is_full_because_adjudications_are_protected() {
    assert!(
        kirra_world_store::compaction::is_protected(
            kirra_world_store::adjudication_record::ADJUDICATION_RETENTION_CLASS
        ),
        "identity's immunity to compaction rests on this predicate; the \
         resolution below is derived from it, not assumed alongside it"
    );

    let (mut s, path) = open("resolution-full");
    seed(
        &mut s,
        &[assert_id("a"), assert_id("b"), merge(&["a"], "b")],
    );

    let answer = s.resolve_at_whole_graph(&eid("a"), t(2)).expect("resolve");
    assert_eq!(answer.resolution, Resolution::Full);
    assert!(!answer.is_degraded());
    assert_eq!(
        answer.as_known_at_ms,
        t(2),
        "the answer names the instant it was true at"
    );

    drop(s);
    clean(&path);
}

/// **A compacted store still answers `Full` for identity** — because the
/// compaction planner refuses a window holding a protected row, so a recorded
/// citation is evidence that no adjudication was in it.
///
/// This is the case that would be wrong under a naive "any citation means
/// degraded" rule: raw observations were compacted away, and identity is
/// genuinely unaffected.
#[test]
fn compacting_raw_observations_does_not_degrade_a_historical_identity_answer() {
    let (mut s, path) = open("compacted-raw");

    // Raw observations first, so there is something compactable that is not an
    // adjudication.
    for i in 0..4 {
        let event_id = EventId::new(format!("raw-{i}")).expect("event id");
        let observation_id = ObservationId::new(format!("raw-obs-{i}")).expect("obs");
        s.append(&NewEvent {
            event_id: &event_id,
            observation_id: &observation_id,
            txn_time_ms: T0 - 100 + i,
            valid_from_ms: T0 - 100 + i,
            valid_to_ms: None,
            source: "sensor-a",
            source_version: "0.1.0",
            writer_class: WriterClass::Sensor,
            claim_status: ClaimStatus::Confirmed,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "observation",
            subject: "thing",
            subject_ref: None,
            predicate: Some("colour"),
            object: Some("red"),
            payload: r#"{"n":1}"#,
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append raw");
    }
    seed(
        &mut s,
        &[assert_id("a"), assert_id("b"), merge(&["a"], "b")],
    );

    // Compaction runs at a wall clock LATER than the instant queried below.
    //
    // That ordering is the point, not an arbitrary number. `compacted_at_ms` is
    // when compaction RAN; `as_known_at_ms` is the instant asked ABOUT. A
    // compaction that ran after the queried instant is precisely the one that
    // can remove evidence bearing on it, so this is the direction in which a
    // time-narrowing degradation rule fails OPEN. Raised in review on #1413,
    // where the first version filtered citations by
    // `compacted_at_ms <= as_known_at_ms` and would have reported `Full` here
    // over evidence that was gone.
    let outcome = s
        .compact_range(1, 3, T0 + 10_000)
        .expect("compact the raw prefix");
    assert!(
        outcome.removed > 0,
        "nothing was compacted, so the assertion below would hold vacuously"
    );

    let answer = s.resolve_at_whole_graph(&eid("a"), t(2)).expect("resolve");
    assert_eq!(
        answer.resolution,
        Resolution::Full,
        "raw observations were removed; no adjudication could have been, so the \
         identity answer is complete"
    );
    assert_eq!(
        answer.outcome,
        ResolutionOutcome::Located {
            entity: eid("b"),
            hops: 1,
        },
        "and the answer itself survives the compaction intact"
    );

    drop(s);
    clean(&path);
}

// ---------------------------------------------------------------------------
// One engine
// ---------------------------------------------------------------------------

/// **The historical view is an `AdjudicationGraph` like any other**, so the
/// public resolver runs over it directly and gives the same answer as the
/// convenience method.
///
/// This is what "one shared underlying resolution engine" means concretely: not
/// that two implementations agree, but that there is one function and the
/// historical path is a different argument to it.
#[test]
fn the_public_resolver_runs_over_the_historical_view_unchanged() {
    let (mut s, path) = open("one-engine");
    seed(
        &mut s,
        &[assert_id("a"), assert_id("b"), merge(&["a"], "b")],
    );

    let view = s.identity_view_at(t(2)).expect("view");
    assert_eq!(
        resolve(&view, &eid("a")),
        view.resolve_at(&eid("a")).outcome,
        "`resolve_at` must be `resolve` over a restricted graph, not a second \
         implementation that happens to agree"
    );

    drop(s);
    clean(&path);
}
