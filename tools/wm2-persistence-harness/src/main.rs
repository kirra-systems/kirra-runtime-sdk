//! WM-2 persistence harness — the measurement instrument for ADR-0041.
//!
//! ADR-0041 proposes a SQLite append-only event log with materialized
//! projections for Kirra World, and then refuses to ratify it: acceptance is
//! measurement-gated on target hardware. This binary produces those
//! measurements and, just as importantly, refuses to let a development-host run
//! be mistaken for one.
//!
//! **It is not the Kirra World store.** See the manifest for why lifting code
//! out of it is a defect rather than a shortcut.
//!
//! ```text
//! cargo run --release -- platform
//! cargo run --release -- all --db /var/lib/kirra/wm2-bench.sqlite --assert-target
//! cargo run --release -- all --out results.jsonl
//! ```

mod bench;
mod compact;
mod crash;
mod gen;
mod json;
mod platform;
mod pressure;
mod sha256;
mod stall;
mod standin;
mod sweep;

use json::Obj;
use platform::EvidenceStatus;
use standin::Durability;
use std::io::Write;
use std::path::PathBuf;

const USAGE: &str = "\
wm2-persistence-harness — ADR-0041 (WM-2) measurement harness

USAGE:
    wm2-persistence-harness <command> [options]

COMMANDS:
    platform    Classify this machine; print whether a run here would be citable
    append      Append throughput and commit latency, per durability setting
    replay      Projection rebuild: cold vs checkpointed, plus determinism
    query       One measurement per blueprint §12 query family
    growth      Bytes per event and a storage projection
    migrate     Schema migration against a populated store; fail-closed check
    crash       Corruption / restart experiment (tiers A and B; C is manual)
    compact     Compaction-with-citation experiment (ADR-0041 §11.3)
    pressure    Disk-pressure behaviour + reclamation cost (ADR-0041 §SQLite config)
    sweep       Scale ladder across entity counts; classifies the growth curve
                against ADR-0041's own reopening condition (drill §9)
    stall       Repeat one append configuration and hunt the ~29 s write stall,
                sampling system counters across it (ADR-0041 open question 9)
    all         Every command above, into one result stream

  Tier C (manual, needs a power switch — see the drill §8):
    powercut arm     Fsync a known prefix, record it, then append until the
                     power is cut. Does not return.
    powercut verify  After reboot: check the fsynced prefix survived and the
                     chain is intact; append the result to the trials ledger.

OPTIONS:
    --db <path>              Working database (default: a temp file)
    --out <path>             Append JSONL results here (default: stdout)
    --events <n>             Event volume (default: 100000)
    --entities <n>           Distinct entities (default: 1000)
    --batch <n>              Events per transaction (default: 1)
    --durability <f|n|off>   full | normal | off (default: all three for append)
    --payload-bytes <n>      Event payload width (default: 96)
    --repeats <n>            Query repetitions per family (default: 200)
    --seed <n>               Generator seed (default: 20260803)
    --events-per-day <n>     For the growth projection (default: 864000 = 10 Hz)
    --budget-gib <n>         Storage budget for the projection (default: 8)
    --crash-run-ms <n>       How long the crash child runs before SIGKILL (default: 750)
    --trial <n>              Power-cut trial number, for `powercut verify`
    --entity-ladder <a,b,c>  Entity counts for `sweep` (default: 1000,10000,100000)
    --events-per-entity <n>  Hold density CONSTANT across the ladder: each rung
                             uses entities x n events (default: 100). Set 0 to
                             use a fixed --events instead, which DILUTES the
                             ladder and cannot answer the scale question
    --sweep-family <name>    Family to classify: graph | temporal (default: graph)
    --migration-sql <which>  Backfill statement for `migrate`: legacy | grouped
                             (default: legacy). `legacy` is the correlated
                             subquery ADR-0041 D-6 measured, kept so that result
                             stays reproducible; it costs O(events x entities).
                             `grouped` is the single-pass rewrite, O(events).
                             Both are emitted with a `migration_sql` field, so a
                             record can never be mistaken for the other
    --stall-repeats <n>      Repetitions for `stall` (default: 20)
    --assert-target          Operator asserts this is target hardware under
                             representative storage, power and thermal conditions

A run is citable in ADR-0041's ratification checklist only when the harness
reports JETSON-TARGET-MEASURED. Everything else is HOST-INDICATIVE-NOT-TARGET
and is useful for development and regression only.
";

/// Which backfill statement `migrate` applies.
///
/// Selectable rather than replaced. The legacy form is what ADR-0041 D-6
/// measured on target, and deleting it would orphan an archived result; the
/// grouped form is what a migration should look like. Keeping both lets the
/// same ladder measure each, and the emitted `migration_sql` field means a
/// record can never be mistaken for the other.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MigrationSql {
    Legacy,
    Grouped,
}

impl MigrationSql {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "legacy" => Some(Self::Legacy),
            "grouped" => Some(Self::Grouped),
            _ => None,
        }
    }

    fn token(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Grouped => "grouped",
        }
    }

    fn step(self) -> &'static str {
        match self {
            Self::Legacy => standin::SCHEMA_V2_STEP,
            Self::Grouped => standin::SCHEMA_V2_STEP_GROUPED,
        }
    }
}

struct Args {
    command: String,
    db: PathBuf,
    out: Option<PathBuf>,
    events: u64,
    entities: u64,
    batch: usize,
    durability: Option<Durability>,
    payload_bytes: usize,
    repeats: usize,
    seed: u64,
    events_per_day: f64,
    budget_gib: f64,
    crash_run_ms: u64,
    trial: u64,
    entity_ladder: Vec<u64>,
    events_per_entity: u64,
    sweep_family: String,
    stall_repeats: usize,
    migration_sql: MigrationSql,
    /// Positional word after the command, used only by `powercut arm|verify`.
    subcommand: Option<String>,
    assert_target: bool,
}

fn parse_args() -> Result<Args, String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() || raw[0] == "-h" || raw[0] == "--help" {
        return Err(USAGE.to_string());
    }

    let mut default_db = std::env::temp_dir();
    default_db.push("wm2-persistence-harness.sqlite");

    let mut a = Args {
        command: raw[0].clone(),
        db: default_db,
        out: None,
        events: 100_000,
        entities: 1_000,
        batch: 1,
        durability: None,
        payload_bytes: gen::DEFAULT_PAYLOAD_BYTES,
        repeats: 200,
        seed: 20_260_803,
        events_per_day: 864_000.0,
        budget_gib: 8.0,
        crash_run_ms: 750,
        trial: 0,
        entity_ladder: vec![1_000, 10_000, 100_000],
        events_per_entity: 100,
        sweep_family: "graph".into(),
        stall_repeats: 20,
        migration_sql: MigrationSql::Legacy,
        subcommand: None,
        assert_target: false,
    };

    // A single positional word may follow `powercut`, and only `powercut`.
    // Anywhere else it is a typo, and swallowing it silently would run a
    // different experiment than the one that was asked for.
    let mut i = 1;
    if let Some(word) = raw.get(1) {
        if !word.starts_with("--") {
            if a.command != "powercut" {
                return Err(format!(
                    "unexpected argument `{word}` after `{}`\n\n{USAGE}",
                    a.command
                ));
            }
            a.subcommand = Some(word.clone());
            i = 2;
        }
    }
    while i < raw.len() {
        let flag = raw[i].as_str();
        // Every flag but --assert-target takes a value; missing it is a usage
        // error rather than a silent default, because a typo that silently
        // reverts to the default volume produces a real number for the wrong
        // experiment.
        let mut value = |name: &str| -> Result<String, String> {
            i += 1;
            raw.get(i)
                .cloned()
                .ok_or_else(|| format!("{name} requires a value"))
        };
        match flag {
            "--assert-target" => a.assert_target = true,
            "--migration-sql" => {
                let v = value(flag)?;
                a.migration_sql = MigrationSql::parse(&v)
                    .ok_or_else(|| format!("--migration-sql must be legacy|grouped (got `{v}`)"))?;
            }
            "--db" => a.db = PathBuf::from(value(flag)?),
            "--out" => a.out = Some(PathBuf::from(value(flag)?)),
            "--events" => a.events = parse_num(&value(flag)?, flag)?,
            "--entities" => a.entities = parse_num(&value(flag)?, flag)?,
            "--batch" => a.batch = parse_num(&value(flag)?, flag)? as usize,
            "--payload-bytes" => a.payload_bytes = parse_num(&value(flag)?, flag)? as usize,
            "--repeats" => a.repeats = parse_num(&value(flag)?, flag)? as usize,
            "--seed" => a.seed = parse_num(&value(flag)?, flag)?,
            "--crash-run-ms" => a.crash_run_ms = parse_num(&value(flag)?, flag)?,
            "--trial" => a.trial = parse_num(&value(flag)?, flag)?,
            "--stall-repeats" => a.stall_repeats = parse_num(&value(flag)?, flag)? as usize,
            "--events-per-entity" => a.events_per_entity = parse_num(&value(flag)?, flag)?,
            "--sweep-family" => {
                let v = value(flag)?;
                if v != "graph" && v != "temporal" {
                    return Err(format!(
                        "--sweep-family expects `graph` or `temporal`, got `{v}`"
                    ));
                }
                a.sweep_family = v;
            }
            "--entity-ladder" => {
                let v = value(flag)?;
                let mut out = Vec::new();
                for part in v.split(',') {
                    let part = part.trim();
                    if part.is_empty() {
                        continue;
                    }
                    out.push(parse_num(part, "--entity-ladder")?);
                }
                // A ladder shorter than the classifier's minimum cannot yield a
                // verdict, so reject it here rather than running a long sweep
                // that is guaranteed to report INSUFFICIENT.
                if out.len() < sweep::MIN_SWEEP_POINTS {
                    return Err(format!(
                        "--entity-ladder needs at least {} values (got {}): two points define \
                         a slope, and detecting a CHANGE in slope needs three",
                        sweep::MIN_SWEEP_POINTS,
                        out.len()
                    ));
                }
                a.entity_ladder = out;
            }
            "--events-per-day" => a.events_per_day = parse_float(&value(flag)?, flag)?,
            "--budget-gib" => a.budget_gib = parse_float(&value(flag)?, flag)?,
            "--durability" => {
                let v = value(flag)?;
                a.durability =
                    Some(Durability::parse(&v).ok_or_else(|| format!("unknown durability `{v}`"))?);
            }
            other => return Err(format!("unknown option `{other}`\n\n{USAGE}")),
        }
        i += 1;
    }
    Ok(a)
}

fn parse_num(s: &str, flag: &str) -> Result<u64, String> {
    s.parse()
        .map_err(|_| format!("{flag} expects a non-negative integer, got `{s}`"))
}

fn parse_float(s: &str, flag: &str) -> Result<f64, String> {
    s.parse()
        .map_err(|_| format!("{flag} expects a number, got `{s}`"))
}

struct Sink {
    out: Box<dyn Write>,
}

impl Sink {
    fn open(path: Option<&PathBuf>) -> std::io::Result<Self> {
        let out: Box<dyn Write> = match path {
            Some(p) => Box::new(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)?,
            ),
            None => Box::new(std::io::stdout()),
        };
        Ok(Self { out })
    }

    /// Flush after every record. A harness that dies during the crash tier must
    /// leave its earlier measurements on disk — buffered output would take them
    /// with it, which is a poor property for the tool whose job is surviving
    /// unexpected process death.
    fn emit(&mut self, record: Obj) {
        let _ = writeln!(self.out, "{}", record.render());
        let _ = self.out.flush();
    }
}

/// Fields stamped on every record: the provenance that makes a number
/// interpretable six months from now.
fn stamp(class: &platform::Classification, facts: &platform::PlatformFacts, a: &Args) -> Obj {
    Obj::new()
        .str("evidence_status", class.status.token())
        .str("standin_schema_digest", &standin::schema_digest())
        .str("harness_version", env!("CARGO_PKG_VERSION"))
        .str("sqlite_version", rusqlite::version())
        .str("build_profile", &facts.build_profile)
        // Build identity: which binary produced this number. `build_profile`
        // says release-or-debug; these say *which* release. Captured in
        // build.rs — see its header for why a dirty tree is labelled rather
        // than rejected.
        .str("rustc_version", env!("WM2_RUSTC_VERSION"))
        .str("git_commit", env!("WM2_GIT_COMMIT"))
        .str("source_digest", env!("WM2_SOURCE_DIGEST"))
        .str("arch", &facts.arch)
        .opt_str("device_model", facts.model.as_deref())
        .opt_str("db_fs_type", facts.db_fs_type.as_deref())
        .opt_str("db_fs_source", facts.db_fs_source.as_deref())
        .int("seed", a.seed)
}

fn durabilities(a: &Args) -> Vec<Durability> {
    match a.durability {
        Some(d) => vec![d],
        None => Durability::all().to_vec(),
    }
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(if msg.starts_with("wm2-") { 0 } else { 2 });
        }
    };

    // The child half of the crash drill: no reporting, no classification, just
    // append until killed.
    if args.command == "crash-child" {
        crash::crash_child(&args.db, args.durability.unwrap_or(Durability::Full));
    }

    // Tier C. Deliberately outside the reporting pipeline: `arm` never returns
    // (the operator ends it with the power switch) and `verify` runs after a
    // reboot, so neither fits the run/stage shape the other commands share.
    if args.command == "powercut" {
        if let Some(parent) = args.db.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match args.subcommand.as_deref() {
            Some("arm") => {
                crash::powercut_arm(&args.db, 400, args.entities, args.seed);
            }
            Some("verify") => {
                if args.trial == 0 {
                    eprintln!(
                        "powercut verify requires --trial <n> (1-based). The number is \
                         recorded in the ledger so trials can be counted and re-run \
                         individually."
                    );
                    std::process::exit(2);
                }
                let rec = match crash::powercut_verify(&args.db, args.trial) {
                    Ok(r) => r,
                    Err(e) => {
                        // A refused verification is not a failed measurement —
                        // it is the harness declining to record something that
                        // would not mean what it appears to mean.
                        eprintln!("powercut verify refused: {e}");
                        std::process::exit(2);
                    }
                };
                println!(
                    "trial {} [arm {}]: {} — {}",
                    rec.trial,
                    rec.arm_id,
                    rec.outcome.token(),
                    rec.outcome.detail()
                );
                let all = crash::read_trials(&args.db);
                let agg = crash::tier_c_from_trials(&all);
                // Distinct armings, not ledger rows: a trial is an arming that a
                // power cut was applied to, and quoting the row count beside a
                // verdict that counted armings would contradict it.
                println!(
                    "\ntier C after {} arming(s) across {} row(s): {} — {}",
                    crash::distinct_armings(&all),
                    all.len(),
                    agg.token(),
                    agg.detail()
                );
                // A failed or still-insufficient tier C exits non-zero, for the
                // same reason every other unusable measurement does.
                std::process::exit(i32::from(agg.fails_the_run()));
            }
            other => {
                eprintln!(
                    "powercut needs a subcommand: `arm` or `verify` (got {})",
                    other.unwrap_or("nothing")
                );
                std::process::exit(2);
            }
        }
    }

    if let Some(parent) = args.db.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let facts = platform::gather(&args.db, args.assert_target);
    let class = platform::classify(&facts);

    banner(&class, &args);

    let mut sink = match Sink::open(args.out.as_ref()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open --out: {e}");
            std::process::exit(2);
        }
    };

    sink.emit(
        stamp(&class, &facts, &args)
            .str("record", "run")
            .str("command", &args.command)
            .int("events", args.events)
            .int("entities", args.entities)
            .int("payload_bytes", args.payload_bytes as u64)
            .bool("citable", class.status.is_citable())
            .str_array("blockers", &class.blockers),
    );

    let run = |name: &str| args.command == name || args.command == "all";
    let mut failures = 0usize;

    if args.command == "platform" {
        // The banner is the whole output; the run record above carries it in
        // machine-readable form.
        return;
    }

    if run("append") {
        for d in durabilities(&args) {
            for batch in dedup(vec![args.batch, 64]) {
                match bench::append(&args.db, d, args.events, args.entities, batch, args.seed) {
                    Ok(r) => sink.emit(
                        stamp(&class, &facts, &args)
                            .str("record", "append")
                            // From the store, not the request: this is the
                            // setting SQLite actually ran under.
                            .str("durability", r.durability.pragma())
                            .int("batch", r.batch as u64)
                            .int("events", r.events)
                            .float("events_per_second", r.events_per_second)
                            .float("hash_share_percent", r.hash_share_percent)
                            .obj("timing", r.timing.to_json()),
                    ),
                    Err(e) => {
                        failures += 1;
                        sink.emit(error_record(&class, &facts, &args, "append", &e));
                    }
                }
            }
        }
    }

    if run("replay") {
        let d = args.durability.unwrap_or(Durability::Normal);
        match bench::replay(&args.db, d, args.events, args.entities, args.seed, 0.9) {
            Ok(r) => {
                if !r.deterministic {
                    failures += 1;
                }
                sink.emit(
                    stamp(&class, &facts, &args)
                        .str("record", "replay")
                        .str("durability", d.pragma())
                        .int("events", r.events)
                        .int("checkpoint_at_generation", r.checkpoint_at)
                        .int("tail_events", r.tail_events)
                        .bool("deterministic", r.deterministic)
                        .obj("cold_rebuild", r.cold_rebuild.to_json())
                        .obj("checkpointed_resume", r.checkpointed_resume.to_json()),
                );
            }
            Err(e) => {
                failures += 1;
                sink.emit(error_record(&class, &facts, &args, "replay", &e));
            }
        }
    }

    if run("query") {
        let d = args.durability.unwrap_or(Durability::Normal);
        match bench::queries(
            &args.db,
            d,
            args.events,
            args.entities,
            args.seed,
            args.repeats,
        ) {
            Ok(r) => {
                for t in &r.timings {
                    let rows = r
                        .rows_returned
                        .iter()
                        .find(|(n, _)| *n == t.label)
                        .map(|(_, v)| *v)
                        .unwrap_or(0);
                    if rows == 0 {
                        // A family that matched nothing is a broken benchmark,
                        // not a fast one.
                        failures += 1;
                    }
                    sink.emit(
                        stamp(&class, &facts, &args)
                            .str("record", "query")
                            .str("family", &t.label)
                            .int("rows_matched_total", rows)
                            .obj("timing", t.to_json()),
                    );
                }
            }
            Err(e) => {
                failures += 1;
                sink.emit(error_record(&class, &facts, &args, "query", &e));
            }
        }
    }

    if run("growth") {
        let d = args.durability.unwrap_or(Durability::Normal);
        match bench::growth(
            &args.db,
            d,
            args.events,
            args.entities,
            args.seed,
            args.payload_bytes,
        ) {
            Ok(r) => {
                let budget = args.budget_gib * 1024.0 * 1024.0 * 1024.0;
                sink.emit(
                    stamp(&class, &facts, &args)
                        .str("record", "growth")
                        .int("events", r.events)
                        .int("payload_bytes", r.payload_bytes as u64)
                        .int("log_only_bytes", r.log_only_bytes)
                        .int("with_projections_bytes", r.with_projections_bytes)
                        .float("bytes_per_event", r.bytes_per_event)
                        .float(
                            "bytes_per_event_with_projections",
                            r.bytes_per_event_with_projections,
                        )
                        .float("assumed_events_per_day", args.events_per_day)
                        .float("budget_gib", args.budget_gib)
                        .float(
                            "days_to_fill_budget",
                            bench::days_to_fill(r.bytes_per_event, args.events_per_day, budget),
                        ),
                );
            }
            Err(e) => {
                failures += 1;
                sink.emit(error_record(&class, &facts, &args, "growth", &e));
            }
        }
    }

    if run("migrate") {
        let d = args.durability.unwrap_or(Durability::Normal);
        match bench::migration(
            &args.db,
            d,
            args.events.min(50_000),
            args.entities,
            args.seed,
            args.migration_sql.step(),
        ) {
            Ok(r) => {
                if !r.future_schema_refused || !r.chain_intact_after {
                    failures += 1;
                }
                sink.emit(
                    stamp(&class, &facts, &args)
                        .str("record", "migrate")
                        .str("migration_sql", args.migration_sql.token())
                        .int("events", r.events)
                        // Self-describing on purpose. `migrate`'s cost depends
                        // on BOTH axes (ADR-0041 D-13), so a record carrying
                        // only `events` cannot be placed on the ladder without
                        // pairing it against the preceding `run` record by
                        // position. Reconstructing an axis from record ORDER is
                        // fragile — a reordered, filtered or partially copied
                        // file silently yields the wrong ladder — so the field
                        // that determines the cost travels with the measurement.
                        .int("entities", args.entities)
                        .int("version_after", r.version_after as u64)
                        .bool("future_schema_refused", r.future_schema_refused)
                        .bool("chain_intact_after", r.chain_intact_after)
                        .obj("timing", r.apply.to_json()),
                );
            }
            Err(e) => {
                failures += 1;
                sink.emit(error_record(&class, &facts, &args, "migrate", &e));
            }
        }
    }

    if run("compact") {
        let d = args.durability.unwrap_or(Durability::Normal);
        // A fixed transaction timestamp: the generator's clock is synthetic, so
        // reading the wall clock here would make the experiment irreproducible
        // from its recorded seed for no benefit.
        let now_ms = 1_700_000_000_000i64 + (args.events as i64) * 50 + 1_000_000;
        match compact::run(&args.db, d, args.events, args.entities, args.seed, now_ms) {
            Ok(r) => {
                let checks = [
                    ("citation_digest_reverifies", r.citation_digest_reverifies),
                    ("protected_window_refused", r.protected_window_refused),
                    ("protected_events_survived", r.protected_events_survived),
                    ("chain_intact_across_window", r.chain_intact_across_window),
                    (
                        "query_into_window_is_degraded",
                        r.query_into_window_is_degraded,
                    ),
                    (
                        "query_outside_window_is_unknown",
                        r.query_outside_window_is_unknown,
                    ),
                    (
                        "redaction_keeps_chain_intact",
                        r.redaction_keeps_chain_intact,
                    ),
                    (
                        "tampered_summary_breaks_chain",
                        r.tampered_summary_breaks_chain,
                    ),
                    ("all_windows_reported", r.all_windows_reported),
                ];
                let mut record = stamp(&class, &facts, &args)
                    .str("record", "compact")
                    .int("events_before", r.events_before)
                    .int("events_after", r.events_after)
                    .int("compacted", r.compacted)
                    .int("bytes_before", r.bytes_before)
                    .int("bytes_after", r.bytes_after)
                    .int("bytes_after_vacuum", r.bytes_after_vacuum)
                    // Cast to f64 BEFORE subtracting. On u64 this underflows
                    // when a vacuum leaves the file larger, and the wrapped
                    // value renders as a plausible-looking percentage in the
                    // hundreds of trillions rather than as a negative number.
                    .float(
                        "bytes_reclaimed_percent",
                        if r.bytes_before > 0 {
                            100.0 * (r.bytes_before as f64 - r.bytes_after_vacuum as f64)
                                / r.bytes_before as f64
                        } else {
                            f64::NAN
                        },
                    )
                    .float("compact_ms", r.compact_ms)
                    .float("verify_after_ms", r.verify_ms)
                    .int("compacted_windows_reported", r.compacted_windows_reported)
                    .obj(
                        "citation",
                        Obj::new()
                            .int("summary_generation", r.citation.summary_generation)
                            .int("lo_generation", r.citation.lo)
                            .int("hi_generation", r.citation.hi)
                            .int("event_count", r.citation.event_count)
                            .str("range_digest", &r.citation.range_digest)
                            .str("chain_before", &r.citation.chain_before)
                            .str("chain_after", &r.citation.chain_after),
                    );
                for (name, ok) in checks {
                    if !ok {
                        failures += 1;
                    }
                    record = record.bool(name, ok);
                }
                sink.emit(record);
            }
            Err(e) => {
                failures += 1;
                sink.emit(error_record(&class, &facts, &args, "compact", &e));
            }
        }
    }

    if run("pressure") {
        let d = args.durability.unwrap_or(Durability::Normal);
        match pressure::run(&args.db, d, args.seed) {
            Ok(r) => {
                let checks = [
                    ("write_refused", r.write_refused),
                    ("refusal_is_disk_full", r.refusal_is_disk_full),
                    ("partial_batch_rolled_back", r.partial_batch_rolled_back),
                    ("chain_intact_after_refusal", r.chain_intact_after_refusal),
                    ("reads_serve_while_full", r.reads_serve_while_full),
                    ("recovers_when_space_returns", r.recovers_when_space_returns),
                    ("chain_intact_after_recovery", r.chain_intact_after_recovery),
                ];
                let mut record = stamp(&class, &facts, &args)
                    .str("record", "pressure")
                    .int("events_before_full", r.events_before_full)
                    .int("page_cap", r.page_cap.max(0) as u64)
                    .int("projection_rows_while_full", r.projection_rows_while_full)
                    .str("refusal_message", &r.refusal_message);
                for (name, ok) in checks {
                    if !ok {
                        failures += 1;
                    }
                    record = record.bool(name, ok);
                }
                sink.emit(record);
            }
            Err(e) => {
                failures += 1;
                sink.emit(error_record(&class, &facts, &args, "pressure", &e));
            }
        }

        // Reclamation cost, measured on a store with something to reclaim —
        // the number a maintenance scheduler needs, separate from compaction.
        match reclaim_measurement(&args, args.durability.unwrap_or(Durability::Normal)) {
            Ok(r) => sink.emit(
                stamp(&class, &facts, &args)
                    .str("record", "reclaim")
                    .float("vacuum_ms", r.vacuum_ms)
                    .int("bytes_before", r.bytes_before)
                    .int("bytes_after", r.bytes_after)
                    .float("bytes_freed", r.bytes_freed as f64)
                    .float("bytes_per_ms", r.bytes_per_ms)
                    // Transient cost. `transient_overhead_bytes` — not
                    // `bytes_freed` — is what a minimum-free-space reserve has
                    // to be set from: VACUUM materialises a second copy before
                    // it can shrink anything.
                    .int("peak_bytes_during", r.peak_bytes_during)
                    .float(
                        "transient_overhead_bytes",
                        r.transient_overhead_bytes as f64,
                    )
                    .float("transient_overhead_ratio", r.transient_overhead_ratio)
                    .int("footprint_samples", r.footprint_samples)
                    // Where the second copy lands. A volatile temp filesystem
                    // means reclamation runs through RAM.
                    .str("temp_dir", &r.temp_dir)
                    .opt_str("temp_dir_fs_type", r.temp_dir_fs_type.as_deref())
                    .bool("temp_on_same_fs_as_db", r.temp_on_same_fs_as_db)
                    .bool("temp_fs_is_volatile", r.temp_fs_is_volatile)
                    // Availability: what a robot cannot do while this runs.
                    .int("concurrent_append_attempts", r.concurrent_append_attempts)
                    .int("concurrent_appends_blocked", r.concurrent_appends_blocked)
                    .float("max_append_stall_ms", r.max_append_stall_ms),
            ),
            Err(e) => {
                failures += 1;
                sink.emit(error_record(&class, &facts, &args, "reclaim", &e));
            }
        }
    }

    // `sweep` and `stall` are explicit-only, NOT part of `all`. The sweep runs
    // the full query suite once per ladder rung and the stall repeats an append
    // --stall-repeats times; folding either into the routine run would multiply
    // every drill by a large constant and pressure people into skipping `all`.
    if args.command == "sweep" {
        // ADR-0041's reopening condition is about a CHANGE in exponent, so the
        // ladder is measured end to end and classified as a whole. Emitting the
        // points without the verdict would put the judgement back where it was:
        // in whoever reads the file.
        let d = args.durability.unwrap_or(Durability::Normal);
        let mut points: Vec<sweep::SweepPoint> = Vec::new();
        let mut ladder_ok = true;

        for &entities in &args.entity_ladder {
            // Constant density by default. Sweeping entity count at a FIXED
            // total event count makes each entity thinner, so fan-out and cost
            // FALL — which measures density, not scale, and cannot answer the
            // reopening condition. `--events-per-entity 0` opts back into the
            // fixed-events ladder; the classifier refuses the result.
            let events = if args.events_per_entity > 0 {
                entities.saturating_mul(args.events_per_entity)
            } else {
                args.events
            };
            match bench::queries(&args.db, d, events, entities, args.seed, args.repeats) {
                Ok(q) => {
                    let want = if args.sweep_family == "graph" {
                        "graph_bounded_reach"
                    } else {
                        "temporal_changes_since"
                    };
                    let t = q.timings.iter().find(|t| t.label == want);
                    let rows = q
                        .rows_returned
                        .iter()
                        .find(|(l, _)| l == want)
                        .map(|(_, n)| *n)
                        .unwrap_or(0);
                    match t {
                        Some(t) => {
                            points.push(sweep::SweepPoint {
                                entities,
                                median_us: t.median_us,
                            });
                            sink.emit(
                                stamp(&class, &facts, &args)
                                    .str("record", "sweep_point")
                                    .str("family", want)
                                    .int("entities", entities)
                                    .int("events", events)
                                    .int("events_per_entity", args.events_per_entity)
                                    .float("median_us", t.median_us)
                                    .float("p99_us", t.p99_us)
                                    .int("rows_matched_total", rows),
                            );
                        }
                        None => {
                            ladder_ok = false;
                            failures += 1;
                            sink.emit(error_record(
                                &class,
                                &facts,
                                &args,
                                "sweep_point",
                                &format!("family `{want}` missing from the query result"),
                            ));
                        }
                    }
                }
                Err(e) => {
                    ladder_ok = false;
                    failures += 1;
                    sink.emit(error_record(&class, &facts, &args, "sweep_point", &e));
                }
            }
        }

        // A ladder with a hole cannot be classified: the missing point might be
        // the knee. Refuse rather than classifying the survivors.
        let verdict = if ladder_ok {
            sweep::classify_scaling(&points)
        } else {
            sweep::ScalingVerdict::Insufficient(
                "one or more ladder points failed to measure; the missing point may be the \
                 knee, so the survivors are not classifiable"
                    .into(),
            )
        };
        // Only count the verdict when the ladder itself measured cleanly. A
        // broken rung already incremented `failures`, and the resulting
        // INSUFFICIENT verdict is a CONSEQUENCE of that same problem — counting
        // both inflates the "N measurement(s) failed" summary and makes one
        // fault look like two. The exit code is unaffected either way, because
        // the rung failure alone already fails the run.
        if ladder_ok && verdict.fails_the_run() {
            failures += 1;
        }
        sink.emit(
            stamp(&class, &facts, &args)
                .str("record", "sweep_summary")
                .str(
                    "family",
                    if args.sweep_family == "graph" {
                        "graph_bounded_reach"
                    } else {
                        "temporal_changes_since"
                    },
                )
                .int("points", points.len() as u64)
                .int("min_points_required", sweep::MIN_SWEEP_POINTS as u64)
                .str("verdict", verdict.token())
                .bool("supports_option_a", verdict.supports_option_a())
                .str("detail", verdict.detail()),
        );
    }

    if args.command == "stall" {
        // Deliberately NOT part of `all`: it is a targeted investigation of one
        // configuration, and folding it into the standard run would multiply
        // every drill by --stall-repeats.
        let d = args.durability.unwrap_or(Durability::Normal);
        let r = stall::run(
            &args.db,
            d,
            args.events,
            args.entities,
            args.batch,
            args.seed,
            args.stall_repeats,
        );
        if r.fails_the_run() {
            failures += 1;
        }
        sink.emit(
            stamp(&class, &facts, &args)
                .str("record", "stall")
                .str("durability", d.pragma())
                .int("batch", args.batch as u64)
                .int("repeats", r.repeats as u64)
                .int("completed", r.completed as u64)
                .int("stalls_observed", r.stalls_observed as u64)
                .float("stall_rate", r.stall_rate())
                .float("stall_threshold_ms", stall::STALL_THRESHOLD_MS)
                .float("worst_commit_ms", r.worst_commit_ms)
                .float("median_worst_ms", r.median_worst_ms)
                .float("median_throughput_eps", r.median_throughput_eps())
                .str("attribution", r.attribution.token())
                .str("detail", r.attribution.detail())
                .float(
                    "psi_io_stall_us",
                    r.delta_at_worst
                        .psi_io_stall_us
                        .map_or(f64::NAN, |v| v as f64),
                )
                .float(
                    "disk_io_ms",
                    r.delta_at_worst.disk_io_ms.map_or(f64::NAN, |v| v as f64),
                )
                .float(
                    "peak_dirty_writeback_kb",
                    r.delta_at_worst
                        .peak_dirty_writeback_kb
                        .map_or(f64::NAN, |v| v as f64),
                )
                .float(
                    "thermal_max_mc",
                    r.delta_at_worst
                        .thermal_max_mc
                        .map_or(f64::NAN, |v| v as f64),
                ),
        );
    }

    if run("crash") {
        let d = args.durability.unwrap_or(Durability::Full);
        let r = crash::run(&args.db, d, args.crash_run_ms, 400, 400);
        for (tier, outcome) in [
            ("A_sigkill_crash_consistency", &r.tier_a_sigkill),
            ("B_wal_loss_prefix_validity", &r.tier_b_wal_loss),
            ("C_power_cut_durability", &r.tier_c_power_cut),
        ] {
            if outcome.fails_the_run() {
                failures += 1;
            }
            sink.emit(
                stamp(&class, &facts, &args)
                    .str("record", "crash")
                    .str("tier", tier)
                    .str("durability", d.pragma())
                    .str("outcome", outcome.token())
                    // Recorded manual power-cut trials found next to the
                    // database. Stamped on every tier so a reader of any one
                    // row can tell a genuine NOT-RUN from a drill that
                    // stopped short of the required count.
                    .int("tier_c_trials_recorded", r.tier_c_trials)
                    .int("tier_c_trials_required", crash::MIN_TIER_C_TRIALS)
                    .str("detail", outcome.detail()),
            );
        }
    }

    eprintln!();
    if failures > 0 {
        eprintln!("{failures} measurement(s) failed or were unusable — see the result stream.");
        std::process::exit(1);
    }
    eprintln!("Done. Results are {}.", class.status.token());
    if !class.status.is_citable() {
        eprintln!(
            "They may NOT be entered against ADR-0041's ratification checklist. \
             Run on the target device with --assert-target for that."
        );
    }
}

/// Populate a store, delete most of it, and time the reclamation.
///
/// A `VACUUM` on a store with nothing to reclaim measures the rewrite cost but
/// not the trade a scheduler is deciding, so the fixture deliberately creates
/// free pages first.
fn reclaim_measurement(a: &Args, d: Durability) -> Result<pressure::ReclaimResult, String> {
    let mut path = a.db.clone();
    path.set_extension("reclaim.sqlite");
    let _ = std::fs::remove_file(&path);

    let mut store = standin::Store::open(&path, d).map_err(|e| e.to_string())?;
    let events = a.events.min(50_000);
    for chunk in gen::events(0, events, a.entities, a.seed).chunks(512) {
        store.append_batch(chunk).map_err(|e| e.to_string())?;
    }
    store
        .conn
        .execute(
            "DELETE FROM world_events WHERE generation < ?1",
            rusqlite::params![(events as i64) * 3 / 4],
        )
        .map_err(|e| e.to_string())?;

    let result = pressure::reclaim(&store, &path);
    drop(store);
    let _ = std::fs::remove_file(&path);
    result
}

fn error_record(
    class: &platform::Classification,
    facts: &platform::PlatformFacts,
    a: &Args,
    what: &str,
    err: &str,
) -> Obj {
    stamp(class, facts, a)
        .str("record", "error")
        .str("stage", what)
        .str("error", err)
}

fn dedup(mut v: Vec<usize>) -> Vec<usize> {
    v.sort_unstable();
    v.dedup();
    v
}

fn banner(class: &platform::Classification, a: &Args) {
    eprintln!("wm2-persistence-harness — ADR-0041 (WM-2) measurement, NOT the Kirra World store");
    eprintln!("  database        : {}", a.db.display());
    eprintln!("  stand-in schema : {}", standin::schema_digest());
    eprintln!("  sqlite          : {}", rusqlite::version());
    eprintln!("  evidence status : {}", class.status.token());
    match class.status {
        EvidenceStatus::JetsonTargetMeasured => {
            eprintln!("  -> citable in ADR-0041's ratification checklist.");
        }
        EvidenceStatus::HostIndicativeNotTarget => {
            eprintln!("  -> NOT citable. Blocking reasons:");
            for b in &class.blockers {
                eprintln!("       - {b}");
            }
        }
    }
    eprintln!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_collapses_a_repeated_batch_size() {
        // `--batch 64` must not run the 64 case twice.
        assert_eq!(dedup(vec![64, 64]), vec![64]);
        assert_eq!(dedup(vec![1, 64]), vec![1, 64]);
    }

    #[test]
    fn stamp_carries_the_schema_digest_and_status() {
        let facts = platform::PlatformFacts {
            arch: "aarch64".into(),
            build_profile: "release".into(),
            ..Default::default()
        };
        let class = platform::classify(&facts);
        let args = Args {
            command: "all".into(),
            db: PathBuf::from("/tmp/x.sqlite"),
            out: None,
            events: 1,
            entities: 1,
            batch: 1,
            durability: None,
            payload_bytes: 96,
            repeats: 1,
            seed: 7,
            events_per_day: 1.0,
            budget_gib: 1.0,
            crash_run_ms: 1,
            trial: 0,
            entity_ladder: vec![1_000, 10_000, 100_000],
            events_per_entity: 100,
            sweep_family: "graph".into(),
            stall_repeats: 20,
            migration_sql: MigrationSql::Legacy,
            subcommand: None,
            assert_target: false,
        };
        let rendered = stamp(&class, &facts, &args).render();
        assert!(rendered.contains(&standin::schema_digest()));
        assert!(rendered.contains("HOST-INDICATIVE-NOT-TARGET"));
        assert!(rendered.contains(r#""seed":7"#));
    }

    #[test]
    fn parse_num_rejects_a_non_number_rather_than_defaulting() {
        assert!(parse_num("12x", "--events").is_err());
        assert_eq!(parse_num("12", "--events").unwrap(), 12);
    }

    #[test]
    fn durability_selection_defaults_to_all_three() {
        let mut a = Args {
            command: "append".into(),
            db: PathBuf::from("/tmp/x"),
            out: None,
            events: 1,
            entities: 1,
            batch: 1,
            durability: None,
            payload_bytes: 96,
            repeats: 1,
            seed: 1,
            events_per_day: 1.0,
            budget_gib: 1.0,
            crash_run_ms: 1,
            trial: 0,
            entity_ladder: vec![1_000, 10_000, 100_000],
            events_per_entity: 100,
            sweep_family: "graph".into(),
            stall_repeats: 20,
            migration_sql: MigrationSql::Legacy,
            subcommand: None,
            assert_target: false,
        };
        assert_eq!(durabilities(&a).len(), 3);
        a.durability = Some(Durability::Full);
        assert_eq!(durabilities(&a), vec![Durability::Full]);
    }
}
