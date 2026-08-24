//! **Tier 4 box 4c — the generation-addressed evidence lookup.**
//!
//! `explain::ClaimLabels` is generation-addressed, and nothing public could
//! implement it: every route to a `ProjectedClaim` is subject-scoped or
//! whole-world-current, and `read_at_generation` returns a projection whose
//! rows are private and reachable only per-subject.
//!
//! # The missing primitive was a TRUTH problem
//!
//! A provenance walk follows citations into whatever events carry the cited
//! observations, and those routinely belong to OTHER subjects. A `ClaimLabels`
//! built on a subject-scoped map would return `None` for every cross-subject
//! node — and `project_explanation` DEFINES `None` as *"the event is gone"*,
//! rendering `DELETED_CLAIM_LABEL`. The artifact would have said evidence was
//! deleted while it sat in the log, and every gate would have stayed green,
//! because nothing checks whether a label is true.
//!
//! So the suite is organised around the three cases being genuinely distinct:
//!
//! | Case | Must be | Must never be |
//! |---|---|---|
//! | retained at that generation | `Some(claim)` | — |
//! | genuinely absent | `None` | the current claim, or a neighbour |
//! | present but undecodable | `Err` | `None` |
//!
//! The middle row is why there is a no-fallback test, and the bottom row is why
//! `origin` was added to the tamper allowlist: without it, *"a malformed row
//! errors rather than reading as absent"* was a claim with no test behind it.

use kirra_world_store::{
    provenance_graph::GraphSpec, ClaimStatus, EventId, NewEvent, ObservationId, WorldStore,
    WriterClass,
};

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-claimgen-{name}-{}-{n}.sqlite",
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

/// Append one claim about `subject`, citing `cited`.
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

// ===========================================================================
// The three cases, kept apart
// ===========================================================================

#[test]
fn a_retained_generation_returns_that_exact_claim() {
    let (mut s, p) = store("retained");
    let g = append_ev(&mut s, "a", "obs-a", "package_17", &[]);

    let got = s
        .claim_at_generation(g)
        .expect("read")
        .expect("the event is retained, so this must be Some");

    // The EXACT event, not merely something about the subject.
    assert_eq!(got.generation, g);
    assert_eq!(got.event_id, "ev-a");
    assert_eq!(got.subject, "package_17");
    assert_eq!(got.predicate.as_deref(), Some("last_seen_at"));
    assert_eq!(got.object.as_deref(), Some("dock_a"));
    assert_eq!(got.txn_time_ms, T0);
    assert!(
        !got.chain_digest.is_empty(),
        "the provenance handle must survive the read — an evidence lookup that \
         drops the citable digest cannot support an auditable explanation"
    );
    clean(&p);
}

#[test]
fn a_generation_that_was_never_written_is_none() {
    let (mut s, p) = store("never-written");
    let g = append_ev(&mut s, "a", "obs-a", "package_17", &[]);

    assert!(
        s.claim_at_generation(g + 1).expect("read").is_none(),
        "a generation past the head is absent, not an error and not a neighbour"
    );
    assert!(
        s.claim_at_generation(9_999_999).expect("read").is_none(),
        "a far-future generation is absent"
    );
    assert!(
        s.claim_at_generation(0).expect("read").is_none(),
        "generation 0 is not an event — the log is 1-based"
    );
    clean(&p);
}

#[test]
fn another_subjects_generation_still_resolves() {
    // The case that motivates the whole primitive: a subject-scoped map would
    // return None here and the artifact would call this evidence deleted.
    let (mut s, p) = store("cross-subject");
    let a = append_ev(&mut s, "a", "obs-a", "package_17", &[]);
    let b = append_ev(&mut s, "b", "obs-b", "dock_a", &[]);
    let c = append_ev(&mut s, "c", "obs-c", "scanner_3", &[]);

    for (g, subject) in [(a, "package_17"), (b, "dock_a"), (c, "scanner_3")] {
        let got = s
            .claim_at_generation(g)
            .expect("read")
            .unwrap_or_else(|| panic!("generation {g} is retained and must resolve"));
        assert_eq!(
            got.subject, subject,
            "the lookup must not be scoped to any one subject"
        );
    }
    clean(&p);
}

// ===========================================================================
// No fallback — the property that separates "absent" from "answered anyway"
// ===========================================================================

#[test]
fn a_compacted_generation_is_none_and_does_not_fall_back_to_a_neighbour() {
    let (mut s, p) = store("compacted-no-fallback");
    // THREE claims about the same subject, and the MIDDLE one is compacted.
    //
    // The middle matters. A two-event fixture with the OLDEST compacted cannot
    // catch a `WHERE generation <= ?1 ORDER BY generation DESC LIMIT 1`
    // fallback, because there is nothing below the hole for it to find — the
    // test would pass against a mutant that silently answers with a neighbour.
    // With a retained event on BOTH sides, every fallback direction has
    // something plausible to return, so `None` is the only correct answer.
    let older = append_ev(&mut s, "older", "obs-older", "package_17", &[]);
    let gone = append_ev(&mut s, "gone", "obs-gone", "package_17", &[]);
    let newer = append_ev(&mut s, "newer", "obs-newer", "package_17", &[]);

    assert!(
        s.claim_at_generation(gone).expect("read").is_some(),
        "precondition: the middle generation is retained before compaction"
    );

    s.compact_range(gone, gone, T0 + 1).expect("compact");

    let got = s.claim_at_generation(gone).expect("read");
    assert!(
        got.is_none(),
        "a compacted generation must read as absent — got {got:?}, which means          the lookup answered with a neighbour or with current state"
    );

    // Both neighbours are untouched, which is what proves the None above is
    // about that COORDINATE rather than about the subject.
    for (g, tag) in [(older, "ev-older"), (newer, "ev-newer")] {
        let still = s
            .claim_at_generation(g)
            .expect("read")
            .unwrap_or_else(|| panic!("{tag} is retained and must still resolve"));
        assert_eq!(still.event_id, tag);
    }
    clean(&p);
}

// ===========================================================================
// Present but undecodable — Err, never None
// ===========================================================================

#[test]
fn the_schema_refuses_to_create_the_corrupt_shape_at_all() {
    // This test began as "a malformed row errors rather than reading as
    // absent", because Err and None converging would turn *"something is
    // wrong"* into *"this evidence was deleted"*. Trying to build such a row
    // established something better: it cannot be built.
    //
    // The axis CHECK enforces that the three stored axes travel together
    // (`(adjudication IS NULL) = (origin IS NULL)`), so a partial set is
    // refused even by `tamper_for_test`, which bypasses the WRITE path but not
    // the storage constraints. Enumerating the rest closes the case: every
    // tamperable column the decoder reads is either NOT NULL — so it cannot be
    // cleared — or already `Option`, so a NULL decodes cleanly to `None`.
    //
    // So `claim_at_generation` has no reachable Err-on-decode path through the
    // supported surface, and the decoder's refusal arms are defence in depth
    // against a row edited outside SQLite. That is worth PINNING rather than
    // leaving as an assumption: if a later migration relaxes the CHECK, this
    // test fails and says so, instead of the Err arm quietly becoming
    // reachable with nothing exercising it.
    let (mut s, p) = store("schema-refuses");
    let g = append_ev(&mut s, "a", "obs-a", "package_17", &[]);

    // `observed` is a VALID origin token — the schema's vocabulary CHECK admits
    // it — so what is being refused here is the COMBINATION, not the value.
    let refused = s.tamper_for_test(g, "origin", Some("observed"));
    let err = refused.expect_err(
        "a partial axis set was accepted — the axes no longer travel together,          and claim_at_generation's Err-on-decode arm is now reachable with no          test exercising it",
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("CHECK constraint failed"),
        "the refusal must come from the SCHEMA, not from the tamper allowlist          or the writer — those would be weaker guarantees. Got: {msg}"
    );

    // And the row is untouched, so the lookup still reads it: a refused edit
    // must not leave the evidence half-written.
    let still = s
        .claim_at_generation(g)
        .expect("read")
        .expect("the event is still retained after the refused tamper");
    assert_eq!(still.event_id, "ev-a");
    assert!(
        still.trust.is_none(),
        "the refused edit must not have landed a partial axis set"
    );
    clean(&p);
}

// ===========================================================================
// The 3a fit: every node of a real cross-subject provenance tree resolves
// ===========================================================================

#[test]
fn every_node_of_a_cross_subject_provenance_tree_resolves() {
    // This is the test that says the primitive is the right SHAPE, not merely
    // that it works: it walks a real tree and demands the lookup answer for
    // every node the walk produced. A primitive that answered for the root's
    // subject only would pass all the tests above and fail this one.
    let (mut s, p) = store("tree-fit");
    let leaf = append_ev(&mut s, "leaf", "obs-leaf", "scanner_3", &[]);
    let mid = append_ev(&mut s, "mid", "obs-mid", "dock_a", &["obs-leaf"]);
    let root = append_ev(&mut s, "root", "obs-root", "package_17", &["obs-mid"]);

    let tree = s
        .provenance_tree(root, root, GraphSpec::widest())
        .expect("provenance tree");

    assert!(
        tree.nodes.len() >= 3,
        "fixture must actually produce a multi-node tree, got {} node(s) — a \
         one-node tree would make the assertion below vacuous",
        tree.nodes.len()
    );

    let subjects: std::collections::BTreeSet<String> = tree
        .nodes
        .iter()
        .map(|n| {
            s.claim_at_generation(n.generation)
                .expect("read")
                .unwrap_or_else(|| {
                    panic!(
                        "node at generation {} did not resolve — project_explanation \
                         would render this as DELETED_CLAIM_LABEL",
                        n.generation
                    )
                })
                .subject
        })
        .collect();

    assert!(
        subjects.len() >= 2,
        "the tree must span more than one subject or it does not exercise the \
         cross-subject case at all; spanned {subjects:?}"
    );
    assert_eq!(leaf, tree.nodes.iter().map(|n| n.generation).min().unwrap());
    assert!(mid > leaf && root > mid);
    clean(&p);
}
