//! WP-20 (MGA G-11) — declarative execution manager.
//!
//! The verifier runs ~7 background safety/observability loops (the posture-engine
//! worker, the SG-003 telemetry watchdog, the HA heartbeat writer + promotion
//! monitor, the OTA campaign monitor, the cert-expiry monitor, the WORM audit
//! shipper). Today they are `spawn_supervised`-ed in an IMPLICIT order hand-coded in
//! `main()` — there is no declared dependency graph, no per-task scheduling intent,
//! and no deadline accounting.
//!
//! This module is the declarative execution-management core WP-20 introduces:
//! - **[`TASK_MANIFEST`]** — the tasks as data (name + deps + criticality +
//!   scheduling intent + optional per-cycle deadline). Documentation as code, and
//!   the single source the startup order + deadline monitors derive from.
//! - **[`resolve_startup_order`]** — a topological sort of the dependency DAG giving
//!   a valid startup order (every task after its deps), FAIL-CLOSED on a duplicate
//!   name, a dependency on an unknown task, or a cycle — mirroring the system's
//!   existing gray/black DAG discipline (a cycle is never a partial/ambiguous order).
//! - **[`deadline_missed`] / [`DeadlineStats`]** — the per-task deadline-miss
//!   primitive + counter.
//!
//! **Scope (WP-20 slice 1):** the pure, unit-tested core + the self-validating
//! manifest. ADOPTING the resolved order in `main()`'s spawn sequence, APPLYING the
//! scheduling intent as real syscalls (`sched_setaffinity` / `SCHED_FIFO` where the
//! platform permits, degrade-with-warning where not), and FEEDING `DeadlineStats`
//! into `/metrics` + supervisor escalation are the recorded follow-up — the running
//! startup sequence is untouched here, so this slice changes no runtime behaviour.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// The OS scheduling INTENT for a background task. The real syscall application
/// (`SCHED_FIFO` / affinity where permitted, degrade-with-warning otherwise) is the
/// recorded follow-up; the manifest declares the intent so it is reviewable + typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulingClass {
    /// Normal tokio scheduling (the default for every task today).
    Normal,
    /// Real-time intent: elevated fixed priority (`SCHED_FIFO`) where the platform
    /// permits it. `priority` is the relative RT priority (higher = more urgent).
    RealTime { priority: u8 },
}

/// Whether a task's repeated failure escalates the fleet to fail-closed `LockedOut`
/// — mirrors the `critical` flag `spawn_supervised` already takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Criticality {
    /// Repeated failure trips the supervisor escalation (fleet → LockedOut).
    Critical,
    /// A wedged task cannot make anything unsafe; restart, never escalate.
    NonCritical,
}

/// A declarative spec for one supervised background task.
#[derive(Debug, Clone, Copy)]
pub struct TaskSpec {
    /// Stable task name (matches the `spawn_supervised` label).
    pub name: &'static str,
    /// Tasks (by name) that MUST be started before this one.
    pub deps: &'static [&'static str],
    pub criticality: Criticality,
    pub scheduling: SchedulingClass,
    /// Per-cycle deadline budget (ms). A cycle exceeding it is a deadline MISS,
    /// counted for observability. `None` = no deadline tracked (an event-driven or
    /// legitimately-slow task). Enforcement wiring is follow-up.
    pub deadline_ms: Option<u64>,
}

/// The verifier's background-task manifest — the tasks as data. Dependencies are
/// conservative (only genuine start-before relationships): the telemetry watchdog
/// and the OTA campaign monitor both feed / read the posture engine, so they start
/// after the posture-engine worker; the rest are independent loops.
pub const TASK_MANIFEST: &[TaskSpec] = &[
    TaskSpec {
        name: "posture_engine_worker",
        deps: &[],
        criticality: Criticality::Critical,
        scheduling: SchedulingClass::RealTime { priority: 10 },
        deadline_ms: None, // event-driven (drains a trigger channel)
    },
    TaskSpec {
        name: "telemetry_watchdog",
        deps: &["posture_engine_worker"], // sends PostureRecalcTrigger to the worker
        criticality: Criticality::Critical,
        scheduling: SchedulingClass::RealTime { priority: 9 },
        deadline_ms: Some(100), // AV_WATCHDOG_SWEEP_MS — a slow sweep is a miss
    },
    TaskSpec {
        name: "ha_heartbeat_writer",
        deps: &[],
        criticality: Criticality::NonCritical, // a dead heartbeat IS the failover signal
        scheduling: SchedulingClass::Normal,
        deadline_ms: None,
    },
    TaskSpec {
        name: "ha_promotion_monitor",
        deps: &[],
        criticality: Criticality::NonCritical,
        scheduling: SchedulingClass::Normal,
        deadline_ms: None,
    },
    TaskSpec {
        name: "campaign_monitor",
        deps: &["posture_engine_worker"], // resolves posture the worker populates
        criticality: Criticality::NonCritical,
        scheduling: SchedulingClass::Normal,
        deadline_ms: None,
    },
    TaskSpec {
        name: "cert_expiry_monitor",
        deps: &[],
        criticality: Criticality::NonCritical,
        scheduling: SchedulingClass::Normal,
        deadline_ms: None,
    },
    TaskSpec {
        name: "audit_shipper",
        deps: &[],
        criticality: Criticality::NonCritical,
        scheduling: SchedulingClass::Normal,
        deadline_ms: None,
    },
];

/// Why a manifest could not be resolved into a valid startup order — all fail-closed
/// (the caller aborts startup rather than run tasks in an undefined order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// Two specs share a name.
    DuplicateTask(String),
    /// A task depends on a name not in the manifest.
    MissingDependency { task: String, dep: String },
    /// The dependency graph has a cycle; the listed tasks are the unresolvable set.
    DependencyCycle(Vec<String>),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::DuplicateTask(n) => write!(f, "duplicate task {n:?}"),
            ManifestError::MissingDependency { task, dep } => {
                write!(f, "task {task:?} depends on unknown task {dep:?}")
            }
            ManifestError::DependencyCycle(c) => {
                write!(f, "dependency cycle among tasks {c:?}")
            }
        }
    }
}
impl std::error::Error for ManifestError {}

/// Resolve a manifest into a valid startup ORDER (topological sort): every task
/// appears after all its dependencies. Deterministic — among tasks whose deps are
/// all satisfied, declaration order wins. FAIL-CLOSED: a duplicate name, a
/// dependency on an unknown task, or a cycle is a hard error, never a partial or
/// ambiguous order (the same discipline as the fleet DAG's cycle → LockedOut).
pub fn resolve_startup_order(manifest: &[TaskSpec]) -> Result<Vec<&'static str>, ManifestError> {
    let n = manifest.len();
    // Index by name; reject duplicates.
    let mut index: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, t) in manifest.iter().enumerate() {
        if index.insert(t.name, i).is_some() {
            return Err(ManifestError::DuplicateTask(t.name.to_string()));
        }
    }
    // Build the DAG: an edge dep -> task (dep must start first). Validate deps exist.
    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, t) in manifest.iter().enumerate() {
        for dep in t.deps {
            let j = *index.get(dep).ok_or_else(|| ManifestError::MissingDependency {
                task: t.name.to_string(),
                dep: (*dep).to_string(),
            })?;
            dependents[j].push(i);
            in_degree[i] += 1;
        }
    }
    // Kahn's algorithm, emitting the lowest-index ready task each step for a
    // deterministic, declaration-order-preferring result.
    let mut order: Vec<&'static str> = Vec::with_capacity(n);
    let mut emitted = vec![false; n];
    loop {
        // Lowest-index task with all deps already emitted and not yet emitted.
        let next = (0..n).find(|&i| !emitted[i] && in_degree[i] == 0);
        let Some(i) = next else { break };
        emitted[i] = true;
        order.push(manifest[i].name);
        for &k in &dependents[i] {
            in_degree[k] -= 1;
        }
    }
    if order.len() != n {
        // The un-emitted tasks are exactly those in (or fed only by) a cycle.
        let cycle: Vec<String> = (0..n)
            .filter(|&i| !emitted[i])
            .map(|i| manifest[i].name.to_string())
            .collect();
        return Err(ManifestError::DependencyCycle(cycle));
    }
    Ok(order)
}

/// Pure deadline-miss decision: a task cycle that took `elapsed_ms` MISSED its
/// `deadline_ms` budget (STRICTLY over — exactly on budget is on time).
#[must_use]
pub fn deadline_missed(elapsed_ms: u64, deadline_ms: u64) -> bool {
    elapsed_ms > deadline_ms
}

/// Per-task deadline-miss counter (lock-free). `record` observes one cycle and
/// returns whether it missed; `misses`/`cycles` surface the totals for `/metrics`
/// and supervisor observability (the wiring is the recorded follow-up).
#[derive(Debug, Default)]
pub struct DeadlineStats {
    cycles: AtomicU64,
    misses: AtomicU64,
}

impl DeadlineStats {
    /// Record one task cycle against its deadline; returns `true` on a miss.
    pub fn record(&self, elapsed_ms: u64, deadline_ms: u64) -> bool {
        self.cycles.fetch_add(1, Ordering::Relaxed);
        let missed = deadline_missed(elapsed_ms, deadline_ms);
        if missed {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        missed
    }
    #[must_use]
    pub fn cycles(&self) -> u64 {
        self.cycles.load(Ordering::Relaxed)
    }
    #[must_use]
    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &'static str, deps: &'static [&'static str]) -> TaskSpec {
        TaskSpec {
            name,
            deps,
            criticality: Criticality::NonCritical,
            scheduling: SchedulingClass::Normal,
            deadline_ms: None,
        }
    }

    /// The REAL manifest resolves cleanly — self-validating in CI, so a future
    /// dependency edit that introduces a cycle / typo'd dep fails the build.
    #[test]
    fn the_real_manifest_resolves_with_deps_before_dependents() {
        let order = resolve_startup_order(TASK_MANIFEST).expect("manifest resolves");
        assert_eq!(order.len(), TASK_MANIFEST.len(), "every task is scheduled exactly once");
        let pos = |name: &str| order.iter().position(|&t| t == name).unwrap();
        // The posture engine worker starts before its dependents.
        assert!(pos("posture_engine_worker") < pos("telemetry_watchdog"));
        assert!(pos("posture_engine_worker") < pos("campaign_monitor"));
    }

    #[test]
    fn a_linear_chain_orders_deps_first() {
        let m = [spec("c", &["b"]), spec("b", &["a"]), spec("a", &[])];
        assert_eq!(resolve_startup_order(&m).unwrap(), vec!["a", "b", "c"]);
    }

    #[test]
    fn a_diamond_dag_resolves() {
        // a → {b, c} → d
        let m = [spec("a", &[]), spec("b", &["a"]), spec("c", &["a"]), spec("d", &["b", "c"])];
        let order = resolve_startup_order(&m).unwrap();
        let pos = |name: &str| order.iter().position(|&t| t == name).unwrap();
        assert_eq!(pos("a"), 0);
        assert!(pos("b") < pos("d") && pos("c") < pos("d"));
    }

    #[test]
    fn a_cycle_is_refused_fail_closed() {
        let m = [spec("a", &["c"]), spec("b", &["a"]), spec("c", &["b"])];
        match resolve_startup_order(&m) {
            Err(ManifestError::DependencyCycle(cycle)) => {
                assert_eq!(cycle.len(), 3, "all three tasks are in the cycle");
            }
            other => panic!("expected a cycle error, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_dependency_is_refused() {
        let m = [spec("a", &["ghost"])];
        assert_eq!(
            resolve_startup_order(&m),
            Err(ManifestError::MissingDependency { task: "a".into(), dep: "ghost".into() })
        );
    }

    #[test]
    fn a_duplicate_task_is_refused() {
        let m = [spec("a", &[]), spec("a", &[])];
        assert_eq!(resolve_startup_order(&m), Err(ManifestError::DuplicateTask("a".into())));
    }

    #[test]
    fn deadline_miss_is_strict_over_budget() {
        assert!(!deadline_missed(99, 100), "under budget is on time");
        assert!(!deadline_missed(100, 100), "exactly on budget is on time");
        assert!(deadline_missed(101, 100), "over budget is a miss");
    }

    #[test]
    fn deadline_stats_counts_misses() {
        let s = DeadlineStats::default();
        assert!(!s.record(50, 100));
        assert!(s.record(150, 100));
        assert!(s.record(200, 100));
        assert_eq!(s.cycles(), 3);
        assert_eq!(s.misses(), 2);
    }
}
