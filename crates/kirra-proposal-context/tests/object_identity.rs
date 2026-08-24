//! **Tier 3 box 3c, consumer half — an object that names an entity is not a
//! string.**
//!
//! `mission_context` matches a claim's object against the candidates it was
//! offered. Before this, it compared the STORED object literally, so every
//! merge the world had recorded was invisible to it: a package last seen at
//! `dock_old`, with `dock_old` since merged into `dock_b`, matched no candidate
//! at all — the world knew the answer and the consumer could not read it.
//!
//! Four claims are pinned, and the fourth is the one that makes the others
//! worth having:
//!
//! 1. A merged object resolves to what it became, and matches.
//! 2. An object on a store with no adjudications matches literally — the
//!    overwhelmingly common case must not regress.
//! 3. A STALE identity graph refuses rather than matching the raw object.
//! 4. An ambiguous resolution refuses rather than guessing a successor.
//!
//! # Why case 3 is the sharp one
//!
//! A first draft gated resolution on *"has the entity projection been folded"*,
//! which is wrong in both directions and dangerous in one. It refused every
//! object-bearing claim on a store that simply has no adjudications (case 2,
//! which is most stores) — and it **admitted** the genuinely bad case, a
//! projection folded once with merges recorded since, because such a projection
//! is not unfolded. The check is now against the LOG: has identity consumed
//! every adjudication recorded? Case 3 is what holds that line.

use kirra_proposal_context::{mission_context, ContextId, ObjectResolution, WorldSilence};
use kirra_world::adjudication::{
    AssertIdentity, IdentityAdjudication, Justification, MergeEntities, SplitEntity,
};
use kirra_world::observation::{ClockDomain, DomainInstant};
use kirra_world::reference::{EntityId, EventId, ObservationId};
use kirra_world_store::adjudication_record::AdjudicationRow;
use kirra_world_store::{ClaimStatus, NewEvent, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-object-identity-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    for suffix in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

fn id(s: &str) -> ContextId {
    ContextId::new(s).expect("non-empty id")
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

fn candidates() -> Vec<ContextId> {
    vec![id("dock_a"), id("dock_b")]
}

/// Record `package_17 last_seen_at <object>`.
fn record_claim(store: &mut WorldStore, object: &str) {
    store
        .append(&NewEvent {
            event_id: &EventId::new("ev-last-seen").expect("event id"),
            observation_id: &ObservationId::new("obs-last-seen").expect("observation id"),
            txn_time_ms: T0,
            valid_from_ms: T0,
            valid_to_ms: None,
            source: "warehouse-scanner",
            source_version: "1.0.0",
            writer_class: WriterClass::Sensor,
            claim_status: ClaimStatus::Confirmed,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "mission",
            subject: "package_17",
            subject_ref: None,
            predicate: Some("last_seen_at"),
            object: Some(object),
            payload: "{}",
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append claim");
    store.fold().expect("fold claims");
}

fn record_adjudication(store: &mut WorldStore, tag: &str, a: &IdentityAdjudication) {
    store
        .append_adjudication(
            &AdjudicationRow {
                event_id: &EventId::new(format!("ev-adj-{tag}")).expect("event id"),
                observation_id: &ObservationId::new(format!("obs-adj-{tag}")).expect("obs"),
                txn_time_ms: T0 + 1,
                valid_from_ms: T0 + 1,
                source: "operator-console",
                source_version: "1.0.0",
                writer_class: WriterClass::Operator,
            },
            a,
        )
        .expect("append adjudication");
}

fn ask(store: &WorldStore) -> kirra_proposal_context::ProposalContext {
    mission_context(
        store,
        &id("package_17"),
        &id("last_seen_at"),
        &candidates(),
        T0,
        None,
    )
    .expect("context")
}

// ---------------------------------------------------------------------------
// 1. The positive witness
// ---------------------------------------------------------------------------

/// **A merged object resolves to what it became.**
///
/// The whole point of the box, in one assertion. `dock_old` is not a candidate
/// and never will be; the world records that it merged into `dock_b`, which is.
/// Matching the stored string finds nothing. Resolving finds the answer the
/// world already had.
#[test]
fn a_merged_object_resolves_to_the_candidate_it_became() {
    let path = tmp("merged");
    let mut store = WorldStore::open(&path).expect("open");

    record_claim(&mut store, "dock_old");
    record_adjudication(
        &mut store,
        "1",
        &IdentityAdjudication::Assert(AssertIdentity::new(eid("dock_old"), who(), just(), at())),
    );
    record_adjudication(
        &mut store,
        "2",
        &IdentityAdjudication::Assert(AssertIdentity::new(eid("dock_b"), who(), just(), at())),
    );
    record_adjudication(
        &mut store,
        "3",
        &IdentityAdjudication::Merge(
            MergeEntities::new(vec![eid("dock_old")], eid("dock_b"), who(), just(), at())
                .expect("merge"),
        ),
    );
    store
        .fold_entity_projection()
        .expect("fold entity projection");

    let ctx = ask(&store);
    assert_eq!(
        ctx.preferred_destination(),
        Some(&id("dock_b")),
        "the object merged into dock_b, so the context must prefer dock_b — \
         matching the stored object literally finds nothing here"
    );
    assert_eq!(ctx.silence(), None, "the world expressed a preference");

    drop(store);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// 2. The common case must not regress
// ---------------------------------------------------------------------------

/// **An object on a store with no adjudications matches literally.**
///
/// Most stores never record an identity adjudication. Their entity projection
/// is empty and unfolded, and that is not a fault — there are no merges to
/// miss, so an object stands for itself. A gate that refused here would have
/// broken every existing consumer to buy no safety at all.
#[test]
fn an_object_with_no_adjudications_matches_literally() {
    let path = tmp("literal");
    let mut store = WorldStore::open(&path).expect("open");
    record_claim(&mut store, "dock_b");

    assert!(
        !store.has_entity_projection().expect("catalogue"),
        "this fixture is only meaningful while the entity projection is absent"
    );

    let ctx = ask(&store);
    assert_eq!(
        ctx.preferred_destination(),
        Some(&id("dock_b")),
        "with nothing adjudicated there is nothing to resolve, and the object \
         stands for itself"
    );

    drop(store);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// 3. The sharp case: a stale graph refuses
// ---------------------------------------------------------------------------

/// **A stale identity graph refuses rather than matching the raw object.**
///
/// The merge is recorded and NOT folded. The stored object is `dock_b`, which
/// is a candidate — so the pre-3c code, and any fallback-to-raw-string design,
/// would happily prefer `dock_b`. But the log says an adjudication about this
/// world has not been consumed, so the graph cannot support the claim that
/// `dock_b` is still what it was. Refusing is the only honest answer.
///
/// Note what makes this test sharp: the literal match would SUCCEED. A refusal
/// that only ever fired when the literal match also failed would be
/// indistinguishable from doing nothing.
#[test]
fn a_stale_identity_graph_refuses_rather_than_matching_the_raw_object() {
    let path = tmp("stale");
    let mut store = WorldStore::open(&path).expect("open");

    record_claim(&mut store, "dock_b");
    record_adjudication(
        &mut store,
        "1",
        &IdentityAdjudication::Assert(AssertIdentity::new(eid("dock_b"), who(), just(), at())),
    );
    // Deliberately NOT folded: the adjudication sits in the log unconsumed.

    let ctx = ask(&store);
    assert_eq!(
        ctx.preferred_destination(),
        None,
        "a stale identity graph must not shape a proposal"
    );
    assert_eq!(
        ctx.silence(),
        Some(&WorldSilence::ObjectUnresolved(
            ObjectResolution::GraphStale
        )),
        "and it must say WHY — an unfolded adjudication is an operator's \
         problem to fix, not 'the world named something off the menu'"
    );

    // Non-vacuity: fold, and the very same store answers.
    store
        .fold_entity_projection()
        .expect("fold entity projection");
    assert_eq!(
        ask(&store).preferred_destination(),
        Some(&id("dock_b")),
        "once identity has caught up the same question is answerable — the \
         refusal above is about staleness, not about this fixture"
    );

    drop(store);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// 4. Ambiguity refuses rather than guessing
// ---------------------------------------------------------------------------

/// **An object that resolved to more than one entity is refused.**
///
/// `dock_b` is partitioned into two successors that do not reconverge. One of
/// them could be picked — they are both live entities — and picking one would
/// be a guess wearing an answer's clothes.
#[test]
fn an_ambiguous_object_refuses_rather_than_picking_a_successor() {
    let path = tmp("ambiguous");
    let mut store = WorldStore::open(&path).expect("open");

    record_claim(&mut store, "dock_b");
    record_adjudication(
        &mut store,
        "1",
        &IdentityAdjudication::Assert(AssertIdentity::new(eid("dock_b"), who(), just(), at())),
    );
    record_adjudication(
        &mut store,
        "2",
        &IdentityAdjudication::Split(
            SplitEntity::partition(
                eid("dock_b"),
                [eid("dock_b1"), eid("dock_b2")],
                who(),
                just(),
                at(),
            )
            .expect("partition"),
        ),
    );
    store
        .fold_entity_projection()
        .expect("fold entity projection");

    let ctx = ask(&store);
    assert_eq!(
        ctx.preferred_destination(),
        None,
        "an object that turned out to be two things must not become a preference"
    );
    assert!(
        matches!(
            ctx.silence(),
            Some(WorldSilence::ObjectUnresolved(
                ObjectResolution::Ambiguous { .. }
            ))
        ),
        "and the ambiguity must be reported as such, not as 'nothing matched': \
         got {:?}",
        ctx.silence()
    );

    drop(store);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// 5. The mirror stays in lock-step with the world's own vocabulary
// ---------------------------------------------------------------------------

/// **Every refusal reason the world can produce has a distinct tag here.**
///
/// `ObjectResolution::Contradictory` carries a `&'static str` rather than the
/// world's `RefusalReason` because that type holds
/// `TraversalBudgetExceeded { limit: usize }` — a primitive numeric, which this
/// crate's public surface may not carry at all. A mirror only stays honest if
/// something walks the real enum, so this does.
///
/// Adding a variant to `RefusalReason` breaks `contradiction_tag`'s exhaustive
/// match at compile time. This catches the half a compiler cannot: two variants
/// quietly sharing one tag, which compiles and then misreports forever.
///
/// An earlier draft of this test compared `format!("{:?}", ..)` of the
/// ObjectIdentity, which embeds the reason's own fields — so it would have
/// passed with every tag set to the same string. It was testing `RefusalReason`
/// derives `Debug`, not that the mirror is faithful.
#[test]
fn every_refusal_reason_has_its_own_tag() {
    use kirra_proposal_context::ObjectResolution;
    use kirra_world::resolution::RefusalReason;
    use kirra_world_service::read_view::ObjectIdentity;

    let all = [
        RefusalReason::RedirectCycle { at: eid("a") },
        RefusalReason::TraversalBudgetExceeded { limit: 64 },
        RefusalReason::DanglingRedirect {
            from: eid("a"),
            to: eid("b"),
        },
        RefusalReason::EmptySupersession { at: eid("a") },
        RefusalReason::ContradictoryHistory { at: eid("a") },
    ];

    let tags: Vec<&'static str> = all
        .iter()
        .map(ObjectResolution::contradiction_tag)
        .collect();

    let mut distinct = tags.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        tags.len(),
        "two refusal reasons collapsed onto one tag: {tags:?}"
    );
    assert!(
        tags.iter().all(|t| !t.is_empty()),
        "an empty tag reports nothing: {tags:?}"
    );

    // The fail-closed contract, on every reason: a refusal never yields
    // something to match a candidate against.
    for reason in &all {
        assert_eq!(
            ObjectIdentity::Refused(reason.clone()).matchable(Some("dock_b")),
            None,
            "a contradictory history must never yield something to match on"
        );
    }
}

/// `KIRRA-WM-IDENTITY-AUTHORITY-001`: every identity adjudication names an
/// authorized adjudicator, so the fixtures do too.
fn who() -> kirra_world::same_as_adjudication::AdjudicationAuthority {
    kirra_world::same_as_adjudication::AdjudicationAuthority::new(
        kirra_world::observation::SourceClass::Operator,
        "test-operator",
    )
    .expect("authority")
}
