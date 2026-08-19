//! **Tier 4 box 3a — the World-side explanation core, end to end.**
//!
//! Real `WorldStore`, real lineage query, real provenance walk, real projection,
//! real store-backed labels. No mocks: the point of 3a is that the labels read
//! the event log, and a fake store would stub out the one thing that can be
//! wrong.
//!
//! # The load-bearing test is the honesty one
//!
//! `every_node_of_a_cross_subject_explanation_is_described` comes first because
//! it is the reason `claim_at_generation` was built. A subject-scoped label
//! implementation passes every other test here and fails that one — by
//! producing an artifact that says evidence was DELETED while it sits in the
//! log.

use kirra_explain_types::{BranchState, ExplanationArtifact, NodeEvidence};
use kirra_world_explain_service::{explain_current_subject, ExplainError, StoreLabels};
use kirra_world_service::explain::ClaimLabels;
use kirra_world_store::{ClaimStatus, EventId, NewEvent, ObservationId, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-explain3a-{name}-{}-{n}.sqlite",
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

fn store(name: &str) -> (WorldStore, std::path::PathBuf) {
    let p = tmp(name);
    let s = WorldStore::open(&p).expect("open");
    (s, p)
}

/// Fold, so the projection coordinate catches up with the appended events.
///
/// The explanation pins to the FOLDED coordinate, so a fixture that appends
/// without folding is asking about a coordinate the projection has not reached
/// — which refuses, correctly. Making that explicit here rather than hiding it
/// inside `append_ev` keeps the fixture honest about what state it is in.
fn fold(s: &mut WorldStore) {
    s.fold().expect("fold");
}

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

fn claim_texts(a: &ExplanationArtifact) -> Vec<String> {
    a.nodes
        .iter()
        .map(|n| n.claim.as_str().to_string())
        .collect()
}

// ===========================================================================
// The honesty case — why claim_at_generation exists
// ===========================================================================

#[test]
fn every_node_of_a_cross_subject_explanation_is_described() {
    let (mut s, p) = store("cross-subject");
    // A citation chain crossing THREE subjects. A subject-scoped label
    // implementation would describe the root and call the rest deleted.
    append_ev(&mut s, "leaf", "obs-leaf", "scanner_3", &[]);
    append_ev(&mut s, "mid", "obs-mid", "dock_a", &["obs-leaf"]);
    append_ev(&mut s, "root", "obs-root", "package_17", &["obs-mid"]);
    fold(&mut s);

    let artifact = explain_current_subject(&s, "package_17").expect("explanation");
    let texts = claim_texts(&artifact);

    assert!(
        artifact.nodes.len() >= 3,
        "fixture must produce a multi-node artifact or the assertion below is \
         vacuous; got {} node(s): {texts:?}",
        artifact.nodes.len()
    );
    for t in &texts {
        assert!(
            !t.contains("deleted") && !t.contains("no longer"),
            "a node was described as deleted while its event is in the log — the \
             exact false statement 3a exists to prevent. Labels: {texts:?}"
        );
    }
    // Non-vacuously: the labels describe the three DIFFERENT subjects rather
    // than repeating the root's.
    for expected in ["package_17", "dock_a", "scanner_3"] {
        assert!(
            texts.iter().any(|t| t.contains(expected)),
            "no label mentions {expected}: {texts:?}"
        );
    }
    clean(&p);
}

#[test]
fn a_compacted_citation_target_is_disclosed_rather_than_invented() {
    // The other direction, and why the test above is not simply "never say
    // deleted": when evidence really IS gone, the artifact must disclose it. A
    // label layer returning a plausible string for every generation passes the
    // cross-subject test and fails this one.
    let (mut s, p) = store("compacted");
    let leaf = append_ev(&mut s, "leaf", "obs-leaf", "scanner_3", &[]);
    // A LATER event for the same subject, so the cited one is not scanner_3's
    // projection head — compaction refuses to remove a projection head
    // (`ProjectionHeadInRange`), which is the store protecting the fold rather
    // than anything to do with explanations.
    append_ev(&mut s, "leaf2", "obs-leaf2", "scanner_3", &[]);
    append_ev(&mut s, "root", "obs-root", "package_17", &["obs-leaf"]);
    fold(&mut s);

    s.compact_range(leaf, leaf, T0 + 1).expect("compact");
    fold(&mut s);

    let artifact = explain_current_subject(&s, "package_17").expect("explanation");
    let texts = claim_texts(&artifact);

    assert!(
        !texts.iter().any(|t| t.contains("scanner_3")),
        "the compacted leaf is still described by its content: {texts:?}"
    );

    let discloses = artifact.completeness.requires_disclosure()
        || artifact.nodes.iter().any(|n| {
            matches!(
                n.evidence,
                NodeEvidence::DeletedByCompaction | NodeEvidence::NotIndexed
            ) || matches!(&n.evidence, NodeEvidence::Recorded { branches, .. }
                if branches.iter().any(|b| matches!(b.state, BranchState::Dangling { .. })))
        });
    assert!(
        discloses,
        "an explanation over compacted evidence must disclose it somewhere, or \
         the renderer narrates a hole as a complete answer: {artifact:?}"
    );
    clean(&p);
}

/// **The labels themselves must admit absence** — the control mutation B needed.
///
/// Added after a mutation SURVIVED: a label layer that invented
/// `"a recorded claim"` for every coordinate passed this entire file. The
/// integration tests above could not see it, and the reason is worth recording
/// — a compacted citation TARGET never becomes a node. The walk records it as a
/// dangling branch on the citing node, so `claim_label` is never asked about the
/// missing generation at all, and the artifact's disclosure comes from the tree
/// rather than from the labels.
///
/// So the artifact was the wrong place to look. This asks the label layer
/// directly, at a coordinate proven absent, which is where the fabrication
/// would happen.
#[test]
fn the_labels_return_none_for_a_generation_the_store_no_longer_holds() {
    let (mut s, p) = store("labels-admit-absence");
    let leaf = append_ev(&mut s, "leaf", "obs-leaf", "scanner_3", &[]);
    append_ev(&mut s, "leaf2", "obs-leaf2", "scanner_3", &[]);
    append_ev(&mut s, "root", "obs-root", "package_17", &["obs-leaf"]);
    fold(&mut s);
    s.compact_range(leaf, leaf, T0 + 1).expect("compact");
    fold(&mut s);

    let labels = StoreLabels::new(&s);

    // Positive control FIRST: the labels must actually describe a retained
    // generation, or the assertions below pass against a layer that returns
    // None for everything.
    let retained: Option<kirra_explain_types::DisplayLabel> = labels.claim_label(3).expect("read");
    assert!(
        retained.is_some_and(|l| l.as_str().contains("package_17")),
        "the labels do not describe a retained generation, so the None checks \
         below would be vacuous"
    );

    assert!(
        labels.claim_label(leaf).expect("read").is_none(),
        "the label layer invented a description for a compacted generation — \
         project_explanation reads None as 'the event is gone', so a fabricated \
         Some here is a false statement to an operator"
    );
    assert!(
        labels.evidence(leaf).expect("read").is_none(),
        "the evidence layer invented a citation for a compacted generation"
    );
    // ...and a generation that never existed, which is the same absence
    // arrived at a different way.
    assert!(labels.claim_label(9_999).expect("read").is_none());
    clean(&p);
}

// ===========================================================================
// Unavailable is not empty
// ===========================================================================

#[test]
fn an_unknown_subject_is_nothing_recorded_not_an_empty_explanation() {
    let (mut s, p) = store("unknown-subject");
    append_ev(&mut s, "a", "obs-a", "package_17", &[]);
    fold(&mut s);

    match explain_current_subject(&s, "a_subject_that_was_never_written") {
        Err(ExplainError::NothingRecorded) => {}
        Err(other) => panic!("wrong error for an unknown subject: {other}"),
        Ok(a) => panic!(
            "an unknown subject produced an artifact with {} node(s) — an empty \
             explanation reads as 'nothing happened', a claim the store cannot \
             support",
            a.nodes.len()
        ),
    }
    clean(&p);
}

#[test]
fn an_empty_store_is_nothing_recorded() {
    let (s, p) = store("empty");
    assert!(
        matches!(
            explain_current_subject(&s, "package_17"),
            Err(ExplainError::NothingRecorded)
        ),
        "an empty store has nothing to explain and must say so"
    );
    clean(&p);
}

// ===========================================================================
// The bounds are the server's, and they hold
// ===========================================================================

#[test]
fn the_walk_is_bounded_by_this_crates_constants_not_by_the_data() {
    use kirra_world_explain_service::explain::EXPLAIN_NODES;

    let (mut s, p) = store("bounded");
    let mut prev: Option<String> = None;
    for i in 0..(EXPLAIN_NODES * 2) {
        let obs = format!("obs-{i}");
        let cited: Vec<&str> = prev.iter().map(String::as_str).collect();
        append_ev(&mut s, &format!("e{i}"), &obs, "package_17", &cited);
        prev = Some(obs);
    }
    fold(&mut s);

    let artifact = explain_current_subject(&s, "package_17").expect("explanation");
    assert!(
        artifact.nodes.len() <= EXPLAIN_NODES,
        "the walk exceeded its own node ceiling: {} > {EXPLAIN_NODES}",
        artifact.nodes.len()
    );
    assert!(
        artifact.completeness.requires_disclosure(),
        "a truncated explanation must oblige a caveat, or the renderer narrates \
         a partial chain as the whole one"
    );
    clean(&p);
}

// ===========================================================================
// Determinism — the property the absent clock parameter buys
// ===========================================================================

#[test]
fn two_calls_against_an_unchanged_store_are_identical() {
    let (mut s, p) = store("deterministic");
    append_ev(&mut s, "leaf", "obs-leaf", "scanner_3", &[]);
    append_ev(&mut s, "root", "obs-root", "package_17", &["obs-leaf"]);
    fold(&mut s);

    let a = explain_current_subject(&s, "package_17").expect("first");
    let b = explain_current_subject(&s, "package_17").expect("second");
    assert_eq!(
        a, b,
        "the artifact is a pure function of the store's state — a difference \
         means something read a clock or a random source"
    );
    clean(&p);
}
