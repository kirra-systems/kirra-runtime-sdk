//! What a query gets when it reaches into a compacted window (ADR-0041 §11.3).
//!
//! §11.3: *"compaction must never silently rewrite history. A time-travel query
//! into a compacted window returns the summary **and says so** — degraded
//! resolution, never silent fabrication."*
//!
//! #1354 landed the first half of that sentence — the citation, and the chain
//! that verifies across a hole. It did not land the second. A `history()` into
//! a compacted window simply returned fewer rows, and nothing distinguished
//! *"nothing was known about this"* from *"we deleted it"*. Both looked like a
//! complete answer.
//!
//! These tests are about that distinction. The properties they hold to:
//!
//! * a query whose range was compacted is reported **degraded**, and carries
//!   the retained per-key summaries;
//! * a query the compaction could not have borne on is reported **full**, so
//!   the signal stays worth reading;
//! * the direction of error is fixed — over-report, never under-report,
//!   including for a store compacted before summaries existed;
//! * summaries are **confirmed-only**, so an LLM candidate never stands in for
//!   a deleted observation;
//! * `current()` is provably never degraded, which is the second thing the
//!   projection-head refusal buys.

use kirra_world_store::{
    ClaimStatus, EventId, NewEvent, ObservationId, Resolution, StoreError, WorldStore, WriterClass,
};

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-world-degraded-{}-{}.sqlite",
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

struct Ev {
    event_id: EventId,
    observation_id: ObservationId,
    subject: String,
    predicate: Option<String>,
    object: Option<String>,
    payload: String,
    valid_from_ms: i64,
    txn_time_ms: i64,
    status: ClaimStatus,
    retention_class: String,
}

impl Ev {
    fn new(event_id: &str, subject: &str, valid_from_ms: i64) -> Self {
        Self {
            event_id: EventId::new(event_id).expect("admissible event id"),
            observation_id: ObservationId::new(event_id).expect("admissible observation id"),
            subject: subject.to_string(),
            predicate: Some("position".to_string()),
            object: None,
            payload: r#"{"n":1}"#.to_string(),
            valid_from_ms,
            txn_time_ms: valid_from_ms,
            status: ClaimStatus::Confirmed,
            retention_class: "raw".to_string(),
        }
    }
    fn object(mut self, o: &str) -> Self {
        self.object = Some(o.to_string());
        self
    }
    fn payload(mut self, p: &str) -> Self {
        self.payload = p.to_string();
        self
    }
    fn predicate(mut self, p: Option<&str>) -> Self {
        self.predicate = p.map(str::to_string);
        self
    }
    fn txn(mut self, t: i64) -> Self {
        self.txn_time_ms = t;
        self
    }
    fn candidate(mut self) -> Self {
        self.status = ClaimStatus::Candidate;
        self
    }
    fn class(mut self, c: &str) -> Self {
        self.retention_class = c.to_string();
        self
    }
    fn as_new(&self) -> NewEvent<'_> {
        NewEvent {
            event_id: &self.event_id,
            observation_id: &self.observation_id,
            txn_time_ms: self.txn_time_ms,
            valid_from_ms: self.valid_from_ms,
            valid_to_ms: None,
            source: "sensor-a",
            source_version: "0.1.0",
            writer_class: match self.status {
                ClaimStatus::Candidate => WriterClass::LlmCandidate,
                ClaimStatus::Confirmed => WriterClass::Sensor,
            },
            claim_status: self.status,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "observation",
            subject: &self.subject,
            predicate: self.predicate.as_deref(),
            object: self.object.as_deref(),
            payload: &self.payload,
            payload_schema: 1,
            retention_class: &self.retention_class,
        }
    }
}

fn write(s: &mut WorldStore, e: &Ev) -> i64 {
    s.append(&e.as_new()).expect("append")
}

/// Six observations of `cup-0`'s position, then six of `cup-1`'s. Compacting
/// 1..=6 removes `cup-0`'s history entirely and leaves `cup-1` untouched —
/// which is what makes "an untouched subject is not degraded" testable.
fn seeded(path: &std::path::Path) -> WorldStore {
    let mut s = WorldStore::open(path).expect("open");
    for i in 1..=6i64 {
        write(
            &mut s,
            &Ev::new(&format!("a{i}"), "cup-0", T0 + i * 100).object(&format!("p{i}")),
        );
    }
    for i in 1..=6i64 {
        write(
            &mut s,
            &Ev::new(&format!("b{i}"), "cup-1", T0 + 1_000 + i * 100).object(&format!("q{i}")),
        );
    }
    s
}

// ---------------------------------------------------------------------------
// The signal itself
// ---------------------------------------------------------------------------

/// The gap #1354 left. A `history()` into a compacted window used to return
/// fewer rows and say nothing; it must now say the window was compacted and
/// hand back what stands in for it.
#[test]
fn history_across_a_compacted_window_is_degraded_and_carries_the_summary() {
    let p = tmp("history-degraded");
    let mut s = seeded(&p);
    let out = s.compact_range(1, 6, T0 + 9_000).expect("compact");

    let h = s.history("cup-0").unwrap();
    assert!(h.is_empty(), "every cup-0 observation was in the window");
    assert!(
        h.is_degraded(),
        "an empty answer after a compaction must not read as `nothing was known`"
    );

    let summaries = h.resolution.summaries();
    assert_eq!(summaries.len(), 1, "one (subject, predicate) key");
    let sm = &summaries[0];
    assert_eq!(sm.subject, "cup-0");
    assert_eq!(sm.predicate.as_deref(), Some("position"));
    assert_eq!(sm.event_count, 6);
    assert_eq!(sm.first_valid_from_ms, T0 + 100);
    assert_eq!(sm.last_valid_from_ms, T0 + 600);
    assert_eq!(sm.last_object.as_deref(), Some("p6"));

    // The span that hid it is named, and it is the citation that was minted.
    let spans = h.resolution.spans();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].lo_generation, out.citation.lo_generation);
    assert_eq!(spans[0].range_digest, out.citation.range_digest);
    clean(&p);
}

/// The outcome reports what it retained, so "compaction buys summaries, not
/// observations" (OQ2 rule 2) is visible at the call site rather than only in
/// the database.
#[test]
fn the_outcome_returns_the_summaries_it_retained() {
    let p = tmp("outcome-summaries");
    let mut s = seeded(&p);
    let out = s.compact_range(1, 6, T0 + 9_000).expect("compact");
    assert_eq!(out.removed, 6);
    assert_eq!(out.summaries.len(), 1);
    assert_eq!(out.summaries[0].event_count, 6);
    assert_eq!(out.summaries, s.summaries().unwrap(), "as persisted");
    clean(&p);
}

/// The signal has to be worth reading. If every query after any compaction
/// came back degraded, the caveat would be ignored within a week.
#[test]
fn a_subject_the_window_never_held_is_not_degraded() {
    let p = tmp("untouched");
    let mut s = seeded(&p);
    s.compact_range(1, 6, T0 + 9_000).expect("compact");

    let h = s.history("cup-1").unwrap();
    assert_eq!(h.len(), 6, "cup-1 was not in the window");
    assert_eq!(
        h.resolution,
        Resolution::Full,
        "a compaction that removed nothing about this subject must not \
         degrade its answer"
    );
    clean(&p);
}

// ---------------------------------------------------------------------------
// The bitemporal filters, and the direction they are allowed to be wrong in
// ---------------------------------------------------------------------------

/// Sound on the valid-time axis: a removed event with `valid_from > valid_at`
/// could never have appeared in the answer (`holds_at` requires
/// `valid_from <= now`), so ruling the window out is exact, not optimistic.
///
/// Both directions are asserted, so the test cannot pass by always answering
/// `Full`.
#[test]
fn as_of_before_the_window_began_is_full_and_after_it_is_degraded() {
    let p = tmp("valid-axis");
    let mut s = seeded(&p);
    s.compact_range(1, 6, T0 + 9_000).expect("compact");

    let before = s.as_of("cup-0", T0 + 50, T0 + 9_000).unwrap();
    assert_eq!(
        before.resolution,
        Resolution::Full,
        "the earliest removed observation began at T0+100; nothing in the \
         window could hold at T0+50"
    );

    let after = s.as_of("cup-0", T0 + 100, T0 + 9_000).unwrap();
    assert!(
        after.is_degraded(),
        "at T0+100 the first removed observation held — the answer is missing it"
    );
    clean(&p);
}

/// The same soundness on the transaction axis, which is why the summary
/// carries `first_txn_time_ms` at all. Without it, `as_of` could only rule a
/// window out on one of its two dimensions.
#[test]
fn as_of_before_the_window_was_known_is_full() {
    let p = tmp("txn-axis");
    let mut s = WorldStore::open(&p).expect("open");
    // Valid long ago, but only learned at T0+5_000.
    write(
        &mut s,
        &Ev::new("late", "cup-0", T0 + 100)
            .txn(T0 + 5_000)
            .object("p"),
    );
    write(&mut s, &Ev::new("keep", "cup-9", T0 + 200));
    s.compact_range(1, 1, T0 + 9_000).expect("compact");

    let unknown_then = s.as_of("cup-0", T0 + 1_000, T0 + 1_000).unwrap();
    assert_eq!(
        unknown_then.resolution,
        Resolution::Full,
        "as known at T0+1000 the store had not learned it; its removal cannot \
         have changed that answer"
    );

    let known_now = s.as_of("cup-0", T0 + 1_000, T0 + 6_000).unwrap();
    assert!(known_now.is_degraded(), "by T0+6000 it was known");
    clean(&p);
}

/// The upgrade path, and the one case where the rule is *deliberately*
/// pessimistic. A store compacted before summaries were retained has citations
/// and nothing else. Reading that as "no summary, so nothing was lost" would
/// turn a missing feature into the exact silent rewrite §11.3 forbids.
#[test]
fn a_store_compacted_before_summaries_existed_reports_degraded_with_nothing_to_show() {
    let p = tmp("legacy-store");
    let mut s = seeded(&p);
    s.compact_range(1, 6, T0 + 9_000).expect("compact");
    s.drop_summaries_for_test().unwrap();

    // Even a subject the window never held is degraded here — with the
    // summaries gone there is no longer any evidence of what it did hold.
    for subject in ["cup-0", "cup-1"] {
        let h = s.history(subject).unwrap();
        assert!(
            h.is_degraded(),
            "{subject}: a citation with no summary must still degrade the answer"
        );
        assert!(
            h.resolution.summaries().is_empty(),
            "{subject}: there is nothing to show, and it must not invent any"
        );
        assert_eq!(
            h.resolution.spans().len(),
            1,
            "{subject}: the span is named"
        );
    }
    clean(&p);
}

/// A summary and its citation are two halves of one record. A `Degraded`
/// answer carrying only the first cannot be traced back to the range and digest
/// it is degraded by — so the read path must **error**, not quietly return a
/// degraded answer with the span dropped. Compaction is defensible because the
/// admission is checkable; half an admission is the fail-open version.
///
/// `verify_chain` already refuses this store as an uncited gap, but a reader is
/// not obliged to have verified the chain first, and this is the path that
/// reader is on.
#[test]
fn a_summary_whose_citation_is_missing_fails_closed() {
    let p = tmp("orphan-summary");
    let mut s = seeded(&p);
    s.compact_range(1, 6, T0 + 9_000).expect("compact");
    s.history("cup-0").expect("healthy before");

    s.delete_citation_for_test(1).unwrap();

    match s.history("cup-0") {
        Err(StoreError::CitationBroken { lo_generation, .. }) => assert_eq!(lo_generation, 1),
        other => panic!("expected a fail-closed citation error, got {other:?}"),
    }
    match s.as_of("cup-0", T0 + 10_000, T0 + 10_000) {
        Err(StoreError::CitationBroken { .. }) => {}
        other => panic!("as_of must fail closed the same way, got {other:?}"),
    }

    // The store with zero citation ROWS is the most damaged one here, not the
    // healthiest — a row-count short-circuit would have reported it as full.
    assert!(!s.has_citations().unwrap());
    clean(&p);
}

/// `has_citations()` answers "has a compaction *run*", so a refused attempt
/// must not make it true. `compact_range` installs its DDL before running its
/// refusals, so the tables exist either way and the question has to be about
/// recorded rows.
#[test]
fn a_refused_compaction_does_not_report_a_citation() {
    let p = tmp("refused-no-citation");
    let mut s = WorldStore::open(&p).expect("open");
    write(&mut s, &Ev::new("e1", "cup-0", T0 + 100));
    write(&mut s, &Ev::new("e2", "cup-0", T0 + 200).class("incident"));
    write(&mut s, &Ev::new("keep", "cup-9", T0 + 1_000));
    assert!(!s.has_citations().unwrap(), "nothing attempted yet");

    assert!(
        s.compact_range(1, 2, T0 + 9_000).is_err(),
        "protected class"
    );
    assert!(
        !s.has_citations().unwrap(),
        "a refused attempt installs the tables but records nothing; \
         `has_citations` must not read as `a compaction has run`"
    );
    assert!(s.citations().unwrap().is_empty());

    // And it stays true across a later successful compaction of a clean range.
    s.compact_range(1, 1, T0 + 9_100)
        .expect("gen 1 alone is raw");
    assert!(s.has_citations().unwrap(), "now one really has run");
    clean(&p);
}

// ---------------------------------------------------------------------------
// What the summary is allowed to say
// ---------------------------------------------------------------------------

/// "Last" is by `(valid_from_ms, generation)` — the supersession order — not by
/// generation. A backfill arriving later about an earlier instant must not
/// become the window's last value, or the summary and the projection would
/// disagree about which observation was the latest one.
#[test]
fn a_backfill_does_not_become_the_summarys_last_value() {
    let p = tmp("backfill-last");
    let mut s = WorldStore::open(&p).expect("open");
    write(
        &mut s,
        &Ev::new("e1", "cup-0", T0 + 500).object("late-in-world"),
    );
    write(
        &mut s,
        &Ev::new("e2", "cup-0", T0 + 100)
            .txn(T0 + 900)
            .object("backfill"),
    );
    write(&mut s, &Ev::new("keep", "cup-9", T0 + 1_000));

    let out = s.compact_range(1, 2, T0 + 9_000).expect("compact");
    let sm = &out.summaries[0];
    assert_eq!(
        sm.last_object.as_deref(),
        Some("late-in-world"),
        "generation 2 arrived later but is about an earlier instant"
    );
    assert_eq!(sm.last_valid_from_ms, T0 + 500);
    assert_eq!(sm.first_valid_from_ms, T0 + 100);
    assert_eq!(sm.event_count, 2, "both are still counted");
    clean(&p);
}

/// Confirmed-only, for the reason the fold is: this is what a degraded answer
/// returns *in place of* evidence, and an LLM proposal standing there is
/// exactly what SD-2 exists to prevent. The candidate is the newest event in
/// the window, so a summary built without the filter would pick it.
#[test]
fn a_candidate_never_stands_in_for_a_deleted_observation() {
    let p = tmp("candidate-summary");
    let mut s = WorldStore::open(&p).expect("open");
    write(
        &mut s,
        &Ev::new("real", "cup-0", T0 + 100).object("observed"),
    );
    write(
        &mut s,
        &Ev::new("guess", "cup-0", T0 + 500)
            .object("proposed")
            .candidate(),
    );
    write(&mut s, &Ev::new("keep", "cup-9", T0 + 1_000));

    let out = s.compact_range(1, 2, T0 + 9_000).expect("compact");
    assert_eq!(out.removed, 2, "both rows were removed");

    let sm = &out.summaries[0];
    assert_eq!(
        sm.last_object.as_deref(),
        Some("observed"),
        "the candidate is newer, and must still not be what the summary reports"
    );
    assert_eq!(
        sm.event_count, 1,
        "the candidate is not counted as an observation"
    );

    // The arithmetic the confirmed-only rule implies: the per-key counts sum to
    // at MOST the citation's count, and the difference is the candidates.
    let summed: i64 = out.summaries.iter().map(|s| s.event_count).sum();
    assert_eq!(summed, out.citation.event_count - 1);
    clean(&p);
}

/// One row per key, not per event — the cost argument D-21 measured for the
/// projection, applied to retention. If this were per-event, a summary set
/// would be bounded like a log and compaction would buy nothing.
#[test]
fn summaries_are_bounded_by_keys_not_by_events() {
    let p = tmp("key-bounded");
    let mut s = WorldStore::open(&p).expect("open");
    for i in 1..=60i64 {
        let subj = format!("cup-{}", i % 3);
        write(&mut s, &Ev::new(&format!("e{i}"), &subj, T0 + i * 10));
    }
    write(&mut s, &Ev::new("keep", "cup-9", T0 + 10_000));

    let out = s.compact_range(1, 60, T0 + 20_000).expect("compact");
    assert_eq!(out.removed, 60);
    assert_eq!(
        out.summaries.len(),
        3,
        "three subjects × one predicate = three keys, regardless of 60 events"
    );
    assert_eq!(
        out.summaries.iter().map(|s| s.event_count).sum::<i64>(),
        60,
        "and no observation is uncounted"
    );
    clean(&p);
}

/// A predicate-less claim has its own slot, exactly as in the projection —
/// `predicate_key` is `''` there and here, so the two cannot key differently.
#[test]
fn a_predicate_less_claim_gets_its_own_summary() {
    let p = tmp("null-predicate");
    let mut s = WorldStore::open(&p).expect("open");
    write(&mut s, &Ev::new("e1", "cup-0", T0 + 100).predicate(None));
    write(
        &mut s,
        &Ev::new("e2", "cup-0", T0 + 200).predicate(Some("position")),
    );
    write(&mut s, &Ev::new("keep", "cup-9", T0 + 1_000));

    let out = s.compact_range(1, 2, T0 + 9_000).expect("compact");
    assert_eq!(out.summaries.len(), 2);
    assert!(out.summaries.iter().any(|s| s.predicate.is_none()));
    assert!(out
        .summaries
        .iter()
        .any(|s| s.predicate.as_deref() == Some("position")));
    clean(&p);
}

/// Successive compactions of the same key each carry their own summary, and a
/// query gets both. Collapsing them would lose the window boundaries, which are
/// the only thing locating the counts in time.
#[test]
fn successive_compactions_each_retain_their_own_summary() {
    let p = tmp("successive-summaries");
    let mut s = WorldStore::open(&p).expect("open");
    for i in 1..=8i64 {
        write(
            &mut s,
            &Ev::new(&format!("e{i}"), "cup-0", T0 + i * 100).object(&format!("p{i}")),
        );
    }
    write(&mut s, &Ev::new("keep", "cup-9", T0 + 10_000));

    s.compact_range(1, 4, T0 + 20_000).expect("first");
    s.compact_range(5, 8, T0 + 20_100).expect("second");
    s.verify_chain().expect("chain still verifies across both");

    let h = s.history("cup-0").unwrap();
    assert!(h.is_empty() && h.is_degraded());
    let sums = h.resolution.summaries();
    assert_eq!(sums.len(), 2, "one per span");
    assert_eq!(sums[0].lo_generation, 1);
    assert_eq!(sums[0].last_object.as_deref(), Some("p4"));
    assert_eq!(sums[1].lo_generation, 5);
    assert_eq!(sums[1].last_object.as_deref(), Some("p8"));
    assert_eq!(h.resolution.spans().len(), 2);
    clean(&p);
}

/// The payload survives, not just the object — a summary whose payload was
/// dropped could not answer "what did the last reading say".
#[test]
fn the_summary_retains_the_last_payload() {
    let p = tmp("last-payload");
    let mut s = WorldStore::open(&p).expect("open");
    write(
        &mut s,
        &Ev::new("e1", "cup-0", T0 + 100).payload(r#"{"n":1}"#),
    );
    write(
        &mut s,
        &Ev::new("e2", "cup-0", T0 + 200).payload(r#"{"n":42}"#),
    );
    write(&mut s, &Ev::new("keep", "cup-9", T0 + 1_000));

    let out = s.compact_range(1, 2, T0 + 9_000).expect("compact");
    assert_eq!(out.summaries[0].last_payload, r#"{"n":42}"#);
    clean(&p);
}

// ---------------------------------------------------------------------------
// Refusals leave nothing behind
// ---------------------------------------------------------------------------

/// A refused compaction must not retain a summary. A summary for a window that
/// still exists would double-count it at read time, and worse, would suggest
/// evidence was removed when it was not.
#[test]
fn a_refused_compaction_retains_no_summary() {
    let p = tmp("refused-no-summary");
    let mut s = WorldStore::open(&p).expect("open");
    write(&mut s, &Ev::new("e1", "cup-0", T0 + 100));
    write(&mut s, &Ev::new("e2", "cup-0", T0 + 200).class("safety"));
    write(&mut s, &Ev::new("keep", "cup-9", T0 + 1_000));

    match s.compact_range(1, 2, T0 + 9_000) {
        Err(StoreError::ProtectedClassInRange { generation, .. }) => assert_eq!(generation, 2),
        other => panic!("expected a protected-class refusal, got {other:?}"),
    }
    assert!(s.summaries().unwrap().is_empty());
    assert_eq!(s.count().unwrap(), 3, "nothing was removed");
    assert_eq!(
        s.history("cup-0").unwrap().resolution,
        Resolution::Full,
        "a refusal must not make later answers read as degraded"
    );
    clean(&p);
}

/// The second thing the projection-head refusal buys, stated as a test:
/// **current state survives retention.** `current()` reads exactly the events
/// compaction is forbidden to remove, so a device that has compacted its way
/// back under budget still knows where everything is.
#[test]
fn current_state_survives_compaction() {
    let p = tmp("current-survives");
    let mut s = seeded(&p);
    s.fold().expect("fold");
    let before = s.current("cup-0", i64::MAX).unwrap();
    assert_eq!(before.len(), 1);

    // Everything below the head is compactable; the head itself is refused.
    let limit = s
        .largest_compactable_prefix(1, 6)
        .unwrap()
        .expect("a prefix must be available");
    assert_eq!(limit, 5, "generation 6 is cup-0's current claim");
    s.compact_range(1, limit, T0 + 9_000).expect("compact");

    match s.compact_range(6, 6, T0 + 9_100) {
        Err(StoreError::ProjectionHeadInRange {
            generation,
            subject,
        }) => {
            assert_eq!((generation, subject.as_str()), (6, "cup-0"));
        }
        other => panic!("the head must be refused, got {other:?}"),
    }

    assert_eq!(
        s.current("cup-0", i64::MAX).unwrap(),
        before,
        "current state is untouched by compaction, by construction"
    );
    // And the trajectory that led there is gone, and says so.
    let h = s.history("cup-0").unwrap();
    assert_eq!(h.len(), 1, "only the head survives");
    assert!(h.is_degraded());
    assert_eq!(h.resolution.summaries()[0].event_count, 5);
    clean(&p);
}
