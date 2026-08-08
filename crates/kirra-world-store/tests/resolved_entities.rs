//! The resolved-entity projection — `subject_summary` keyed on `(kind, subject)`.
//!
//! This closes the gap `subject_projection`'s own module docs recorded: the
//! layer *"flattens all four into one `subject TEXT NOT NULL` column and keeps
//! no discriminant, so a fold here **cannot** restrict itself to resolved
//! entities."* It can now.
//!
//! Three properties carry the weight, and none of them is the happy path:
//!
//! 1. **An entity and a frame sharing an id string are separate rows.** That is
//!    why the kind is half the KEY rather than a column beside it.
//! 2. **Unlabelled rows still fold.** The key column is NOT NULL with a
//!    sentinel, because `NULL` never conflicts with `NULL` in SQLite and a
//!    nullable key would stop the upsert matching — for every row in a store
//!    today, all of which are unlabelled.
//! 3. **A projection of the old shape is rebuilt from zero, not resumed.**

use kirra_world_store::{
    ClaimStatus, EntityId, EventId, FrameId, NewEvent, ObservationId, SubjectRef, SummaryKind,
    WorldStore, WriterClass,
};

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-world-resolved-{}-{}.sqlite",
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

fn ev<'a>(
    id: &'a Ids,
    subject: &'a str,
    subject_ref: Option<&'a SubjectRef>,
    txn_time_ms: i64,
) -> NewEvent<'a> {
    NewEvent {
        event_id: &id.event,
        observation_id: &id.observation,
        txn_time_ms,
        valid_from_ms: txn_time_ms,
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

fn entity(id: &str) -> SubjectRef {
    SubjectRef::Entity(EntityId::new(id).expect("entity id"))
}

fn frame(id: &str) -> SubjectRef {
    SubjectRef::Frame(FrameId::new(id).expect("frame id"))
}

// ---------------------------------------------------------------------------
// The query this projection was rebuilt to answer
// ---------------------------------------------------------------------------

/// `resolved_entities` returns resolved entities and nothing else.
#[test]
fn resolved_entities_excludes_frames_and_unlabelled_subjects() {
    let path = tmp("resolved");
    let mut s = WorldStore::open(&path).expect("open");

    let e = entity("cup-1");
    let f = frame("base_link");
    s.append(&ev(&ids("a"), "cup-1", Some(&e), T0)).expect("a");
    s.append(&ev(&ids("b"), "base_link", Some(&f), T0 + 1))
        .expect("b");
    s.append(&ev(&ids("c"), "mystery-1", None, T0 + 2))
        .expect("c");
    s.fold_subject_summary().expect("fold");

    let resolved = s.resolved_entities().expect("read");
    let names: Vec<&str> = resolved.iter().map(|r| r.subject.as_str()).collect();
    assert_eq!(names, vec!["cup-1"], "only the resolved entity");
    assert!(resolved
        .iter()
        .all(|r| r.subject_kind == SummaryKind::Entity));

    // ...and the others are still SUMMARISED, just not as entities. Asserted so
    // the filter cannot be mistaken for the projection having dropped them: a
    // fold that silently omitted rows would under-report, and under-reporting
    // is indistinguishable from "never observed".
    assert_eq!(s.subject_summaries().expect("all").len(), 3);
    let _ = std::fs::remove_file(&path);
}

/// **An entity and a frame with the same id string are two rows.**
///
/// The case that makes the kind part of the key rather than a column beside it.
/// Keyed on the subject string alone these fold into one row whose counts and
/// time bounds are summed across two different things and whose kind flip-flops
/// with whichever event was seen last — a summary of neither.
#[test]
fn one_id_string_under_two_kinds_is_two_summaries() {
    let path = tmp("collide");
    let mut s = WorldStore::open(&path).expect("open");

    let e = entity("base_link");
    let f = frame("base_link");
    s.append(&ev(&ids("a"), "base_link", Some(&e), T0))
        .expect("as entity");
    s.append(&ev(&ids("b"), "base_link", Some(&f), T0 + 5_000))
        .expect("as frame");
    s.fold_subject_summary().expect("fold");

    let both = s.subject_summaries_for("base_link").expect("read");
    assert_eq!(both.len(), 2, "one string, two things");

    let as_entity = s
        .subject_summary(SummaryKind::Entity, "base_link")
        .expect("read")
        .expect("present");
    let as_frame = s
        .subject_summary(SummaryKind::Frame, "base_link")
        .expect("read")
        .expect("present");

    // Each counts ONLY its own events. A single merged row would show 2 here,
    // which is the specific wrongness the composite key prevents.
    assert_eq!(as_entity.observation_count, 1);
    assert_eq!(as_frame.observation_count, 1);
    assert_eq!(as_entity.first_observed_ms, T0);
    assert_eq!(as_frame.first_observed_ms, T0 + 5_000);

    // And the entity query returns the entity, not "whichever SQLite yielded".
    let resolved = s.resolved_entities().expect("read");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].subject_kind, SummaryKind::Entity);
    let _ = std::fs::remove_file(&path);
}

/// Unlabelled subjects still fold into ONE row per subject.
///
/// The regression guard for the sentinel — and it guards a narrower thing than
/// it first appears, which is worth stating because the difference decides
/// whether a future reader is actually protected.
///
/// `ON CONFLICT (subject_kind, subject)` stops matching when the stored value is
/// `NULL`, because `NULL` never conflicts with `NULL` in SQLite. Measured before
/// the design rather than recalled. Every row in a store today is unlabelled, so
/// that fold would produce one row of `observation_count = 1` per EVENT while
/// looking entirely plausible.
///
/// **What fires this test is writing `NULL`, not the column being nullable.**
/// A control that only relaxed the DDL to `TEXT` (dropping `NOT NULL`) failed
/// *nothing* — the driver still wrote the `unlabelled` token, so the fold still
/// folded. Adding the second half, a driver that writes `None` for
/// [`SummaryKind::Unlabelled`], fires this test and three others. So the
/// `NOT NULL` is a belt to the driver's braces, and this test is the braces.
#[test]
fn unlabelled_subjects_still_fold_into_one_row() {
    let path = tmp("sentinel");
    let mut s = WorldStore::open(&path).expect("open");
    for (n, t) in [("a", T0), ("b", T0 + 1), ("c", T0 + 2)] {
        s.append(&ev(&ids(n), "mystery-1", None, t))
            .expect("append");
    }
    s.fold_subject_summary().expect("fold");

    let all = s.subject_summaries().expect("read");
    assert_eq!(all.len(), 1, "three events, ONE subject, one row");
    assert_eq!(all[0].observation_count, 3, "folded, not split");
    assert_eq!(all[0].subject_kind, SummaryKind::Unlabelled);
    assert_eq!(all[0].first_observed_ms, T0);
    assert_eq!(all[0].last_observed_ms, T0 + 2);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// The shape change
// ---------------------------------------------------------------------------

/// A projection installed before `subject_kind` is dropped and rebuilt, and the
/// checkpoint goes with it.
///
/// Both halves matter and only one is obvious. `SUBJECT_PROJECTION_V1` is all
/// `CREATE ... IF NOT EXISTS`, so an existing table of the old shape survives
/// untouched — hence the drop. But dropping the table while leaving
/// `projection_checkpoint` at generation N makes the next fold resume from N
/// into an EMPTY table, producing a projection missing everything below N. That
/// failure reports success and returns a plausible, short answer.
#[test]
fn an_old_shape_projection_is_rebuilt_from_zero_not_resumed() {
    let path = tmp("oldshape");
    let mut s = WorldStore::open(&path).expect("open");
    for (n, t) in [("a", T0), ("b", T0 + 1), ("c", T0 + 2)] {
        s.append(&ev(&ids(n), "mystery-1", None, t))
            .expect("append");
    }
    s.fold_subject_summary().expect("fold");
    assert_eq!(s.subject_summaries().expect("read").len(), 1);
    let before = s.subject_summary_generation().expect("checkpoint");
    assert!(
        before > 0,
        "the checkpoint advanced, so resuming is possible"
    );

    // Recreate the PRE-kind shape underneath the store, leaving the checkpoint
    // exactly where a real old store's would be.
    // One call per statement: `raw_execute_for_test` is `Connection::execute`,
    // which runs a SINGLE statement and silently ignores everything after the
    // first semicolon. A batch here dropped the table without recreating it,
    // and the test then failed for that reason rather than the one it is about.
    s.raw_execute_for_test("DROP TABLE subject_summary")
        .expect("drop");
    s.raw_execute_for_test(
        "CREATE TABLE subject_summary (
            subject            TEXT    PRIMARY KEY,
            first_observed_ms  INTEGER NOT NULL,
            last_observed_ms   INTEGER NOT NULL,
            provenance_head    TEXT    NOT NULL,
            observation_count  INTEGER NOT NULL,
            last_generation    INTEGER NOT NULL,
            last_event_id      TEXT    NOT NULL)",
    )
    .expect("stage the old shape");
    assert_eq!(
        s.subject_summary_generation().expect("checkpoint"),
        before,
        "the checkpoint is still advanced -- this is what makes the test non-vacuous"
    );

    // The next fold must notice the shape, rebuild, and re-scan from zero.
    s.fold_subject_summary().expect("fold after shape change");
    let all = s.subject_summaries().expect("read");
    assert_eq!(all.len(), 1, "the subject came back");
    assert_eq!(
        all[0].observation_count, 3,
        "every event was re-scanned; a resumed fold would have found none"
    );
    let _ = std::fs::remove_file(&path);
}

/// The evidence `CHECK` refuses a kind token outside its vocabulary.
///
/// # Why this is not the fail-closed fold test I first wrote
///
/// The fold resolves `subject_kind` at the boundary and returns
/// `StoreError::CorruptRow` for a token it cannot read. I tried to exercise that
/// by writing `'nonsense'` into the column and folding — and could not, because
/// **this `CHECK` refuses the write**, which is what the failure below shows.
///
/// Getting past it needs `PRAGMA writable_schema` surgery on `sqlite_master`,
/// which depends on the exact stored DDL text and is a fragile thing to pin a
/// test to. So the honest position is recorded instead of a hack: while the
/// `CHECK` holds, the fold's refusal is **unreachable through SQLite** and is
/// defence-in-depth against a file corrupted outside it. The mapping itself is
/// covered directly by `SummaryKind::from_event_column`'s unit tests; what this
/// test owns is the line of defence that actually fires.
#[test]
fn the_evidence_check_refuses_an_unreadable_kind_token() {
    let path = tmp("corrupt");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&ev(&ids("a"), "cup-1", None, T0)).expect("append");

    let err = s
        .raw_execute_for_test(
            "UPDATE world_events SET subject_kind = 'nonsense' WHERE generation = 1",
        )
        .expect_err("the CHECK must refuse a token outside the vocabulary");
    assert!(
        format!("{err}").contains("CHECK"),
        "expected a constraint failure, got {err}"
    );

    // The refused write left the store readable and the fold working, so the
    // refusal is a refusal rather than damage.
    s.fold_subject_summary().expect("fold still works");
    assert_eq!(s.subject_summaries().expect("read").len(), 1);
    let _ = std::fs::remove_file(&path);
}
