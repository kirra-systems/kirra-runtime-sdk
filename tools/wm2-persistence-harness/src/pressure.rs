//! Behaviour under disk pressure — ADR-0041's two stated properties, tested.
//!
//! The SQLite-configuration table in ADR-0041 asserts two things that nothing
//! had ever exercised:
//!
//! | Setting | Proposal |
//! |---|---|
//! | Read-only degraded mode | Serve projections read-only if the log is unwritable — **never** silently drop writes |
//! | Disk-full | Refuse new observations with `Unavailable`; never overwrite |
//!
//! Both are claims about the worst moment in a store's life, and both are the
//! kind of claim that is true right up until someone tests it. A robot that
//! fills its disk mid-mission and *silently* stops recording observations —
//! while continuing to answer queries as though its knowledge were current — is
//! a far worse failure than one that refuses loudly.
//!
//! # Simulating a full disk portably
//!
//! `PRAGMA max_page_count` caps the database at a fixed number of pages. Writing
//! past it returns `SQLITE_FULL` — the *same* error, through the same code path,
//! that a genuinely full filesystem produces. That makes the experiment
//! deterministic, privilege-free, and runnable in CI, where actually filling a
//! runner's disk is neither.
//!
//! The honest limit, stated so nobody reads more into a pass than it carries:
//! this exercises SQLite's full-database behaviour, not the *filesystem's*
//! ENOSPC behaviour, and not what the surrounding OS does when a Jetson's eMMC
//! is genuinely at 100 % (where the journal, the WAL, and every other process
//! sharing that mount are also failing). It establishes that the store refuses
//! cleanly rather than corrupting; it does not establish that the device stays
//! healthy. The drill's §6 disk-pressure step covers the second question and
//! needs the real device.
//!
//! # The property that matters most
//!
//! `append_batch` writes N events in ONE transaction. If a full database
//! half-committed a batch, the generation sequence would be torn and the hash
//! chain would fork — turning a recoverable out-of-space condition into
//! permanent evidence corruption. `partial_batch_rolled_back` is the check that
//! this cannot happen.

use crate::gen;
use crate::standin::{ChainStatus, Durability, Store};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct PressureResult {
    pub events_before_full: u64,
    pub page_cap: i64,
    /// The append past the cap returned an error rather than succeeding.
    pub write_refused: bool,
    /// The refusal names a full database, not some unrelated fault.
    pub refusal_is_disk_full: bool,
    pub refusal_message: String,
    /// The whole failing batch rolled back — no torn generation sequence.
    pub partial_batch_rolled_back: bool,
    /// The chain still verifies after the refusal.
    pub chain_intact_after_refusal: bool,
    /// ADR-0041's read-only degraded mode: queries still answer while full.
    pub reads_serve_while_full: bool,
    pub projection_rows_while_full: u64,
    /// Raising the cap makes the store writable again — full is recoverable,
    /// not terminal.
    pub recovers_when_space_returns: bool,
    pub chain_intact_after_recovery: bool,
}

/// Fill the store to a page cap, then probe what it does.
pub fn run(path: &Path, durability: Durability, seed: u64) -> Result<PressureResult, String> {
    let _ = std::fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let mut p = path.as_os_str().to_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(p));
    }

    let mut store = Store::open(path, durability).map_err(|e| e.to_string())?;
    // Seed a modest log, then cap the database just above its current size so
    // the next sizeable batch cannot fit.
    store
        .append_batch(&gen::events(0, 2_000, 100, seed))
        .map_err(|e| e.to_string())?;
    // Materialize projections BEFORE the store fills. Without this the
    // read-only-degraded-mode check is degenerate: it would find zero
    // projection rows and report "reads work" about a store that had nothing
    // to serve. ADR-0041's claim is that ALREADY-DERIVED views stay answerable
    // when the log becomes unwritable, so they have to exist first.
    store.fold_from(0).map_err(|e| e.to_string())?;
    store.durable_checkpoint().map_err(|e| e.to_string())?;

    let page_cap: i64 = store
        .conn
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    store
        .conn
        .execute_batch(&format!("PRAGMA max_page_count={page_cap};"))
        .map_err(|e| e.to_string())?;

    let events_before_full = store.count_events().map_err(|e| e.to_string())?;

    // Push until it refuses. A few rounds, because SQLite may satisfy a small
    // write from a free page before the cap actually bites.
    let mut refusal: Option<String> = None;
    for round in 0..12 {
        let start = events_before_full + round * 4_000;
        match store.append_batch(&gen::events(start, 4_000, 100, seed + round)) {
            Ok(()) => continue,
            Err(e) => {
                refusal = Some(e.to_string());
                break;
            }
        }
    }

    let refusal_message = refusal.clone().unwrap_or_default();
    let write_refused = refusal.is_some();
    // rusqlite surfaces SQLITE_FULL as "database or disk is full".
    let lower = refusal_message.to_ascii_lowercase();
    let refusal_is_disk_full = lower.contains("full");

    // The batch that failed must have left NOTHING behind. Any events written
    // by earlier successful rounds are fine; what must not happen is a batch
    // committing partway.
    let after_refusal = store.count_events().map_err(|e| e.to_string())?;
    let partial_batch_rolled_back = after_refusal >= events_before_full
        && (after_refusal - events_before_full).is_multiple_of(4_000);

    let chain_intact_after_refusal = matches!(
        store.verify_chain().map_err(|e| e.to_string())?,
        ChainStatus::Intact { .. }
    );

    // Read-only degraded mode: the log is unwritable, but knowledge already
    // recorded must still be answerable. A store that goes dark on reads when
    // it runs out of space takes the operator's diagnostic tools away at
    // exactly the moment they are needed.
    //
    // Note what is NOT attempted here: re-folding. A fold WRITES, so it is
    // subject to the same refusal as an append — which surfaces a real question
    // the drill has to settle on target, namely what a store that fills
    // MID-fold leaves behind. Projections would be partial with nothing marking
    // them as such. That is a design gap, recorded rather than papered over.
    let projection_rows_while_full: u64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM entities_projection", [], |r| {
            r.get::<_, i64>(0)
        })
        .map(|n| n as u64)
        .unwrap_or(0);
    // Both halves: the event log AND the derived projections must still answer.
    // Requiring projection rows > 0 is what stops this passing vacuously.
    let log_readable = store
        .conn
        .query_row("SELECT COUNT(*) FROM world_events", [], |r| {
            r.get::<_, i64>(0)
        })
        .map(|n| n > 0)
        .unwrap_or(false);
    let reads_serve_while_full = log_readable && projection_rows_while_full > 0;

    // Space returns. The store must become writable again without intervention
    // beyond the space itself — being full is a condition, not a state the
    // store gets stuck in.
    store
        .conn
        .execute_batch(&format!("PRAGMA max_page_count={};", page_cap * 4))
        .map_err(|e| e.to_string())?;
    let resume_from = store.count_events().map_err(|e| e.to_string())?;
    let recovers_when_space_returns = store
        .append_batch(&gen::events(resume_from, 100, 100, seed))
        .is_ok();
    let chain_intact_after_recovery = matches!(
        store.verify_chain().map_err(|e| e.to_string())?,
        ChainStatus::Intact { .. }
    );

    Ok(PressureResult {
        events_before_full,
        page_cap,
        write_refused,
        refusal_is_disk_full,
        refusal_message,
        partial_batch_rolled_back,
        chain_intact_after_refusal,
        reads_serve_while_full,
        projection_rows_while_full,
        recovers_when_space_returns,
        chain_intact_after_recovery,
    })
}

/// Time a `VACUUM` on a populated store — ADR-0041's reclamation cost.
///
/// Separated from the compaction measurement because it is a separate
/// *operation*: the ADR now models `logical compaction → separately scheduled
/// reclamation`, and a reclamation that has to be scheduled against power,
/// thermal and mission state needs its own number to be scheduled against.
///
/// `bytes_freed` can legitimately be zero or negative on a store with nothing
/// to reclaim; that is reported rather than clamped, because a `VACUUM` that
/// costs seconds and frees nothing is exactly the case a scheduler must avoid.
pub struct ReclaimResult {
    pub vacuum_ms: f64,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub bytes_freed: i64,
    pub bytes_per_ms: f64,

    // --- transient cost: what reclamation *needs*, not what it returns ------
    /// Peak combined footprint (database directory + temp directory) observed
    /// while the `VACUUM` was running.
    pub peak_bytes_during: u64,
    /// `peak_bytes_during - bytes_before`: the free space a `VACUUM` consumes
    /// **on top of** the database it is shrinking. This, not `bytes_freed`, is
    /// the number a minimum-free-space reserve has to be set from.
    pub transient_overhead_bytes: i64,
    /// `peak_bytes_during / bytes_before`.
    ///
    /// Measured at 1.36 on the host baseline, *not* the 2.0 a "VACUUM needs
    /// twice the space" rule of thumb predicts — because the copy it builds is
    /// the size of the **result**, not of the source. The overhead therefore
    /// scales with what will remain, and the worst case is a store with
    /// nothing to reclaim (copy ≈ source ⇒ ratio ≈ 2.0), not a bloated one.
    ///
    /// The conservative reserve is consequently ~1× the current database size
    /// rather than the measured 0.36×: a maintenance window that only ever ran
    /// when there was a lot to reclaim would be sized from the easy case.
    pub transient_overhead_ratio: f64,
    pub footprint_samples: u64,

    // --- where the second copy lands ---------------------------------------
    pub temp_dir: String,
    pub temp_dir_fs_type: Option<String>,
    /// True when the temp copy shares a filesystem with the database, so the
    /// reserve must cover both.
    pub temp_on_same_fs_as_db: bool,
    /// True when the temp directory is `tmpfs`/`ramfs` — the copy is built in
    /// **RAM**, and the failure mode is the OOM killer rather than `ENOSPC`.
    pub temp_fs_is_volatile: bool,

    // --- availability during reclamation ------------------------------------
    /// Appends attempted from a second connection while the `VACUUM` ran.
    pub concurrent_append_attempts: u64,
    /// How many of those were refused or blocked. `VACUUM` takes an exclusive
    /// lock, so a robot cannot record events for its duration.
    pub concurrent_appends_blocked: u64,
    pub max_append_stall_ms: f64,
}

/// Time a `VACUUM` on a populated store, and measure what it costs the *system*
/// — not just how long it takes.
///
/// Three things beyond duration matter to ADR-0041's reclamation preconditions,
/// and none of them are visible from `bytes_freed`:
///
/// 1. **Transient space.** `VACUUM` writes a complete copy and only then swaps
///    it in, so it *consumes* free space in order to release it. A device that
///    waits until it is nearly full to reclaim is a device that cannot
///    reclaim. The reserve threshold follows from `transient_overhead_bytes`,
///    not from the reclaimed figure — and see that field's note for why the
///    conservative reserve is ~1× the database size even though the measured
///    overhead is smaller.
/// 2. **Where the copy goes.** SQLite builds it in the temp directory, which
///    may be a different filesystem — and on a Jetson is frequently `tmpfs`.
///    Reclaiming an 8 GiB store through RAM is a different and worse failure
///    than running out of disk, so the medium is reported, not assumed.
/// 3. **Availability.** `VACUUM` holds an exclusive lock for its whole
///    duration. The store cannot accept events during it, which is precisely
///    why the ADR now requires a robot state rather than an opportunistic
///    schedule. The stall is measured from a second connection rather than
///    argued from the documentation.
pub fn reclaim(store: &Store, path: &Path) -> Result<ReclaimResult, String> {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;

    store.durable_checkpoint().map_err(|e| e.to_string())?;
    let bytes_before = crate::bench::db_bytes(path);

    let db_dir = path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let temp_dir = resolve_sqlite_temp_dir();
    let temp_fs = crate::platform::fs_type_of(&temp_dir);
    let db_fs = crate::platform::fs_type_of(&db_dir);

    let running = Arc::new(AtomicBool::new(true));
    let peak = Arc::new(AtomicU64::new(bytes_before));
    let samples = Arc::new(AtomicU64::new(0));

    // Footprint sampler. Directory-summing rather than free-space polling: it
    // is attributable to this operation, where free space on a shared
    // filesystem moves for reasons that have nothing to do with the VACUUM.
    let sampler = {
        let running = Arc::clone(&running);
        let peak = Arc::clone(&peak);
        let samples = Arc::clone(&samples);
        let db_dir = db_dir.clone();
        let temp_dir = temp_dir.clone();
        std::thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                let total = dir_bytes(&db_dir) + sqlite_temp_bytes(&temp_dir);
                peak.fetch_max(total, Ordering::Relaxed);
                samples.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(1));
            }
        })
    };

    // Availability probe: a second connection trying to do what a robot would
    // be doing. Opened before the VACUUM so connection setup is not counted as
    // a stall.
    let attempts = Arc::new(AtomicU64::new(0));
    let blocked = Arc::new(AtomicU64::new(0));
    let max_stall_us = Arc::new(AtomicU64::new(0));
    let writer = {
        let running = Arc::clone(&running);
        let attempts = Arc::clone(&attempts);
        let blocked = Arc::clone(&blocked);
        let max_stall_us = Arc::clone(&max_stall_us);
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            // A short busy timeout: the question is whether writes stall, so
            // waiting indefinitely would measure nothing and hang the probe.
            let Ok(conn) = Connection::open(&path) else {
                return;
            };
            let _ = conn.busy_timeout(Duration::from_millis(50));
            while running.load(Ordering::Relaxed) {
                let t = Instant::now();
                let r = conn.execute_batch(
                    "BEGIN IMMEDIATE; \
                     INSERT INTO projection_checkpoints(name, generation) \
                     VALUES ('reclaim_probe', 1) \
                     ON CONFLICT(name) DO UPDATE SET generation = generation + 1; \
                     COMMIT;",
                );
                let us = t.elapsed().as_micros() as u64;
                attempts.fetch_add(1, Ordering::Relaxed);
                if r.is_err() {
                    blocked.fetch_add(1, Ordering::Relaxed);
                    // A failed BEGIN IMMEDIATE leaves no transaction, but a
                    // failure after it would; roll back defensively so the
                    // probe cannot itself hold a lock.
                    let _ = conn.execute_batch("ROLLBACK;");
                }
                max_stall_us.fetch_max(us, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };

    let t = Instant::now();
    let vacuum = store.conn.execute_batch("VACUUM;");
    let vacuum_ms = t.elapsed().as_secs_f64() * 1e3;

    running.store(false, Ordering::Relaxed);
    let _ = sampler.join();
    let _ = writer.join();

    vacuum.map_err(|e| e.to_string())?;

    store.durable_checkpoint().map_err(|e| e.to_string())?;
    let bytes_after = crate::bench::db_bytes(path);
    let bytes_freed = bytes_before as i64 - bytes_after as i64;

    let peak_bytes_during = peak.load(Ordering::Relaxed).max(bytes_before);

    Ok(ReclaimResult {
        vacuum_ms,
        bytes_before,
        bytes_after,
        bytes_freed,
        bytes_per_ms: if vacuum_ms > 0.0 {
            bytes_freed as f64 / vacuum_ms
        } else {
            f64::NAN
        },
        peak_bytes_during,
        transient_overhead_bytes: peak_bytes_during as i64 - bytes_before as i64,
        transient_overhead_ratio: if bytes_before > 0 {
            peak_bytes_during as f64 / bytes_before as f64
        } else {
            f64::NAN
        },
        footprint_samples: samples.load(Ordering::Relaxed),
        temp_dir: temp_dir.to_string_lossy().into_owned(),
        temp_on_same_fs_as_db: match (&temp_fs, &db_fs) {
            (Some(a), Some(b)) => a == b && same_device(&temp_dir, &db_dir),
            _ => false,
        },
        temp_fs_is_volatile: temp_fs
            .as_deref()
            .is_some_and(crate::platform::is_non_durable_fs),
        temp_dir_fs_type: temp_fs,
        concurrent_append_attempts: attempts.load(Ordering::Relaxed),
        concurrent_appends_blocked: blocked.load(Ordering::Relaxed),
        max_append_stall_ms: max_stall_us.load(Ordering::Relaxed) as f64 / 1e3,
    })
}

/// Where SQLite will build the `VACUUM` copy, following its unix temp-file
/// search order.
///
/// Replicated rather than queried because there is no pragma that reports the
/// resolved answer — `temp_store_directory` reports only an override, and is
/// deprecated. Being explicit about the order is the point: on a Jetson the
/// difference between `/var/tmp` and a `tmpfs` `/tmp` is the difference between
/// a slow reclamation and an out-of-memory kill.
fn resolve_sqlite_temp_dir() -> PathBuf {
    for var in ["SQLITE_TMPDIR", "TMPDIR"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() && Path::new(&v).is_dir() {
                return PathBuf::from(v);
            }
        }
    }
    for fallback in ["/var/tmp", "/usr/tmp", "/tmp"] {
        if Path::new(fallback).is_dir() {
            return PathBuf::from(fallback);
        }
    }
    PathBuf::from(".")
}

/// Total bytes of regular files directly in `dir`.
///
/// Non-recursive: the database directory's own contents are what a reserve has
/// to cover, and descending into unrelated subdirectories would attribute
/// someone else's data to this operation.
fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// Bytes of SQLite's own temp files in `dir`.
///
/// SQLite names them `etilqs_*` ("sqlite" reversed). Filtering by that prefix
/// keeps an unrelated large file in `/tmp` from being charged to the VACUUM.
fn sqlite_temp_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(SQLITE_TEMP_PREFIX)
        })
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

const SQLITE_TEMP_PREFIX: &str = "etilqs_";

/// Whether two paths resolve to the same mount source.
///
/// Filesystem *type* matching is not sufficient — two separate ext4 volumes
/// would compare equal and understate the reserve, which is the unsafe
/// direction.
fn same_device(a: &Path, b: &Path) -> bool {
    match (
        crate::platform::fs_source_of(a),
        crate::platform::fs_source_of(b),
    ) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "wm2-pressure-{}-{}.sqlite",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn a_full_database_refuses_cleanly_and_recovers() {
        let p = temp("full");
        let r = run(&p, Durability::Off, 3).unwrap();

        assert!(
            r.write_refused,
            "a capped database accepted an oversized write"
        );
        assert!(
            r.refusal_is_disk_full,
            "refusal was not a full-database error: {}",
            r.refusal_message
        );
        // The one that turns a recoverable condition into corruption if wrong.
        assert!(
            r.partial_batch_rolled_back,
            "a batch committed partway under pressure — the generation sequence is torn"
        );
        assert!(
            r.chain_intact_after_refusal,
            "the refusal corrupted the chain"
        );

        // ADR-0041's read-only degraded mode.
        assert!(
            r.reads_serve_while_full,
            "the store stopped answering reads when it filled — the operator loses \
             diagnostics exactly when they are needed"
        );
        assert!(
            r.projection_rows_while_full > 0,
            "no projection rows to serve, so the read-only-degraded-mode check was vacuous"
        );

        assert!(
            r.recovers_when_space_returns,
            "the store stayed unwritable after space returned"
        );
        assert!(r.chain_intact_after_recovery);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn nothing_is_written_past_the_cap() {
        // Non-vacuity: if the cap did not actually bind, every assertion in the
        // test above would be about a store that never filled.
        let p = temp("cap-binds");
        let r = run(&p, Durability::Off, 5).unwrap();
        assert!(r.page_cap > 0);
        assert!(
            r.write_refused,
            "the page cap never bound, so the experiment proved nothing"
        );
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn reclaim_reports_time_and_bytes() {
        let p = temp("reclaim");
        let mut s = Store::open(&p, Durability::Off).unwrap();
        s.append_batch(&gen::events(0, 4_000, 200, 7)).unwrap();
        s.conn
            .execute("DELETE FROM world_events WHERE generation < 3000", [])
            .unwrap();

        let r = reclaim(&s, &p).unwrap();
        assert!(r.vacuum_ms > 0.0, "VACUUM took no measurable time");
        assert!(
            r.bytes_freed > 0,
            "deleting 3000 of 4000 events then vacuuming freed nothing: {} -> {}",
            r.bytes_before,
            r.bytes_after
        );
        assert!(r.bytes_per_ms.is_finite());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn reclaim_on_a_dense_store_frees_little_but_still_costs_time() {
        // The case a scheduler must avoid: pay the rewrite, get nothing back.
        // Reported, not clamped.
        let p = temp("reclaim-dense");
        let mut s = Store::open(&p, Durability::Off).unwrap();
        s.append_batch(&gen::events(0, 2_000, 100, 8)).unwrap();
        let r = reclaim(&s, &p).unwrap();
        assert!(r.vacuum_ms > 0.0);
        assert!(
            r.bytes_freed.abs() < r.bytes_before as i64,
            "implausible reclaim on a dense store"
        );
        let _ = std::fs::remove_file(p);
    }
}
