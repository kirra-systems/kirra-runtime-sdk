//! The measurements ADR-0041's ratification checklist asks for.
//!
//! Four of the six gates live here (append/replay, queries, growth, migration);
//! the corruption/restart gate lives in [`crate::crash`] because it needs to
//! kill a process.
//!
//! # Percentiles, not averages
//!
//! Every timing reports min / median / p99 / max, and the max is the number
//! that matters on a robot. A mean append latency hides exactly the event that
//! causes trouble — the commit that lands on a WAL checkpoint, or the fsync
//! that queues behind perception writing to the same eMMC. ADR-0041 is deciding
//! whether this substrate is viable on a device that is *already*
//! resource-constrained, and that decision turns on the tail.
//!
//! These are wall-clock host measurements under Linux scheduling. They are not
//! WCET and nothing here should be cited as a timing guarantee; the repository
//! keeps that distinction sharp for the governor path
//! (`docs/safety/WCET_MEASUREMENT_METHODOLOGY.md`) and the same discipline
//! applies to a store that will run alongside it.

use crate::gen;
use crate::json::Obj;
use crate::standin::{Durability, Store};
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct Timing {
    pub label: String,
    pub samples: usize,
    pub min_us: f64,
    pub median_us: f64,
    pub p99_us: f64,
    pub max_us: f64,
    pub total_ms: f64,
}

impl Timing {
    pub fn from_durations(label: &str, mut d: Vec<Duration>) -> Self {
        let total_ms = d.iter().map(|x| x.as_secs_f64() * 1e3).sum();
        d.sort_unstable();
        let us = |x: &Duration| x.as_secs_f64() * 1e6;
        let at = |q: f64| -> f64 {
            if d.is_empty() {
                return f64::NAN;
            }
            // Nearest-rank. With a handful of samples an interpolated p99 is a
            // fiction; nearest-rank at least names a sample that happened.
            let idx = ((q * d.len() as f64).ceil() as usize).saturating_sub(1);
            us(&d[idx.min(d.len() - 1)])
        };
        Self {
            label: label.to_string(),
            samples: d.len(),
            min_us: d.first().map(us).unwrap_or(f64::NAN),
            median_us: at(0.50),
            p99_us: at(0.99),
            max_us: d.last().map(us).unwrap_or(f64::NAN),
            total_ms,
        }
    }

    pub fn to_json(&self) -> Obj {
        Obj::new()
            .str("label", &self.label)
            .int("samples", self.samples as u64)
            .float("min_us", self.min_us)
            .float("median_us", self.median_us)
            .float("p99_us", self.p99_us)
            .float("max_us", self.max_us)
            .float("total_ms", self.total_ms)
    }
}

fn remove_db(path: &Path) {
    let _ = std::fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let mut p = path.as_os_str().to_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(p));
    }
}

/// Total bytes on disk for the database, including the WAL and shared-memory
/// files.
///
/// Counting only the main file understates growth by the whole WAL, which on a
/// write-heavy log is where the recent data actually is — and understating
/// growth is the direction that makes a device run out of disk in the field.
pub fn db_bytes(path: &Path) -> u64 {
    let mut total = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    for suffix in ["-wal", "-shm"] {
        let mut p = path.as_os_str().to_os_string();
        p.push(suffix);
        total += std::fs::metadata(std::path::PathBuf::from(p))
            .map(|m| m.len())
            .unwrap_or(0);
    }
    total
}

// ---------------------------------------------------------------------------
// Append
// ---------------------------------------------------------------------------

/// When one commit happened, in milliseconds from a stated origin.
///
/// The origin is the caller's if one is supplied to [`append_from`], otherwise
/// the start of the append loop. **Which origin matters more than it looks.**
/// These offsets exist to be looked up in a concurrently-sampled series, and two
/// `Instant::now()` calls taken at different moments are two different
/// coordinate systems — a lookup across them lands on the wrong span entirely,
/// not merely a slightly shifted one. `append` does event generation and a hash
/// calibration before its loop, so that gap is seconds, not microseconds.
///
/// Exists so a stall can be correlated with system counters sampled *across it*
/// rather than across the whole run. Without a window, the only honest thing to
/// say about a concurrent counter is that it accumulated somewhere during the
/// run — see [`crate::stall`], where reading a whole-run counter against a
/// one-commit duration was a real defect (ADR-0041 open question 9).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommitWindow {
    pub start_ms: f64,
    pub end_ms: f64,
}

impl CommitWindow {
    pub fn duration_ms(&self) -> f64 {
        self.end_ms - self.start_ms
    }
}

pub struct AppendResult {
    pub timing: Timing,
    pub durability: Durability,
    pub batch: usize,
    pub events: u64,
    pub events_per_second: f64,
    pub hash_share_percent: f64,
    /// The window of the single slowest commit. `None` only when no commit ran.
    ///
    /// This is the slowest by the same measurement that produces
    /// `timing.max_us`, so the two always describe the same commit.
    pub slowest_commit: Option<CommitWindow>,
}

/// Append `events` in transactions of `batch` events each, timing every commit.
///
/// `hash_share_percent` reports how much of the append cost is the harness's
/// own SHA-256 rather than SQLite. It is the honesty check on the local hash
/// substitution documented in [`crate::sha256`]: if this is small, the
/// substitution does not move the decision; if it is large, the run must be
/// repeated against the real primitive before anything is concluded.
pub fn append(
    path: &Path,
    durability: Durability,
    events: u64,
    entities: u64,
    batch: usize,
    seed: u64,
) -> Result<AppendResult, String> {
    append_from(path, durability, events, entities, batch, seed, None)
}

/// [`append`], with [`CommitWindow`] offsets measured from a caller-supplied
/// origin.
///
/// Pass `Some(origin)` when something else is sampling concurrently and needs
/// its timestamps to share a coordinate system with the commit windows — see
/// [`crate::stall::run`], which starts its sampler at that origin. `None`
/// measures from the start of the append loop, which is right for every caller
/// that is not correlating against an external series.
#[allow(clippy::too_many_arguments)]
pub fn append_from(
    path: &Path,
    durability: Durability,
    events: u64,
    entities: u64,
    batch: usize,
    seed: u64,
    origin: Option<Instant>,
) -> Result<AppendResult, String> {
    remove_db(path);
    let mut store = Store::open(path, durability).map_err(|e| e.to_string())?;
    let all = gen::events(0, events, entities, seed);

    // Measured before the appends so the two are not competing for cache.
    let hash_ns = {
        let sample: Vec<Vec<u8>> = all.iter().take(1000).map(|e| e.canonical_bytes()).collect();
        let start = Instant::now();
        for bytes in &sample {
            std::hint::black_box(crate::sha256::hex(bytes));
        }
        start.elapsed().as_secs_f64() * 1e9 / sample.len().max(1) as f64
    };

    let mut durations = Vec::with_capacity(all.len() / batch.max(1) + 1);
    let mut slowest: Option<(Duration, CommitWindow)> = None;
    let wall = Instant::now();
    // The origin the windows are reported against. Defaults to the loop start;
    // a concurrent sampler supplies its own so the two agree.
    let window_origin = origin.unwrap_or(wall);
    for chunk in all.chunks(batch.max(1)) {
        let t = Instant::now();
        store.append_batch(chunk).map_err(|e| e.to_string())?;
        let took = t.elapsed();
        let start_ms = t.duration_since(window_origin).as_secs_f64() * 1e3;
        if slowest.as_ref().is_none_or(|(d, _)| took > *d) {
            slowest = Some((
                took,
                CommitWindow {
                    start_ms,
                    end_ms: start_ms + took.as_secs_f64() * 1e3,
                },
            ));
        }
        durations.push(took);
    }
    let wall = wall.elapsed().as_secs_f64();

    let timing = Timing::from_durations("append_commit", durations);
    // Two hashes per event: the payload digest and the chain link.
    let hash_total_ms = hash_ns * 2.0 * events as f64 / 1e6;
    Ok(AppendResult {
        durability: store.durability,
        batch: batch.max(1),
        events,
        events_per_second: if wall > 0.0 {
            events as f64 / wall
        } else {
            f64::NAN
        },
        hash_share_percent: if timing.total_ms > 0.0 {
            100.0 * hash_total_ms / timing.total_ms
        } else {
            f64::NAN
        },
        slowest_commit: slowest.map(|(_, w)| w),
        timing,
    })
}

// ---------------------------------------------------------------------------
// Replay
// ---------------------------------------------------------------------------

pub struct ReplayResult {
    pub events: u64,
    pub cold_rebuild: Timing,
    pub checkpointed_resume: Timing,
    pub checkpoint_at: u64,
    pub tail_events: u64,
    pub deterministic: bool,
}

/// ADR-0041's replay gate: full rebuild time, "with the checkpointed case
/// measured separately".
///
/// `deterministic` is the load-bearing assertion, not the timings. A rebuild
/// that is fast but produces a different projection than the incremental fold
/// makes both numbers meaningless, so the digests are compared and the result
/// is reported rather than asserted away — a false here should stop
/// ratification regardless of how good the milliseconds look.
pub fn replay(
    path: &Path,
    durability: Durability,
    events: u64,
    entities: u64,
    seed: u64,
    checkpoint_fraction: f64,
) -> Result<ReplayResult, String> {
    remove_db(path);
    let mut store = Store::open(path, durability).map_err(|e| e.to_string())?;
    let all = gen::events(0, events, entities, seed);
    for chunk in all.chunks(512) {
        store.append_batch(chunk).map_err(|e| e.to_string())?;
    }

    // Incremental reference state.
    store.fold_from(0).map_err(|e| e.to_string())?;
    let reference = store.projection_digest().map_err(|e| e.to_string())?;

    // Cold: from zero, no checkpoint.
    let mut cold = Vec::new();
    for _ in 0..3 {
        store.clear_projections().map_err(|e| e.to_string())?;
        let t = Instant::now();
        store.fold_from(0).map_err(|e| e.to_string())?;
        cold.push(t.elapsed());
    }
    let cold_digest = store.projection_digest().map_err(|e| e.to_string())?;

    // Checkpointed: fold the prefix, checkpoint, then resume over the tail.
    let checkpoint_at = ((events as f64) * checkpoint_fraction.clamp(0.0, 1.0)) as u64;
    let tail_events = events.saturating_sub(checkpoint_at);
    let mut resume = Vec::new();
    for _ in 0..3 {
        store.clear_projections().map_err(|e| e.to_string())?;
        store.fold_from(0).map_err(|e| e.to_string())?;
        // The checkpoint is written from a state folded to `checkpoint_at`;
        // resuming means folding only the tail.
        store.write_checkpoint("bench").map_err(|e| e.to_string())?;
        // Read the resume point back rather than reusing `checkpoint_at`: a
        // restart has only the stored checkpoint to go on, so that is the path
        // worth timing.
        let (resume_from, _) = store
            .read_checkpoint("bench")
            .map_err(|e| e.to_string())?
            .ok_or("checkpoint vanished immediately after being written")?;
        let t = Instant::now();
        store
            .fold_from(resume_from.min(checkpoint_at))
            .map_err(|e| e.to_string())?;
        resume.push(t.elapsed());
    }

    Ok(ReplayResult {
        events,
        cold_rebuild: Timing::from_durations("cold_rebuild", cold),
        checkpointed_resume: Timing::from_durations("checkpointed_resume", resume),
        checkpoint_at,
        tail_events,
        deterministic: cold_digest == reference,
    })
}

// ---------------------------------------------------------------------------
// Queries — blueprint §12's four families
// ---------------------------------------------------------------------------

pub struct QueryResult {
    pub timings: Vec<Timing>,
    pub rows_returned: Vec<(String, u64)>,
}

/// One measurement per query family, plus the bitemporal point query that the
/// projection cannot answer.
///
/// The split between `point_latest` and `point_time_travel` is the interesting
/// one. The latest view is a projection lookup and should be trivial; time
/// travel goes to the event log, and its cost is the real question behind
/// "should this be a graph database". Reporting only the fast one would make
/// the substrate look better than it is.
///
/// `rows_returned` is recorded alongside, because a query that is fast because
/// it matched nothing is not a fast query — it is a broken benchmark, and this
/// is how a reader catches one.
pub fn queries(
    path: &Path,
    durability: Durability,
    events: u64,
    entities: u64,
    seed: u64,
    repeats: usize,
) -> Result<QueryResult, String> {
    remove_db(path);
    let mut store = Store::open(path, durability).map_err(|e| e.to_string())?;
    for chunk in gen::events(0, events, entities, seed).chunks(512) {
        store.append_batch(chunk).map_err(|e| e.to_string())?;
    }
    store.fold_from(0).map_err(|e| e.to_string())?;

    let mut timings = Vec::new();
    let mut rows_returned = Vec::new();
    let mut rng = gen::Rng::new(seed ^ 0xa5a5);

    // --- Point (latest view): where_is(entity) ---------------------------
    let mut d = Vec::with_capacity(repeats);
    let mut rows = 0u64;
    for _ in 0..repeats {
        let who = format!("entity-{:06}", rng.below(entities.max(1)));
        let t = Instant::now();
        let got: Option<String> = store
            .conn
            .query_row(
                "SELECT place_ref FROM entities_projection WHERE entity_id = ?1",
                [&who],
                |r| r.get(0),
            )
            .ok();
        d.push(t.elapsed());
        rows += got.is_some() as u64;
    }
    timings.push(Timing::from_durations("point_latest", d));
    rows_returned.push(("point_latest".to_string(), rows));

    // --- Point (time travel): where_is(entity, valid_time, txn_time) -----
    let mut d = Vec::with_capacity(repeats);
    let mut rows = 0u64;
    let mid_ms = 1_700_000_000_000i64 + (events as i64 / 2) * 50;
    for _ in 0..repeats {
        let who = format!("entity-{:06}", rng.below(entities.max(1)));
        let t = Instant::now();
        let got: Option<String> = store
            .conn
            .query_row(
                "SELECT payload FROM world_events \
                  WHERE subject = ?1 AND kind = 'observation' \
                    AND valid_from_ms <= ?2 AND txn_time_ms <= ?3 \
                  ORDER BY valid_from_ms DESC, generation DESC LIMIT 1",
                rusqlite::params![who, mid_ms, mid_ms],
                |r| r.get(0),
            )
            .ok();
        d.push(t.elapsed());
        rows += got.is_some() as u64;
    }
    timings.push(Timing::from_durations("point_time_travel", d));
    rows_returned.push(("point_time_travel".to_string(), rows));

    // --- Set: entities_in(place) -----------------------------------------
    let mut d = Vec::with_capacity(repeats);
    let mut rows = 0u64;
    for _ in 0..repeats {
        let place = format!("place-{:04}", rng.below(64));
        let t = Instant::now();
        let n: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM entities_projection WHERE place_ref = ?1 AND kind = 'entity'",
                [&place],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        d.push(t.elapsed());
        rows += n as u64;
    }
    timings.push(Timing::from_durations("set_entities_in", d));
    rows_returned.push(("set_entities_in".to_string(), rows));

    // --- Graph: bounded path_between(a, b) -------------------------------
    // Bounded at depth 4, because blueprint §12 forbids offering unbounded
    // traversal at all: "an unbounded query in a robot is an availability
    // hazard". Benchmarking an unbounded traversal would measure a query the
    // design does not permit.
    let mut d = Vec::with_capacity(repeats);
    let mut rows = 0u64;
    for _ in 0..repeats {
        let from = format!("entity-{:06}", rng.below(entities.max(1)));
        let t = Instant::now();
        let n: i64 = store
            .conn
            .query_row(
                "WITH RECURSIVE reach(node, depth) AS ( \
                   SELECT ?1, 0 \
                   UNION \
                   SELECT r.object, reach.depth + 1 FROM relationships_projection r \
                     JOIN reach ON r.subject = reach.node WHERE reach.depth < 4 \
                 ) SELECT COUNT(*) FROM reach",
                [&from],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        d.push(t.elapsed());
        rows += n as u64;
    }
    timings.push(Timing::from_durations("graph_bounded_reach", d));
    rows_returned.push(("graph_bounded_reach".to_string(), rows));

    // --- Temporal: changes_since(t) --------------------------------------
    let mut d = Vec::with_capacity(repeats);
    let mut rows = 0u64;
    for _ in 0..repeats {
        let since = 1_700_000_000_000i64 + (rng.below(events.max(1)) as i64) * 50;
        let t = Instant::now();
        let n: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(DISTINCT subject) FROM world_events WHERE txn_time_ms > ?1",
                [since],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        d.push(t.elapsed());
        rows += n as u64;
    }
    timings.push(Timing::from_durations("temporal_changes_since", d));
    rows_returned.push(("temporal_changes_since".to_string(), rows));

    Ok(QueryResult {
        timings,
        rows_returned,
    })
}

// ---------------------------------------------------------------------------
// Growth
// ---------------------------------------------------------------------------

pub struct GrowthResult {
    pub events: u64,
    pub payload_bytes: usize,
    pub log_only_bytes: u64,
    pub with_projections_bytes: u64,
    pub bytes_per_event: f64,
    pub bytes_per_event_with_projections: f64,
}

/// Measured bytes per event, log-only and with projections materialized.
///
/// Both are reported because they answer different questions. Log-only is what
/// grows forever under ADR-0041's append-only rule (P2) and therefore what
/// compaction has to bound. The projection cost is roughly fixed in the entity
/// count and is *rebuildable* — it can be discarded and regenerated, which is
/// why open question 3 asks whether projections belong in a sidecar file at
/// all. One combined number would hide that distinction.
pub fn growth(
    path: &Path,
    durability: Durability,
    events: u64,
    entities: u64,
    seed: u64,
    payload_bytes: usize,
) -> Result<GrowthResult, String> {
    remove_db(path);
    let mut store = Store::open(path, durability).map_err(|e| e.to_string())?;
    for chunk in gen::events_sized(0, events, entities, seed, payload_bytes).chunks(512) {
        store.append_batch(chunk).map_err(|e| e.to_string())?;
    }
    store.durable_checkpoint().map_err(|e| e.to_string())?;
    let log_only_bytes = db_bytes(path);

    store.fold_from(0).map_err(|e| e.to_string())?;
    store.durable_checkpoint().map_err(|e| e.to_string())?;
    let with_projections_bytes = db_bytes(path);

    Ok(GrowthResult {
        events,
        payload_bytes,
        log_only_bytes,
        with_projections_bytes,
        bytes_per_event: log_only_bytes as f64 / events.max(1) as f64,
        bytes_per_event_with_projections: with_projections_bytes as f64 / events.max(1) as f64,
    })
}

/// Days until a budget is exhausted at a given observation rate.
///
/// Pure, and separated from the measurement so the projection can be re-run
/// against a different rate or budget without re-running the benchmark — which
/// is what will actually happen when someone asks "what if the camera runs at
/// 30 Hz instead of 10?".
pub fn days_to_fill(bytes_per_event: f64, events_per_day: f64, budget_bytes: f64) -> f64 {
    if bytes_per_event <= 0.0 || events_per_day <= 0.0 {
        return f64::INFINITY;
    }
    budget_bytes / (bytes_per_event * events_per_day)
}

// ---------------------------------------------------------------------------
// Migration
// ---------------------------------------------------------------------------

pub struct MigrationResult {
    pub events: u64,
    pub apply: Timing,
    pub version_after: i64,
    pub future_schema_refused: bool,
    pub chain_intact_after: bool,
}

/// ADR-0041's migration gate: a schema change applied to a *populated* store,
/// plus the fail-closed refusal of a future schema version.
///
/// `chain_intact_after` is checked because a migration that silently perturbs
/// the event log destroys the tamper evidence the whole design rests on. The
/// v2 step here only touches a projection, so this must hold; if a future step
/// needs to touch the log, this assertion is where that shows up.
pub fn migration(
    path: &Path,
    durability: Durability,
    events: u64,
    entities: u64,
    seed: u64,
    step: &str,
) -> Result<MigrationResult, String> {
    remove_db(path);
    let mut store = Store::open(path, durability).map_err(|e| e.to_string())?;
    for chunk in gen::events(0, events, entities, seed).chunks(512) {
        store.append_batch(chunk).map_err(|e| e.to_string())?;
    }
    store.fold_from(0).map_err(|e| e.to_string())?;

    let t = Instant::now();
    store.migrate_to_v2_using(step).map_err(|e| e.to_string())?;
    let apply = Timing::from_durations("migrate_v1_to_v2", vec![t.elapsed()]);

    let version_after = store.schema_version().map_err(|e| e.to_string())?;
    let chain_intact_after = matches!(
        store.verify_chain().map_err(|e| e.to_string())?,
        crate::standin::ChainStatus::Intact { .. }
    );
    drop(store);

    // The store is now at v2, which this binary (KNOWN_SCHEMA_VERSION = 1)
    // must refuse rather than open optimistically.
    let future_schema_refused = matches!(
        Store::open(path, durability),
        Err(crate::standin::OpenError::FutureSchema { .. })
    );

    Ok(MigrationResult {
        events,
        apply,
        version_after,
        future_schema_refused,
        chain_intact_after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("wm2-bench-{}-{}.sqlite", name, std::process::id()));
        p
    }

    #[test]
    fn timing_percentiles_are_nearest_rank() {
        let d: Vec<Duration> = (1..=100).map(Duration::from_micros).collect();
        let t = Timing::from_durations("x", d);
        assert_eq!(t.samples, 100);
        assert!((t.min_us - 1.0).abs() < 0.5);
        assert!((t.max_us - 100.0).abs() < 0.5);
        assert!((t.median_us - 50.0).abs() < 1.5);
        assert!((t.p99_us - 99.0).abs() < 1.5);
    }

    #[test]
    fn timing_of_a_single_sample_does_not_panic() {
        let t = Timing::from_durations("one", vec![Duration::from_micros(7)]);
        assert_eq!(t.samples, 1);
        assert!((t.p99_us - 7.0).abs() < 0.5);
    }

    #[test]
    fn timing_of_no_samples_is_nan_not_zero() {
        // Zero would read as "instantaneous"; NaN renders as JSON null and is
        // visibly absent.
        let t = Timing::from_durations("none", vec![]);
        assert!(t.median_us.is_nan() && t.max_us.is_nan());
    }

    #[test]
    fn replay_rebuild_matches_the_incremental_state() {
        let p = temp("replay");
        let r = replay(&p, Durability::Off, 2_000, 100, 3, 0.8).unwrap();
        assert!(
            r.deterministic,
            "cold rebuild diverged from the incremental fold"
        );
        assert_eq!(r.tail_events, 400);
        assert!(r.cold_rebuild.samples == 3 && r.checkpointed_resume.samples == 3);
        remove_db(&p);
    }

    #[test]
    fn every_query_family_returns_rows() {
        // A query benchmark that matches nothing measures the empty case and
        // looks excellent doing it.
        let p = temp("queries");
        let q = queries(&p, Durability::Off, 4_000, 60, 11, 40).unwrap();
        assert_eq!(q.timings.len(), 5);
        for (name, rows) in &q.rows_returned {
            assert!(*rows > 0, "query family `{name}` matched nothing");
        }
        remove_db(&p);
    }

    #[test]
    fn growth_reports_more_bytes_with_projections_than_without() {
        let p = temp("growth");
        let g = growth(&p, Durability::Off, 3_000, 200, 5, 96).unwrap();
        assert!(g.with_projections_bytes >= g.log_only_bytes);
        assert!(g.bytes_per_event > 0.0);
        remove_db(&p);
    }

    #[test]
    fn a_wider_payload_costs_more_per_event() {
        let p = temp("growth-width");
        let narrow = growth(&p, Durability::Off, 2_000, 100, 5, 32).unwrap();
        let wide = growth(&p, Durability::Off, 2_000, 100, 5, 512).unwrap();
        assert!(
            wide.bytes_per_event > narrow.bytes_per_event,
            "payload width did not move storage cost: {} vs {}",
            wide.bytes_per_event,
            narrow.bytes_per_event
        );
        remove_db(&p);
    }

    #[test]
    fn days_to_fill_is_arithmetic_and_guards_its_edges() {
        // 100 bytes/event, 1e6 events/day, 10 GiB budget.
        let d = days_to_fill(100.0, 1e6, 10.0 * 1024.0 * 1024.0 * 1024.0);
        assert!((d - 107.37).abs() < 0.1, "got {d}");
        assert!(days_to_fill(0.0, 1e6, 1e9).is_infinite());
        assert!(days_to_fill(100.0, 0.0, 1e9).is_infinite());
    }

    #[test]
    fn migration_applies_and_then_fails_closed() {
        let p = temp("migration");
        let m = migration(
            &p,
            Durability::Off,
            1_500,
            80,
            2,
            crate::standin::SCHEMA_V2_STEP,
        )
        .unwrap();
        assert_eq!(m.version_after, 2);
        assert!(
            m.future_schema_refused,
            "a v2 store was opened by a v1 binary"
        );
        assert!(m.chain_intact_after, "migration disturbed the event log");
        remove_db(&p);
    }

    #[test]
    fn append_reports_the_hash_share() {
        let p = temp("append");
        let a = append(&p, Durability::Off, 2_000, 100, 1, 4).unwrap();
        assert_eq!(a.events, 2_000);
        assert!(a.timing.samples == 2_000, "batch=1 should time every event");
        assert!(a.hash_share_percent.is_finite() && a.hash_share_percent >= 0.0);
        remove_db(&p);
    }

    #[test]
    fn the_slowest_commit_window_describes_the_same_commit_as_max_us() {
        // The stall attribution correlates system counters against this window,
        // so a window that named a DIFFERENT commit than `max_us` would put the
        // counters on the wrong two seconds and report confidently from them.
        // Cheap to assert here, and the alternative is unfalsifiable on target.
        let p = temp("append-window");
        let a = append(&p, Durability::Off, 2_000, 100, 16, 4).unwrap();
        let w = a
            .slowest_commit
            .expect("commits ran, so there is a slowest");
        assert!(
            (w.duration_ms() - a.timing.max_us / 1e3).abs() < 1e-6,
            "window {:.6} ms != max_us {:.6} ms — they name different commits",
            w.duration_ms(),
            a.timing.max_us / 1e3
        );
        assert!(w.start_ms >= 0.0 && w.end_ms >= w.start_ms);
        assert!(
            w.end_ms <= a.timing.total_ms + 1.0,
            "window ends past the whole append loop"
        );
        remove_db(&p);
    }

    #[test]
    fn commit_windows_are_measured_from_the_supplied_origin() {
        // The correlation in `stall` is only sound if the commit windows and the
        // sampler share one origin. `append` does event generation and a hash
        // calibration before its loop, so an unshared origin is off by that much
        // — seconds, not microseconds — and a lookup lands on an unrelated span
        // of the series while still looking like a measurement.
        //
        // The sleep stands in for that setup gap: with the origin honoured the
        // offset must include it; with the loop start used instead it would not.
        let p = temp("append-origin");
        let origin = Instant::now();
        std::thread::sleep(Duration::from_millis(60));
        let a = append_from(&p, Durability::Off, 2_000, 100, 16, 4, Some(origin)).unwrap();
        let w = a.slowest_commit.expect("commits ran");
        assert!(
            w.start_ms >= 60.0,
            "window start {:.1} ms ignores the supplied origin",
            w.start_ms
        );
        // And the default still measures from the loop start, so every other
        // caller is unaffected. The invariant there is containment in the loop,
        // NOT an upper bound in milliseconds: the slowest commit may fall
        // anywhere in the run, so "start_ms is small" would be a coincidence
        // dressed as an assertion.
        let b = append(&p, Durability::Off, 2_000, 100, 16, 4).unwrap();
        let bw = b.slowest_commit.expect("commits ran");
        assert!(
            bw.start_ms >= 0.0 && bw.end_ms <= b.timing.total_ms + 1.0,
            "default-origin window [{:.1}, {:.1}] ms escapes its own {:.1} ms loop",
            bw.start_ms,
            bw.end_ms,
            b.timing.total_ms
        );
        remove_db(&p);
    }

    /// BEST-OF-3 per arm, and the statistic is `max` on purpose — the mirror of
    /// the `min` used for elapsed time in `standin`. This is a THROUGHPUT
    /// comparison, and interference from other tests (`cargo test` runs them as
    /// threads of one process) can only ever push events/second DOWN, never up.
    /// So the maximum of repeated trials is the least-contaminated estimate,
    /// and averaging would fold the interference in.
    ///
    /// Not a loosened threshold: the assertion is still strict `>`, on the same
    /// two batch sizes. Measured here, the honest margin is 4.8x-7.6x. CI saw
    /// it INVERT to 0.62x — batched 14 292 ev/s against unbatched 23 138 —
    /// with the batched arm collapsing from a local 64 000-97 000. A five-fold
    /// margin does not invert because batching stopped working; it inverts
    /// because that arm lost the CPU. Three trials must all be contended for
    /// the comparison to still flip.
    ///
    /// Third and rarest of the flakes in this lane: it did not fire once in the
    /// 20 full-suite runs that verified the other two.
    #[test]
    fn batching_amortizes_the_commit() {
        const TRIALS: usize = 3;
        let best = |batch: usize| -> f64 {
            (0..TRIALS)
                .map(|i| {
                    let p = temp(&format!("append-batch-{batch}-{i}"));
                    let r = append(&p, Durability::Off, 4_000, 100, batch, 4).unwrap();
                    remove_db(&p);
                    r.events_per_second
                })
                .fold(f64::NEG_INFINITY, f64::max)
        };
        let one = best(1);
        let many = best(256);
        assert!(
            many > one,
            "batching did not help: {many} vs {one} (best of {TRIALS} per arm, \
             so this is not contention)"
        );
    }

    #[test]
    fn db_bytes_counts_the_wal() {
        let p = temp("dbbytes");
        remove_db(&p);
        let mut s = Store::open(&p, Durability::Normal).unwrap();
        s.append_batch(&gen::events(0, 500, 20, 1)).unwrap();
        let with_wal = db_bytes(&p);
        let main_only = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        assert!(with_wal > main_only, "WAL bytes were not counted");
        remove_db(&p);
    }
}
