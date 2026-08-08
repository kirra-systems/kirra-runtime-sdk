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

use kirra_world_store::{ClaimStatus, EventId, NewEvent, ObservationId, WorldStore, WriterClass};

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
use kirra_world_store::subject_projection::{SummaryCoverage, SummaryKind};

/// The population every case below shares.
///
/// Generations 1..=3 belong to `window-only` and nothing else, so compacting
/// 1..=3 removes ALL of that subject's evidence and none of anyone else's —
/// which is the case that used to make it disappear.
fn seed_mixed(s: &mut WorldStore) {
    for i in 1..=3i64 {
        s.append(&ev(&ids(&format!("w{i}")), "window-only", T0 + i * 100))
            .expect("append");
    }
    for i in 4..=6i64 {
        s.append(&ev(&ids(&format!("p{i}")), "partly", T0 + i * 100))
            .expect("append");
    }
    // A predicate-less confirmed event, which `build_summaries` keys on "".
    {
        let e = ids("bare-1");
        let mut n = ev(&e, "partly", T0 + 650);
        n.predicate = None;
        n.object = None;
        s.append(&n).expect("append predicate-less");
    }
    for i in 8..=9i64 {
        s.append(&ev(&ids(&format!("u{i}")), "untouched", T0 + i * 100))
            .expect("append");
    }
    // `partly-extra` has `partly` as a PREFIX, and is never compacted. Without
    // it, a coverage lookup matching on prefix rather than equality would
    // over-attribute another subject's citations and no test would notice --
    // the negative control for that mutation fired nothing until this existed.
    for i in 10..=11i64 {
        s.append(&ev(&ids(&format!("x{i}")), "partly-extra", T0 + i * 100))
            .expect("append");
    }
}

fn answer<'a>(
    all: &'a [kirra_world_store::subject_projection::SubjectSummaryAnswer],
    subject: &str,
) -> &'a kirra_world_store::subject_projection::SubjectSummaryAnswer {
    all.iter()
        .find(|a| a.subject == subject)
        .unwrap_or_else(|| {
            panic!(
                "no answer for {subject}; got {:?}",
                all.iter().map(|a| &a.subject).collect::<Vec<_>>()
            )
        })
}

/// **The reconciliation identity** — the property that replaces digest equality.
///
/// Digest equality cannot be the assertion here: step 1 does not restore the
/// aggregates, so asserting it would mean either weakening the digest or
/// asserting a falsehood. What must hold is that no evidence was lost track
/// of, only relocated — surviving rows plus citations still add up to what the
/// fold originally counted.
#[test]
fn compaction_relocates_evidence_it_does_not_lose_it() {
    let p = tmp("reconcile");
    let mut s = WorldStore::open(&p).expect("open");
    seed_mixed(&mut s);
    s.fold_subject_summary().expect("fold");

    let before: Vec<(String, i64, i64)> = s
        .subject_summaries()
        .expect("read")
        .into_iter()
        .map(|r| (r.subject, r.observation_count, r.first_observed_ms))
        .collect();

    s.compact_range(1, 5, T0 + 90_000).expect("compact");
    s.rebuild_subject_summary().expect("rebuild");

    let after = s.subject_summaries_with_coverage().expect("read");

    for (subject, count, first) in &before {
        let a = answer(&after, subject);
        assert_eq!(
            a.reconciled_observation_count(),
            *count,
            "{subject}: surviving rows + citations must still account for every observation"
        );
        assert_eq!(
            a.reconciled_first_observed_ms(),
            Some(*first),
            "{subject}: the original first-observed must be recoverable"
        );
    }
    clean(&p);
}

/// A subject with both surviving and compacted evidence reports Degraded.
#[test]
fn a_partly_compacted_subject_is_degraded() {
    let p = tmp("partly");
    let mut s = WorldStore::open(&p).expect("open");
    seed_mixed(&mut s);
    s.fold_subject_summary().expect("fold");
    s.compact_range(1, 5, T0 + 90_000).expect("compact");
    s.rebuild_subject_summary().expect("rebuild");

    let all = s.subject_summaries_with_coverage().expect("read");
    let a = answer(&all, "partly");
    assert!(
        a.coverage.is_degraded(),
        "partly-compacted must not read as complete"
    );
    assert!(a.retained.is_some(), "it still has surviving evidence");
    assert!(a.coverage.compacted_observations() > 0);
    clean(&p);
}

/// **The case that used to vanish.** Every contributing event compacted, so
/// there is no retained row at all — and the subject must still be returned.
#[test]
fn a_subject_with_no_surviving_evidence_is_still_returned() {
    let p = tmp("vanished");
    let mut s = WorldStore::open(&p).expect("open");
    seed_mixed(&mut s);
    s.fold_subject_summary().expect("fold");
    s.compact_range(1, 3, T0 + 90_000)
        .expect("compact exactly window-only's evidence");
    s.rebuild_subject_summary().expect("rebuild");

    // The bare projection has genuinely lost it -- that is the defect.
    assert!(
        !s.subject_summaries()
            .expect("read")
            .iter()
            .any(|r| r.subject == "window-only"),
        "precondition: the raw projection really does drop it, or this proves nothing"
    );

    let all = s.subject_summaries_with_coverage().expect("read");
    let a = answer(&all, "window-only");
    assert!(a.retained.is_none(), "nothing survives to aggregate");
    assert!(a.coverage.is_degraded());
    assert_eq!(
        a.reconciled_observation_count(),
        3,
        "all three observations are still accounted for by citations"
    );
    assert_eq!(a.reconciled_first_observed_ms(), Some(T0 + 100));
    clean(&p);
}

/// A subject no compaction touched reports Complete — so `Degraded` means
/// something.
#[test]
fn an_untouched_subject_is_complete() {
    let p = tmp("untouched");
    let mut s = WorldStore::open(&p).expect("open");
    seed_mixed(&mut s);
    s.fold_subject_summary().expect("fold");
    s.compact_range(1, 3, T0 + 90_000).expect("compact");
    s.rebuild_subject_summary().expect("rebuild");

    let all = s.subject_summaries_with_coverage().expect("read");
    let a = answer(&all, "untouched");
    assert_eq!(a.coverage, SummaryCoverage::Complete);
    assert_eq!(a.coverage.compacted_observations(), 0);
    assert_eq!(a.coverage.earliest_compacted_txn_ms(), None);

    // And the prefix neighbour: `partly` IS compacted, `partly-extra` is not.
    // A lookup matching on prefix instead of equality would report this one
    // degraded on evidence that belongs to a different subject.
    let x = answer(&all, "partly-extra");
    assert_eq!(
        x.coverage,
        SummaryCoverage::Complete,
        "coverage must match subjects exactly, not by prefix"
    );
    assert_eq!(x.reconciled_observation_count(), 2);
    clean(&p);
}

/// A predicate-less event participates. `build_summaries` keys it on `""`, and
/// a coverage scheme that only matched predicated rows would under-count.
#[test]
fn a_predicate_less_event_is_covered() {
    let p = tmp("bare");
    let mut s = WorldStore::open(&p).expect("open");
    seed_mixed(&mut s);
    s.fold_subject_summary().expect("fold");
    // 1..=7 includes the predicate-less event at generation 7.
    s.compact_range(1, 7, T0 + 90_000).expect("compact");
    s.rebuild_subject_summary().expect("rebuild");

    let all = s.subject_summaries_with_coverage().expect("read");
    let a = answer(&all, "partly");
    let bare = match &a.coverage {
        SummaryCoverage::Degraded { summaries } => {
            summaries.iter().filter(|x| x.predicate.is_none()).count()
        }
        SummaryCoverage::Complete => 0,
    };
    assert_eq!(bare, 1, "the predicate-less event must appear in coverage");
    assert_eq!(a.reconciled_observation_count(), 4, "3 predicated + 1 bare");
    clean(&p);
}

/// **Step 2 cannot be forgotten.** `compaction_summaries` records no
/// `subject_kind`, so a subject known only by citation is reported
/// `Unlabelled` — honest, but a guess. This pins the current behaviour so that
/// adding the column is a deliberate change with a failing test, not a silent
/// improvement nobody notices.
#[test]
fn fixme_step2_a_cited_only_subject_has_no_recorded_kind() {
    let p = tmp("kindgap");
    let mut s = WorldStore::open(&p).expect("open");
    seed_mixed(&mut s);
    s.fold_subject_summary().expect("fold");
    s.compact_range(1, 3, T0 + 90_000).expect("compact");
    s.rebuild_subject_summary().expect("rebuild");

    let all = s.subject_summaries_with_coverage().expect("read");
    let a = answer(&all, "window-only");
    assert_eq!(
        a.subject_kind,
        SummaryKind::Unlabelled,
        "FIXME step 2: citations record no discriminant, so this is a guess. \
         When `subject_kind` lands on `compaction_summaries`, this assertion \
         should fail and be replaced by the real kind."
    );
    clean(&p);
}
