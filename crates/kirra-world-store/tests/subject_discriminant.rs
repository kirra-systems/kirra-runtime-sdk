//! The v3 subject discriminant, against a real store.
//!
//! The gap this closes is `subject_projection`'s own: the layer flattened
//! `SubjectRef`'s four cases into one `subject TEXT NOT NULL` column and kept
//! no discriminant, so a fold could not restrict itself to *resolved entities*.
//!
//! Two properties carry the weight here, and neither is about the happy path:
//!
//! 1. **An unlabelled row is byte-identical to one written before the column
//!    existed.** Pinned by `canonical_form.rs`, which asserts the canonical JSON
//!    and its SHA-256 against a fixture — tests written long before this slice,
//!    which is what makes them evidence rather than agreement.
//! 2. **A label cannot be stored without being hashed.** That was the v2 review
//!    finding (an orphan `corroboration_n` would have been editable in place
//!    without breaking the chain) and it is the one property the design exists
//!    to deny.

use kirra_world_store::{
    ClaimStatus, EntityId, EventId, FrameId, NewEvent, ObservationId, StoreError, SubjectRef,
    WorldStore, WriterClass,
};

fn eid(v: &str) -> EntityId {
    EntityId::new(v).expect("test entity id")
}

fn fid(v: &str) -> FrameId {
    FrameId::new(v).expect("test frame id")
}

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-world-subjkind-{}-{}.sqlite",
        name,
        std::process::id()
    ));
    for suffix in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

struct Ids {
    event: EventId,
    observation: ObservationId,
}

fn ids(n: &str) -> Ids {
    Ids {
        event: EventId::new(format!("evt-{n}")).unwrap(),
        observation: ObservationId::new(format!("obs-{n}")).unwrap(),
    }
}

fn ev<'a>(id: &'a Ids, subject: &'a str, subject_ref: Option<&'a SubjectRef>) -> NewEvent<'a> {
    NewEvent {
        event_id: &id.event,
        observation_id: &id.observation,
        txn_time_ms: T0,
        valid_from_ms: T0,
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
        subject_ref,
        predicate: Some("colour"),
        object: Some("red"),
        payload: r#"{"n":1}"#,
        payload_schema: 1,
        retention_class: "raw",
        trust: None,
    }
}

// ---------------------------------------------------------------------------
// The label round-trips
// ---------------------------------------------------------------------------

/// Each storable kind survives a write and a full rehash.
///
/// Walked over **every** storable kind rather than a sample, because
/// `verify_chain` rebuilds the row from its columns — a kind that failed to
/// re-admit would break the chain, and a kind that re-admitted as a *different*
/// one would not.
///
/// That is two kinds, not three: `KIRRA-WM-CANDIDATE-ID-001` removed `candidate`
/// from the stored vocabulary. Phrased as "every storable kind" rather than a
/// number so it cannot go stale again the way it just did — the earlier wording
/// said "all three" and survived the narrowing that made it false.
#[test]
fn every_storable_kind_survives_a_write_and_a_rehash() {
    let path = tmp("kinds");
    let mut s = WorldStore::open(&path).expect("open");

    let cases = [
        SubjectRef::Entity(eid("cup-1")),
        SubjectRef::Frame(fid("base_link")),
    ];
    for (n, r) in cases.iter().enumerate() {
        let id = ids(&format!("k{n}"));
        let subject = r
            .stored_id()
            .expect("storable kinds carry an id")
            .to_owned();
        s.append(&ev(&id, &subject, Some(r))).expect("append");
    }

    s.verify_chain().expect("labelled rows verify");
    assert_eq!(s.count().unwrap(), 2);
    let _ = std::fs::remove_file(&path);
}

/// Labelled and unlabelled rows coexist in one chain.
///
/// The mixed case is the real one — a store predating this column keeps its
/// rows, and new writes may be labelled. If the two forms could not sit in one
/// chain the migration would be a rewrite rather than an addition.
#[test]
fn a_chain_may_mix_labelled_and_unlabelled_rows() {
    let path = tmp("mixed");
    let mut s = WorldStore::open(&path).expect("open");
    let r = SubjectRef::Entity(eid("cup-1"));

    s.append(&ev(&ids("a"), "cup-1", None)).expect("unlabelled");
    s.append(&ev(&ids("b"), "cup-1", Some(&r)))
        .expect("labelled");
    s.append(&ev(&ids("c"), "cup-1", None)).expect("unlabelled");

    s.verify_chain().expect("a mixed chain verifies");
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// The label cannot be stored without being hashed
// ---------------------------------------------------------------------------

/// **Labelling a row after the fact breaks the chain.**
///
/// This is the assertion the whole column design rests on. The v2 axes shipped
/// with an orphan-column gap found in review: a value that is *stored but not
/// hashed* can be edited in place, and nothing reports it.
///
/// Here the row was written unlabelled, so the canonical form emitted no
/// `subject_kind` key and the digest was computed without one. Adding the column
/// value afterwards makes the recomputation diverge — which is what proves the
/// column is inside the evidence rather than beside it.
#[test]
fn labelling_a_row_after_the_fact_breaks_the_chain() {
    let path = tmp("relabel-add");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&ev(&ids("a"), "cup-1", None)).expect("unlabelled");
    s.verify_chain().expect("clean before tamper");

    s.tamper_for_test(1, "subject_kind", Some("entity"))
        .expect("the CHECK admits the token; it is the CHAIN that must object");

    match s.verify_chain() {
        Err(StoreError::ChainBroken { generation }) => assert_eq!(generation, 1),
        other => panic!("an added label must break the chain, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

/// ...and so does **changing** one.
///
/// Stated separately from the test above because the two are different edits:
/// adding a label to an unlabelled row, and altering one that was already
/// hashed.
///
/// **This test used to promote a candidate to an entity**, which was the
/// sharpest version of the attack — silently upgrading something
/// unadjudicated into a resolved entity. `KIRRA-WM-CANDIDATE-ID-001` removed
/// `candidate` from the stored vocabulary, so that edit can no longer be
/// staged: there is no such row to tamper with. Recorded rather than quietly
/// rewritten, because the ruling narrowed what this file is able to prove, and
/// a reader comparing it against the PR that introduced it should find out why
/// here rather than from a diff.
///
/// A frame relabelled as an entity is the surviving form of the same edit.
#[test]
fn relabelling_a_frame_as_an_entity_breaks_the_chain() {
    let path = tmp("relabel-change");
    let mut s = WorldStore::open(&path).expect("open");
    let r = SubjectRef::Frame(fid("base_link"));
    s.append(&ev(&ids("a"), "base_link", Some(&r)))
        .expect("labelled frame");
    s.verify_chain().expect("clean before tamper");

    s.tamper_for_test(1, "subject_kind", Some("entity"))
        .expect("tamper");

    match s.verify_chain() {
        Err(StoreError::ChainBroken { generation }) => assert_eq!(generation, 1),
        other => panic!(
            "silently changing what kind of thing a row is about is the exact \
             failure the discriminant exists to prevent, got {other:?}"
        ),
    }
    let _ = std::fs::remove_file(&path);
}

/// A candidate subject is refused, and the refusal names the ruling.
///
/// `KIRRA-WM-CANDIDATE-ID-001`: candidate clustering is a **pure** step, so a
/// candidate id is derived from other rows in this store rather than observed.
/// Freezing it into append-only hashed bytes would record a derivation that a
/// later clustering run cannot correct and nothing detects.
#[test]
fn a_candidate_subject_is_not_storable() {
    let path = tmp("candidate");
    let mut s = WorldStore::open(&path).expect("open");
    let r = SubjectRef::Candidate("cand-1".into());
    let err = s
        .append(&ev(&ids("a"), "cand-1", Some(&r)))
        .expect_err("refused");
    assert!(matches!(err, StoreError::CandidateSubjectNotStorable));
    assert_eq!(s.count().unwrap(), 0, "a refused append writes nothing");

    // The SAME observation is recordable unlabelled -- the ruling removes the
    // label, not the evidence. Asserted so the refusal cannot be read as
    // "observations about candidates cannot be stored", which would be a much
    // larger claim than the one that was adopted.
    s.append(&ev(&ids("a"), "cand-1", None))
        .expect("the observation itself is still recordable");
    s.verify_chain().expect("verifies");
    let _ = std::fs::remove_file(&path);
}

/// Stripping a label breaks the chain too — the third direction.
#[test]
fn stripping_a_label_breaks_the_chain() {
    let path = tmp("relabel-strip");
    let mut s = WorldStore::open(&path).expect("open");
    let r = SubjectRef::Entity(eid("cup-1"));
    s.append(&ev(&ids("a"), "cup-1", Some(&r))).expect("append");
    s.verify_chain().expect("clean before tamper");

    s.tamper_for_test(1, "subject_kind", None).expect("tamper");

    match s.verify_chain() {
        Err(StoreError::ChainBroken { generation }) => assert_eq!(generation, 1),
        other => panic!("a stripped label must break the chain, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// `unbound` is refused by the **schema**, not only by the write path.
///
/// The write path cannot even offer it (see below), so this is the assertion
/// that the vocabulary is closed against raw SQL as well — the same standard
/// `sd2_is_a_schema_check_not_only_a_write_path_check` holds the axes to.
#[test]
fn the_vocabulary_is_closed_against_raw_sql() {
    let path = tmp("closed-vocab");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&ev(&ids("a"), "cup-1", None)).expect("append");

    for rejected in ["candidate", "unbound", "Entity", "", "nonsense"] {
        assert!(
            s.tamper_for_test(1, "subject_kind", Some(rejected))
                .is_err(),
            "the CHECK must refuse {rejected:?} even via raw SQL"
        );
    }
    // ...and a token the vocabulary DOES contain is admitted, so the refusals
    // above are about the vocabulary rather than about the column being
    // unwritable.
    s.tamper_for_test(1, "subject_kind", Some("frame"))
        .expect("a real token is admitted");
    let _ = std::fs::remove_file(&path);
}

/// An unbound subject cannot be labelled, and the refusal names the reason.
#[test]
fn an_unbound_subject_cannot_be_labelled() {
    let path = tmp("unbound");
    let mut s = WorldStore::open(&path).expect("open");
    let err = s
        .append(&ev(&ids("a"), "cup-1", Some(&SubjectRef::Unbound)))
        .expect_err("refused");
    assert!(matches!(err, StoreError::UnboundSubjectNotStorable));
    assert_eq!(s.count().unwrap(), 0, "a refused append writes nothing");
    s.verify_chain()
        .expect("the chain is intact after a refusal");
    let _ = std::fs::remove_file(&path);
}

/// A label naming a different id than `subject` is refused.
///
/// Both values would enter the hashed bytes, and afterwards nothing could say
/// which was meant. The two are separate fields, so this is *enforced* rather
/// than structurally impossible — and the error carries both sides so the
/// disagreement is readable without opening the row.
#[test]
fn a_label_that_names_a_different_id_is_refused() {
    let path = tmp("mismatch");
    let mut s = WorldStore::open(&path).expect("open");
    let r = SubjectRef::Entity(eid("cup-2"));
    let err = s
        .append(&ev(&ids("a"), "cup-1", Some(&r)))
        .expect_err("refused");
    match err {
        StoreError::SubjectRefDisagreesWithSubject { reference, subject } => {
            assert_eq!(reference, "cup-2");
            assert_eq!(subject, "cup-1");
        }
        other => panic!("wrong error: {other:?}"),
    }
    assert_eq!(s.count().unwrap(), 0);

    // The agreeing form of the same write is admitted, so the refusal is about
    // the disagreement and not about labelling being broken.
    let ok = SubjectRef::Entity(eid("cup-1"));
    s.append(&ev(&ids("a"), "cup-1", Some(&ok)))
        .expect("agreeing label");
    s.verify_chain().expect("verifies");
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// Compaction — the rebuild site that is easy to miss
// ---------------------------------------------------------------------------

/// A labelled row survives compaction, and the chain still verifies across the
/// hole.
///
/// **What this does NOT cover, stated because I claimed otherwise first.** An
/// earlier version of this comment said the test exercised `summarize_range`'s
/// rebuild — compaction's rehash. A negative control patching that site alone
/// failed *no test*, including this one. The range digest is computed before the
/// rows are deleted and never re-derived, so nothing observes it and no test
/// can. That site is unverifiable rather than merely uncovered, which is why the
/// two rebuild sites share one helper instead of relying on a test to catch a
/// divergence.
#[test]
fn a_labelled_row_survives_compactions_rehash() {
    let path = tmp("compact");
    let mut s = WorldStore::open(&path).expect("open");
    let r = SubjectRef::Entity(eid("cup-1"));
    for n in 0..4 {
        let id = ids(&format!("c{n}"));
        let labelled = if n % 2 == 0 { Some(&r) } else { None };
        s.append(&ev(&id, "cup-1", labelled)).expect("append");
    }
    s.verify_chain().expect("clean before compaction");

    // Asserted, NOT conditional. An `if ... .is_ok()` here would pass whether or
    // not a compaction ever happened, so a refusal would leave the rehash path
    // this test exists for completely unexercised while the test still reported
    // success.
    let outcome = s
        .compact_range(1, 2, T0 + 1)
        .expect("the window must actually compact, or this test proves nothing");
    s.verify_chain()
        .expect("the chain verifies across a compacted span of labelled rows");
    let _ = outcome;
    let _ = std::fs::remove_file(&path);
}
