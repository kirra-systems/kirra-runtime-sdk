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
use std::path::Path;
use std::time::Instant;

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
}

pub fn reclaim(store: &Store, path: &Path) -> Result<ReclaimResult, String> {
    store.durable_checkpoint().map_err(|e| e.to_string())?;
    let bytes_before = crate::bench::db_bytes(path);

    let t = Instant::now();
    store
        .conn
        .execute_batch("VACUUM;")
        .map_err(|e| e.to_string())?;
    let vacuum_ms = t.elapsed().as_secs_f64() * 1e3;

    store.durable_checkpoint().map_err(|e| e.to_string())?;
    let bytes_after = crate::bench::db_bytes(path);
    let bytes_freed = bytes_before as i64 - bytes_after as i64;

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
    })
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
