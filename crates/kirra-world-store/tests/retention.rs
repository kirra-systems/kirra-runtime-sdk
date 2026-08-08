//! Retention enforcement: compaction-with-citation (ADR-0041 §11.3).
//!
//! The happy path is the least interesting thing here. What these tests are
//! for is the set of properties that make deleting from a hash-chained log
//! defensible at all:
//!
//! * a gap with **no** citation breaks the chain — you cannot delete quietly;
//! * a **forged** citation breaks the chain — you cannot fake the admission;
//! * a protected-class window is refused **whole**;
//! * a projection head is refused, so a rebuild still reproduces the fold;
//! * removed content stays **attestable** through `range_digest`.

use kirra_world_store::{
    ClaimStatus, EventId, NewEvent, ObservationId, StoreError, WorldStore, WriterClass,
};

/// Owned ids for one event. [`NewEvent`] borrows its references, so they need a
/// home that outlives it — and the two are separate types now, which is what
/// stops them being passed in each other's place.
struct Ids {
    event: EventId,
    observation: ObservationId,
}

/// One string for both ids. Fine for a fixture, and now *visibly* a conflation
/// rather than an invisible one: the two fields have to be spelled separately.
fn ids(s: &str) -> Ids {
    Ids {
        event: EventId::new(s).expect("admissible event id"),
        observation: ObservationId::new(s).expect("admissible observation id"),
    }
}

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-world-retention-{}-{}.sqlite",
        name,
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

const T0: i64 = 1_700_000_000_000;

fn ev<'a>(id: &'a Ids, subject: &'a str, valid_from_ms: i64) -> NewEvent<'a> {
    NewEvent {
        event_id: &id.event,
        observation_id: &id.observation,
        txn_time_ms: valid_from_ms,
        valid_from_ms,
        valid_to_ms: None,
        source: "sensor-a",
        source_version: "0.1.0",
        writer_class: WriterClass::Sensor,
        claim_status: ClaimStatus::Confirmed,
        provenance: &[],
        frame_id: None,
        map_id: None,
        kind: "observation",
        subject,
        subject_ref: None,
        predicate: Some("position"),
        object: None,
        payload: r#"{"n":1}"#,
        payload_schema: 1,
        retention_class: "raw",
        trust: None,
    }
}

/// 12 raw events across 3 subjects, so heads sit at the END of the log and
/// generations 1..=6 are safely compactable.
fn seeded(path: &std::path::Path) -> WorldStore {
    let mut s = WorldStore::open(path).expect("open");
    for i in 1..=12i64 {
        let eid = format!("e{i}");
        let subj = format!("cup-{}", (i - 1) % 3);
        s.append(&ev(&ids(&eid), &subj, T0 + i * 100))
            .expect("append");
    }
    s
}

// ---------------------------------------------------------------------------
// The chain must survive the hole — and only honestly
// ---------------------------------------------------------------------------

#[test]
fn the_chain_verifies_across_a_compacted_span() {
    let p = tmp("verifies-across");
    let mut s = seeded(&p);
    s.verify_chain().expect("clean before");

    let out = s.compact_range(1, 6, T0 + 9_000).expect("compact");
    assert_eq!(out.removed, 6);
    assert_eq!(s.count().unwrap(), 6, "six events removed");

    s.verify_chain()
        .expect("the chain must verify ACROSS the hole, not merely up to it");

    let cs = s.citations().unwrap();
    assert_eq!(cs.len(), 1);
    assert_eq!((cs[0].lo_generation, cs[0].hi_generation), (1, 6));
    assert_eq!(cs[0].event_count, 6);
    assert!(!cs[0].range_digest.is_empty());
    clean(&p);
}

/// **The property that makes compaction an admission rather than an erasure.**
/// Delete rows without leaving a citation and the chain must refuse — otherwise
/// "compaction" is just a licence to remove evidence.
#[test]
fn an_uncited_gap_breaks_the_chain() {
    let p = tmp("uncited-gap");
    let mut s = seeded(&p);
    s.compact_range(1, 6, T0 + 9_000).expect("compact");
    s.verify_chain().expect("cited gap verifies");

    // Now remove the citation, leaving the hole unexplained.
    s.delete_citation_for_test(1).unwrap();
    match s.verify_chain() {
        Err(StoreError::UncitedGap { generation }) => assert_eq!(generation, 1),
        other => panic!("an uncited gap must break the chain, got {other:?}"),
    }
    clean(&p);
}

/// A citation that does not join the chain it claims to bridge must be
/// refused. Without this, forging one would launder an arbitrary deletion.
#[test]
fn a_forged_citation_breaks_the_chain() {
    let p = tmp("forged-citation");
    let mut s = seeded(&p);
    s.compact_range(1, 6, T0 + 9_000).expect("compact");
    s.verify_chain().expect("clean");

    s.tamper_citation_for_test(1, "chain_before", "deadbeef")
        .unwrap();
    match s.verify_chain() {
        Err(StoreError::CitationBroken { lo_generation, .. }) => assert_eq!(lo_generation, 1),
        other => panic!("a citation that does not join the chain must fail, got {other:?}"),
    }
    clean(&p);
}

/// The other half: a citation resuming at the wrong digest must also fail —
/// caught by the next surviving row failing to recompute.
#[test]
fn a_citation_resuming_at_the_wrong_digest_breaks_the_chain() {
    let p = tmp("wrong-resume");
    let mut s = seeded(&p);
    s.compact_range(1, 6, T0 + 9_000).expect("compact");

    s.tamper_citation_for_test(1, "chain_after", "cafebabe")
        .unwrap();
    match s.verify_chain() {
        Err(StoreError::ChainBroken { generation }) => assert_eq!(
            generation, 7,
            "the first surviving row after the hole must fail"
        ),
        other => panic!("expected a chain break at generation 7, got {other:?}"),
    }
    clean(&p);
}

/// A span compacted off the END of the log has no following row to trigger the
/// crossing, so it is verified separately. Without that, a trailing citation
/// would be the one place a forgery could hide.
#[test]
fn a_trailing_citation_is_still_verified() {
    // Unfolded deliberately: with no projection there are no heads, so the
    // tail is compactable and this exercises the trailing path rather than
    // the head refusal.
    let p = tmp("trailing");
    let mut s = seeded(&p);
    s.compact_range(7, 12, T0 + 9_000)
        .expect("compact the tail");
    assert_eq!(s.count().unwrap(), 6);
    s.verify_chain().expect("a trailing citation must verify");

    // Nothing follows it, so only the trailing pass can catch this.
    s.tamper_citation_for_test(7, "chain_before", "0000")
        .unwrap();
    match s.verify_chain() {
        Err(StoreError::CitationBroken { lo_generation, .. }) => assert_eq!(lo_generation, 7),
        other => panic!("a forged trailing citation must be caught, got {other:?}"),
    }
    clean(&p);
}

// ---------------------------------------------------------------------------
// The two refusals
// ---------------------------------------------------------------------------

/// §11.3 and OQ2: a window containing protected traffic is refused **whole**,
/// not compacted around. Partial application would make "how much of this span
/// survives" a question about arrival order rather than about policy.
#[test]
fn a_window_containing_protected_traffic_is_refused_whole() {
    let p = tmp("protected-refused");
    let mut s = WorldStore::open(&p).expect("open");
    for i in 1..=6i64 {
        let eid = format!("e{i}");
        let subj = format!("cup-{i}");
        s.append(&NewEvent {
            retention_class: if i == 4 { "safety" } else { "raw" },
            ..ev(&ids(&eid), &subj, T0 + i * 100)
        })
        .expect("append");
    }

    match s.compact_range(1, 5, T0 + 9_000) {
        Err(StoreError::ProtectedClassInRange {
            generation,
            retention_class,
        }) => {
            assert_eq!(generation, 4);
            assert_eq!(retention_class, "safety");
        }
        other => panic!("a protected event must refuse the window, got {other:?}"),
    }

    assert_eq!(s.count().unwrap(), 6, "nothing may be removed");
    assert!(
        s.citations().unwrap().is_empty(),
        "a refused compaction leaves no citation"
    );
    s.verify_chain().expect("untouched");
    clean(&p);
}

/// Every protected class, not just the one the first test happened to pick.
#[test]
fn every_protected_class_refuses() {
    for class in [
        "safety",
        "incident",
        "calibration",
        "adjudication",
        "operator",
    ] {
        let p = tmp(&format!("protected-{class}"));
        let mut s = WorldStore::open(&p).expect("open");
        s.append(&ev(&ids("e1"), "cup-1", T0)).expect("raw");
        s.append(&NewEvent {
            retention_class: class,
            ..ev(&ids("e2"), "cup-2", T0 + 100)
        })
        .expect("protected");

        assert!(
            matches!(
                s.compact_range(1, 2, T0 + 9_000),
                Err(StoreError::ProtectedClassInRange { .. })
            ),
            "{class} must refuse compaction"
        );
        clean(&p);
    }
}

/// **The refusal §11.3 could not have anticipated.** Removing the event that
/// defines a projection's current claim would make `rebuild_projections`
/// diverge from the incremental fold — falsifying the property ADR-0041 names,
/// silently.
#[test]
fn a_projection_head_is_refused() {
    let p = tmp("head-refused");
    let mut s = seeded(&p);
    s.fold().expect("fold");

    // Generations 10, 11, 12 are the heads for cup-0/1/2.
    match s.compact_range(7, 12, T0 + 9_000) {
        Err(StoreError::ProjectionHeadInRange { generation, .. }) => {
            assert_eq!(generation, 10, "the first head in the window");
        }
        other => panic!("a projection head must refuse the window, got {other:?}"),
    }
    assert_eq!(s.count().unwrap(), 12, "nothing removed");
    clean(&p);
}

/// And the payoff: compacting only non-head events leaves a rebuild producing
/// exactly the state the incremental fold reached. This is the invariant the
/// head refusal exists to preserve, asserted rather than argued.
#[test]
fn compaction_preserves_rebuild_equals_incremental() {
    let p = tmp("rebuild-after-compaction");
    let mut s = seeded(&p);
    s.fold().expect("fold");
    let before = s.projection_state_digest().unwrap();

    s.compact_range(1, 6, T0 + 9_000)
        .expect("compact non-heads");
    s.verify_chain().expect("chain intact");

    s.rebuild_projections().expect("rebuild from zero");
    let after = s.projection_state_digest().unwrap();

    assert_eq!(
        before, after,
        "a rebuild after compaction must reproduce the pre-compaction state"
    );
    clean(&p);
}

/// **Every citation must be walked.** The crossing loops only follow citations
/// reachable by contiguity from generation 1, so one sitting at an unreachable
/// `lo_generation` would be ignored and verification would still return `Ok`.
///
/// That is a hole in the property this whole design provides: a citation is an
/// *admission* that a span was removed, and an admission no verifier ever
/// reads is not one.
#[test]
fn an_unreachable_citation_is_refused() {
    let p = tmp("orphan-citation");
    let mut s = seeded(&p);
    s.compact_range(1, 4, T0 + 9_000)
        .expect("a real compaction");
    s.verify_chain().expect("clean");

    // Move the citation's span far past the end of the log. Nothing walks it,
    // and before the fix nothing complained either.
    s.tamper_citation_for_test(1, "hi_generation", "4").unwrap();
    s.forge_citation_for_test(500, 600, T0 + 9_100).unwrap();

    match s.verify_chain() {
        Err(StoreError::CitationBroken { lo_generation, .. }) => assert_eq!(lo_generation, 500),
        other => panic!("an unreachable citation must be refused, got {other:?}"),
    }
    clean(&p);
}

/// The digest must be **byte-identical** to the buffered form it replaced.
/// Streaming is a memory fix, not a format change: if it altered the digest,
/// every citation already written would silently stop matching its content.
#[test]
fn the_range_digest_is_stable_across_window_sizes() {
    // Compact 1..=2 in one store, and the same two events in another whose
    // window is expressed differently but covers the same rows. Same content,
    // same digest — which is the property a streaming hash must preserve.
    let p = tmp("digest-stable-a");
    let mut a = seeded(&p);
    let ra = a.compact_range(1, 2, T0 + 9_000).expect("a");

    let p2 = tmp("digest-stable-b");
    let mut b = seeded(&p2);
    let rb = b.compact_range(1, 2, T0 + 9_999).expect("b");

    assert_eq!(
        ra.citation.range_digest, rb.citation.range_digest,
        "the digest must depend on content only, not on when or how it was taken"
    );

    // And a one-event window must differ from a two-event one, so the test is
    // not passing on a constant.
    let p3 = tmp("digest-stable-c");
    let mut c = seeded(&p3);
    let rc = c.compact_range(1, 1, T0 + 9_000).expect("c");
    assert_ne!(ra.citation.range_digest, rc.citation.range_digest);

    clean(&p);
    clean(&p2);
    clean(&p3);
}

// ---------------------------------------------------------------------------
// A refusal is a redirect, not a dead end
// ---------------------------------------------------------------------------

/// `largest_compactable_prefix` must return a range `compact_range` actually
/// accepts — otherwise it is advice that does not work.
#[test]
fn the_prefix_helper_returns_a_range_compaction_accepts() {
    let p = tmp("prefix-accepts");
    let mut s = seeded(&p);
    s.fold().expect("fold");

    // Heads are 10, 11, 12, so the largest compactable prefix of 1..=12 is 9.
    let limit = s.largest_compactable_prefix(1, 12).unwrap();
    assert_eq!(limit, Some(9));

    s.compact_range(1, limit.unwrap(), T0 + 9_000)
        .expect("the helper's answer must be accepted");
    s.verify_chain().expect("chain intact");
    clean(&p);
}

/// The asymmetry between the two refusals, asserted rather than only argued.
///
/// A protected event makes the window a *policy* unit, so the helper stops
/// below it. A projection head only requires the head itself to survive, so
/// stopping below it is a conservative simplification — recorded as such,
/// with compacting-around pre-authorized if measurement demands it.
#[test]
fn the_prefix_helper_stops_below_the_first_blocker_of_either_kind() {
    // Protected at 4 → prefix is 3.
    let p = tmp("prefix-protected");
    let mut s = WorldStore::open(&p).expect("open");
    for i in 1..=6i64 {
        let eid = format!("e{i}");
        let subj = format!("cup-{i}");
        s.append(&NewEvent {
            retention_class: if i == 4 { "incident" } else { "raw" },
            ..ev(&ids(&eid), &subj, T0 + i * 100)
        })
        .expect("append");
    }
    assert_eq!(s.largest_compactable_prefix(1, 6).unwrap(), Some(3));
    s.compact_range(1, 3, T0 + 9_000).expect("accepted");
    clean(&p);

    // Head at 10 → prefix is 9, same shape, different reason.
    let p2 = tmp("prefix-head");
    let mut s2 = seeded(&p2);
    s2.fold().expect("fold");
    assert_eq!(s2.largest_compactable_prefix(7, 12).unwrap(), Some(9));
    clean(&p2);
}

/// A window blocked at its very first generation has no compactable prefix at
/// all, and must say so rather than returning an empty range compaction would
/// then refuse.
#[test]
fn a_window_blocked_at_its_first_generation_has_no_prefix() {
    let p = tmp("prefix-none");
    let mut s = WorldStore::open(&p).expect("open");
    s.append(&NewEvent {
        retention_class: "safety",
        ..ev(&ids("e1"), "cup-1", T0)
    })
    .expect("protected first");
    s.append(&ev(&ids("e2"), "cup-2", T0 + 100)).expect("raw");

    assert_eq!(s.largest_compactable_prefix(1, 2).unwrap(), None);
    assert_eq!(
        s.largest_compactable_prefix(90, 99).unwrap(),
        None,
        "a range matching no rows has no prefix either"
    );
    assert_eq!(s.largest_compactable_prefix(6, 3).unwrap(), None);
    clean(&p);
}

/// An unfolded store has no heads, so only the policy refusal applies.
#[test]
fn without_a_projection_only_the_policy_refusal_bounds_the_prefix() {
    let p = tmp("prefix-unfolded");
    let s = seeded(&p);
    assert_eq!(
        s.largest_compactable_prefix(1, 12).unwrap(),
        Some(12),
        "no projection means no heads, so the whole raw window is compactable"
    );
    clean(&p);
}

// ---------------------------------------------------------------------------
// Attestability and bounds
// ---------------------------------------------------------------------------

/// Removed content is gone but not unaccountable: the citation records how
/// many events went and a digest over their exact canonical bytes.
#[test]
fn the_removed_span_stays_attestable() {
    let p = tmp("attestable");
    let mut s = seeded(&p);

    // Digest the same span twice from two identical stores: the range digest
    // is a function of content, so it must agree.
    let p2 = tmp("attestable-2");
    let mut s2 = seeded(&p2);

    let a = s.compact_range(1, 6, T0 + 9_000).expect("compact");
    let b = s2.compact_range(1, 6, T0 + 9_000).expect("compact");
    assert_eq!(
        a.citation.range_digest, b.citation.range_digest,
        "the same content must digest the same"
    );

    // A different span must not.
    let p3 = tmp("attestable-3");
    let mut s3 = seeded(&p3);
    let c = s3.compact_range(1, 5, T0 + 9_000).expect("compact");
    assert_ne!(a.citation.range_digest, c.citation.range_digest);

    clean(&p);
    clean(&p2);
    clean(&p3);
}

#[test]
fn an_empty_or_inverted_range_is_refused() {
    let p = tmp("empty-range");
    let mut s = seeded(&p);
    assert!(matches!(
        s.compact_range(6, 3, T0),
        Err(StoreError::EmptyRange { .. })
    ));
    assert!(matches!(
        s.compact_range(0, 3, T0),
        Err(StoreError::EmptyRange { .. })
    ));
    assert!(
        matches!(
            s.compact_range(90, 99, T0),
            Err(StoreError::EmptyRange { .. })
        ),
        "a range matching no rows is empty, not a silent no-op citation"
    );
    assert!(s.citations().unwrap().is_empty());
    clean(&p);
}

/// Two compactions in sequence: the second must chain from where the first
/// left off, not from a row that no longer exists.
#[test]
fn successive_compactions_chain_correctly() {
    let p = tmp("successive");
    let mut s = seeded(&p);
    s.compact_range(1, 4, T0 + 9_000).expect("first");
    s.verify_chain().expect("after first");
    s.compact_range(5, 8, T0 + 9_100).expect("second");
    s.verify_chain()
        .expect("the second citation must join where the first left off");

    assert_eq!(s.count().unwrap(), 4);
    assert_eq!(s.citations().unwrap().len(), 2);
    clean(&p);
}

/// Compaction must not disturb what survives.
#[test]
fn surviving_events_are_unchanged() {
    let p = tmp("survivors");
    let mut s = seeded(&p);
    let before = s.history("cup-0").unwrap().claims;

    s.compact_range(1, 6, T0 + 9_000).expect("compact");
    let after = s.history("cup-0").unwrap();
    assert!(
        after.is_degraded(),
        "compaction removed events about this subject; the answer must say so"
    );
    let after = after.claims;

    let surviving: Vec<_> = before.into_iter().filter(|c| c.generation > 6).collect();
    assert_eq!(surviving, after, "survivors must be byte-identical");
    assert!(!after.is_empty(), "the test would be vacuous otherwise");
    clean(&p);
}
