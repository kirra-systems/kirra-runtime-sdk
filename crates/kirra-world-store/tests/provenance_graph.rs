//! **Tier 4 box 4b — resolving citations at a historical coordinate.**
//!
//! Box 4a built the citation relation and deliberately stopped short of
//! resolving it. This is where resolution happens, and the whole box is
//! organised around one property:
//!
//! > A citation dangling at *T* must still read as dangling when a query is
//! > pinned to *T*, however resolvable it became afterwards.
//!
//! That is `KIRRA-WM-PROVENANCE-GRAPH-001`'s load-bearing test, and it is the
//! first thing in this file for a reason: every other decision here — the shape
//! of the resolution outcome, the coverage floor's refusal, what happens at a
//! compacted target — is downstream of getting it right, and a suite that
//! establishes it last is a suite that has already been built around whatever
//! the implementation happened to do.
//!
//! # The four outcomes a citation can have, and the two collapses ruled out
//!
//! | Outcome | Meaning | The collapse it must not become |
//! |---|---|---|
//! | `Resolved` | exactly one visible event carries the cited id | — |
//! | `Plural` | several do | *"pick the newest"* |
//! | `Dangling` | none do | an empty child list |
//!
//! Both collapses are Tier 3 case 8's absent-because-unknown /
//! absent-because-empty distinction, one tier up. A tree that renders plural as
//! a single parent is not less precise, it is **wrong**: it names one event as
//! the source of a claim when the store cannot tell which of several it was.
//!
//! # And the distinction inside `Dangling`
//!
//! *Nothing ever carried this id* and *whatever carried it was compacted away*
//! are different facts that look identical at the node — both are "no visible
//! carrier". Inferring the first from the second is the silent rewrite §11.3
//! forbids, so `DanglingReason` carries which, on the same necessary-condition
//! footing as `Resolution::Degraded`: it may say `PossiblyCompacted` where the
//! id never existed, and may never say `NeverVisible` where evidence was
//! deleted.

use kirra_world_store::provenance_graph::{
    BranchContinuation, CitationResolution, DanglingReason, GraphSpec, NodeCitations,
    NotWalkedReason, ProvenanceTree, MAX_PROVENANCE_DEPTH, MAX_PROVENANCE_NODES,
};
use kirra_world_store::{
    ClaimStatus, EventId, NewEvent, ObservationId, StoreError, WorldStore, WriterClass,
};

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-provgraph-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    clean(&p);
    p
}

fn clean(p: &std::path::Path) {
    for s in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(s);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
}

/// Append one claim carrying observation id `obs`, citing `cited`.
///
/// `event_id` is derived from `tag` and `obs` is passed separately **because
/// observation ids are not unique**: the plural case needs two events carrying
/// the same observation id, which is exactly the cardinality
/// `KIRRA-WM-PROVENANCE-GRAPH-001` says the edge has.
fn append_ev(s: &mut WorldStore, tag: &str, obs: &str, subject: &str, cited: &[&str]) -> i64 {
    let event_id = EventId::new(format!("ev-{tag}")).expect("event id");
    let observation_id = ObservationId::new(obs.to_string()).expect("obs id");
    s.append(&NewEvent {
        event_id: &event_id,
        observation_id: &observation_id,
        txn_time_ms: T0,
        valid_from_ms: T0,
        valid_to_ms: None,
        source: "warehouse-scanner",
        source_version: "1.0.0",
        writer_class: WriterClass::Sensor,
        claim_status: ClaimStatus::Confirmed,
        provenance: cited,
        frame_id: None,
        map_id: None,
        kind: "mission",
        subject,
        subject_ref: None,
        predicate: Some("last_seen_at"),
        object: Some("dock_a"),
        payload: "{}",
        payload_schema: 1,
        retention_class: "raw",
        trust: None,
    })
    .expect("append")
}

fn store(name: &str) -> (WorldStore, std::path::PathBuf) {
    let p = tmp(name);
    let s = WorldStore::open(&p).expect("open");
    (s, p)
}

fn tree(s: &WorldStore, root: i64, at: i64) -> ProvenanceTree {
    s.provenance_tree(root, at, GraphSpec::widest())
        .expect("provenance tree")
}

/// The branches of a node that is expected to have an indexed citation set.
fn branches(t: &ProvenanceTree, node: usize) -> &[kirra_world_store::provenance_graph::Branch] {
    match &t.nodes[node].citations {
        NodeCitations::Indexed { branches, .. } => branches,
        other => panic!("node {node} has no indexed citations: {other:?}"),
    }
}

/// The single branch of a node expected to have exactly one citation.
fn only_branch(t: &ProvenanceTree, node: usize) -> &kirra_world_store::provenance_graph::Branch {
    let b = branches(t, node);
    assert_eq!(b.len(), 1, "expected exactly one citation on node {node}");
    &b[0]
}

// ---------------------------------------------------------------------------
// 0. The bound on the span enumeration
// ---------------------------------------------------------------------------

/// **`provenance_tree` is bounded in its LAST unbounded dimension.**
///
/// The span list used to be a `SELECT ... FROM compaction_citations` with no
/// `LIMIT`, so one dimension of an otherwise-bounded query grew with the store's
/// whole compaction history. This is the store-level proof that it no longer
/// does — the resolver's own tests cannot show it, because they hand the rule a
/// list that SQL already produced.
///
/// The assertions are deliberately three: the list is capped, the caller is told
/// it was capped, and — the part that matters — the dangle is still qualified as
/// possibly-compacted. A bound that achieved the first two by quietly reporting
/// "nothing was ever recorded" would be worse than the unbounded scan.
#[test]
fn the_span_enumeration_is_capped_and_says_so_without_losing_the_qualification() {
    let (mut s, p) = store("span-cap");
    // One event citing an id nothing carries: a dangle needing qualification.
    let src = append_ev(&mut s, "src", "obs-src", "robot-1", &["obs-gone"]);
    // More compacted spans than the ceiling admits, every one of them below the
    // pin so every one qualifies.
    let over = kirra_world_store::provenance_graph::MAX_COMPACTED_SPANS + 12;
    for i in 0..over {
        s.forge_citation_for_test(-(i as i64) - 1, -(i as i64) - 1, T0)
            .expect("forge citation");
    }

    let t = tree(&s, src, src);
    match &only_branch(&t, 0).resolution {
        CitationResolution::Dangling {
            reason: DanglingReason::PossiblyCompacted { spans, truncated },
        } => {
            assert_eq!(
                spans.len(),
                kirra_world_store::provenance_graph::MAX_COMPACTED_SPANS,
                "the enumeration is capped at the ceiling, not at the store size"
            );
            assert!(truncated, "and the caller is told the account is partial");
        }
        other => panic!("a capped span list must still qualify the dangle: {other:?}"),
    }
    clean(&p);
}

/// Below the ceiling nothing changes: the full list, and no truncation claim.
///
/// The negative control for the test above. Without it, a rule that reported
/// `truncated` on every compacted dangle would pass — and "some evidence is
/// missing from this account" is not a caveat to emit unconditionally.
#[test]
fn a_span_list_under_the_ceiling_is_complete_and_not_flagged() {
    let (mut s, p) = store("span-uncapped");
    let src = append_ev(&mut s, "src", "obs-src", "robot-1", &["obs-gone"]);
    for i in 0..3 {
        s.forge_citation_for_test(-i - 1, -i - 1, T0)
            .expect("forge citation");
    }

    let t = tree(&s, src, src);
    match &only_branch(&t, 0).resolution {
        CitationResolution::Dangling {
            reason: DanglingReason::PossiblyCompacted { spans, truncated },
        } => {
            assert_eq!(spans.len(), 3, "every qualifying span is named");
            assert!(!truncated, "and nothing claims more were held back");
        }
        other => panic!("expected a qualified dangle: {other:?}"),
    }
    clean(&p);
}

// ---------------------------------------------------------------------------
// 1. The load-bearing property: historical honesty
// ---------------------------------------------------------------------------

/// **The test `WM_SCOPE.md` §7 names.** If this ever returns the T2 node for a
/// query pinned at T1, Tier 4 has become historically dishonest.
///
/// ```text
/// T1: A cites observation X; nothing carries X   → Dangling(X)
/// T2: an event carrying observation_id X appended → a current query resolves X
///     query pinned to T1                          → MUST still say Dangling(X)
/// ```
///
/// The middle line is not decoration. A test that only checked T1 would pass
/// against a store where resolution had been baked in at append — because at
/// append time the answer *was* dangling. It is the pair that discriminates:
/// the same source, the same citation, two coordinates, two different and both
/// correct answers.
#[test]
fn a_citation_dangling_at_t1_still_reads_dangling_when_pinned_to_t1() {
    let (mut s, p) = store("t1t2");

    // T1 — A cites obs-x, and nothing carries obs-x.
    let a = append_ev(&mut s, "a", "obs-a", "package_17", &["obs-x"]);
    let t1 = a;

    let at_t1 = tree(&s, a, t1);
    assert_eq!(
        only_branch(&at_t1, 0).resolution,
        CitationResolution::Dangling {
            reason: DanglingReason::NeverVisible
        },
        "nothing carries obs-x at T1"
    );

    // T2 — an event carrying obs-x is appended.
    let x = append_ev(&mut s, "x", "obs-x", "package_17", &[]);
    let t2 = x;

    // A query at the CURRENT coordinate resolves it. This half is what makes
    // the assertion below non-vacuous: without it, "still dangling at T1" would
    // also pass on a store that could never resolve anything at all.
    assert_eq!(
        only_branch(&tree(&s, a, t2), 0).resolution,
        CitationResolution::Resolved {
            target_generation: x
        },
        "the citation IS resolvable today — otherwise the T1 assertion proves nothing"
    );

    // The property.
    assert_eq!(
        only_branch(&tree(&s, a, t1), 0).resolution,
        CitationResolution::Dangling {
            reason: DanglingReason::NeverVisible
        },
        "pinned to T1 the citation must STILL read dangling: the event carrying \
         obs-x was not visible then, and reporting it would be resolving the \
         graph that happens to be resolvable today"
    );

    clean(&p);
}

/// The plural counterpart from the same ruling: *"a query before the second row
/// resolves one; after it, reports plural."*
///
/// The `Plural` arm is the one most likely to be quietly collapsed, because
/// picking the newest carrier always produces a tidier tree and never fails a
/// test that only counts nodes.
#[test]
fn a_second_carrier_turns_a_resolved_citation_plural_at_the_later_coordinate() {
    let (mut s, p) = store("plural");

    let x1 = append_ev(&mut s, "x1", "obs-x", "package_17", &[]);
    let a = append_ev(&mut s, "a", "obs-a", "package_17", &["obs-x"]);
    let before = a;

    assert_eq!(
        only_branch(&tree(&s, a, before), 0).resolution,
        CitationResolution::Resolved {
            target_generation: x1
        },
        "one carrier is visible, so the citation resolves"
    );

    // A SECOND event carrying the same observation id. Legal: observation_id is
    // indexed, not unique — many-to-many, not a parent pointer.
    let x2 = append_ev(&mut s, "x2", "obs-x", "package_17", &[]);

    assert_eq!(
        only_branch(&tree(&s, a, x2), 0).resolution,
        CitationResolution::Plural {
            target_generations: vec![x1, x2],
            truncated: false
        },
        "two carriers must report BOTH — 'pick the newest' would name one event \
         as the source of a claim the store cannot attribute"
    );

    // And the earlier coordinate is unmoved by the later append.
    assert_eq!(
        only_branch(&tree(&s, a, before), 0).resolution,
        CitationResolution::Resolved {
            target_generation: x1
        },
        "pinning to before the second carrier must still see one"
    );

    clean(&p);
}

/// `Dangling` is a node, not a missing one — the second collapse the ruling
/// names.
///
/// A source that cited nothing and a source whose one citation resolves to
/// nothing are different facts. If dangling were rendered as an absent child,
/// both would be a childless node and an explanation could not tell *"this
/// claim rested on nothing"* from *"this claim rested on evidence Kirra cannot
/// find"*.
#[test]
fn a_dangling_citation_is_a_branch_not_an_absent_child() {
    let (mut s, p) = store("dangling-present");

    let cited_nothing = append_ev(&mut s, "n", "obs-n", "package_17", &[]);
    let cited_missing = append_ev(&mut s, "m", "obs-m", "package_17", &["obs-ghost"]);

    let nothing = tree(&s, cited_nothing, cited_missing);
    assert!(
        matches!(
            &nothing.nodes[0].citations,
            NodeCitations::Indexed { branches, .. } if branches.is_empty()
        ),
        "a source that cited nothing has an indexed, empty citation set"
    );

    let missing = tree(&s, cited_missing, cited_missing);
    let branch = only_branch(&missing, 0);
    assert_eq!(branch.cited_observation_id, "obs-ghost");
    assert_eq!(
        branch.resolution,
        CitationResolution::Dangling {
            reason: DanglingReason::NeverVisible
        }
    );
    assert_eq!(
        branch.continuation,
        BranchContinuation::NotWalked(NotWalkedReason::Nothing),
        "there is nothing to walk, and that is a different statement from a \
         bound having been reached"
    );

    clean(&p);
}

// ---------------------------------------------------------------------------
// 2. The coverage floor: refusing rather than interpreting absence
// ---------------------------------------------------------------------------

/// Box 4a records the highest generation the index does **not** cover. This is
/// the consumer of that record, and the reason 4a wrote it.
///
/// Below the floor an empty edge set is not evidence of *"cited nothing"* — it
/// is what an un-backfilled store makes every source look like. Answering it
/// would be a positive claim about provenance, made silently, about the whole
/// log.
#[test]
fn a_root_at_or_below_the_coverage_floor_is_refused_not_answered_empty() {
    let (mut s, p) = store("floor-root");

    let a = append_ev(&mut s, "a", "obs-a", "package_17", &["obs-x"]);
    // Simulate a store migrated from v6: the events are there, the index is not
    // known to cover them.
    s.set_provenance_edges_floor_for_test(a).expect("set floor");

    match s.provenance_tree(a, a, GraphSpec::widest()) {
        Err(StoreError::ProvenanceIndexIncomplete { requested, floor }) => {
            assert_eq!((requested, floor), (a, a));
        }
        other => panic!("expected a refusal below the floor, got {other:?}"),
    }

    // And backfilling — the operational step that makes the claim true — turns
    // the refusal into an answer, without the events changing.
    s.backfill_provenance_edges().expect("backfill");
    assert_eq!(
        only_branch(&tree(&s, a, a), 0).cited_observation_id,
        "obs-x",
        "once the index covers the generation, the same question has an answer"
    );

    clean(&p);
}

/// The floor applies **per node**, not only at the root, and a node below it
/// reports that its citations are unknown — never that it had none.
///
/// This is the case a root-only check would miss: citations point at earlier
/// evidence, so a partially-backfilled store has its uncovered generations
/// exactly where a walk ends up.
#[test]
fn a_node_below_the_floor_reports_unknown_citations_not_an_empty_set() {
    let (mut s, p) = store("floor-node");

    let deep = append_ev(&mut s, "deep", "obs-deep", "package_17", &["obs-deeper"]);
    let mid = append_ev(&mut s, "mid", "obs-mid", "package_17", &["obs-deep"]);
    let root = append_ev(&mut s, "root", "obs-root", "package_17", &["obs-mid"]);

    // The floor covers the root and mid, but not `deep`.
    s.set_provenance_edges_floor_for_test(deep)
        .expect("set floor");

    let t = tree(&s, root, root);
    let mid_node = t
        .nodes
        .iter()
        .position(|n| n.generation == mid)
        .expect("the mid node is reached");
    assert!(
        matches!(t.nodes[mid_node].citations, NodeCitations::Indexed { .. }),
        "mid is above the floor, so its citations ARE known — the control that \
         keeps the assertion below from passing on a walk that gave up early"
    );
    let deep_node = t
        .nodes
        .iter()
        .position(|n| n.generation == deep)
        .expect("the deep node is reached");
    assert_eq!(
        t.nodes[deep_node].citations,
        NodeCitations::BelowCoverageFloor,
        "the index makes no claim about this generation, so neither may the tree"
    );
    assert_ne!(
        t.nodes[deep_node].citations,
        NodeCitations::Indexed {
            branches: vec![],
            truncated: false
        },
        "reporting it as 'cited nothing' is the exact silent claim the floor exists to prevent"
    );

    clean(&p);
}

// ---------------------------------------------------------------------------
// 3. Degradation: compacted evidence, at the source and at the target
// ---------------------------------------------------------------------------

/// A `Dangling` whose id *could* have been carried by a compacted event must
/// say so. `NeverVisible` and `PossiblyCompacted` are different facts about the
/// world, and only one of them is "nothing was ever recorded here".
#[test]
fn a_citation_into_a_compacted_window_is_dangling_but_says_it_may_have_been_deleted() {
    let (mut s, p) = store("compacted-target");

    // Build a window worth compacting, then a source citing into it.
    let first = append_ev(&mut s, "c1", "obs-c1", "package_17", &[]);
    let last = append_ev(&mut s, "c2", "obs-c2", "package_17", &[]);
    let a = append_ev(&mut s, "a", "obs-a", "package_17", &["obs-c2"]);

    assert_eq!(
        only_branch(&tree(&s, a, a), 0).resolution,
        CitationResolution::Resolved {
            target_generation: last
        },
        "before compaction the citation resolves — the control for what follows"
    );

    s.compact_range(first, last, T0 + 60_000).expect("compact");

    let branch_resolution = only_branch(&tree(&s, a, a), 0).resolution.clone();
    match branch_resolution {
        CitationResolution::Dangling {
            reason: DanglingReason::PossiblyCompacted { spans, .. },
        } => assert_eq!(
            spans,
            vec![first],
            "the span that could have held it is named, so an investigator can \
             go to the citation rather than conclude nothing was there"
        ),
        other => panic!(
            "a citation whose target was compacted must be dangling-because-deleted, got {other:?}"
        ),
    }

    clean(&p);
}

/// The other half of the same distinction, and the reason the qualification
/// cannot be inferred from the resolution alone: a store that has compacted
/// *something* must not start reporting every dangling citation as possibly
/// deleted.
#[test]
fn an_id_that_never_existed_stays_never_visible_even_in_a_compacted_store() {
    let (mut s, p) = store("compacted-unrelated");

    let first = append_ev(&mut s, "c1", "obs-c1", "package_17", &[]);
    let last = append_ev(&mut s, "c2", "obs-c2", "package_17", &[]);
    s.compact_range(first, last, T0 + 60_000).expect("compact");

    let a = append_ev(&mut s, "a", "obs-a", "package_17", &["obs-ghost"]);

    // `obs-ghost` was never written, and the compacted window is *below* the
    // pin — so the necessary condition holds and the honest answer is the
    // conservative one.
    match only_branch(&tree(&s, a, a), 0).resolution.clone() {
        CitationResolution::Dangling {
            reason: DanglingReason::PossiblyCompacted { spans, .. },
        } => assert_eq!(spans, vec![first]),
        other => panic!("expected the conservative reading, got {other:?}"),
    }

    // But pinned BELOW the compacted window there is no span that could have
    // held it, and the answer sharpens.
    let (mut s2, p2) = store("compacted-unrelated-2");
    let b = append_ev(&mut s2, "b", "obs-b", "package_17", &["obs-ghost"]);
    assert_eq!(
        only_branch(&tree(&s2, b, b), 0).resolution,
        CitationResolution::Dangling {
            reason: DanglingReason::NeverVisible
        },
        "a store that never compacted anything can say so"
    );

    clean(&p);
    clean(&p2);
}

/// When the SOURCE event is gone, its edges went with it (4a's invariant 4).
/// The tree must report that the evidence was deleted — not that the source
/// cited nothing.
#[test]
fn a_compacted_source_reports_deleted_evidence_rather_than_no_citations() {
    let (mut s, p) = store("compacted-source");

    let a = append_ev(&mut s, "a", "obs-a", "package_17", &["obs-x"]);
    let b = append_ev(&mut s, "b", "obs-b", "package_17", &[]);
    s.compact_range(a, b, T0 + 60_000).expect("compact");

    let t = tree(&s, a, b);
    assert_eq!(
        t.nodes[0].citations,
        NodeCitations::EvidenceCompacted,
        "the source's own statement was deleted, so what it cited is unknowable \
         — the surviving citation index is not evidence and must not stand in"
    );
    assert!(
        t.outcome.is_degraded(),
        "and the tree says so at the top, where a caller cannot miss it"
    );

    clean(&p);
}

// ---------------------------------------------------------------------------
// 4. Cycles are not truncation
// ---------------------------------------------------------------------------

/// Once provenance is a real graph a cycle is possible unless admission proves
/// otherwise, and a depth limit alone would make a malformed cycle look like an
/// ordinary bound being reached — an explanation that says *"there is more"*
/// when the truth is *"this evidence is circular"*.
#[test]
fn a_cycle_is_reported_as_a_cycle_and_not_as_a_bound_being_reached() {
    let (mut s, p) = store("cycle");

    // A cites obs-b; B cites obs-a. Legal to write and impossible to walk.
    let a = append_ev(&mut s, "a", "obs-a", "package_17", &["obs-b"]);
    let b = append_ev(&mut s, "b", "obs-b", "package_17", &["obs-a"]);

    let t = tree(&s, a, b);
    let back = only_branch(&t, 1);
    assert_eq!(
        back.continuation,
        BranchContinuation::NotWalked(NotWalkedReason::CycleDetected {
            back_to_generation: a
        }),
        "the branch that closes the loop names where it returns to"
    );
    assert!(t.outcome.cycle_detected, "and the tree carries the fact");
    assert!(
        !t.outcome.truncated,
        "a cycle is NOT a bound being reached — reporting it as truncation would \
         tell an operator to raise the limit, which cannot help"
    );

    clean(&p);
}

/// The converse control. Without this, a walk that reported *everything* as a
/// cycle would pass the test above.
#[test]
fn an_ordinary_depth_bound_is_truncation_and_not_a_cycle() {
    let (mut s, p) = store("depth");

    // A chain, no loop: c → b → a.
    let a = append_ev(&mut s, "a", "obs-a", "package_17", &[]);
    let b = append_ev(&mut s, "b", "obs-b", "package_17", &["obs-a"]);
    let c = append_ev(&mut s, "c", "obs-c", "package_17", &["obs-b"]);

    let shallow = s
        .provenance_tree(c, c, GraphSpec::new(1, MAX_PROVENANCE_NODES).expect("spec"))
        .expect("tree");
    assert_eq!(
        shallow.nodes[1].generation, b,
        "the one edge walked reached b"
    );
    assert_eq!(
        only_branch(&shallow, 1).continuation,
        BranchContinuation::NotWalked(NotWalkedReason::DepthLimit),
        "the walk stopped because it was told to, and says which bound"
    );
    assert!(shallow.outcome.truncated);
    assert!(
        !shallow.outcome.cycle_detected,
        "an acyclic chain must never be reported circular"
    );

    // The same graph, unbounded enough to finish, is complete.
    let full = tree(&s, c, c);
    assert!(full.outcome.is_complete(), "{:?}", full.outcome);
    assert_eq!(
        full.nodes.iter().map(|n| n.generation).collect::<Vec<_>>(),
        vec![c, b, a],
        "pre-order from the root, following the chain"
    );

    clean(&p);
}

/// A diamond is not a cycle either. Two paths reaching the same event is
/// ordinary provenance — a claim resting on two claims that rest on one
/// observation — and a walk that tracked *visited* rather than *on the current
/// path* would report it as circular.
///
/// This is the gray-set/black-set distinction the fleet DAG traversal makes for
/// the same reason, and getting it wrong is the classic way a cycle check
/// becomes a memoisation bug.
#[test]
fn a_diamond_is_not_a_cycle() {
    let (mut s, p) = store("diamond");

    let base = append_ev(&mut s, "base", "obs-base", "package_17", &[]);
    let left = append_ev(&mut s, "left", "obs-left", "package_17", &["obs-base"]);
    let right = append_ev(&mut s, "right", "obs-right", "package_17", &["obs-base"]);
    let top = append_ev(
        &mut s,
        "top",
        "obs-top",
        "package_17",
        &["obs-left", "obs-right"],
    );
    let _ = (base, left, right);

    let t = tree(&s, top, top);
    assert!(
        !t.outcome.cycle_detected,
        "two paths to one event is a diamond, not a loop"
    );
    assert!(t.outcome.is_complete(), "{:?}", t.outcome);

    clean(&p);
}

// ---------------------------------------------------------------------------
// 5. Bounds — Rule 2, and the 4a precedent for how a bound is refused
// ---------------------------------------------------------------------------

/// Both dimensions refuse rather than clamp, for 4a's reason: a clamp answers a
/// smaller question and reports it as the one that was asked.
#[test]
fn a_zero_or_oversized_bound_is_refused_in_both_dimensions() {
    assert!(GraphSpec::new(0, 8).is_err(), "zero depth");
    assert!(GraphSpec::new(8, 0).is_err(), "zero nodes");
    assert!(
        GraphSpec::new(MAX_PROVENANCE_DEPTH + 1, 8).is_err(),
        "depth over ceiling"
    );
    assert!(
        GraphSpec::new(8, MAX_PROVENANCE_NODES + 1).is_err(),
        "nodes over ceiling"
    );
    assert!(GraphSpec::new(MAX_PROVENANCE_DEPTH, MAX_PROVENANCE_NODES).is_ok());
}

/// The node budget is a real bound, distinct from depth: a shallow graph can be
/// arbitrarily wide, and a depth limit alone would not bound the walk.
#[test]
fn the_node_budget_bounds_a_wide_shallow_graph() {
    let (mut s, p) = store("wide");

    let mut cited: Vec<String> = Vec::new();
    for i in 0..6 {
        let tag = format!("w{i}");
        append_ev(&mut s, &tag, &format!("obs-{tag}"), "package_17", &[]);
        cited.push(format!("obs-{tag}"));
    }
    let refs: Vec<&str> = cited.iter().map(String::as_str).collect();
    let root = append_ev(&mut s, "root", "obs-root", "package_17", &refs);

    let t = s
        .provenance_tree(
            root,
            root,
            GraphSpec::new(MAX_PROVENANCE_DEPTH, 3).expect("spec"),
        )
        .expect("tree");
    assert!(
        t.nodes.len() <= 3,
        "the budget is honoured: {}",
        t.nodes.len()
    );
    assert!(t.outcome.truncated);
    assert!(!t.outcome.cycle_detected);
    assert!(
        branches(&t, 0)
            .iter()
            .any(|b| b.continuation == BranchContinuation::NotWalked(NotWalkedReason::NodeLimit)),
        "and the branches that were not walked say which bound stopped them"
    );

    clean(&p);
}

// ---------------------------------------------------------------------------
// 6. Order and duplicates survive resolution
// ---------------------------------------------------------------------------

/// 4a keeps the recorded array verbatim; resolution must not quietly tidy it.
/// A source citing the same observation twice said so twice, and a tree that
/// deduplicated would describe an array the hash does not cover.
#[test]
fn recorded_order_and_duplicates_survive_into_the_tree() {
    let (mut s, p) = store("verbatim");

    let x = append_ev(&mut s, "x", "obs-x", "package_17", &[]);
    let root = append_ev(
        &mut s,
        "root",
        "obs-root",
        "package_17",
        &["obs-x", "obs-ghost", "obs-x"],
    );

    let t = tree(&s, root, root);
    let b = branches(&t, 0);
    assert_eq!(
        b.iter()
            .map(|e| (e.ordinal, e.cited_observation_id.as_str()))
            .collect::<Vec<_>>(),
        vec![(0, "obs-x"), (1, "obs-ghost"), (2, "obs-x")],
        "order and duplicates are the source's statement"
    );
    assert_eq!(
        b[0].resolution,
        CitationResolution::Resolved {
            target_generation: x
        }
    );
    assert_eq!(
        b[2].resolution,
        CitationResolution::Resolved {
            target_generation: x
        },
        "the repeat resolves the same way — but it is a second branch, not one"
    );

    clean(&p);
}
