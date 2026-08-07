//! The per-subject summary projection, against a real store.
//!
//! The pure fold is unit-tested in `subject_projection`. What can only be tested
//! here is that the **SQL upsert computes the same thing** — the fold exists
//! twice, once in Rust and once as an `ON CONFLICT DO UPDATE`, and two
//! implementations of one rule drift unless something compares them.

use kirra_world_store::{
    subject_fold_all, ClaimStatus, EventId, NewEvent, ObservationId, SubjectObservation,
    WorldStore, WriterClass,
};

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-world-subjsum-{}-{}.sqlite",
        name,
        std::process::id()
    ));
    clean(&p);
    p
}

/// Remove the database AND its WAL sidecars.
///
/// `synchronous=FULL` in WAL mode leaves `-wal` and `-shm` beside the main
/// file, so removing only the `.sqlite` leaks two files per test — and worse,
/// a surviving `-wal` beside a recreated database of the same name is a
/// recovery source, which can make a later run see rows it never wrote.
fn clean(p: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
}

struct Ids {
    event: EventId,
    observation: ObservationId,
}

fn ids(n: i64) -> Ids {
    Ids {
        event: EventId::new(format!("evt-{n}")).unwrap(),
        observation: ObservationId::new(format!("obs-{n}")).unwrap(),
    }
}

fn ev<'a>(id: &'a Ids, subject: &'a str, txn_time_ms: i64, status: ClaimStatus) -> NewEvent<'a> {
    NewEvent {
        event_id: &id.event,
        observation_id: &id.observation,
        txn_time_ms,
        valid_from_ms: txn_time_ms,
        valid_to_ms: None,
        source: "sensor-a",
        source_version: "0.1.0",
        writer_class: match status {
            ClaimStatus::Candidate => WriterClass::LlmCandidate,
            ClaimStatus::Confirmed => WriterClass::Sensor,
        },
        claim_status: status,
        provenance: &[],
        frame_id: None,
        map_id: None,
        kind: "observation",
        subject,
        predicate: Some("colour"),
        object: Some("red"),
        payload: r#"{"n":1}"#,
        payload_schema: 1,
        retention_class: "raw",
        trust: None,
    }
}

/// D-20's `log_only_bytes` is the size of a store holding only the event log.
/// Installing projection tables at `open` would add their root pages to every
/// store — including one that never projects — silently moving that figure and
/// invalidating the comparison the retention horizons rest on.
#[test]
fn open_leaves_no_subject_summary_table() {
    let path = tmp("lazy");
    let s = WorldStore::open(&path).expect("open");
    assert!(
        !s.has_subject_summary().expect("catalogue"),
        "the summary table must not exist until the first fold"
    );
    assert_eq!(s.subject_summary_generation().expect("generation"), 0);
    assert!(s.subject_summaries().expect("read").is_empty());
    clean(&path);
}

#[test]
fn the_three_fields_are_folded_from_the_log() {
    let path = tmp("fields");
    let mut s = WorldStore::open(&path).expect("open");
    for (n, txn) in [(1, 100), (2, 300), (3, 700)] {
        s.append(&ev(&ids(n), "cup-1", txn, ClaimStatus::Confirmed))
            .expect("append");
    }
    s.fold_subject_summary().expect("fold");

    let got = s.subject_summary("cup-1").expect("read").expect("present");
    assert_eq!(got.first_observed_ms, 100);
    assert_eq!(got.last_observed_ms, 700);
    assert_eq!(got.observation_count, 3);
    assert_eq!(got.last_generation, 3);
    assert_eq!(got.last_event_id, "evt-3");

    // provenance_head names a real position in the tamper-evident log, which is
    // what makes the summary citable rather than merely informative.
    assert!(
        !got.provenance_head.is_empty(),
        "the head must be a chain digest"
    );
    clean(&path);
}

/// **The two implementations of the fold must agree.**
///
/// `subject_fold_step` is the rule in Rust; the `ON CONFLICT DO UPDATE` is the
/// same rule in SQL. Nothing but this test stops them drifting.
#[test]
fn the_sql_upsert_computes_what_the_pure_fold_computes() {
    let path = tmp("agree");
    let mut s = WorldStore::open(&path).expect("open");

    let rows: [(i64, &str, i64); 6] = [
        (1, "cup-1", 100),
        (2, "cup-2", 150),
        (3, "cup-1", 300),
        (4, "cup-3", 90),
        (5, "cup-2", 500),
        (6, "cup-1", 200), // out of time order on purpose
    ];
    for (n, subject, txn) in rows {
        s.append(&ev(&ids(n), subject, txn, ClaimStatus::Confirmed))
            .expect("append");
    }
    s.fold_subject_summary().expect("fold");

    // Rebuild the same input for the pure fold, reading the chain digests back
    // out of the store so both sides see identical values.
    let stored = s.subject_summaries().expect("read");
    let observations: Vec<SubjectObservation> = rows
        .iter()
        .map(|(n, subject, txn)| SubjectObservation {
            subject: (*subject).to_string(),
            txn_time_ms: *txn,
            generation: *n,
            event_id: format!("evt-{n}"),
            chain_digest: stored
                .iter()
                .find(|p| p.subject == *subject && p.last_generation == *n)
                .map_or_else(String::new, |p| p.provenance_head.clone()),
        })
        .collect();
    let pure = subject_fold_all(&observations);

    assert_eq!(stored.len(), pure.len(), "same subjects");
    for p in &stored {
        let expected = pure.get(&p.subject).expect("subject in the pure fold");
        assert_eq!(p.first_observed_ms, expected.first_observed_ms, "{p:?}");
        assert_eq!(p.last_observed_ms, expected.last_observed_ms, "{p:?}");
        assert_eq!(p.observation_count, expected.observation_count, "{p:?}");
        assert_eq!(p.last_generation, expected.last_generation, "{p:?}");
        assert_eq!(p.last_event_id, expected.last_event_id, "{p:?}");
        // Comparable because the lookup above supplies the real digest for
        // exactly the head generation of each subject — the non-head entries
        // carry an empty placeholder, and by construction none of them can win
        // the head. If the two folds disagreed about WHICH event is the head,
        // one side would land on that placeholder and this would fail.
        assert_eq!(p.provenance_head, expected.provenance_head, "{p:?}");
        assert!(
            !p.provenance_head.is_empty(),
            "a real digest, not the placeholder"
        );
    }
    clean(&path);
}

/// The head follows generation, not time — asserted through the real store, not
/// only through the pure fold. Generation 6 carries an earlier timestamp than
/// generation 3, and must still be the head.
#[test]
fn the_head_follows_generation_through_the_store_too() {
    let path = tmp("head-gen");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&ev(&ids(1), "cup-1", 100, ClaimStatus::Confirmed))
        .expect("a");
    s.append(&ev(&ids(2), "cup-1", 900, ClaimStatus::Confirmed))
        .expect("b");
    s.append(&ev(&ids(3), "cup-1", 200, ClaimStatus::Confirmed))
        .expect("c");
    s.fold_subject_summary().expect("fold");

    let got = s.subject_summary("cup-1").expect("read").expect("present");
    assert_eq!(got.last_generation, 3, "the head is the newest generation");
    assert_eq!(got.last_event_id, "evt-3");
    assert_eq!(
        got.last_observed_ms, 900,
        "but the observed bound is the latest TIME, which is a different event"
    );
    assert_eq!(got.first_observed_ms, 100);
    clean(&path);
}

/// Confirmed-only, following `world_current`'s precedent. A subject known only
/// from candidates has no row — the honest answer, since nothing confirmed has
/// been observed about it.
#[test]
fn candidates_do_not_contribute() {
    let path = tmp("candidates");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&ev(&ids(1), "cup-1", 100, ClaimStatus::Confirmed))
        .expect("confirmed");
    s.append(&ev(&ids(2), "cup-1", 200, ClaimStatus::Candidate))
        .expect("candidate");
    s.append(&ev(&ids(3), "ghost-1", 300, ClaimStatus::Candidate))
        .expect("candidate-only subject");
    s.fold_subject_summary().expect("fold");

    let cup = s.subject_summary("cup-1").expect("read").expect("present");
    assert_eq!(
        cup.observation_count, 1,
        "the candidate must not count toward the summary"
    );
    assert_eq!(cup.last_observed_ms, 100);
    assert!(
        s.subject_summary("ghost-1").expect("read").is_none(),
        "a candidate-only subject has no confirmed observation to summarise"
    );
    clean(&path);
}

/// ADR-0041's purity property, at the store level: a full rebuild must equal an
/// incremental fold.
#[test]
fn rebuild_equals_incremental() {
    let path = tmp("purity");
    let mut s = WorldStore::open(&path).expect("open");

    for (n, subject, txn) in [(1i64, "cup-1", 100i64), (2, "cup-2", 200)] {
        s.append(&ev(&ids(n), subject, txn, ClaimStatus::Confirmed))
            .expect("append");
    }
    s.fold_subject_summary().expect("first fold");

    for (n, subject, txn) in [(3i64, "cup-1", 300i64), (4, "cup-3", 50)] {
        s.append(&ev(&ids(n), subject, txn, ClaimStatus::Confirmed))
            .expect("append");
    }
    s.fold_subject_summary().expect("incremental fold");
    let incremental = s.subject_summary_state_digest().expect("digest");

    s.rebuild_subject_summary().expect("rebuild");
    let rebuilt = s.subject_summary_state_digest().expect("digest");

    assert_eq!(
        incremental, rebuilt,
        "an incremental fold must equal a rebuild from zero"
    );
    clean(&path);
}

/// The checkpoint advances past every event CONSIDERED, not merely adopted —
/// otherwise a batch of candidates would be re-scanned on every fold forever.
#[test]
fn the_checkpoint_advances_past_candidates() {
    let path = tmp("checkpoint");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&ev(&ids(1), "cup-1", 100, ClaimStatus::Candidate))
        .expect("candidate");
    s.append(&ev(&ids(2), "cup-2", 200, ClaimStatus::Candidate))
        .expect("candidate");
    s.fold_subject_summary().expect("fold");

    assert_eq!(
        s.subject_summary_generation().expect("generation"),
        2,
        "a fold that adopted nothing must still have consumed the log"
    );
    assert!(s.subject_summaries().expect("read").is_empty());
    clean(&path);
}

/// Folding twice with nothing new in between must change nothing — an idempotence
/// property the `observation_count` increment would break if the checkpoint were
/// ignored, since every event would be counted again.
#[test]
fn a_second_fold_with_no_new_events_is_a_no_op() {
    let path = tmp("idempotent");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&ev(&ids(1), "cup-1", 100, ClaimStatus::Confirmed))
        .expect("append");
    s.append(&ev(&ids(2), "cup-1", 200, ClaimStatus::Confirmed))
        .expect("append");
    s.fold_subject_summary().expect("fold");
    let first = s.subject_summary_state_digest().expect("digest");

    s.fold_subject_summary().expect("fold again");
    let second = s.subject_summary_state_digest().expect("digest");

    assert_eq!(first, second, "re-folding must not double-count");
    assert_eq!(
        s.subject_summary("cup-1")
            .expect("read")
            .expect("present")
            .observation_count,
        2
    );
    clean(&path);
}

/// An empty log folds to an empty summary rather than erroring.
#[test]
fn an_empty_log_folds_to_an_empty_summary() {
    let path = tmp("empty");
    let mut s = WorldStore::open(&path).expect("open");
    s.fold_subject_summary().expect("fold an empty log");
    assert!(s.subject_summaries().expect("read").is_empty());
    clean(&path);
}
