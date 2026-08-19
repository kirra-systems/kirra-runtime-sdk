//! **The control for Tier 5 box 5b: retention runs WITHOUT BEING ASKED.**
//!
//! `WM_SCOPE.md` §4 claimed this was true on 2026-08-06. It was not — the
//! sweeper existed and nothing started it. So the property under test is
//! specifically the SCHEDULE, not compaction:
//!
//! > Given a store holding events past the horizon, and nothing calling
//! > `run_retention_pass`, the store empties anyway.
//!
//! A test that called `run_retention_pass` itself would pass against exactly
//! the tree that shipped the false claim. This one starts the process's own
//! entry point and then does nothing at all.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use kirra_world_retention_service::{resolve_interval, start_retention};
use kirra_world_store::{ClaimStatus, EventId, NewEvent, ObservationId, WorldStore, WriterClass};

/// Comfortably past OQ2's 30-day raw horizon.
const DAYS_60_MS: i64 = 60 * 24 * 60 * 60 * 1000;

fn tmp(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-t5-retention-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    for s in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(s);
        let _ = std::fs::remove_file(PathBuf::from(q));
    }
    p
}

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after epoch")
            .as_millis(),
    )
    .expect("in range")
}

/// Append one event stamped 60 days ago — past the raw horizon, so a pass that
/// actually runs must remove it.
fn append_old(s: &mut WorldStore, tag: &str) {
    let event_id = EventId::new(format!("ev-{tag}")).expect("event id");
    let observation_id = ObservationId::new(format!("obs-{tag}")).expect("obs id");
    let old = now_ms() - DAYS_60_MS;
    s.append(&NewEvent {
        event_id: &event_id,
        observation_id: &observation_id,
        txn_time_ms: old,
        valid_from_ms: old,
        valid_to_ms: None,
        source: "warehouse-scanner",
        source_version: "1.0.0",
        writer_class: WriterClass::Sensor,
        claim_status: ClaimStatus::Confirmed,
        provenance: &[],
        frame_id: None,
        map_id: None,
        kind: "mission",
        subject: "package_17",
        subject_ref: None,
        predicate: Some("last_seen_at"),
        object: Some("dock_a"),
        payload: "{}",
        payload_schema: 1,
        retention_class: "raw",
        trust: None,
    })
    .expect("append");
}

// There is deliberately NO row count here. `WorldStore` has no public count,
// and its own sweeper test says why: adding one would widen the store's API for
// a test's convenience. The observable that matters is public anyway — the pass
// reports the RANGE it removed, which is strictly more than a count would say.

/// **The load-bearing test.** Nothing in this function asks for a pass.
///
/// It starts the process's entry point, sleeps, and asserts the store emptied
/// itself. Against the tree as it stood before this crate existed — sweeper
/// written, nobody calling it — this fails, which is the whole point: it
/// observes the SCHEDULE, and the schedule was the missing half.
#[test]
fn the_store_empties_without_anyone_asking() {
    let path = tmp("unasked");
    {
        let mut s = WorldStore::open(&path).expect("open");
        for i in 0..4 {
            append_old(&mut s, &format!("old{i}"));
        }
        s.fold().expect("fold");
    }

    // A 50 ms interval so the test is seconds, not an hour. The INTERVAL is
    // configuration; the scheduling is what is under test.
    let sweeper = start_retention(&path, Duration::from_millis(50)).expect("start");

    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && sweeper.counters().compacted() == 0 {
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(
        sweeper.counters().compacted() > 0,
        "retention never compacted on its own — passes={} pinned={} failed={} last={:?}",
        sweeper.counters().passes(),
        sweeper.counters().pinned(),
        sweeper.counters().failed(),
        sweeper.last_report(),
    );
    let report = sweeper.last_report().expect("a compacting pass reports");
    assert!(
        report.compacted.is_some(),
        "the pass that incremented `compacted` must name the range it removed: {report:?}"
    );
}

/// The report survives to the boundary, carrying the DECISION rather than a
/// count — the reason `last_report` exists at all.
///
/// Without this, `RetentionPassReport` would be named nowhere outside its own
/// module and `retention_driver` would still read as an orphan: the gate cannot
/// see consumption through an inherent method on `WorldStore`.
#[test]
fn the_last_report_carries_why_not_just_how_many() {
    let path = tmp("report");
    {
        let mut s = WorldStore::open(&path).expect("open");
        // FOUR, not one. A single event is also the projection head, so the
        // prefix is fully blocked and no ACTING decision ever exists to observe
        // — the fixture would starve the assertion rather than test it. Found by
        // the tightened wait failing on a clean tree, which is the failure mode
        // a fixture bug should have.
        for i in 0..4 {
            append_old(&mut s, &format!("rep{i}"));
        }
        s.fold().expect("fold");
    }
    let sweeper = start_retention(&path, Duration::from_millis(50)).expect("start");

    // Wait for a report carrying an ACTING decision, not merely any report.
    //
    // The sweeper overwrites `last` every pass, and once the fixture is
    // compacted every later pass reports `NothingOldEnough` — whose range is
    // `None`, making the agreement assertion below `None == None` and therefore
    // hollow. That is how the first version of this test survived a mutation
    // that decoupled the two fields: it was checking the wrong pass.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut acting = None;
    while Instant::now() < deadline && acting.is_none() {
        acting = sweeper
            .last_report()
            .filter(|r| r.decision.range().is_some());
        if acting.is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let report = acting.expect("a compacting pass should have reported by now");

    // THE DECISION AND THE ACTION MUST AGREE.
    //
    // The first version of this assertion was `format!("{:?}", decision).len() >
    // 1`, which cannot fail — `RetentionDecision` derives `Debug`. A vacuous
    // assertion inside the test whose whole purpose is non-vacuity, caught in
    // review. Recorded rather than quietly swapped, because it is the same
    // defect this suite exists to prevent, written by the person warning about
    // it.
    //
    // What is checked instead is falsifiable and is the property that matters:
    // `run_retention_pass` derives `compacted` FROM `decision.range()`, so a
    // report claiming a range it did not remove — or removing one it did not
    // decide — is a report that lies about what happened. Flattening the
    // decision to a flag fails this; so does decoupling the two fields.
    assert_eq!(
        report
            .decision
            .range()
            .map(|(lo, hi)| (lo as i64, hi as i64)),
        report.compacted,
        "the decision and the range actually removed disagree: {report:?}"
    );
}

/// Dropping the handle stops the schedule — so a `main` that let the sweeper go
/// out of scope would start retention and immediately cancel it. Guards the one
/// mistake the binary's shape makes easy.
#[test]
fn dropping_the_handle_stops_the_schedule() {
    let path = tmp("dropped");
    {
        let mut s = WorldStore::open(&path).expect("open");
        append_old(&mut s, "kept");
        s.fold().expect("fold");
    }
    let sweeper = start_retention(&path, Duration::from_millis(50)).expect("start");
    drop(sweeper);
    std::thread::sleep(Duration::from_millis(400));
    // Nothing asserted about WHETHER it compacted before the drop — that race
    // is real and irrelevant. What matters is that the stop is orderly: the
    // store is still openable afterwards, so the thread did not die holding a
    // write lock or leave the database wedged.
    WorldStore::open(&path).expect("the store is still openable after the stop");
}

/// The interval resolver is the binary's one piece of configuration policy, and
/// a refusal here is a startup abort rather than a silent default.
#[test]
fn a_bad_interval_refuses_before_anything_is_compacted() {
    assert!(resolve_interval(Some("0")).is_err());
    assert!(resolve_interval(Some("later")).is_err());
    assert!(resolve_interval(None).is_ok());
}
