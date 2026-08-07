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
//! payload width, event count).
//!
//! **Two arms, both reported.** `bytes_per_event` is log-only;
//! `bytes_per_event_with_projections` is after the current-state projection is
//! materialized. D-2 reported both for the stand-in and **OQ2's budget derives
//! from the with-projections figure** — so until `kirra-world-store` grew a
//! read path, D-20 could only compare log-only against a with-projections
//! budget and had to record itself as optimistic. Both are measured now
//! (ADR-0041 D-21), and the second arm runs strictly AFTER the first so it
//! cannot disturb it.
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

/// Platform classification — **the harness's module, included verbatim**, not
/// a copy of it.
///
/// This closes the gap both D-20 and D-21 evidence bundles recorded against
/// themselves: their `TARGET-MEASURED` line was an operator assertion typed
/// into a README, because this tool carried `arch`/`os` and nothing that could
/// *refuse* to be called target evidence. `wm2-persistence-harness` could
/// refuse; this one could not, so two bundles read as equally attested when
/// they were not.
///
/// # Why `#[path]`, and not a copy
///
/// The thing being shared decides **whether evidence may be cited**. Two copies
/// would be two definitions of "target" that can drift apart silently — both
/// tools would still emit a token, and the token would quietly stop meaning one
/// thing. A digest-pinned duplicate (the approach [`gen`] takes, where the
/// shared object is a *data shape* and drift is what the pin exists to catch)
/// would detect that only after it happened.
///
/// Including the source makes drift impossible rather than detectable, which is
/// the same reasoning `verification/kani` uses to `#[path]`-include the frozen
/// kinematics talisman. If the harness moves, this stops compiling — a loud
/// failure in this tool's own CI lane, which is the correct one.
///
/// # One rule is stricter than this instrument needs, deliberately
///
/// The classifier forfeits target status on a non-durable filesystem, because
/// `fsync` cost dominates the harness's measurements. It does **not** dominate
/// this one: D-20 established `bytes_per_event` as the *logical* length of a
/// SQLite database, reproduced byte-for-byte across x86_64 and aarch64, so a
/// `tmpfs` run would give the same answer.
///
/// Reusing the rule anyway is the safe direction, and the alternative is worse
/// than the strictness: a second, laxer definition of "target" living in a
/// second file is exactly how a token stops being evidence.
///
/// `dead_code` is allowed **here, at the include site** rather than fixed in
/// the shared source: this tool uses a subset of the module (the harness's
/// reclamation stage calls the rest), and annotating the original to suit a
/// consumer would be editing shared evidence machinery for one caller's
/// convenience. The allow is scoped to this `mod`, so it silences nothing in
/// this tool's own code.
#[allow(dead_code)]
#[path = "../../wm2-persistence-harness/src/platform.rs"]
mod platform;

use std::path::{Path, PathBuf};
use std::time::Instant;

use fill::Fill;
use kirra_world_store::{
    ClaimStatus, EventId, FrameId, MapId, NewEvent, ObservationId, WorldStore, WriterClass,
};

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
    /// Size after the current-state projection is materialized.
    ///
    /// D-2 reported this for the stand-in and **OQ2's budget derives from it**,
    /// not from the log-only figure. Until `kirra-world-store` grew a read
    /// path there was no ratified counterpart, so D-20 could only compare
    /// log-only against a with-projections budget and had to record itself as
    /// optimistic. This is the number that closes that gap.
    with_projections_bytes: u64,
    bytes_per_event_with_projections: f64,
    projected_rows: i64,
    fold_elapsed_s: f64,
}

fn growth(
    path: &Path,
    fill: Fill,
    events: u64,
    entities: u64,
    seed: u64,
    payload_bytes: usize,
) -> Result<Growth, String> {
    // Fail closed on a degenerate run rather than dividing by `events.max(1)`
    // and reporting the fixed schema overhead as a "bytes per event" figure.
    // Zero events produces a real number from a real database — it is just a
    // number about nothing, and this instrument's whole purpose is that its
    // outputs cannot be mistaken for measurements of something they are not.
    if events == 0 {
        return Err("events must be > 0: a zero-event run has no bytes/event".to_string());
    }
    if entities == 0 {
        return Err("entities must be > 0".to_string());
    }
    if payload_bytes == 0 {
        return Err("payload_bytes must be > 0".to_string());
    }

    remove_db(path);
    let mut store = WorldStore::open(path).map_err(|e| e.to_string())?;

    let stream = gen::events_sized(0, events, entities, seed, payload_bytes);
    let t = Instant::now();
    for e in &stream {
        let a = fill::added(fill, e.generation, seed);
        let prov: Vec<&str> = a.provenance.iter().map(String::as_str).collect();
        // The store's identity and spatial-reference types are validated, so a
        // generated id that the store would refuse must fail the RUN rather
        // than be silently reshaped — a measurement taken over rows the real
        // writer would not accept is not a measurement of the real schema.
        let event_id = EventId::new(e.event_id.clone())
            .map_err(|err| format!("generated event_id at generation {}: {err}", e.generation))?;
        let observation_id = ObservationId::new(a.observation_id.clone()).map_err(|err| {
            format!(
                "generated observation_id at generation {}: {err}",
                e.generation
            )
        })?;
        let frame_id = a
            .frame_id
            .as_deref()
            .map(FrameId::new)
            .transpose()
            .map_err(|err| format!("generated frame_id at generation {}: {err}", e.generation))?;
        let map_id = a
            .map_id
            .as_deref()
            .map(MapId::new)
            .transpose()
            .map_err(|err| format!("generated map_id at generation {}: {err}", e.generation))?;
        store
            .append(&NewEvent {
                event_id: &event_id,
                observation_id: &observation_id,
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
                frame_id: frame_id.as_ref(),
                map_id: map_id.as_ref(),
                kind: e.kind,
                subject: &e.subject,
                predicate: e.predicate.as_deref(),
                object: e.object.as_deref(),
                payload: &e.payload,
                payload_schema: 1,
                retention_class: e.retention_class,
                trust: a.trust.as_ref(),
            })
            .map_err(|err| format!("append at generation {}: {err}", e.generation))?;
    }
    let elapsed_s = t.elapsed().as_secs_f64();

    // Close the writer BEFORE checkpointing. `checkpoint` opens its own
    // connection, and `wal_checkpoint(TRUNCATE)` against a file another
    // connection still holds can return SQLITE_BUSY. That would not merely
    // fail loudly — a partial checkpoint leaves bytes in the WAL that a
    // successful one would have folded into the main file, so the measurement
    // would vary with lock timing rather than with the schema.
    drop(store);

    checkpoint(path)?;
    let log_only_bytes = db_bytes(path);
    let (page_size, page_count) = page_stats(path)?;

    // --- second arm: materialize the projection and re-measure ------------
    //
    // Strictly AFTER the log-only figure is taken, so this arm cannot move it.
    // The projection tables do not exist until the fold installs them (that is
    // deliberate in `kirra-world-store`, for exactly this reason), so the
    // log-only number above is of a store holding only the event log — the
    // same thing D-2 measured on the stand-in.
    let mut store = WorldStore::open(path).map_err(|e| e.to_string())?;
    let ft = Instant::now();
    store.fold().map_err(|e| e.to_string())?;
    let fold_elapsed_s = ft.elapsed().as_secs_f64();
    // The TABLE's row count, not `current_all(..).len()`. The latter applies
    // `holds_at`, so a bounded claim whose `valid_to_ms` has passed would be
    // excluded while still occupying a row and its bytes — an undercount in a
    // measurement whose entire subject is size. The generated stream sets no
    // `valid_to_ms`, so the two agree today; relying on that would make the
    // figure correct by accident.
    let projected_rows = store.projected_row_count().map_err(|e| e.to_string())?;
    drop(store);

    checkpoint(path)?;
    let with_projections_bytes = db_bytes(path);

    if with_projections_bytes < log_only_bytes {
        return Err(format!(
            "with-projections ({with_projections_bytes}) below log-only \
             ({log_only_bytes}) — the fold cannot shrink the database, so one \
             of the two measurements is wrong"
        ));
    }

    Ok(Growth {
        fill,
        events,
        payload_bytes,
        log_only_bytes,
        with_projections_bytes,
        bytes_per_event_with_projections: with_projections_bytes as f64 / events as f64,
        projected_rows,
        fold_elapsed_s,
        // `events` is > 0 by the guard above, so this is not a max(1) fudge.
        bytes_per_event: log_only_bytes as f64 / events as f64,
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
    /// A float field, refusing anything JSON cannot represent.
    ///
    /// Rust prints `f64::INFINITY` as `inf` and NaN as `NaN`, neither of which
    /// is valid JSON — a record carrying one produces an evidence file that
    /// **will not parse**, which is a worse outcome than any wrong number
    /// because it silently breaks every downstream reader.
    ///
    /// The upstream inputs are validated in `main`, so reaching this is a bug
    /// rather than bad user input. It panics for exactly that reason: an
    /// instrument that cannot emit a valid record must stop, not write a
    /// half-valid line and exit 0.
    fn f(mut self, k: &str, v: f64) -> Self {
        assert!(
            v.is_finite(),
            "refusing to emit non-finite value for {k:?} ({v}) — JSON has no \
             representation for it and the record would not parse"
        );
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
fn facts(rec: Rec, seed: u64, cls: &platform::Classification, pf: &platform::PlatformFacts) -> Rec {
    let rec = rec
        .s("evidence_status", cls.status.token())
        .s(
            "not_target_because",
            &if cls.blockers.is_empty() {
                String::new()
            } else {
                cls.blockers.join("; ")
            },
        )
        // The facts the verdict was computed FROM, so a reader can check the
        // classification rather than take it. A status with no corroborating
        // facts is the same unverifiable assertion this flag exists to retire.
        .s("platform_model", pf.model.as_deref().unwrap_or(""))
        .s("db_fs_type", pf.db_fs_type.as_deref().unwrap_or(""))
        .s("db_fs_source", pf.db_fs_source.as_deref().unwrap_or(""))
        .i(
            "operator_asserted_target",
            i64::from(pf.operator_asserted_target),
        );
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
  --assert-target         Operator asserts this is target hardware under
                          representative conditions. Required for, but not
                          sufficient for, JETSON-TARGET-MEASURED: the machine
                          must corroborate it. Same classifier as
                          wm2-persistence-harness, so the token means the same
                          thing in both instruments' records.
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
    let mut assert_target = false;
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
            "--assert-target" => assert_target = true,
            "--db" => db = PathBuf::from(val()),
            "--out" => out = Some(PathBuf::from(val())),
            other => {
                eprintln!("unknown flag {other}\n{USAGE}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    // Validate before measuring, not after. Every one of these feeds a
    // division whose result is emitted as JSON, and JSON has no `inf` — a run
    // with `--events-per-day 0` would otherwise produce an evidence file that
    // does not parse. Rejecting the input costs a second; a corrupt evidence
    // bundle costs whoever tries to read it later.
    for (name, v) in [
        ("--events-per-day", events_per_day),
        ("--budget-gib", budget_gib),
    ] {
        if !v.is_finite() || v <= 0.0 {
            eprintln!("{name} must be a finite positive number (got {v})");
            std::process::exit(2);
        }
    }
    if let Some(sb) = standin_bpe {
        if !sb.is_finite() || sb <= 0.0 {
            eprintln!("--standin-bpe must be a finite positive number (got {sb})");
            std::process::exit(2);
        }
    }
    for (name, v) in [
        ("--events", events),
        ("--entities", entities),
        ("--payload-bytes", payload_bytes as u64),
    ] {
        if v == 0 {
            eprintln!("{name} must be > 0");
            std::process::exit(2);
        }
    }

    // Classify BEFORE measuring, and print the verdict first. An operator who
    // is about to spend two minutes on a run that cannot be cited should learn
    // it now, not from a field at the bottom of a JSONL file afterwards.
    let pf = platform::gather(&db, assert_target);
    let cls = platform::classify(&pf);
    eprintln!("evidence status : {}", cls.status);
    for b in &cls.blockers {
        eprintln!("  not target    : {b}");
    }
    if !cls.status.is_citable() {
        eprintln!(
            "  -> useful for development and regression; NOT ratification \
evidence for ADR-0041, and the records say so."
        );
    }

    let budget_bytes = budget_gib * 1024.0 * 1024.0 * 1024.0;
    let mut lines: Vec<String> = Vec::new();
    let mut measured: Vec<Growth> = Vec::new();

    for f in [Fill::Lean, Fill::Populated] {
        eprintln!("measuring fill={} events={events} ...", f.as_str());
        match growth(&db, f, events, entities, seed, payload_bytes) {
            Ok(g) => {
                let rec = facts(Rec::new(), seed, &cls, &pf)
                    .s("record", "growth")
                    .s("fill", g.fill.as_str())
                    .s("fill_describes", g.fill.describe())
                    .i("events", g.events as i64)
                    .i("entities", entities as i64)
                    .i("payload_bytes", g.payload_bytes as i64)
                    .i("log_only_bytes", g.log_only_bytes as i64)
                    .f("bytes_per_event", g.bytes_per_event)
                    .i("with_projections_bytes", g.with_projections_bytes as i64)
                    .f(
                        "bytes_per_event_with_projections",
                        g.bytes_per_event_with_projections,
                    )
                    .i("projected_rows", g.projected_rows)
                    .f("fold_elapsed_s", g.fold_elapsed_s)
                    .i("page_size", g.page_size)
                    .i("page_count", g.page_count)
                    .f("append_elapsed_s", g.elapsed_s)
                    .f("assumed_events_per_day", events_per_day)
                    .f("budget_gib", budget_gib)
                    .f(
                        "days_to_fill_budget",
                        days_to_fill(g.bytes_per_event, events_per_day, budget_bytes),
                    )
                    // The horizon OQ2's budget is actually derived from. D-2
                    // used the with-projections figure; comparing a log-only
                    // number against it is what made D-20 optimistic.
                    .f(
                        "days_to_fill_budget_with_projections",
                        days_to_fill(
                            g.bytes_per_event_with_projections,
                            events_per_day,
                            budget_bytes,
                        ),
                    );
                println!("{}", rec.clone_line(&mut lines));
                measured.push(g);
            }
            Err(e) => {
                let rec = facts(Rec::new(), seed, &cls, &pf)
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
            let rec = facts(Rec::new(), seed, &cls, &pf)
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

    /// Test databases keyed by PID *and* test name. Rust runs tests in
    /// parallel threads of ONE process, so a PID-only path is shared by every
    /// test that uses it — and `growth` starts by deleting the file it is
    /// about to write.
    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "wm2-growth-test-{}-{}.sqlite",
            name,
            std::process::id()
        ))
    }

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
        let p = tmp("band");
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

    /// A zero-event run must be refused, not reported. `events.max(1)` would
    /// have divided the fixed schema overhead by one and emitted it as a
    /// bytes/event figure — a real number about nothing.
    #[test]
    fn a_degenerate_run_is_refused_rather_than_reported() {
        let p = tmp("degenerate");
        assert!(growth(&p, Fill::Lean, 0, 50, 7, 96).is_err(), "0 events");
        assert!(growth(&p, Fill::Lean, 10, 0, 7, 96).is_err(), "0 entities");
        assert!(growth(&p, Fill::Lean, 10, 50, 7, 0).is_err(), "0 payload");
        remove_db(&p);
    }

    /// The emitter must refuse a value JSON cannot represent. Rust prints
    /// infinity as `inf`, which parses as nothing — so a record carrying one
    /// would silently produce an evidence file no reader can load.
    #[test]
    #[should_panic(expected = "refusing to emit non-finite")]
    fn the_emitter_refuses_infinity() {
        let _ = Rec::new().f("days_to_fill_budget", f64::INFINITY).line();
    }

    #[test]
    #[should_panic(expected = "refusing to emit non-finite")]
    fn the_emitter_refuses_nan() {
        let _ = Rec::new().f("ratio", f64::NAN).line();
    }

    /// Everything the instrument actually emits stays finite for the
    /// parameters it validates — the assert above is a backstop, not the gate.
    #[test]
    fn validated_inputs_produce_only_finite_output() {
        let d = days_to_fill(612.18816, 864_000.0, 8.0 * 1024.0 * 1024.0 * 1024.0);
        assert!(d.is_finite() && d > 0.0);
        let l = Rec::new().f("days_to_fill_budget", d).line();
        assert!(!l.contains("inf") && !l.contains("NaN"), "{l}");
    }

    // -----------------------------------------------------------------
    // Evidence classification
    // -----------------------------------------------------------------

    fn host_facts() -> platform::PlatformFacts {
        platform::PlatformFacts {
            arch: "x86_64".into(),
            build_profile: "release".into(),
            db_fs_type: Some("ext4".into()),
            ..Default::default()
        }
    }

    fn jetson_facts() -> platform::PlatformFacts {
        platform::PlatformFacts {
            arch: "aarch64".into(),
            model: Some("NVIDIA Jetson Orin NX Engineering Reference Developer Kit".into()),
            tegra_release: Some("# R36 (release), REVISION: 3.0".into()),
            db_fs_type: Some("ext4".into()),
            db_fs_source: Some("/dev/nvme0n1p1".into()),
            build_profile: "release".into(),
            operator_asserted_target: true,
        }
    }

    /// **The gap this closes.** Every record must carry the classification, so
    /// a bundle's `TARGET-MEASURED` line stops being an operator's claim in a
    /// README and becomes something the instrument said — and could have
    /// refused to say.
    ///
    /// A guard rather than a formality: dropping these fields in a refactor
    /// would silently restore the un-attested state, and nothing else in this
    /// tool would notice, because the numbers would still be right.
    #[test]
    fn every_record_carries_the_evidence_classification() {
        let pf = host_facts();
        let cls = platform::classify(&pf);
        let line = facts(Rec::new(), 7, &cls, &pf).line();

        assert!(line.contains("\"evidence_status\":\"HOST-INDICATIVE-NOT-TARGET\""));
        assert!(line.contains("\"operator_asserted_target\":0"));
        // The facts the verdict rests on travel with it, or the verdict is
        // just another assertion.
        assert!(line.contains("\"db_fs_type\":\"ext4\""));
        assert!(
            line.contains("not_target_because") && line.contains("aarch64"),
            "the reason must be IN the record, not only on stderr: {line}"
        );
    }

    /// The citable case, so the test above cannot pass by the tool being
    /// incapable of ever emitting target evidence.
    #[test]
    fn a_corroborated_asserted_run_is_recorded_as_citable() {
        let pf = jetson_facts();
        let cls = platform::classify(&pf);
        assert!(cls.status.is_citable(), "{:?}", cls.blockers);

        let line = facts(Rec::new(), 7, &cls, &pf).line();
        assert!(line.contains("\"evidence_status\":\"JETSON-TARGET-MEASURED\""));
        assert!(line.contains("\"not_target_because\":\"\""));
        assert!(line.contains("\"operator_asserted_target\":1"));
        assert!(line.contains("/dev/nvme0n1p1"));
    }

    /// The token is the SAME token the harness emits, because both tools run
    /// the same classifier over the same facts. That is the point of sharing
    /// the module rather than copying it: two bundles carrying
    /// `JETSON-TARGET-MEASURED` now mean it in one sense, not two.
    #[test]
    fn the_token_is_the_harnesss_token() {
        assert_eq!(
            platform::EvidenceStatus::JetsonTargetMeasured.token(),
            "JETSON-TARGET-MEASURED"
        );
        assert_eq!(
            platform::EvidenceStatus::HostIndicativeNotTarget.token(),
            "HOST-INDICATIVE-NOT-TARGET"
        );
    }
}
