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

pub fn tier_c_not_run() -> TierOutcome {
    TierOutcome::NotRun(
        "a real power cut cannot be performed from software: nothing in-process can \
         tell an honest fsync from a device cache that acknowledged and buffered it. \
         Manual procedure in docs/hardware/JETSON_WM2_PERSISTENCE_DRILL.md §6; until \
         it is run and its result recorded, no durability claim is supported."
            .to_string(),
    )
}

pub fn run(
    path: &Path,
    durability: Durability,
    run_ms: u64,
    prefix_events: u64,
    tail_events: u64,
) -> CrashResult {
    CrashResult {
        tier_a_sigkill: tier_a_sigkill(path, durability, run_ms),
        tier_b_wal_loss: tier_b_wal_loss(path, durability, prefix_events, tail_events),
        tier_c_power_cut: tier_c_not_run(),
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
}
