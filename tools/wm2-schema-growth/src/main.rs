//! WM-2 — bytes/event against the ratified schema.
//!
//! Discharges the obligation `KIRRA-WM2-SCHEMA-001` §8.4 created: ADR-0041
//! D-2's 458.51 B/event, and OQ2's retention horizons derived from it, were
//! measured against the harness's stand-in schema. This measures the real one.
//!
//! # Method, stated so the number can be checked rather than believed
//!
//! **Counting unit:** bytes of on-disk database per appended event, where
//! "bytes" is `len(main) + len(-wal) + len(-shm)` after a TRUNCATE checkpoint
//! — the same `db_bytes` definition `wm2-persistence-harness::bench` uses, so
//! the two are the same measurement of the same thing.
//!
//! **Independence unit:** one database build. Every run starts from a removed
//! file; the events within a build are NOT independent of each other (SQLite
//! page fill depends on insertion order) and no per-event variance is claimed.
//!
//! **Held fixed:** platform, SQLite build, event stream (seed, entity count,
//! payload width, event count), and the log-only condition — no projections
//! exist in `kirra-world-store` yet, so the comparable D-2 figure is
//! `bytes_per_event`, never `bytes_per_event_with_projections`.
//!
//! **Varied:** the schema, and the fill of the columns the schema added
//! (see [`fill`]).
//!
//! **The claim this supports:** the multiplicative change in log-only growth
//! attributable to the ratified schema, on one platform. It does NOT by
//! itself restate D-2's Jetson figure — see the `paired` record and the
//! evidence set's README for what carrying the ratio to the Jetson does and
//! does not license.

mod fill;
mod gen;

use std::path::{Path, PathBuf};
use std::time::Instant;

use fill::Fill;
use kirra_world_store::{ClaimStatus, NewEvent, WorldStore, WriterClass};

// ---------------------------------------------------------------------------
// db_bytes — deliberately identical to the harness's definition
// ---------------------------------------------------------------------------

fn db_bytes(path: &Path) -> u64 {
    let mut total = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    for suffix in ["-wal", "-shm"] {
        let mut p = path.as_os_str().to_os_string();
        p.push(suffix);
        total += std::fs::metadata(PathBuf::from(p))
            .map(|m| m.len())
            .unwrap_or(0);
    }
    total
}

fn remove_db(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(p));
    }
}

/// Checkpoint the WAL back into the main file and truncate it.
///
/// The store exposes no connection, and should not grow one for a benchmark.
/// A second connection to the same file is the honest way to reach `PRAGMA
/// wal_checkpoint` without widening the store's API for the instrument's
/// convenience.
fn checkpoint(path: &Path) -> Result<(), String> {
    let conn = rusqlite::Connection::open(path).map_err(|e| e.to_string())?;
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn page_stats(path: &Path) -> Result<(i64, i64), String> {
    let conn = rusqlite::Connection::open(path).map_err(|e| e.to_string())?;
    let page_size: i64 = conn
        .query_row("PRAGMA page_size", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let page_count: i64 = conn
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    Ok((page_size, page_count))
}

fn sqlite_version() -> String {
    rusqlite::Connection::open_in_memory()
        .and_then(|c| c.query_row("SELECT sqlite_version()", [], |r| r.get::<_, String>(0)))
        .unwrap_or_else(|_| "unknown".to_string())
}

// ---------------------------------------------------------------------------
// The measurement
// ---------------------------------------------------------------------------

struct Growth {
    fill: Fill,
    events: u64,
    payload_bytes: usize,
    log_only_bytes: u64,
    bytes_per_event: f64,
    page_size: i64,
    page_count: i64,
    elapsed_s: f64,
}

fn growth(
    path: &Path,
    fill: Fill,
    events: u64,
    entities: u64,
    seed: u64,
    payload_bytes: usize,
) -> Result<Growth, String> {
    remove_db(path);
    let mut store = WorldStore::open(path).map_err(|e| e.to_string())?;

    let stream = gen::events_sized(0, events, entities, seed, payload_bytes);
    let t = Instant::now();
    for e in &stream {
        let a = fill::added(fill, e.generation, seed);
        let prov: Vec<&str> = a.provenance.iter().map(String::as_str).collect();
        store
            .append(&NewEvent {
                event_id: &e.event_id,
                observation_id: &a.observation_id,
                txn_time_ms: e.txn_time_ms,
                valid_from_ms: e.valid_from_ms,
                valid_to_ms: None,
                source: &e.source,
                source_version: "0.1.0",
                // Pinned to the sensor path: the generated stream is sensor
                // traffic, and these are closed vocabularies a few bytes wide.
                writer_class: WriterClass::Sensor,
                claim_status: ClaimStatus::Confirmed,
                provenance: &prov,
                frame_id: a.frame_id.as_deref(),
                map_id: a.map_id.as_deref(),
                kind: e.kind,
                subject: &e.subject,
                predicate: e.predicate.as_deref(),
                object: e.object.as_deref(),
                payload: &e.payload,
                payload_schema: 1,
                retention_class: e.retention_class,
            })
            .map_err(|err| format!("append at generation {}: {err}", e.generation))?;
    }
    let elapsed_s = t.elapsed().as_secs_f64();

    checkpoint(path)?;
    let log_only_bytes = db_bytes(path);
    let (page_size, page_count) = page_stats(path)?;

    Ok(Growth {
        fill,
        events,
        payload_bytes,
        log_only_bytes,
        bytes_per_event: log_only_bytes as f64 / events.max(1) as f64,
        page_size,
        page_count,
        elapsed_s,
    })
}

/// Days until a budget is exhausted. Same form as the harness's
/// `bench::days_to_fill`, so a horizon computed here is computed the way OQ2's
/// horizons were.
fn days_to_fill(bytes_per_event: f64, events_per_day: f64, budget_bytes: f64) -> f64 {
    if bytes_per_event <= 0.0 || events_per_day <= 0.0 {
        return f64::INFINITY;
    }
    budget_bytes / (bytes_per_event * events_per_day)
}

// ---------------------------------------------------------------------------
// Emission
// ---------------------------------------------------------------------------

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

struct Rec(Vec<String>);

impl Rec {
    fn new() -> Self {
        Self(Vec::new())
    }
    fn s(mut self, k: &str, v: &str) -> Self {
        self.0.push(format!("\"{}\":\"{}\"", k, json_escape(v)));
        self
    }
    fn i(mut self, k: &str, v: i64) -> Self {
        self.0.push(format!("\"{k}\":{v}"));
        self
    }
    fn f(mut self, k: &str, v: f64) -> Self {
        self.0.push(format!("\"{k}\":{v}"));
        self
    }
    fn line(self) -> String {
        format!("{{{}}}", self.0.join(","))
    }
}

/// Provenance every record carries.
///
/// `schema_digest` is the load-bearing one: it comes from the store itself,
/// so a result can never be re-read as being about a schema that was edited
/// after the run.
fn facts(rec: Rec, seed: u64) -> Rec {
    rec.s("tool", "wm2-schema-growth")
        .s("tool_version", env!("CARGO_PKG_VERSION"))
        .s("schema_digest", &kirra_world_store::schema_digest())
        .i("schema_version", kirra_world_store::SCHEMA_VERSION)
        .s("chain_algorithm", kirra_world_store::CHAIN_ALGORITHM)
        .s("stream_shape", gen::STREAM_SHAPE_DIGEST)
        .s("sqlite_version", &sqlite_version())
        .s(
            "build_profile",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
        )
        .s("arch", std::env::consts::ARCH)
        .s("os", std::env::consts::OS)
        .i("seed", seed as i64)
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

const USAGE: &str = "\
wm2-schema-growth — bytes/event against the ratified WM-2 schema

  --events <n>            Events per build (default: 100000, = ADR-0041 D-2)
  --entities <n>          Distinct entities (default: 1000, = D-2)
  --payload-bytes <n>     Payload width (default: 96, = D-2)
  --seed <n>              Stream seed (default: 20260803, = D-2)
  --events-per-day <f>    For the horizon (default: 864000, = D-2)
  --budget-gib <f>        Disk budget for the horizon (default: 8.0, = D-2)
  --standin-bpe <f>       Stand-in bytes/event measured on THIS host, to pair
                          against. Omit and no ratio is emitted — a ratio
                          against a figure from another machine is not a
                          schema ratio and this tool will not fabricate one.
  --db <path>             Scratch database path (default: ./wm2-growth.sqlite)
  --out <path>            Append JSONL here (default: stdout only)
";

fn main() {
    let mut events = 100_000u64;
    let mut entities = 1_000u64;
    let mut payload_bytes = gen::DEFAULT_PAYLOAD_BYTES;
    let mut seed = 20_260_803u64;
    let mut events_per_day = 864_000.0f64;
    let mut budget_gib = 8.0f64;
    let mut standin_bpe: Option<f64> = None;
    let mut db = PathBuf::from("wm2-growth.sqlite");
    let mut out: Option<PathBuf> = None;

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let flag = argv[i].as_str();
        let mut val = || {
            i += 1;
            argv.get(i).cloned().unwrap_or_else(|| {
                eprintln!("{flag} needs a value");
                std::process::exit(2);
            })
        };
        match flag {
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            "--events" => events = val().parse().expect("--events"),
            "--entities" => entities = val().parse().expect("--entities"),
            "--payload-bytes" => payload_bytes = val().parse().expect("--payload-bytes"),
            "--seed" => seed = val().parse().expect("--seed"),
            "--events-per-day" => events_per_day = val().parse().expect("--events-per-day"),
            "--budget-gib" => budget_gib = val().parse().expect("--budget-gib"),
            "--standin-bpe" => standin_bpe = Some(val().parse().expect("--standin-bpe")),
            "--db" => db = PathBuf::from(val()),
            "--out" => out = Some(PathBuf::from(val())),
            other => {
                eprintln!("unknown flag {other}\n{USAGE}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let budget_bytes = budget_gib * 1024.0 * 1024.0 * 1024.0;
    let mut lines: Vec<String> = Vec::new();
    let mut measured: Vec<Growth> = Vec::new();

    for f in [Fill::Lean, Fill::Populated] {
        eprintln!("measuring fill={} events={events} ...", f.as_str());
        match growth(&db, f, events, entities, seed, payload_bytes) {
            Ok(g) => {
                let rec = facts(Rec::new(), seed)
                    .s("record", "growth")
                    .s("fill", g.fill.as_str())
                    .s("fill_describes", g.fill.describe())
                    .i("events", g.events as i64)
                    .i("entities", entities as i64)
                    .i("payload_bytes", g.payload_bytes as i64)
                    .i("log_only_bytes", g.log_only_bytes as i64)
                    .f("bytes_per_event", g.bytes_per_event)
                    .i("page_size", g.page_size)
                    .i("page_count", g.page_count)
                    .f("append_elapsed_s", g.elapsed_s)
                    .f("assumed_events_per_day", events_per_day)
                    .f("budget_gib", budget_gib)
                    .f(
                        "days_to_fill_budget",
                        days_to_fill(g.bytes_per_event, events_per_day, budget_bytes),
                    );
                println!("{}", rec.clone_line(&mut lines));
                measured.push(g);
            }
            Err(e) => {
                let rec = facts(Rec::new(), seed)
                    .s("record", "error")
                    .s("stage", "growth")
                    .s("fill", f.as_str())
                    .s("error", &e);
                println!("{}", rec.clone_line(&mut lines));
                eprintln!("FAILED: {e}");
                std::process::exit(1);
            }
        }
    }
    remove_db(&db);

    // The paired ratio. Emitted ONLY when a same-host stand-in figure was
    // supplied, because a ratio taken across two machines is not a schema
    // ratio — it is a schema ratio confounded with a platform difference, and
    // the whole point of pairing is to remove that confound.
    if let Some(sb) = standin_bpe {
        for g in &measured {
            let rec = facts(Rec::new(), seed)
                .s("record", "paired_ratio")
                .s("fill", g.fill.as_str())
                .f("standin_bytes_per_event_same_host", sb)
                .f("ratified_bytes_per_event", g.bytes_per_event)
                .f("ratio_ratified_over_standin", g.bytes_per_event / sb)
                .s(
                    "held_fixed",
                    "platform, SQLite build, event stream (seed/entities/payload/count), log-only",
                )
                .s("varied", "schema; fill of the added columns");
            println!("{}", rec.clone_line(&mut lines));
        }
    } else {
        eprintln!(
            "NOTE: no --standin-bpe given, so no ratio was emitted. \
             Run the harness's growth on THIS host and pass its bytes_per_event."
        );
    }

    if let Some(p) = out {
        use std::io::Write;
        let mut fh = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
            .expect("open --out");
        for l in &lines {
            writeln!(fh, "{l}").expect("write --out");
        }
        eprintln!("wrote {} records to {}", lines.len(), p.display());
    }
}

impl Rec {
    /// Emit and retain, so stdout and the `--out` file cannot disagree.
    fn clone_line(self, sink: &mut Vec<String>) -> String {
        let l = self.line();
        sink.push(l.clone());
        l
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_to_fill_is_the_harness_formula() {
        // 458.50624 B/event, 864 000 events/day, 8 GiB — D-2's own inputs.
        let d = days_to_fill(458.50624, 864_000.0, 8.0 * 1024.0 * 1024.0 * 1024.0);
        assert!(
            (d - 21.683574).abs() < 1e-4,
            "formula diverged from D-2: got {d}"
        );
    }

    #[test]
    fn days_to_fill_refuses_nonsense_rather_than_dividing_by_zero() {
        assert!(days_to_fill(0.0, 100.0, 1.0).is_infinite());
        assert!(days_to_fill(100.0, 0.0, 1.0).is_infinite());
    }

    #[test]
    fn json_escaping_survives_a_quote_and_a_backslash() {
        let l = Rec::new().s("k", "a\"b\\c").line();
        assert_eq!(l, r#"{"k":"a\"b\\c"}"#);
    }

    /// A growth run must actually grow, and the populated fill must cost more
    /// than the lean one — if it does not, the band is not a band and the
    /// horizon derived from its upper end is not conservative.
    #[test]
    fn populated_costs_more_per_event_than_lean() {
        let dir = std::env::temp_dir();
        let p = dir.join(format!("wm2-growth-test-{}.sqlite", std::process::id()));
        let lean = growth(&p, Fill::Lean, 2_000, 50, 7, 96).expect("lean");
        let pop = growth(&p, Fill::Populated, 2_000, 50, 7, 96).expect("populated");
        remove_db(&p);
        assert!(lean.bytes_per_event > 0.0);
        assert!(
            pop.bytes_per_event > lean.bytes_per_event,
            "populated {} did not exceed lean {}",
            pop.bytes_per_event,
            lean.bytes_per_event
        );
    }
}
