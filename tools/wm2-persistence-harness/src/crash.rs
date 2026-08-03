//! The corruption / restart experiment — ADR-0041's fourth ratification gate,
//! "power-loss-class behaviour, in the spirit of the existing audit-chain
//! crash-consistency drill".
//!
//! That drill (`tests/audit_chain_prefix_on_kill.rs`) already established the
//! shape and, more usefully, established the *distinction* this module has to
//! preserve. There are three separate properties here and they are routinely
//! collapsed into one claim:
//!
//! | Tier | Property | How | Automatable |
//! |---|---|---|---|
//! | **A** | Crash consistency — a process death mid-append never forks or tears the chain | `SIGKILL` a real child mid-append | yes |
//! | **B** | Prefix validity — the main database file alone is always a valid prefix | snapshot the main file without its `-wal` | yes |
//! | **C** | Durability — a committed write survives an actual power cut | pull power from the device | **no** |
//!
//! # Why A does not imply C
//!
//! `SIGKILL` leaves the operating system's page cache intact. Everything the
//! process wrote is still on its way to disk and still arrives. Tier A
//! therefore proves the *protocol* is crash-consistent and proves nothing at
//! all about durability — which is exactly the confusion that makes "we tested
//! power loss" one of the least reliable sentences in storage engineering.
//!
//! # Why B is conservative rather than exact
//!
//! Tier B discards the entire WAL, which is *more* than a real power cut takes:
//! under `synchronous=FULL` the WAL is fsynced per commit and would survive.
//! Passing tier B is therefore a stronger result than power loss requires, and
//! the property it establishes is the useful one — the main file, alone, is
//! never a torn or forked state, only a shorter one.
//!
//! # Tier C cannot be faked and is not attempted
//!
//! Nothing in software distinguishes a filesystem that honoured `fsync` from
//! one that acknowledged it and buffered the write in a device cache. That is a
//! property of the eMMC or microSD in the actual robot, it is a common failure
//! on embedded storage, and the only instrument for it is a power switch. The
//! runbook (`docs/hardware/JETSON_WM2_PERSISTENCE_DRILL.md`) carries the manual
//! procedure; this module reports tier C as `NOT-RUN` so a report can never
//! imply it happened.

use crate::gen;
use crate::standin::{ChainStatus, Durability, Store};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TierOutcome {
    Pass(String),
    Fail(String),
    /// The experiment did not establish its precondition, so neither a pass nor
    /// a failure would mean anything. Reported distinctly because an
    /// inconclusive run silently counted as a pass is how a drill becomes
    /// decoration.
    Inconclusive(String),
    NotRun(String),
}

impl TierOutcome {
    pub fn token(&self) -> &'static str {
        match self {
            Self::Pass(_) => "PASS",
            Self::Fail(_) => "FAIL",
            Self::Inconclusive(_) => "INCONCLUSIVE",
            Self::NotRun(_) => "NOT-RUN",
        }
    }

    /// Whether this outcome should fail the overall run.
    ///
    /// `INCONCLUSIVE` counts, and that is the whole point of the variant. The
    /// experiment never established its precondition — the child was killed
    /// before committing, or the tail was checkpointed away so nothing was at
    /// risk — so neither a pass nor a failure would mean anything. Letting the
    /// run exit 0 anyway produces a results file that looks complete while a
    /// load-bearing gate is silently missing, which is exactly the "an
    /// inconclusive run silently counted as a pass is how a drill becomes
    /// decoration" failure the variant exists to prevent.
    ///
    /// `NOT-RUN` is exempt, and must be: tier C is ALWAYS `NOT-RUN` by
    /// construction (a power cut cannot be performed from software), so
    /// counting it would make every run exit 1 and the exit code would stop
    /// carrying information.
    pub fn fails_the_run(&self) -> bool {
        matches!(self, Self::Fail(_) | Self::Inconclusive(_))
    }

    pub fn detail(&self) -> &str {
        match self {
            Self::Pass(d) | Self::Fail(d) | Self::Inconclusive(d) | Self::NotRun(d) => d,
        }
    }
}

pub struct CrashResult {
    pub tier_a_sigkill: TierOutcome,
    pub tier_b_wal_loss: TierOutcome,
    pub tier_c_power_cut: TierOutcome,
    /// How many manual power-cut trials were found recorded next to the
    /// database. Reported alongside the outcome so a reader can tell a genuine
    /// `NOT-RUN` from a drill that stopped short.
    pub tier_c_trials: u64,
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut p = path.as_os_str().to_os_string();
    p.push(suffix);
    PathBuf::from(p)
}

fn remove_db(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(sidecar(path, "-wal"));
    let _ = std::fs::remove_file(sidecar(path, "-shm"));
}

// ---------------------------------------------------------------------------
// Tier A — SIGKILL mid-append
// ---------------------------------------------------------------------------

/// Re-exec this binary as an appending child, kill it mid-append, and check
/// what the log looks like on reopen.
///
/// The child is a real separate process rather than a thread on purpose: a
/// thread cannot be killed in a way that leaves the parent's SQLite connection
/// in the state a crash leaves it in, and simulating the crash inside the
/// process under test is how a crash-consistency drill ends up testing its own
/// simulation.
pub fn tier_a_sigkill(path: &Path, durability: Durability, run_ms: u64) -> TierOutcome {
    remove_db(path);
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return TierOutcome::Inconclusive(format!("cannot locate own executable: {e}")),
    };

    let mut child = match std::process::Command::new(exe)
        .arg("crash-child")
        .arg("--db")
        .arg(path)
        .arg("--durability")
        .arg(durability.pragma())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return TierOutcome::Inconclusive(format!("cannot spawn child: {e}")),
    };

    std::thread::sleep(Duration::from_millis(run_ms));
    // `kill()` is SIGKILL on Unix: uncatchable, no unwinding, no flush.
    let _ = child.kill();
    let _ = child.wait();

    let store = match Store::open(path, durability) {
        Ok(s) => s,
        Err(e) => return TierOutcome::Fail(format!("database unopenable after crash: {e}")),
    };
    let status = match store.verify_chain() {
        Ok(s) => s,
        Err(e) => return TierOutcome::Fail(format!("chain verification errored: {e}")),
    };

    match status {
        ChainStatus::Intact { entries, .. } if entries > 0 => TierOutcome::Pass(format!(
            "chain INTACT across {entries} entries after SIGKILL mid-append \
             (crash consistency; says nothing about durability — see tier C)"
        )),
        ChainStatus::Intact { .. } | ChainStatus::Empty => TierOutcome::Inconclusive(
            "the child was killed before it committed anything, so an intact empty \
             log proves nothing — raise --crash-run-ms"
                .to_string(),
        ),
        ChainStatus::Broken { at_generation } => {
            TierOutcome::Fail(format!("chain BROKEN at generation {at_generation}"))
        }
        ChainStatus::Gap { after_generation } => {
            TierOutcome::Fail(format!("generation gap after {after_generation}"))
        }
    }
}

/// The child half of tier A: append until killed. Never returns normally.
pub fn crash_child(path: &Path, durability: Durability) -> ! {
    let mut store = match Store::open(path, durability) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("crash-child: {e}");
            std::process::exit(2);
        }
    };
    let mut generation = 0u64;
    loop {
        let batch = gen::events(generation, 16, 64, 0xc0ffee);
        if store.append_batch(&batch).is_err() {
            std::process::exit(3);
        }
        generation += 16;
    }
}

// ---------------------------------------------------------------------------
// Tier B — WAL loss
// ---------------------------------------------------------------------------

/// Establish a durable prefix, append a tail that stays in the WAL, then open a
/// copy of the main file alone.
pub fn tier_b_wal_loss(
    path: &Path,
    durability: Durability,
    prefix_events: u64,
    tail_events: u64,
) -> TierOutcome {
    remove_db(path);
    let snapshot = sidecar(path, ".snapshot");
    remove_db(&snapshot);

    let (prefix_entries, full_entries) = {
        let mut store = match Store::open(path, durability) {
            Ok(s) => s,
            Err(e) => return TierOutcome::Inconclusive(format!("cannot open store: {e}")),
        };
        // Without this SQLite auto-checkpoints at 1000 pages and quietly moves
        // the tail into the main file, which would make the experiment pass by
        // never having a tail to lose.
        if let Err(e) = store.conn.execute_batch("PRAGMA wal_autocheckpoint=0;") {
            return TierOutcome::Inconclusive(format!("cannot disable autocheckpoint: {e}"));
        }

        if let Err(e) = store.append_batch(&gen::events(0, prefix_events, 64, 1)) {
            return TierOutcome::Inconclusive(format!("prefix append failed: {e}"));
        }
        if let Err(e) = store.durable_checkpoint() {
            return TierOutcome::Inconclusive(format!("durable checkpoint failed: {e}"));
        }
        let prefix_entries = store.count_events().unwrap_or(0);

        if let Err(e) = store.append_batch(&gen::events(prefix_events, tail_events, 64, 2)) {
            return TierOutcome::Inconclusive(format!("tail append failed: {e}"));
        }
        let full_entries = store.count_events().unwrap_or(0);

        // Copy the main file ONLY — no -wal, no -shm. This is the whole
        // experiment: what does the durable artifact alone contain?
        if let Err(e) = std::fs::copy(path, &snapshot) {
            return TierOutcome::Inconclusive(format!("cannot snapshot main file: {e}"));
        }
        (prefix_entries, full_entries)
    };

    if full_entries <= prefix_entries {
        return TierOutcome::Inconclusive(format!(
            "the tail did not survive as uncheckpointed WAL data \
             (prefix {prefix_entries}, full {full_entries}) — nothing was at risk, \
             so recovering the prefix proves nothing"
        ));
    }

    let recovered = match Store::open(&snapshot, durability) {
        Ok(s) => s,
        Err(e) => {
            remove_db(&snapshot);
            return TierOutcome::Fail(format!("snapshot unopenable: {e}"));
        }
    };
    let status = recovered.verify_chain();
    let count = recovered.count_events().unwrap_or(0);
    drop(recovered);
    remove_db(&snapshot);

    match status {
        Ok(ChainStatus::Intact { entries, .. }) if entries == prefix_entries => {
            TierOutcome::Pass(format!(
                "recovered exactly the durable prefix: {entries} entries, chain INTACT; \
                 the {} uncheckpointed tail entries are cleanly absent, not torn",
                full_entries - prefix_entries
            ))
        }
        Ok(ChainStatus::Intact { entries, .. }) => TierOutcome::Fail(format!(
            "recovered {entries} entries but the durable prefix was {prefix_entries} \
             — the main file is not the prefix it claimed to be"
        )),
        Ok(other) => TierOutcome::Fail(format!(
            "recovered log is not intact: {other:?} ({count} entries)"
        )),
        Err(e) => TierOutcome::Fail(format!("verification errored: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Tier C
// ---------------------------------------------------------------------------

/// How many power cuts a tier C claim requires.
///
/// A single surviving cut proves nothing: device-cache loss is probabilistic,
/// depends where in the erase-block cycle the cut lands, and a store that
/// survives once can lose an acknowledged write on the next attempt. Five is
/// the drill's stated floor — enough that a pass is not a coincidence, few
/// enough to actually perform by hand.
pub const MIN_TIER_C_TRIALS: u64 = 5;

/// One recorded power-cut trial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrialRecord {
    pub trial: u64,
    /// Which arming this trial verifies.
    ///
    /// The load-bearing field. Trials were previously counted as ledger rows,
    /// so re-running `verify` without re-arming appended another PASS against
    /// the same surviving store — five "trials" were satisfiable by one power
    /// cut and four no-ops. Counting distinct arm IDs makes a trial mean an
    /// arming, which is the thing a power cut is applied to.
    pub arm_id: String,
    pub outcome: TierOutcome,
    /// Generation of the last event fsynced *before* the cut. Everything up to
    /// here was acknowledged as durable and must survive.
    pub durable_generation: u64,
    /// Highest generation present after the reboot.
    pub recovered_generation: u64,
    pub chain: String,
}

/// What `powercut arm` fsyncs before the tail starts, so that after the reboot
/// there is an independent statement of what *should* have survived.
///
/// Without this the trial is unfalsifiable: a store that came back with fewer
/// events than it acknowledged would be indistinguishable from one that was
/// simply cut earlier than the operator thought.
pub struct ArmedMarker {
    /// Unique per arming. Two markers minted from the same store state at
    /// different times differ, so a re-verified marker is detectable.
    pub arm_id: String,
    pub durable_generation: u64,
    pub durable_head: String,
}

impl ArmedMarker {
    fn render(&self) -> String {
        format!(
            "{}\n{}\n{}\n",
            self.arm_id, self.durable_generation, self.durable_head
        )
    }

    fn parse(s: &str) -> Option<Self> {
        let mut lines = s.lines();
        let arm_id = lines.next()?.trim().to_string();
        if arm_id.is_empty() {
            return None;
        }
        let durable_generation = lines.next()?.trim().parse().ok()?;
        let durable_head = lines.next()?.trim().to_string();
        if durable_head.is_empty() {
            return None;
        }
        Some(Self {
            arm_id,
            durable_generation,
            durable_head,
        })
    }
}

/// Monotonic within this process, so two mints cannot collide however coarse
/// the wall clock is. See [`mint_arm_id`].
static ARM_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Mint an arm ID unique to this arming.
///
/// Derived from the durable head and boundary (so it is tied to the state it
/// describes) plus wall-clock nanos, the pid, and a process-local counter (so
/// two armings of an identical-looking store still differ). Truncated to 64
/// bits, which is far more than enough to distinguish a handful of manual
/// trials.
///
/// The counter is not redundant with the clock. Wall-clock resolution is a
/// platform property — coarse enough on some targets that two calls in quick
/// succession read the same value — and an id collision here does not degrade
/// gracefully: it makes a legitimate second arming look like a re-verification
/// of the first and get refused. Across processes the pid and the clock
/// separate; within one, the counter does.
fn mint_arm_id(head: &str, generation: u64) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = ARM_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut buf = Vec::new();
    buf.extend_from_slice(b"wm2-tierc-arm-v1\x00");
    buf.extend_from_slice(head.as_bytes());
    buf.extend_from_slice(&generation.to_le_bytes());
    buf.extend_from_slice(&nanos.to_le_bytes());
    buf.extend_from_slice(&std::process::id().to_le_bytes());
    buf.extend_from_slice(&seq.to_le_bytes());
    crate::sha256::hex(&buf)[..16].to_string()
}

pub fn armed_marker_path(db: &Path) -> PathBuf {
    sidecar(db, "-tierc-armed")
}

pub fn trials_ledger_path(db: &Path) -> PathBuf {
    sidecar(db, "-tierc-trials.jsonl")
}

/// Judge one trial from what the marker promised and what came back.
///
/// Pure, so the three outcomes that matter can be tested without a power
/// supply. The distinction that carries the whole tier:
///
/// - a **shorter** log than armed is a `FAIL` only if it is shorter than the
///   *fsynced prefix*. Losing un-fsynced tail events is correct behaviour —
///   that is what tier B already establishes portably.
/// - a log shorter than the fsynced prefix means the device acknowledged a
///   write it had not persisted. That is the device-cache lie, it is the only
///   thing tier C can detect that tiers A and B cannot, and it is a `FAIL`.
/// - a broken chain is a torn or forked write: also `FAIL`, and worse.
pub fn judge_trial(
    trial: u64,
    marker: Option<&ArmedMarker>,
    recovered: &Result<ChainStatus, String>,
) -> TrialRecord {
    let Some(marker) = marker else {
        return TrialRecord {
            trial,
            arm_id: String::new(),
            outcome: TierOutcome::Inconclusive(
                "no armed marker: `powercut arm` did not fsync a durable prefix before \
                 the cut, so nothing states what should have survived"
                    .into(),
            ),
            durable_generation: 0,
            recovered_generation: 0,
            chain: "unknown".into(),
        };
    };

    let (chain_token, recovered_generation, verdict) = match recovered {
        Ok(ChainStatus::Intact { entries, .. }) => {
            let entries = *entries;
            if entries >= marker.durable_generation {
                (
                    "intact",
                    entries,
                    TierOutcome::Pass(format!(
                        "chain intact after power cut; fsynced prefix of {} event(s) survived \
                         ({} present)",
                        marker.durable_generation, entries
                    )),
                )
            } else {
                (
                    "intact",
                    entries,
                    TierOutcome::Fail(format!(
                        "DURABILITY VIOLATION: {} event(s) were fsynced and acknowledged before \
                         the cut, but only {} came back. The storage device acknowledged writes \
                         it had not persisted — this is the failure tier C exists to detect, and \
                         no `synchronous` setting can compensate for it.",
                        marker.durable_generation, entries
                    )),
                )
            }
        }
        Ok(ChainStatus::Broken { at_generation }) => (
            "broken",
            *at_generation,
            TierOutcome::Fail(format!(
                "chain broken at generation {at_generation} after power cut: a torn or forked \
                 write, not merely a lost tail"
            )),
        ),
        Ok(ChainStatus::Gap { after_generation }) => (
            "gap",
            *after_generation,
            TierOutcome::Fail(format!(
                "generation gap after {after_generation}: the log came back missing an interior \
                 run, which a valid prefix cannot contain"
            )),
        ),
        Ok(ChainStatus::Empty) => (
            "empty",
            0,
            TierOutcome::Fail(format!(
                "store came back empty although {} event(s) were fsynced before the cut",
                marker.durable_generation
            )),
        ),
        Err(e) => (
            "unreadable",
            0,
            TierOutcome::Fail(format!(
                "store could not be reopened after the power cut: {e}"
            )),
        ),
    };

    TrialRecord {
        trial,
        arm_id: marker.arm_id.clone(),
        outcome: verdict,
        durable_generation: marker.durable_generation,
        recovered_generation,
        chain: chain_token.into(),
    }
}

/// Aggregate recorded trials into the tier C outcome.
///
/// Zero trials stays `NOT-RUN` — the default for every automated run, so the
/// exit code keeps its meaning. Between one and [`MIN_TIER_C_TRIALS`] is
/// `INCONCLUSIVE` rather than a pass: partial evidence toward a durability
/// claim is not a durability claim, and it fails the run so a half-finished
/// drill cannot be filed as a complete one.
pub fn tier_c_from_trials(trials: &[TrialRecord]) -> TierOutcome {
    if trials.is_empty() {
        return tier_c_not_run();
    }

    let failures: Vec<&TrialRecord> = trials
        .iter()
        .filter(|t| matches!(t.outcome, TierOutcome::Fail(_)))
        .collect();
    if let Some(first) = failures.first() {
        return TierOutcome::Fail(format!(
            "{} of {} recorded row(s) failed. First: trial {} — {}",
            failures.len(),
            trials.len(),
            first.trial,
            first.outcome.detail()
        ));
    }

    let inconclusive = trials
        .iter()
        .filter(|t| matches!(t.outcome, TierOutcome::Inconclusive(_)))
        .count();
    if inconclusive > 0 {
        return TierOutcome::Inconclusive(format!(
            "{inconclusive} of {} row(s) never established a durable prefix; re-run those cuts",
            trials.len()
        ));
    }

    // Count DISTINCT armings, not ledger rows. This is the whole correction:
    // a row records a verification, and re-verifying a surviving store without
    // re-arming used to append another PASS. Five "trials" were satisfiable by
    // one power cut and four no-ops, which defeats the point of requiring five.
    //
    // A row with no arm_id cannot be attributed to an arming at all; it is
    // counted as unattributable rather than silently dropped, because dropping
    // it would make an unattributable row indistinguishable from a row that was
    // never written.
    let unattributed = trials.iter().filter(|t| t.arm_id.is_empty()).count();
    if unattributed > 0 {
        return TierOutcome::Inconclusive(format!(
            "{unattributed} of {} row(s) carry no arm id, so they cannot be attributed to a \
             distinct arming. Ledger rows written before arm ids existed read this way; restart \
             tier C on a fresh database.",
            trials.len()
        ));
    }

    let unique: std::collections::BTreeSet<&str> =
        trials.iter().map(|t| t.arm_id.as_str()).collect();
    let n = unique.len() as u64;
    let rows = trials.len();

    if n < MIN_TIER_C_TRIALS {
        return TierOutcome::Inconclusive(format!(
            "only {n} DISTINCT arming(s) recorded across {rows} ledger row(s); \
             {MIN_TIER_C_TRIALS} are required. Re-verifying without re-arming does not add a \
             trial: a single survived cut is not evidence of durability, because device-cache \
             loss is probabilistic."
        ));
    }

    TierOutcome::Pass(format!(
        "{n} distinct power-cut arming(s) recorded across {rows} row(s), all preserving the \
         fsynced prefix with an intact chain"
    ))
}

/// How many DISTINCT armings a ledger records.
///
/// The number a tier C progress report must quote. Row count over-reports:
/// three rows carrying one arm id are three verifications of one power cut, and
/// printing "3 trials" beside an `INCONCLUSIVE` verdict that counted 1 would
/// contradict itself.
pub fn distinct_armings(trials: &[TrialRecord]) -> u64 {
    trials
        .iter()
        .filter(|t| !t.arm_id.is_empty())
        .map(|t| t.arm_id.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len() as u64
}

pub fn tier_c_not_run() -> TierOutcome {
    TierOutcome::NotRun(
        "a real power cut cannot be performed from software: nothing in-process can \
         tell an honest fsync from a device cache that acknowledged and buffered it. \
         Manual procedure in docs/hardware/JETSON_WM2_PERSISTENCE_DRILL.md §8; until \
         it is run and its result recorded, no durability claim is supported. Record \
         trials with `powercut arm` / `powercut verify`."
            .to_string(),
    )
}

/// Read whatever trials have been recorded next to this database.
///
/// A malformed line is skipped rather than fatal: the ledger is appended to
/// across reboots and power cuts, so a torn final line is an expected state,
/// and losing the trials already recorded because of it would be perverse.
pub fn read_trials(db: &Path) -> Vec<TrialRecord> {
    let Ok(text) = std::fs::read_to_string(trials_ledger_path(db)) else {
        return Vec::new();
    };
    text.lines().filter_map(parse_trial_line).collect()
}

/// Minimal field extraction from a ledger line.
///
/// The ledger is written by this harness in a fixed shape, so a full JSON
/// parser would be a dependency bought for nothing.
fn parse_trial_line(line: &str) -> Option<TrialRecord> {
    let field = |key: &str| -> Option<String> {
        let pat = format!("\"{key}\":");
        let start = line.find(&pat)? + pat.len();
        let rest = line[start..].trim_start();
        if let Some(stripped) = rest.strip_prefix('"') {
            let end = stripped.find('"')?;
            Some(stripped[..end].to_string())
        } else {
            let end = rest.find([',', '}'])?;
            Some(rest[..end].trim().to_string())
        }
    };

    let trial = field("trial")?.parse().ok()?;
    let token = field("outcome")?;
    let detail = field("detail").unwrap_or_default();
    let outcome = match token.as_str() {
        "PASS" => TierOutcome::Pass(detail),
        "FAIL" => TierOutcome::Fail(detail),
        "INCONCLUSIVE" => TierOutcome::Inconclusive(detail),
        _ => return None,
    };
    Some(TrialRecord {
        trial,
        arm_id: field("arm_id").unwrap_or_default(),
        outcome,
        durable_generation: field("durable_generation")?.parse().ok()?,
        recovered_generation: field("recovered_generation")?.parse().ok()?,
        chain: field("chain").unwrap_or_default(),
    })
}

pub fn append_trial(db: &Path, rec: &TrialRecord) -> Result<(), String> {
    use std::io::Write;
    let line = crate::json::Obj::new()
        .int("trial", rec.trial)
        .str("arm_id", &rec.arm_id)
        .str("outcome", rec.outcome.token())
        .int("durable_generation", rec.durable_generation)
        .int("recovered_generation", rec.recovered_generation)
        .str("chain", &rec.chain)
        .str("detail", rec.outcome.detail())
        .render();

    let path = trials_ledger_path(db);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    writeln!(f, "{line}").map_err(|e| e.to_string())?;
    // The ledger records evidence about power loss and is itself written on a
    // machine that is about to lose power again. fsync it.
    f.sync_all().map_err(|e| e.to_string())?;
    // And fsync the directory: on the trial that CREATES the ledger, syncing
    // the file alone leaves the directory entry un-durable, so a cut can lose
    // the whole file — and with it every trial recorded so far. Same reason
    // `write_marker` does it.
    if let Some(dir) = path.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// Arm a power-cut trial: fsync a known prefix, record it, then append forever.
///
/// Never returns — the operator ends it with the power switch. That is the
/// instrument, and it is why this cannot be a normal stage.
pub fn powercut_arm(db: &Path, prefix_events: u64, entities: u64, seed: u64) -> ! {
    // An unused marker means the previous arming was never verified. Arming
    // over it would orphan that trial AND leave a marker describing a store
    // state that no longer exists — which is how a later `verify` came to
    // report PASS for a cut that never happened. Refuse; the operator must
    // verify or explicitly discard.
    if armed_marker_path(db).exists() {
        let torn = matches!(read_marker_state(db), MarkerState::Unreadable);
        eprintln!(
            "powercut arm: an unused marker already exists at {}.\n\
             A marker is SINGLE-USE: each trial needs its own arming, and the marker is \
             removed only after its verdict is durably recorded.\n\
             {}",
            armed_marker_path(db).display(),
            if torn {
                "That marker does not parse — torn by the cut, or written by a build that \
                 predates arm ids. It cannot be verified. Delete it and arm again to re-run \
                 this trial."
            } else {
                "Run `powercut verify --trial <n>` for the outstanding arming first, or delete \
                 the marker deliberately if that cut never happened."
            }
        );
        std::process::exit(2);
    }

    let mut store = match Store::open(db, Durability::Full) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("powercut arm: cannot open store: {e}");
            std::process::exit(2);
        }
    };

    // The durable prefix, one transaction per event at synchronous=FULL, then
    // an explicit checkpoint so the content is in the main file and not only
    // in a WAL that the cut may take with it.
    // Continue from the store's own head rather than restarting at 0. Starting
    // at 0 collided with the existing rows on the second arming
    // (`UNIQUE constraint failed: world_events.generation`), so the arm failed
    // while a stale marker survived — and the next `verify` passed against the
    // FIRST trial's data.
    let start = match next_generation(&store) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("powercut arm: cannot read the store's head generation: {e}");
            std::process::exit(2);
        }
    };
    let events = gen::events(start, prefix_events, entities, seed);
    for e in &events {
        if let Err(err) = store.append_batch(std::slice::from_ref(e)) {
            eprintln!("powercut arm: append failed: {err}");
            std::process::exit(2);
        }
    }
    if let Err(e) = store.durable_checkpoint() {
        eprintln!("powercut arm: checkpoint failed: {e}");
        std::process::exit(2);
    }

    let head = match store.verify_chain() {
        Ok(ChainStatus::Intact { entries, head, .. }) => ArmedMarker {
            arm_id: mint_arm_id(&head, entries),
            durable_generation: entries,
            durable_head: head,
        },
        other => {
            eprintln!("powercut arm: prefix did not verify: {other:?}");
            std::process::exit(2);
        }
    };

    if let Err(e) = write_marker(db, &head) {
        eprintln!("powercut arm: cannot record marker: {e}");
        std::process::exit(2);
    }

    println!(
        "ARMED [{}]: {} event(s) fsynced and recorded (this arming appended generations \
         {}..{}). Cut power NOW, at any time.\n\
         After reboot, run: wm2-persistence-harness powercut verify --db {} --trial <n>",
        head.arm_id,
        head.durable_generation,
        start,
        start + prefix_events - 1,
        db.display()
    );
    // This function never returns, so nothing else will ever flush this. To a
    // terminal it would appear anyway; to a pipe it would not, and an operator
    // who cannot see "ARMED" does not know when it is safe to cut power.
    let _ = std::io::Write::flush(&mut std::io::stdout());

    // The tail: appended continuously and deliberately never checkpointed, so
    // the cut always lands mid-write. Losing all of this is correct; losing any
    // of the prefix above is not.
    // The tail starts after the prefix THIS arming appended, not after
    // `prefix_events` counted from zero.
    let mut n = start + prefix_events;
    loop {
        let batch = gen::events(n, 64, entities, seed);
        if store.append_batch(&batch).is_err() {
            // A write error here is expected as the device dies. Keep going;
            // only the power switch ends this.
            continue;
        }
        n += 64;
    }
}

/// The generation the next append should use: `MAX(generation) + 1`, or 0 on an
/// empty store.
///
/// `generation` is the primary key, so restarting at 0 on a populated store is
/// not merely non-independent — it is a constraint violation that aborts the
/// arming.
fn next_generation(store: &Store) -> rusqlite::Result<u64> {
    store.conn.query_row(
        "SELECT COALESCE(MAX(generation) + 1, 0) FROM world_events",
        [],
        |r| r.get::<_, i64>(0).map(|v| v as u64),
    )
}

fn write_marker(db: &Path, marker: &ArmedMarker) -> Result<(), String> {
    use std::io::Write;
    let path = armed_marker_path(db);
    let mut f = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    f.write_all(marker.render().as_bytes())
        .map_err(|e| e.to_string())?;
    // Must be durable before the tail starts, or the trial is unfalsifiable.
    f.sync_all().map_err(|e| e.to_string())?;
    // fsync the directory too, so the file's *existence* survives the cut.
    if let Some(dir) = path.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// What is sitting at the marker path.
///
/// `Unreadable` must be distinct from `Absent`. `arm` refuses while the file
/// exists and `verify` needs a parseable marker, so collapsing the two leaves
/// an operator with a torn marker stuck between "arm refuses, a marker exists"
/// and "verify refuses, no marker" — with no stated way out. A marker torn by
/// the very power cut it was recording is an expected state, not an exotic one.
pub enum MarkerState {
    Absent,
    /// The file is there but does not parse: torn by the cut, or written by a
    /// build that predates arm ids.
    Unreadable,
    Armed(ArmedMarker),
}

pub fn read_marker_state(db: &Path) -> MarkerState {
    let path = armed_marker_path(db);
    match std::fs::read_to_string(&path) {
        Err(_) if !path.exists() => MarkerState::Absent,
        // Unreadable for any other reason — permissions, an I/O error — is
        // still "something is there and it is not usable", which is the state
        // the operator has to be told about.
        Err(_) => MarkerState::Unreadable,
        Ok(text) => match ArmedMarker::parse(&text) {
            Some(m) => MarkerState::Armed(m),
            None => MarkerState::Unreadable,
        },
    }
}

/// Remove the armed marker. Called only after the verdict is durably recorded.
fn consume_marker(db: &Path) -> Result<(), String> {
    let path = armed_marker_path(db);
    std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    // The removal itself must be durable, or a cut between here and the next
    // arming could resurrect a marker whose trial is already in the ledger.
    if let Some(dir) = path.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// Verify one power-cut trial after the reboot and record it.
///
/// Ordering is load-bearing and deliberate:
///
/// 1. **reject duplicates first** — a repeated trial number or a repeated arm
///    id is refused before anything is written, so a mistaken re-run cannot
///    inflate the ledger;
/// 2. **append and fsync the verdict** — the ledger is the evidence;
/// 3. **only then consume the marker.** If the machine dies between 2 and 3 the
///    marker survives and the next `verify` is refused as a duplicate arm id,
///    which is recoverable. The reverse order could lose the verdict entirely
///    while destroying the only record that the arming happened.
pub fn powercut_verify(db: &Path, trial: u64) -> Result<TrialRecord, String> {
    let existing = read_trials(db);

    if let Some(prev) = existing.iter().find(|t| t.trial == trial) {
        return Err(format!(
            "trial {trial} is already recorded (arm {}, {}). Trial numbers must be distinct — \
             re-recording one would count a single arming twice. Use the next unused number.",
            if prev.arm_id.is_empty() {
                "unknown"
            } else {
                prev.arm_id.as_str()
            },
            prev.outcome.token()
        ));
    }

    let marker = match read_marker_state(db) {
        MarkerState::Armed(m) => m,
        MarkerState::Absent => {
            return Err(format!(
                "no armed marker at {}. Each trial requires its OWN arming: run `powercut arm`, \
                 wait for the ARMED line, cut power, then verify. Verifying without arming would \
                 re-read a surviving store and record a pass for a cut that never happened.",
                armed_marker_path(db).display()
            ));
        }
        // Distinct from absent, and it must be: `arm` refuses while this file
        // exists, so reporting it as missing would leave the operator with two
        // refusals and no stated way forward.
        MarkerState::Unreadable => {
            return Err(format!(
                "the marker at {} exists but cannot be read. Either the cut tore it — in which \
                 case nothing states what should have survived and this arming cannot be judged \
                 — or it was written by a build that predates arm ids.\n\
                 Delete it and re-arm to re-run this trial. If it predates arm ids, its earlier \
                 rows cannot be attributed either: restart tier C on a fresh database.",
                armed_marker_path(db).display()
            ));
        }
    };

    if let Some(prev) = existing.iter().find(|t| t.arm_id == marker.arm_id) {
        return Err(format!(
            "arming {} is already recorded as trial {}. A marker is single-use; this one \
             should have been consumed. Re-arm before verifying again.",
            marker.arm_id, prev.trial
        ));
    }

    let recovered = Store::open(db, Durability::Full)
        .map_err(|e| e.to_string())
        .and_then(|s| s.verify_chain().map_err(|e| e.to_string()));
    let rec = judge_trial(trial, Some(&marker), &recovered);

    // Evidence first. A verdict that is not durably recorded did not happen.
    append_trial(db, &rec)?;
    consume_marker(db)?;
    Ok(rec)
}

pub fn run(
    path: &Path,
    durability: Durability,
    run_ms: u64,
    prefix_events: u64,
    tail_events: u64,
) -> CrashResult {
    // Tier A and B rewrite the database, so the trials ledger is read first —
    // it is evidence about earlier power cuts and must not be interpreted
    // against a store this run has since replaced.
    let trials = read_trials(path);
    // Distinct armings, not ledger rows — the same correction `tier_c_from_trials`
    // makes. Reporting the row count here would contradict the verdict beside it.
    let distinct = distinct_armings(&trials);
    CrashResult {
        tier_a_sigkill: tier_a_sigkill(path, durability, run_ms),
        tier_b_wal_loss: tier_b_wal_loss(path, durability, prefix_events, tail_events),
        tier_c_power_cut: tier_c_from_trials(&trials),
        tier_c_trials: distinct,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("wm2-crash-{}-{}.sqlite", name, std::process::id()));
        p
    }

    #[test]
    fn tier_b_recovers_exactly_the_durable_prefix() {
        let p = temp("tier-b");
        let outcome = tier_b_wal_loss(&p, Durability::Full, 400, 400);
        assert!(
            matches!(outcome, TierOutcome::Pass(_)),
            "{}: {}",
            outcome.token(),
            outcome.detail()
        );
        remove_db(&p);
    }

    #[test]
    fn tier_b_reports_inconclusive_when_nothing_was_at_risk() {
        // Zero tail events: the prefix IS everything, so recovering it is
        // vacuous. This must not read as a pass.
        let p = temp("tier-b-vacuous");
        let outcome = tier_b_wal_loss(&p, Durability::Full, 200, 0);
        assert_eq!(outcome.token(), "INCONCLUSIVE", "{}", outcome.detail());
        remove_db(&p);
    }

    #[test]
    fn tier_c_is_always_not_run_and_says_why() {
        let c = tier_c_not_run();
        assert_eq!(c.token(), "NOT-RUN");
        assert!(c.detail().contains("power cut"));
    }

    #[test]
    fn inconclusive_fails_the_run_but_not_run_does_not() {
        // The contract the driver broke: the drill doc says exit status is 1
        // if any measurement "failed OR WAS UNUSABLE", and INCONCLUSIVE is
        // exactly the unusable case. Counting it as a pass produces a results
        // file that looks complete while a load-bearing gate is silently
        // missing.
        assert!(TierOutcome::Fail(String::new()).fails_the_run());
        assert!(TierOutcome::Inconclusive(String::new()).fails_the_run());
        assert!(!TierOutcome::Pass(String::new()).fails_the_run());

        // NOT-RUN must be exempt or the exit code stops carrying information:
        // tier C is ALWAYS NOT-RUN by construction, so counting it would make
        // every single run exit 1.
        assert!(!TierOutcome::NotRun(String::new()).fails_the_run());
        assert!(!tier_c_not_run().fails_the_run());
    }

    #[test]
    fn a_vacuous_tier_b_would_fail_the_run() {
        // End to end rather than on the predicate alone: the zero-tail case is
        // deterministically INCONCLUSIVE, and it must now be fatal.
        let p = temp("tier-b-fatal");
        let outcome = tier_b_wal_loss(&p, Durability::Full, 200, 0);
        assert_eq!(outcome.token(), "INCONCLUSIVE");
        assert!(
            outcome.fails_the_run(),
            "a vacuous tier B would still exit 0"
        );
        remove_db(&p);
    }

    #[test]
    fn outcome_tokens_are_distinct() {
        // The report writer keys on these; two tiers sharing a token would make
        // a failure indistinguishable from a skip in the results file.
        let tokens = [
            TierOutcome::Pass(String::new()).token(),
            TierOutcome::Fail(String::new()).token(),
            TierOutcome::Inconclusive(String::new()).token(),
            TierOutcome::NotRun(String::new()).token(),
        ];
        let unique: std::collections::HashSet<_> = tokens.iter().collect();
        assert_eq!(unique.len(), 4);
    }

    // -----------------------------------------------------------------------
    // Tier C — the judgement is pure, so it is tested without a power supply
    // -----------------------------------------------------------------------

    fn marker(gen: u64) -> ArmedMarker {
        marker_id("arm-fixed", gen)
    }

    fn marker_id(id: &str, gen: u64) -> ArmedMarker {
        ArmedMarker {
            arm_id: id.into(),
            durable_generation: gen,
            durable_head: "deadbeef".into(),
        }
    }

    fn passing_trial_arm(n: u64, id: &str) -> TrialRecord {
        judge_trial(n, Some(&marker_id(id, 400)), &intact(400))
    }

    fn intact(entries: u64) -> Result<ChainStatus, String> {
        Ok(ChainStatus::Intact {
            entries,
            head: "deadbeef".into(),
            compacted_windows: 0,
            redacted: 0,
        })
    }

    #[test]
    fn a_surviving_fsynced_prefix_passes() {
        let r = judge_trial(1, Some(&marker(400)), &intact(400));
        assert!(matches!(r.outcome, TierOutcome::Pass(_)), "{r:?}");
    }

    #[test]
    fn losing_only_the_unfsynced_tail_passes() {
        // The tail is appended without a checkpoint precisely so it can be
        // lost; more events than the prefix came back, which is also fine.
        let r = judge_trial(1, Some(&marker(400)), &intact(512));
        assert!(matches!(r.outcome, TierOutcome::Pass(_)), "{r:?}");
    }

    #[test]
    fn losing_an_fsynced_event_is_the_durability_violation_tier_c_exists_for() {
        let r = judge_trial(1, Some(&marker(400)), &intact(399));
        match r.outcome {
            TierOutcome::Fail(d) => assert!(
                d.contains("DURABILITY VIOLATION"),
                "must name the failure plainly: {d}"
            ),
            other => panic!("a lost acknowledged write must FAIL, got {other:?}"),
        }
    }

    #[test]
    fn a_broken_chain_after_a_cut_fails() {
        let r = judge_trial(
            1,
            Some(&marker(400)),
            &Ok(ChainStatus::Broken { at_generation: 7 }),
        );
        assert!(matches!(r.outcome, TierOutcome::Fail(_)), "{r:?}");
    }

    #[test]
    fn an_unreadable_store_after_a_cut_fails() {
        let r = judge_trial(1, Some(&marker(400)), &Err("disk I/O error".into()));
        assert!(matches!(r.outcome, TierOutcome::Fail(_)), "{r:?}");
    }

    #[test]
    fn an_unarmed_trial_is_inconclusive_not_a_pass() {
        // Nothing states what should have survived, so a survived cut proves
        // nothing — the exact shape of vacuity the tier must refuse.
        let r = judge_trial(1, None, &intact(9_999));
        assert!(matches!(r.outcome, TierOutcome::Inconclusive(_)), "{r:?}");
        assert!(r.outcome.fails_the_run());
    }

    #[test]
    fn no_trials_stays_not_run_and_does_not_fail_the_run() {
        // The default for every automated run. If this ever failed the run,
        // CI would be red permanently and the exit code would stop meaning
        // anything.
        let o = tier_c_from_trials(&[]);
        assert_eq!(o.token(), "NOT-RUN");
        assert!(!o.fails_the_run());
    }

    fn passing_trial(n: u64) -> TrialRecord {
        passing_trial_arm(n, &format!("arm-{n}"))
    }

    #[test]
    fn fewer_than_the_required_trials_is_inconclusive() {
        for n in 1..MIN_TIER_C_TRIALS {
            let trials: Vec<_> = (1..=n).map(passing_trial).collect();
            let o = tier_c_from_trials(&trials);
            assert_eq!(o.token(), "INCONCLUSIVE", "{n} trial(s) must not pass");
            assert!(o.fails_the_run());
        }
    }

    #[test]
    fn the_required_number_of_clean_trials_passes() {
        let trials: Vec<_> = (1..=MIN_TIER_C_TRIALS).map(passing_trial).collect();
        let o = tier_c_from_trials(&trials);
        assert_eq!(o.token(), "PASS", "{}", o.detail());
        assert!(!o.fails_the_run());
    }

    #[test]
    fn one_failure_among_many_passes_still_fails() {
        // A durability violation is not outvoted by successful trials: the
        // device demonstrated it can lose an acknowledged write.
        let mut trials: Vec<_> = (1..=MIN_TIER_C_TRIALS + 3).map(passing_trial).collect();
        trials[4] = judge_trial(5, Some(&marker(400)), &intact(12));
        let o = tier_c_from_trials(&trials);
        assert_eq!(o.token(), "FAIL");
        assert!(o.detail().contains("trial 5"), "{}", o.detail());
    }

    #[test]
    fn the_marker_round_trips() {
        let m = marker(1234);
        let back = ArmedMarker::parse(&m.render()).expect("parses");
        assert_eq!(back.durable_generation, 1234);
        assert_eq!(back.durable_head, "deadbeef");
    }

    #[test]
    fn a_truncated_marker_is_not_silently_zero() {
        // A marker torn by the very power cut it was recording must read as
        // absent, which makes the trial inconclusive — not as generation 0,
        // which would make every trial trivially pass.
        assert!(ArmedMarker::parse("").is_none());
        assert!(ArmedMarker::parse("400").is_none());
        assert!(ArmedMarker::parse("notanumber\nhead\n").is_none());
    }

    #[test]
    fn the_ledger_round_trips_through_its_own_writer() {
        let db = temp("tierc-ledger");
        let _ = std::fs::remove_file(trials_ledger_path(&db));
        let written = [
            passing_trial(1),
            judge_trial(2, Some(&marker(400)), &intact(3)),
            judge_trial(3, None, &intact(400)),
        ];
        for r in &written {
            append_trial(&db, r).expect("append");
        }

        let read = read_trials(&db);
        assert_eq!(read.len(), 3);
        for (w, r) in written.iter().zip(&read) {
            assert_eq!(w.trial, r.trial);
            assert_eq!(w.outcome.token(), r.outcome.token());
            assert_eq!(w.durable_generation, r.durable_generation);
            assert_eq!(w.recovered_generation, r.recovered_generation);
        }
        let _ = std::fs::remove_file(trials_ledger_path(&db));
    }

    #[test]
    fn a_torn_final_ledger_line_does_not_discard_earlier_trials() {
        use std::io::Write;
        let db = temp("tierc-torn");
        let path = trials_ledger_path(&db);
        let _ = std::fs::remove_file(&path);
        append_trial(&db, &passing_trial(1)).expect("append");
        append_trial(&db, &passing_trial(2)).expect("append");
        // The ledger is appended to on a machine that is about to lose power
        // again, so a half-written last line is an expected state.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open");
        write!(f, "{{\"trial\":3,\"outcome\":\"PA").expect("write");
        drop(f);

        assert_eq!(read_trials(&db).len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    // -----------------------------------------------------------------------
    // Tier C independence — one arming is one trial
    // -----------------------------------------------------------------------

    /// Clear a database and every tier C sidecar next to it.
    fn fresh(name: &str) -> PathBuf {
        let db = temp(name);
        remove_db(&db);
        let _ = std::fs::remove_file(trials_ledger_path(&db));
        let _ = std::fs::remove_file(armed_marker_path(&db));
        let _ = std::fs::remove_dir_all(trials_ledger_path(&db));
        db
    }

    fn cleanup(db: &Path) {
        remove_db(db);
        let _ = std::fs::remove_file(trials_ledger_path(db));
        let _ = std::fs::remove_file(armed_marker_path(db));
        let _ = std::fs::remove_dir_all(trials_ledger_path(db));
    }

    #[test]
    fn a_second_arming_starts_after_the_events_the_first_one_wrote() {
        // The defect, at its root. `generation` is the primary key, so a second
        // arming that restarts at 0 does not merely repeat work — it violates
        // the constraint, aborts, and leaves the previous marker in place for a
        // later `verify` to pass against the FIRST trial's data.
        let db = fresh("tierc-next-gen");
        let mut store = Store::open(&db, Durability::Full).expect("open");
        assert_eq!(next_generation(&store).expect("empty"), 0);

        store
            .append_batch(&gen::events(0, 400, 8, 1))
            .expect("arm 1");
        assert_eq!(next_generation(&store).expect("after arm 1"), 400);

        // The second arming appends from there, and must not collide.
        let start = next_generation(&store).expect("read");
        store
            .append_batch(&gen::events(start, 400, 8, 2))
            .expect("a second arming from MAX+1 must not violate the primary key");
        assert_eq!(next_generation(&store).expect("after arm 2"), 800);

        // And the log is still one unbroken run, so a later trial's recovered
        // count remains comparable with its marker.
        match store.verify_chain().expect("verify") {
            ChainStatus::Intact { entries, .. } => assert_eq!(entries, 800),
            other => panic!("continuity broken across two armings: {other:?}"),
        }
        drop(store);
        cleanup(&db);
    }

    #[test]
    fn restarting_at_zero_on_a_populated_store_is_the_constraint_violation_it_looked_like() {
        // Pins the observed failure rather than assuming it: the fix is only
        // meaningful if the unfixed behaviour really does abort.
        let db = fresh("tierc-collision");
        let mut store = Store::open(&db, Durability::Full).expect("open");
        store
            .append_batch(&gen::events(0, 16, 4, 1))
            .expect("first");
        let err = store
            .append_batch(&gen::events(0, 16, 4, 2))
            .expect_err("re-appending from generation 0 must be refused, not merged");
        assert!(
            err.to_string().contains("UNIQUE"),
            "expected a primary-key collision, got {err}"
        );
        drop(store);
        cleanup(&db);
    }

    /// Arm a store the way `powercut_arm` does, minus the never-returning tail.
    fn arm(db: &Path, arm_id: &str) -> ArmedMarker {
        let mut store = Store::open(db, Durability::Full).expect("open");
        let start = next_generation(&store).expect("head");
        store
            .append_batch(&gen::events(start, 64, 8, 7))
            .expect("prefix");
        store.durable_checkpoint().expect("checkpoint");
        let m = match store.verify_chain().expect("verify") {
            ChainStatus::Intact { entries, head, .. } => ArmedMarker {
                arm_id: arm_id.to_string(),
                durable_generation: entries,
                durable_head: head,
            },
            other => panic!("{other:?}"),
        };
        drop(store);
        write_marker(db, &m).expect("marker");
        m
    }

    #[test]
    fn verifying_twice_without_re_arming_is_refused() {
        // The behaviour that produced three PASS rows from one power cut: the
        // marker survived its own verification, so re-running `verify` re-read
        // the same surviving store and recorded another pass.
        let db = fresh("tierc-reverify");
        arm(&db, "arm-A");

        let first = powercut_verify(&db, 1).expect("the first verify is legitimate");
        assert_eq!(first.arm_id, "arm-A");
        assert_eq!(first.outcome.token(), "PASS");

        let err = powercut_verify(&db, 2)
            .expect_err("re-verifying a consumed arming must be refused, not recorded");
        assert!(err.contains("no armed marker"), "{err}");

        // And nothing was appended for the refusal.
        assert_eq!(read_trials(&db).len(), 1);
        cleanup(&db);
    }

    #[test]
    fn a_repeated_trial_number_is_refused_before_anything_is_written() {
        let db = fresh("tierc-dup-trial");
        arm(&db, "arm-A");
        powercut_verify(&db, 1).expect("first");

        arm(&db, "arm-B");
        let err = powercut_verify(&db, 1).expect_err("trial 1 is already recorded");
        assert!(err.contains("already recorded"), "{err}");

        // Refused *before* writing: the ledger still holds one row, and the
        // second arming's marker is untouched so it can still be verified under
        // a correct number.
        assert_eq!(read_trials(&db).len(), 1);
        assert!(armed_marker_path(&db).exists());
        assert_eq!(
            powercut_verify(&db, 2)
                .expect("under a fresh number")
                .arm_id,
            "arm-B"
        );
        cleanup(&db);
    }

    #[test]
    fn a_repeated_arm_id_is_refused_even_under_a_fresh_trial_number() {
        // The recovery case: if the machine dies between the ledger append and
        // the marker removal, the marker comes back. Verifying it again would
        // record the same arming twice under two numbers, which is precisely
        // the inflation the arm id exists to catch.
        let db = fresh("tierc-dup-arm");
        let m = arm(&db, "arm-A");
        powercut_verify(&db, 1).expect("first");

        // Resurrect the marker, as an ill-timed cut would.
        write_marker(&db, &m).expect("marker");
        let err = powercut_verify(&db, 2).expect_err("the same arming must not be counted twice");
        assert!(err.contains("already recorded as trial 1"), "{err}");
        assert_eq!(read_trials(&db).len(), 1);
        cleanup(&db);
    }

    #[test]
    fn the_marker_survives_a_ledger_write_that_fails() {
        // Ordering is the property: evidence first, marker second. If the
        // verdict cannot be durably recorded then the arming is still
        // outstanding, and destroying the marker would lose both the verdict
        // and any record that the cut happened.
        let db = fresh("tierc-ledger-fails");
        arm(&db, "arm-A");
        // A directory where the ledger file goes: the append cannot succeed.
        std::fs::create_dir_all(trials_ledger_path(&db)).expect("block the ledger");

        let err = powercut_verify(&db, 1).expect_err("the append must fail");
        assert!(!err.is_empty());
        assert!(
            armed_marker_path(&db).exists(),
            "the marker was consumed although the verdict was never recorded"
        );
        cleanup(&db);
    }

    #[test]
    fn three_rows_carrying_one_arm_id_count_as_one_trial() {
        // The exact shape of the corrected R2 evidence: three PASS rows, one
        // real power cut. Counting rows read this as 3/5; counting armings
        // reads it as 1/5, which is what happened.
        let rows: Vec<_> = (1..=3).map(|n| passing_trial_arm(n, "arm-A")).collect();
        let o = tier_c_from_trials(&rows);
        assert_eq!(o.token(), "INCONCLUSIVE", "{}", o.detail());
        assert!(
            o.detail().contains("only 1 DISTINCT"),
            "the verdict must say how many armings it counted: {}",
            o.detail()
        );
        assert!(o.fails_the_run());
    }

    #[test]
    fn five_rows_sharing_fewer_than_five_armings_do_not_pass() {
        // Enough rows to satisfy the old row count, from two armings.
        let mut rows: Vec<_> = (1..=4).map(|n| passing_trial_arm(n, "arm-A")).collect();
        rows.push(passing_trial_arm(5, "arm-B"));
        assert_eq!(rows.len() as u64, MIN_TIER_C_TRIALS);
        let o = tier_c_from_trials(&rows);
        assert_eq!(o.token(), "INCONCLUSIVE", "{}", o.detail());
        assert!(o.detail().contains("only 2 DISTINCT"), "{}", o.detail());
    }

    #[test]
    fn four_distinct_armings_are_inconclusive_and_five_pass() {
        let four: Vec<_> = (1..=4)
            .map(|n| passing_trial_arm(n, &format!("arm-{n}")))
            .collect();
        let o = tier_c_from_trials(&four);
        assert_eq!(o.token(), "INCONCLUSIVE", "{}", o.detail());
        assert!(o.fails_the_run());

        let five: Vec<_> = (1..=5)
            .map(|n| passing_trial_arm(n, &format!("arm-{n}")))
            .collect();
        let o = tier_c_from_trials(&five);
        assert_eq!(o.token(), "PASS", "{}", o.detail());
        assert!(o.detail().contains("5 distinct"), "{}", o.detail());
        assert!(!o.fails_the_run());
    }

    #[test]
    fn rows_written_before_arm_ids_existed_cannot_be_counted() {
        // A ledger from the defective build carries no arm id. Dropping those
        // rows would make them indistinguishable from rows that were never
        // written, so they are refused loudly and the tier restarts.
        let stale = TrialRecord {
            trial: 1,
            arm_id: String::new(),
            outcome: TierOutcome::Pass("legacy row".into()),
            durable_generation: 400,
            recovered_generation: 6800,
            chain: "intact".into(),
        };
        let o = tier_c_from_trials(&[stale]);
        assert_eq!(o.token(), "INCONCLUSIVE");
        assert!(o.detail().contains("fresh database"), "{}", o.detail());
        assert!(o.fails_the_run());
    }

    #[test]
    fn two_armings_of_the_same_store_state_still_mint_different_ids() {
        // Two cuts can leave a store looking identical. If the id were derived
        // from the state alone the second arming would be indistinguishable
        // from the first, and the duplicate check would reject a legitimate
        // trial.
        //
        // A thousand back-to-back mints, because a wall clock alone would not
        // survive this on a platform with coarse resolution: the counter in the
        // hashed material is what makes it unconditional rather than a race
        // that usually goes the right way.
        let ids: std::collections::BTreeSet<String> =
            (0..1000).map(|_| mint_arm_id("deadbeef", 400)).collect();
        assert_eq!(ids.len(), 1000, "two mints collided");
        assert!(ids.iter().all(|i| i.len() == 16));
    }

    #[test]
    fn a_torn_marker_is_refused_as_torn_rather_than_as_absent() {
        // `arm` refuses while the marker file exists, so reporting an
        // unparseable one as "no armed marker" leaves the operator with two
        // refusals and no stated way forward. A marker torn by the very cut it
        // was recording is an expected state, not an exotic one.
        let db = fresh("tierc-torn-marker");
        Store::open(&db, Durability::Full).expect("open");
        std::fs::write(armed_marker_path(&db), "400\ndeadbeef\n").expect("legacy 2-line marker");

        assert!(matches!(read_marker_state(&db), MarkerState::Unreadable));

        let err = powercut_verify(&db, 1).expect_err("an unjudgeable arming must be refused");
        assert!(
            err.contains("cannot be read"),
            "the refusal must not claim the marker is absent: {err}"
        );
        assert!(
            err.contains("Delete it and re-arm"),
            "the refusal must name the way out: {err}"
        );
        assert!(read_trials(&db).is_empty(), "a refusal wrote a ledger row");
        cleanup(&db);
    }

    #[test]
    fn an_absent_marker_and_an_unreadable_one_are_different_refusals() {
        let db = fresh("tierc-marker-states");
        Store::open(&db, Durability::Full).expect("open");
        assert!(matches!(read_marker_state(&db), MarkerState::Absent));
        let absent = powercut_verify(&db, 1).expect_err("no marker");
        assert!(absent.contains("no armed marker"), "{absent}");

        std::fs::write(armed_marker_path(&db), "not a marker").expect("write");
        let torn = powercut_verify(&db, 1).expect_err("torn marker");
        assert_ne!(
            absent, torn,
            "the two states must not produce the same message — they need different actions"
        );
        cleanup(&db);
    }
}
