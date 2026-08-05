//! The read path, asserted rather than described.
//!
//! Three of these tests are load-bearing beyond ordinary coverage:
//!
//! * `a_candidate_never_reaches_current_state` — SD-2's guarantee is about the
//!   WRITE path. This asserts the read path does not undo it.
//! * `rebuild_from_zero_equals_incremental` — ADR-0041 names this as a property
//!   to test rather than hope for.
//! * `open_leaves_no_projection_tables` — protects ADR-0041 D-20's
//!   `log_only_bytes` from being silently moved by this feature's existence.

use kirra_world_store::{ClaimStatus, NewEvent, WorldStore, WriterClass};

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-world-read-{}-{}.sqlite",
        name,
        std::process::id()
    ));
    for s in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(s);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

fn clean(p: &std::path::Path) {
    for s in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(s);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
}

#[allow(clippy::too_many_arguments)]
fn ev<'a>(
    event_id: &'a str,
    subject: &'a str,
    predicate: Option<&'a str>,
    object: Option<&'a str>,
    valid_from_ms: i64,
    txn_time_ms: i64,
) -> NewEvent<'a> {
    NewEvent {
        event_id,
        observation_id: event_id,
        txn_time_ms,
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
        predicate,
        object,
        payload: r#"{"n":1}"#,
        payload_schema: 1,
        retention_class: "raw",
    }
}

const T0: i64 = 1_700_000_000_000;

// ---------------------------------------------------------------------------
// The guarantee this module exists to preserve
// ---------------------------------------------------------------------------

/// **SD-2 at the read path.** An LLM may only ever write a candidate. If the
/// fold adopted candidates, that write-side guarantee would end at the API:
/// `current()` would hand back a model's guess in the same shape as a sensor's
/// confirmed observation, at exactly the moment a caller acts on it.
#[test]
fn a_candidate_never_reaches_current_state() {
    let p = tmp("candidate-excluded");
    let mut s = WorldStore::open(&p).expect("open");

    s.append(&NewEvent {
        writer_class: WriterClass::LlmCandidate,
        claim_status: ClaimStatus::Candidate,
        ..ev("e1", "cup-1", Some("colour"), Some("red"), T0, T0)
    })
    .expect("a candidate is a legal write");
    s.fold().expect("fold");

    assert!(
        s.current("cup-1", T0 + 1).unwrap().is_empty(),
        "a candidate must not appear in current state"
    );
    assert!(
        s.current_all(T0 + 1).unwrap().is_empty(),
        "nor in the whole-state view"
    );

    // ...but it is not lost. It is reachable only by naming it.
    let c = s.candidates("cup-1").unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].object.as_deref(), Some("red"));
    clean(&p);
}

/// A candidate must not shadow a confirmed fact either — the confirmed claim
/// stays current even though the candidate is newer.
#[test]
fn a_newer_candidate_does_not_displace_a_confirmed_claim() {
    let p = tmp("candidate-no-shadow");
    let mut s = WorldStore::open(&p).expect("open");

    s.append(&ev("e1", "cup-1", Some("colour"), Some("red"), T0, T0))
        .expect("confirmed");
    s.append(&NewEvent {
        writer_class: WriterClass::LlmCandidate,
        claim_status: ClaimStatus::Candidate,
        ..ev(
            "e2",
            "cup-1",
            Some("colour"),
            Some("blue"),
            T0 + 500,
            T0 + 500,
        )
    })
    .expect("candidate");
    s.fold().expect("fold");

    let cur = s.current("cup-1", T0 + 1000).unwrap();
    assert_eq!(cur.len(), 1);
    assert_eq!(
        cur[0].object.as_deref(),
        Some("red"),
        "the confirmed claim must survive a newer candidate"
    );
    clean(&p);
}

// ---------------------------------------------------------------------------
// The measurement this feature must not disturb
// ---------------------------------------------------------------------------

/// ADR-0041 **D-20** measured `log_only_bytes` on a store holding only the
/// event log. Creating projection tables at `open` would add their root pages
/// to every store — moving that figure and invalidating the comparison against
/// D-2 that the whole measurement rests on. So they are installed lazily, and
/// this asserts it by inspecting what `open` actually created.
#[test]
fn open_leaves_no_projection_tables() {
    let p = tmp("no-projection-at-open");
    let mut s = WorldStore::open(&p).expect("open");
    s.append(&ev("e1", "cup-1", None, None, T0, T0)).expect("a");
    s.append(&ev("e2", "cup-2", None, None, T0, T0)).expect("b");

    assert!(
        !s.has_projections().unwrap(),
        "open + append must not install projection tables"
    );
    assert_eq!(s.projection_generation().unwrap(), 0);
    assert!(
        s.current("cup-1", T0 + 1).unwrap().is_empty(),
        "querying an unfolded store must be empty, not an error"
    );

    s.fold().expect("fold");
    assert!(s.has_projections().unwrap(), "the fold installs them");
    clean(&p);
}

// ---------------------------------------------------------------------------
// ADR-0041's stated property
// ---------------------------------------------------------------------------

/// The property ADR-0041 names: a projection is a pure fold, so rebuilding
/// from zero must land on exactly the state an incremental fold reached.
#[test]
fn rebuild_from_zero_equals_incremental() {
    let p = tmp("rebuild-equals-incremental");
    let mut s = WorldStore::open(&p).expect("open");

    // Fold in three batches, so the incremental path really is incremental.
    //
    // `valid_from` deliberately COLLIDES across events (n / 3) so the data is
    // realistic — a sensor batch shares a timestamp routinely.
    //
    // It does NOT make this test sensitive to the generation tiebreak, and it
    // should not be read as doing so: both fold paths iterate generation
    // ascending, so a non-total rule is applied consistently in each and the
    // two still agree. Negative control confirmed that. The tiebreak is
    // covered behaviourally by
    // `a_tie_in_valid_time_resolves_to_the_later_generation`.
    for batch in 0..3 {
        for i in 0..20 {
            let n = batch * 20 + i;
            let eid = format!("e{n}");
            let subj = format!("entity-{:03}", n % 7);
            s.append(&ev(
                &eid,
                &subj,
                Some("position"),
                None,
                T0 + (n / 3) * 50,
                T0 + n * 50,
            ))
            .expect("append");
        }
        s.fold().expect("incremental fold");
    }

    let incremental = s.projection_state_digest().unwrap();
    let incremental_head = s.projection_generation().unwrap();

    s.rebuild_projections().expect("rebuild");
    let rebuilt = s.projection_state_digest().unwrap();

    assert_eq!(
        incremental, rebuilt,
        "rebuild-from-zero diverged from the incremental fold"
    );
    assert_eq!(incremental_head, s.projection_generation().unwrap());
    clean(&p);
}

/// The digest must be non-vacuous: two different states must not collide.
/// Without this, the equality above could pass on a constant.
#[test]
fn the_state_digest_distinguishes_states() {
    let p = tmp("digest-non-vacuous");
    let mut s = WorldStore::open(&p).expect("open");

    s.append(&ev("e1", "cup-1", Some("colour"), Some("red"), T0, T0))
        .expect("a");
    s.fold().expect("fold");
    let one = s.projection_state_digest().unwrap();

    s.append(&ev(
        "e2",
        "cup-1",
        Some("colour"),
        Some("blue"),
        T0 + 100,
        T0 + 100,
    ))
    .expect("b");
    s.fold().expect("fold");
    let two = s.projection_state_digest().unwrap();

    assert_ne!(one, two, "the digest must move when the state moves");
    clean(&p);
}

// ---------------------------------------------------------------------------
// Supersession and bitemporality
// ---------------------------------------------------------------------------

#[test]
fn the_newest_claim_wins_per_subject_and_predicate() {
    let p = tmp("supersede");
    let mut s = WorldStore::open(&p).expect("open");
    s.append(&ev("e1", "cup-1", Some("colour"), Some("red"), T0, T0))
        .expect("a");
    s.append(&ev(
        "e2",
        "cup-1",
        Some("colour"),
        Some("blue"),
        T0 + 100,
        T0 + 100,
    ))
    .expect("b");
    s.append(&ev(
        "e3",
        "cup-1",
        Some("position"),
        Some("shelf"),
        T0 + 50,
        T0 + 50,
    ))
    .expect("c");
    s.fold().expect("fold");

    let cur = s.current("cup-1", T0 + 1000).unwrap();
    assert_eq!(cur.len(), 2, "two predicates, two live claims");
    let colour = cur
        .iter()
        .find(|c| c.predicate.as_deref() == Some("colour"));
    assert_eq!(colour.unwrap().object.as_deref(), Some("blue"));
    clean(&p);
}

/// Two confirmed claims asserting different values **for the same instant**.
/// The later-recorded one must win: it is the more recent assertion about that
/// instant, and returning the earlier would hand back a staler value than the
/// store holds.
///
/// This is what the `generation` tiebreak in `supersedes` buys. Without it the
/// rule is `>` on valid time alone, a tie means "do not adopt", and the FIRST
/// writer wins permanently — a silent staleness bug, not a crash.
#[test]
fn a_tie_in_valid_time_resolves_to_the_later_generation() {
    let p = tmp("valid-time-tie");
    let mut s = WorldStore::open(&p).expect("open");
    s.append(&ev("e1", "cup-1", Some("colour"), Some("red"), T0, T0))
        .expect("first");
    s.append(&ev(
        "e2",
        "cup-1",
        Some("colour"),
        Some("blue"),
        T0,
        T0 + 100,
    ))
    .expect("second, same valid_from");
    s.fold().expect("fold");

    let cur = s.current("cup-1", T0 + 1000).unwrap();
    assert_eq!(cur.len(), 1);
    assert_eq!(
        cur[0].object.as_deref(),
        Some("blue"),
        "a tie in valid time must resolve to the later generation"
    );

    // And the same through the log-replay path, which folds independently.
    let replayed = s.as_of("cup-1", T0 + 1000, T0 + 1000).unwrap();
    assert_eq!(replayed.len(), 1);
    assert_eq!(
        replayed.claims[0].object.as_deref(),
        Some("blue"),
        "as_of must resolve the tie the same way the projection does"
    );
    clean(&p);
}

/// A late-arriving observation about an EARLIER instant must not overwrite the
/// present. This is what distinguishes a bitemporal store from a log with a
/// last-write-wins cache in front of it.
#[test]
fn a_late_arrival_about_the_past_does_not_overwrite_the_present() {
    let p = tmp("late-arrival");
    let mut s = WorldStore::open(&p).expect("open");

    // Learned at T0+1000 that the cup was blue from T0+500.
    s.append(&ev(
        "e1",
        "cup-1",
        Some("colour"),
        Some("blue"),
        T0 + 500,
        T0 + 1000,
    ))
    .expect("a");
    // Then learned, LATER, that it had been red back at T0.
    s.append(&ev(
        "e2",
        "cup-1",
        Some("colour"),
        Some("red"),
        T0,
        T0 + 2000,
    ))
    .expect("b");
    s.fold().expect("fold");

    let cur = s.current("cup-1", T0 + 5000).unwrap();
    assert_eq!(
        cur[0].object.as_deref(),
        Some("blue"),
        "the backfill is older in VALID time and must not become current"
    );

    // But asking about the past does surface it.
    let then = s.as_of("cup-1", T0 + 100, T0 + 5000).unwrap();
    assert_eq!(then.len(), 1);
    assert_eq!(then.claims[0].object.as_deref(), Some("red"));
    clean(&p);
}

/// The second temporal axis: what the store *believed* at a past transaction
/// time, before the backfill arrived.
#[test]
fn as_of_respects_the_transaction_axis() {
    let p = tmp("txn-axis");
    let mut s = WorldStore::open(&p).expect("open");
    s.append(&ev(
        "e1",
        "cup-1",
        Some("colour"),
        Some("blue"),
        T0 + 500,
        T0 + 1000,
    ))
    .expect("a");
    s.append(&ev(
        "e2",
        "cup-1",
        Some("colour"),
        Some("red"),
        T0,
        T0 + 2000,
    ))
    .expect("b");

    // As known at T0+1500 the backfill had not arrived, so nothing was held
    // about valid time T0+100.
    assert!(
        s.as_of("cup-1", T0 + 100, T0 + 1500).unwrap().is_empty(),
        "a fact learned later must not be visible to an earlier as-known-at"
    );
    // As known at T0+5000 it is.
    assert_eq!(s.as_of("cup-1", T0 + 100, T0 + 5000).unwrap().len(), 1);
    clean(&p);
}

#[test]
fn a_bounded_claim_expires() {
    let p = tmp("expiry");
    let mut s = WorldStore::open(&p).expect("open");
    s.append(&NewEvent {
        valid_to_ms: Some(T0 + 200),
        ..ev("e1", "cup-1", Some("colour"), Some("red"), T0, T0)
    })
    .expect("bounded");
    s.fold().expect("fold");

    assert_eq!(s.current("cup-1", T0 + 199).unwrap().len(), 1);
    assert!(
        s.current("cup-1", T0 + 200).unwrap().is_empty(),
        "valid_to is exclusive"
    );
    clean(&p);
}

// ---------------------------------------------------------------------------
// The rest of the surface
// ---------------------------------------------------------------------------

#[test]
fn history_is_every_confirmed_event_oldest_first() {
    let p = tmp("history");
    let mut s = WorldStore::open(&p).expect("open");
    for i in 0..4 {
        let eid = format!("e{i}");
        s.append(&ev(
            &eid,
            "cup-1",
            Some("colour"),
            None,
            T0 + i * 10,
            T0 + i * 10,
        ))
        .expect("append");
    }
    let h = s.history("cup-1").unwrap().claims;
    assert_eq!(h.len(), 4);
    assert!(
        h.windows(2).all(|w| w[0].generation < w[1].generation),
        "history must be oldest-first"
    );
    clean(&p);
}

#[test]
fn changed_since_uses_transaction_time() {
    let p = tmp("changed-since");
    let mut s = WorldStore::open(&p).expect("open");
    s.append(&ev("e1", "cup-1", None, None, T0, T0)).expect("a");
    // Valid in the past, learned recently — must still be reported.
    s.append(&ev("e2", "cup-2", None, None, T0 - 9_000, T0 + 500))
        .expect("b");

    let changed = s.changed_since(T0 + 100).unwrap();
    assert_eq!(
        changed,
        vec!["cup-2".to_string()],
        "a late arrival about the past is a CHANGE in what we know"
    );
    clean(&p);
}

/// Folding twice with nothing new must be a no-op, and must not re-scan from
/// the start — the checkpoint has to advance past events it considered and
/// rejected, not only those it adopted.
#[test]
fn a_second_fold_with_no_new_events_is_stable() {
    let p = tmp("idempotent");
    let mut s = WorldStore::open(&p).expect("open");
    s.append(&NewEvent {
        writer_class: WriterClass::LlmCandidate,
        claim_status: ClaimStatus::Candidate,
        ..ev("e1", "cup-1", Some("colour"), Some("red"), T0, T0)
    })
    .expect("candidate only");

    let head1 = s.fold().expect("fold");
    let d1 = s.projection_state_digest().unwrap();
    let head2 = s.fold().expect("fold again");
    let d2 = s.projection_state_digest().unwrap();

    assert_eq!(d1, d2, "a no-op fold must not move the state");
    assert_eq!(head1, head2);
    assert_eq!(
        head1, 1,
        "the checkpoint must pass a candidate-only batch, or it re-scans forever"
    );
    clean(&p);
}

/// `projected_row_count` must count the TABLE, not the time-filtered view.
///
/// A bounded claim whose `valid_to_ms` has passed still occupies a row and
/// still costs bytes; counting via `current_all` would exclude it and
/// undercount a figure whose entire subject is size. With every claim expired
/// the two answers differ maximally — `current_all` sees none, the table holds
/// them all.
#[test]
fn projected_row_count_is_not_time_filtered() {
    let p = tmp("row-count");
    let mut s = WorldStore::open(&p).expect("open");
    for i in 1..=5i64 {
        let eid = format!("e{i}");
        let subj = format!("cup-{i}");
        s.append(&NewEvent {
            valid_to_ms: Some(T0 + 100),
            ..ev(&eid, &subj, Some("colour"), None, T0, T0)
        })
        .expect("bounded claim");
    }
    s.fold().expect("fold");

    let after_expiry = T0 + 10_000;
    assert!(
        s.current_all(after_expiry).unwrap().is_empty(),
        "every claim has expired, so the state view is empty"
    );
    assert_eq!(
        s.projected_row_count().unwrap(),
        5,
        "but the rows are still there, and still cost bytes"
    );
    clean(&p);
}

/// The fold's all-or-nothing claim must cover the state digest too. If the
/// digest were written after commit, a failure there would report a fold that
/// had in fact happened as failed — the store and the caller disagreeing about
/// whether it ran.
#[test]
fn a_successful_fold_leaves_a_checkpoint_matching_its_state() {
    let p = tmp("fold-atomic");
    let mut s = WorldStore::open(&p).expect("open");
    s.append(&ev("e1", "cup-1", Some("colour"), Some("red"), T0, T0))
        .expect("a");
    s.fold().expect("fold");

    let recorded = s.checkpoint_state_digest_for_test().unwrap();
    assert_eq!(
        recorded,
        Some(s.projection_state_digest().unwrap()),
        "the committed checkpoint must describe the committed state"
    );

    // And it must keep matching after a second fold moves the state.
    s.append(&ev(
        "e2",
        "cup-1",
        Some("colour"),
        Some("blue"),
        T0 + 100,
        T0 + 100,
    ))
    .expect("b");
    s.fold().expect("fold again");
    assert_eq!(
        s.checkpoint_state_digest_for_test().unwrap(),
        Some(s.projection_state_digest().unwrap())
    );
    clean(&p);
}

/// The chain must still verify after folding — the read path is a derived
/// cache and must never touch the evidence log.
#[test]
fn folding_does_not_disturb_the_chain() {
    let p = tmp("chain-intact");
    let mut s = WorldStore::open(&p).expect("open");
    for i in 0..10 {
        let eid = format!("e{i}");
        s.append(&ev(&eid, "cup-1", Some("colour"), None, T0 + i, T0 + i))
            .expect("append");
    }
    s.fold().expect("fold");
    s.rebuild_projections().expect("rebuild");
    s.verify_chain()
        .expect("the log must be untouched by the read path");
    assert_eq!(s.count().unwrap(), 10);
    clean(&p);
}
