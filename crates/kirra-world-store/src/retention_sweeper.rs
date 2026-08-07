//! The retention sweeper — what finally makes the policy *run*.
//!
//! [`crate::retention_driver`] can execute a pass; nothing invoked it on a
//! schedule, so the horizons still went unenforced. This is the last piece of
//! ADR-0040's Tier 1 exit criterion: **something that empties the store without
//! being asked.**
//!
//! # Why this is not in the verifier, where the other monitors live
//!
//! `src/campaign_monitor.rs` and `src/cert_expiry_monitor.rs` are the obvious
//! precedent, and copying them would have been wrong. Those live in the root
//! `kirra-verifier` crate, which is **inside the safety closure**. Spawning the
//! retention sweeper there would make the closure depend on `kirra-world-store`
//! and therefore on `kirra-world` — which is precisely what ADR-0039's **Fence
//! B** forbids, and `ci/check_kirra_world_bidirectional_fence.py` would red.
//!
//! The precedent worth copying is the *shape* — a sweep interval, an explicit
//! enable, fail-closed on anything unestablished — not the location. So the
//! sweeper lives here, on the Kirra World side of the fence, and the verifier
//! never learns it exists.
//!
//! # Why `std::thread` and not tokio
//!
//! Adding an async runtime to reach a timer would put tokio in `kirra-world`'s
//! dependency closure to schedule a SQLite `DELETE`. A thread and a
//! [`std::sync::mpsc`] channel with a timeout do the same job with **no new
//! dependency**, which keeps the seam measurement in
//! [`WM_Q1_SEAM_BASELINE.md`](../../../docs/design/WM_Q1_SEAM_BASELINE.md)
//! measuring domain types rather than transport.
//!
//! # The sweeper opens its own connection, deliberately
//!
//! `WorldStore` holds a `rusqlite::Connection`, which is not `Sync`, so the
//! sweeper cannot borrow the caller's. It opens its own handle to the same
//! path — supported, because the schema runs in WAL mode. That also means the
//! sweeper is **isolated**: a caller's in-flight transaction is unaffected, and
//! a sweeper failure cannot poison the caller's connection.
//!
//! # Fail-closed, and what that means for a sweeper
//!
//! A monitor that cannot establish its own preconditions must do **nothing**,
//! not guess. [`RetentionSweeper::start`] refuses to launch on an unopenable
//! database or an unbuildable policy, rather than starting a thread that will
//! discover the problem every interval forever. Once running, a failed pass is
//! **counted and skipped** — never retried in a tight loop, and never escalated
//! into stopping the sweeper, because retention failing is not a reason to stop
//! trying to bound the disk.

use crate::{StoreError, WorldStore};
use kirra_world::observation::{ClockDomain, DomainInstant};
use kirra_world::retention::RetentionPolicy;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How often a running sweeper takes a pass, unless told otherwise.
///
/// One hour, matching `cert_expiry_monitor`'s census cadence rather than the
/// second-scale monitors. Retention is a *housekeeping* rhythm: D-20/D-21
/// measured 15.79 days to fill 8 GiB, so nothing is gained by sweeping often,
/// and a compaction holds a write lock while it runs.
pub const RETENTION_SWEEP_MS: u64 = 3_600_000;

/// Counters a caller can read without stopping the sweeper.
///
/// Deliberately more than "passes run". The three outcomes are kept apart for
/// the same reason [`kirra_world::retention::RetentionDecision`] has four
/// variants: **`pinned` climbing while `compacted` stays flat is the failure
/// worth alerting on**, and a single success counter cannot show it.
#[derive(Debug, Default)]
pub struct SweepCounters {
    passes: AtomicU64,
    compacted: AtomicU64,
    pinned: AtomicU64,
    failed: AtomicU64,
}

impl SweepCounters {
    /// Passes attempted.
    #[must_use]
    pub fn passes(&self) -> u64 {
        self.passes.load(Ordering::Relaxed)
    }
    /// Passes that removed a range.
    #[must_use]
    pub fn compacted(&self) -> u64 {
        self.compacted.load(Ordering::Relaxed)
    }
    /// Passes that compacted nothing **because something pinned retention**.
    ///
    /// Distinct from a pass that found nothing old enough, which is not counted
    /// here at all — that is the ordinary state of a young store and alerting
    /// on it would train an operator to ignore this number.
    #[must_use]
    pub fn pinned(&self) -> u64 {
        self.pinned.load(Ordering::Relaxed)
    }
    /// Passes that returned an error.
    #[must_use]
    pub fn failed(&self) -> u64 {
        self.failed.load(Ordering::Relaxed)
    }
}

/// A running sweeper. Dropping it stops the thread.
///
/// The handle owns the shutdown channel, so there is no way to leak a sweeper
/// that outlives the code that started it — the thread's own `recv_timeout`
/// returns [`RecvTimeoutError::Disconnected`] the moment this is dropped.
#[derive(Debug)]
pub struct RetentionSweeper {
    _shutdown: Sender<()>,
    counters: Arc<SweepCounters>,
}

impl RetentionSweeper {
    /// Counters for this sweeper, safe to read at any time.
    #[must_use]
    pub fn counters(&self) -> &Arc<SweepCounters> {
        &self.counters
    }

    /// Start sweeping `path` on `interval`.
    ///
    /// # Fail-closed at startup
    ///
    /// The database is opened **here**, on the caller's thread, so an
    /// unopenable path is an error the caller sees rather than a thread that
    /// fails silently every hour. The opened handle is then moved into the
    /// thread.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] if `path` cannot be opened or migrated.
    pub fn start(
        path: &Path,
        policy: RetentionPolicy,
        interval: Duration,
    ) -> Result<Self, StoreError> {
        // Prove the store opens before promising to sweep it.
        let store = WorldStore::open(path)?;
        drop(store);
        let owned: PathBuf = path.to_path_buf();

        let (tx, rx) = mpsc::channel::<()>();
        let counters = Arc::new(SweepCounters::default());
        let thread_counters = Arc::clone(&counters);

        std::thread::Builder::new()
            .name("kirra-world-retention".into())
            .spawn(move || {
                // Re-opened inside the thread because Connection is not Sync.
                let Ok(mut store) = WorldStore::open(&owned) else {
                    // Openable a moment ago, not now. Nothing useful to do from
                    // a detached thread except decline to run.
                    return;
                };
                loop {
                    match rx.recv_timeout(interval) {
                        // Handle dropped, or an explicit stop.
                        Err(RecvTimeoutError::Disconnected) | Ok(()) => return,
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                    let Some(now) = wall_clock_now() else {
                        // The clock is before the epoch. Skip rather than guess:
                        // retention is the one irreversible operation here.
                        thread_counters.failed.fetch_add(1, Ordering::Relaxed);
                        continue;
                    };
                    thread_counters.passes.fetch_add(1, Ordering::Relaxed);
                    match store.run_retention_pass(&policy, now) {
                        Ok(report) if report.compacted.is_some() => {
                            thread_counters.compacted.fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(report) if report.is_pinned() => {
                            thread_counters.pinned.fetch_add(1, Ordering::Relaxed);
                        }
                        // Nothing old enough — the ordinary state, not counted.
                        Ok(_) => {}
                        Err(_) => {
                            // Counted and skipped. A failing pass is not a
                            // reason to stop trying to bound the disk, and a
                            // tight retry would hold the write lock.
                            thread_counters.failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            })
            .map_err(|e| {
                StoreError::Sqlite(rusqlite::Error::InvalidPath(
                    format!("could not spawn the retention sweeper: {e}").into(),
                ))
            })?;

        Ok(Self {
            _shutdown: tx,
            counters,
        })
    }
}

/// The wall clock, as the domain type retention insists on.
///
/// Returns `None` before the Unix epoch rather than clamping: a clock that far
/// wrong should stop retention, not feed it a plausible substitute.
fn wall_clock_now() -> Option<DomainInstant> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    Some(DomainInstant {
        ms: u64::try_from(ms).ok()?,
        domain: ClockDomain::System,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClaimStatus, EventId, NewEvent, ObservationId, WriterClass};

    fn tmp(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "kirra-world-sweeper-{}-{}.sqlite",
            name,
            std::process::id()
        ));
        for s in ["", "-wal", "-shm"] {
            let mut q = p.as_os_str().to_os_string();
            q.push(s);
            let _ = std::fs::remove_file(PathBuf::from(q));
        }
        p
    }

    fn append_old(s: &mut WorldStore, id: &str) {
        // txn_time well before any plausible cutoff, so the event is eligible
        // the first time the sweeper looks.
        // One string, two types. The fixture conflates them, as fixtures may —
        // but it now has to say so, rather than the conflation being invisible
        // in a pair of adjacent `&str`s.
        let event_id = EventId::new(id).expect("admissible event id");
        let observation_id = ObservationId::new(id).expect("admissible observation id");
        s.append(&NewEvent {
            event_id: &event_id,
            observation_id: &observation_id,
            txn_time_ms: 1,
            valid_from_ms: 1,
            valid_to_ms: None,
            source: "sensor-a",
            source_version: "0.1.0",
            writer_class: WriterClass::Sensor,
            claim_status: ClaimStatus::Confirmed,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "observation",
            subject: "subj",
            predicate: Some("at"),
            object: Some("bench"),
            payload: r#"{"n":1}"#,
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append");
    }

    #[test]
    fn a_running_sweeper_actually_empties_the_store() {
        // THE Tier 1 exit criterion, as a test: something removes events
        // without being asked. Everything before this could compact; nothing
        // did so on its own.
        let path = tmp("empties");
        {
            let mut s = WorldStore::open(&path).unwrap();
            for i in 1..=3 {
                append_old(&mut s, &format!("e-{i}"));
            }
            assert_eq!(count_events(&s), 3);
        }

        let sweeper =
            RetentionSweeper::start(&path, RetentionPolicy::oq2(), Duration::from_millis(20))
                .expect("start");

        // Wait for the first pass to land rather than sleeping a fixed guess.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while sweeper.counters().compacted() == 0 && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(
            sweeper.counters().compacted() >= 1,
            "the sweeper compacted nothing within the deadline"
        );

        let s = WorldStore::open(&path).unwrap();
        assert_eq!(count_events(&s), 0, "the aged events are gone");
        // And the ledger is still verifiable ACROSS the hole — the sweeper used
        // compaction-with-citation, not a DELETE.
        s.verify_chain().expect("chain intact across the citation");
    }

    #[test]
    fn dropping_the_handle_stops_the_thread() {
        // No way to leak a sweeper that outlives its owner: the thread's
        // recv_timeout sees Disconnected as soon as the Sender is dropped.
        let path = tmp("stops");
        {
            let mut s = WorldStore::open(&path).unwrap();
            append_old(&mut s, "e-1");
        }
        let sweeper =
            RetentionSweeper::start(&path, RetentionPolicy::oq2(), Duration::from_millis(10))
                .expect("start");
        let counters = Arc::clone(sweeper.counters());

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while counters.passes() == 0 && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(counters.passes() >= 1);

        drop(sweeper);
        std::thread::sleep(Duration::from_millis(60));
        let after = counters.passes();
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(
            counters.passes(),
            after,
            "the thread kept sweeping after its handle was dropped"
        );
    }

    #[test]
    fn an_unopenable_path_is_refused_at_start_not_discovered_hourly() {
        // Fail-closed: a monitor that cannot establish its preconditions must
        // do nothing, rather than spawn a thread that rediscovers the problem
        // every interval forever.
        let dir = std::env::temp_dir();
        let err = RetentionSweeper::start(&dir, RetentionPolicy::oq2(), Duration::from_secs(1));
        assert!(err.is_err(), "a directory is not a database");
    }

    fn count_events(s: &WorldStore) -> i64 {
        // Queried directly: the store has no public count, and adding one just
        // for a test would widen the API for the test's convenience.
        s.conn
            .query_row("SELECT COUNT(*) FROM world_events", [], |r| r.get(0))
            .expect("count")
    }
}
