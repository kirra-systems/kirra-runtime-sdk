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
    /// A dependency CYCLE makes an ordering impossible. The payload is the full set
    /// of tasks that could NOT be ordered — the cycle members AND any tasks that
    /// (transitively) depend on the cycle (Kahn's algorithm leaves BOTH un-emitted),
    /// so a listed task is not necessarily a cycle member itself.
    Unorderable(Vec<String>),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::DuplicateTask(n) => write!(f, "duplicate task {n:?}"),
            ManifestError::MissingDependency { task, dep } => {
                write!(f, "task {task:?} depends on unknown task {dep:?}")
            }
            ManifestError::Unorderable(tasks) => write!(
                f,
                "unresolvable dependency cycle: these tasks could not be ordered \
                 (a cycle among them and/or tasks blocked by one): {tasks:?}"
            ),
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
        // Kahn's leaves un-emitted every task in a cycle AND every task that
        // (transitively) depends on one — the full unorderable set, not just the
        // cycle members. Report all of it (see `ManifestError::Unorderable`).
        let unorderable: Vec<String> = (0..n)
            .filter(|&i| !emitted[i])
            .map(|i| manifest[i].name.to_string())
            .collect();
        return Err(ManifestError::Unorderable(unorderable));
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

/// The manifest task names, in declaration order — the canonical set every
/// supervised loop must belong to (used to assert no spawn site drifts from the
/// manifest, in either direction).
#[must_use]
pub fn manifest_task_names() -> Vec<&'static str> {
    TASK_MANIFEST.iter().map(|t| t.name).collect()
}

// ---------------------------------------------------------------------------
// WP-20 slice 2c — deadline-miss observability
//
// slice 1 defined per-task `deadline_ms` budgets + a `DeadlineStats` counter but
// left the wiring as follow-up. This registry closes the OBSERVABILITY half: one
// `DeadlineStats` per manifest task that declares a budget, RECORDED by that
// task's loop and EXPORTED on `/metrics` (`kirra_task_deadline_*`). Lock-free —
// the map is built once at startup and the per-task counters mutate via atomics,
// so the task loop records and the scrape reads without contention. (Feeding a
// sustained miss into supervisor posture escalation is a control-path change,
// recorded as follow-up; this is read-only observability.)
// ---------------------------------------------------------------------------

/// Per-task deadline-miss counters for every manifest task with a `deadline_ms`
/// budget. Shared (`Arc`) between the task loops (which [`record`](Self::record))
/// and `/metrics` (which reads via [`append_prometheus`](Self::append_prometheus)).
#[derive(Debug)]
pub struct DeadlineRegistry {
    /// task name → (deadline budget ms, its miss counter).
    tasks: BTreeMap<&'static str, (u64, DeadlineStats)>,
}

impl DeadlineRegistry {
    /// One entry per manifest task that declares a `deadline_ms` budget.
    #[must_use]
    pub fn from_manifest(manifest: &[TaskSpec]) -> Self {
        let tasks = manifest
            .iter()
            .filter_map(|t| t.deadline_ms.map(|d| (t.name, (d, DeadlineStats::default()))))
            .collect();
        Self { tasks }
    }

    /// Record one cycle of `task` against its declared deadline. No-op (returns
    /// `false`) for a task with no budget or not in the manifest — an unbudgeted
    /// loop is never a "miss". Returns whether this cycle exceeded the budget.
    pub fn record(&self, task: &str, elapsed_ms: u64) -> bool {
        match self.tasks.get(task) {
            Some((deadline_ms, stats)) => stats.record(elapsed_ms, *deadline_ms),
            None => false,
        }
    }

    /// The miss counter for a budgeted task (`None` if unbudgeted) — for tests +
    /// direct observability.
    #[must_use]
    pub fn stats(&self, task: &str) -> Option<&DeadlineStats> {
        self.tasks.get(task).map(|(_, s)| s)
    }

    /// Append the Prometheus deadline series — one labelled line per budgeted task
    /// for cycles observed and cycles that missed. Counters (monotonic).
    pub fn append_prometheus(&self, out: &mut String) {
        use std::fmt::Write;
        out.push_str(
            "# HELP kirra_task_deadline_cycles_total Supervised-task cycles observed against a per-cycle deadline budget.\n\
             # TYPE kirra_task_deadline_cycles_total counter\n",
        );
        for (name, (_, stats)) in &self.tasks {
            let _ = writeln!(out, "kirra_task_deadline_cycles_total{{task=\"{name}\"}} {}", stats.cycles());
        }
        out.push_str(
            "# HELP kirra_task_deadline_misses_total Supervised-task cycles that exceeded their deadline budget.\n\
             # TYPE kirra_task_deadline_misses_total counter\n",
        );
        for (name, (_, stats)) in &self.tasks {
            let _ = writeln!(out, "kirra_task_deadline_misses_total{{task=\"{name}\"}} {}", stats.misses());
        }
    }
}

// ---------------------------------------------------------------------------
// WP-20 slice 2b — manifest-driven spawn dispatch
//
// slice 2a resolves the manifest at boot (a fail-closed gate); slice 2b makes the
// resolved order DRIVE the actual spawns. A `SpawnRegistry` maps each covered
// manifest task name to a spawn action; `dispatch_in_order` validates the registry
// against the manifest (fail-closed on any drift) and invokes the actions in the
// manifest's resolved dependency order — so the spawn ORDER is derived from the
// declared deps, not hand-maintained, and a spawner can never silently diverge
// from its manifest entry.
// ---------------------------------------------------------------------------

/// A single registered spawn action: invoked with the spawn context to start one
/// supervised loop.
type SpawnAction<C> = Box<dyn Fn(&C)>;

/// A registry of spawn actions keyed by manifest task name, invoked in the
/// manifest's resolved dependency order by [`dispatch_in_order`]. Generic over the
/// spawn context type `C` — each action receives `&C`, and [`dispatch_in_order`]
/// takes `ctx: &C`. The live path uses `C = Arc<ServiceState>` (actions receive
/// `&Arc<ServiceState>`); tests use a mock `C`, so the ordering + drift logic is
/// unit-tested without a runtime.
pub struct SpawnRegistry<C> {
    actions: Vec<(&'static str, SpawnAction<C>)>,
}

impl<C> Default for SpawnRegistry<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C> SpawnRegistry<C> {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self { actions: Vec::new() }
    }

    /// Register the spawn `action` for the manifest task `name`.
    pub fn register(&mut self, name: &'static str, action: impl Fn(&C) + 'static) -> &mut Self {
        self.actions.push((name, Box::new(action)));
        self
    }
}

/// Why a [`dispatch_in_order`] could not proceed — every variant is fail-closed
/// (the caller aborts startup rather than spawn from a drifted/ambiguous set).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    /// The manifest itself is unresolvable (duplicate / unknown-dep / cycle).
    Manifest(ManifestError),
    /// A `cover` task name is not in the manifest.
    UnknownTask(String),
    /// A registered spawner's task is not in `cover` (registry↔cover drift).
    SpawnerNotCovered(String),
    /// A `cover` task has no registered spawner.
    MissingSpawner(String),
    /// A task was registered more than once.
    DuplicateSpawner(String),
    /// A `cover` name appears more than once.
    DuplicateCover(String),
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DispatchError::Manifest(e) => write!(f, "manifest unresolvable: {e}"),
            DispatchError::UnknownTask(n) => write!(f, "cover task {n:?} is not in the manifest"),
            DispatchError::SpawnerNotCovered(n) => {
                write!(f, "registered spawner {n:?} is not in the cover set")
            }
            DispatchError::MissingSpawner(n) => write!(f, "cover task {n:?} has no registered spawner"),
            DispatchError::DuplicateSpawner(n) => write!(f, "task {n:?} registered more than once"),
            DispatchError::DuplicateCover(n) => write!(f, "cover task {n:?} listed more than once"),
        }
    }
}
impl std::error::Error for DispatchError {}

/// Dispatch `registry`'s spawn actions in `manifest`'s resolved dependency order,
/// restricted to the `cover` task names (the subset this registry is responsible
/// for — other lifecycle blocks spawn the rest). FAIL-CLOSED: the registry keys
/// must EXACTLY equal `cover`, and every `cover` name must be a manifest task — an
/// unknown task, a missing spawner, or a duplicate is a hard error, so a spawner
/// and its manifest entry can never silently drift. Returns the resolved order
/// actually dispatched (for the boot log). Covered tasks run in the FULL manifest's
/// resolved order (so a covered task's dep on an uncovered task — already spawned
/// by another block — is still honoured positionally).
pub fn dispatch_in_order<C>(
    registry: &SpawnRegistry<C>,
    manifest: &[TaskSpec],
    cover: &[&str],
    ctx: &C,
) -> Result<Vec<&'static str>, DispatchError> {
    use std::collections::BTreeSet;

    // No duplicate registrations.
    let mut registered: BTreeSet<&str> = BTreeSet::new();
    for (name, _) in &registry.actions {
        if !registered.insert(name) {
            return Err(DispatchError::DuplicateSpawner((*name).to_string()));
        }
    }

    // No duplicate cover entries (the "registry keys EXACTLY equal cover" contract
    // is meaningless if cover itself has dups — they would silently collapse).
    let mut cover_set: BTreeSet<&str> = BTreeSet::new();
    for c in cover {
        if !cover_set.insert(*c) {
            return Err(DispatchError::DuplicateCover((*c).to_string()));
        }
    }

    let manifest_names: BTreeSet<&str> = manifest.iter().map(|t| t.name).collect();

    // Every covered task must be a real manifest task with a registered spawner.
    for c in &cover_set {
        if !manifest_names.contains(c) {
            return Err(DispatchError::UnknownTask((*c).to_string()));
        }
        if !registered.contains(c) {
            return Err(DispatchError::MissingSpawner((*c).to_string()));
        }
    }
    // Every registered spawner must be in cover (registry↔cover drift).
    for r in &registered {
        if !cover_set.contains(r) {
            return Err(DispatchError::SpawnerNotCovered((*r).to_string()));
        }
    }

    // Resolve the FULL manifest order, then dispatch the covered actions in it.
    let order = resolve_startup_order(manifest).map_err(DispatchError::Manifest)?;
    let mut dispatched = Vec::new();
    for name in order {
        if cover_set.contains(name) {
            if let Some((_, action)) = registry.actions.iter().find(|(n, _)| *n == name) {
                action(ctx);
                dispatched.push(name);
            }
        }
    }
    Ok(dispatched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

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
            Err(ManifestError::Unorderable(tasks)) => {
                assert_eq!(tasks.len(), 3, "all three tasks are in the cycle");
            }
            other => panic!("expected an unorderable error, got {other:?}"),
        }
    }

    /// The unorderable set includes tasks merely BLOCKED by a cycle, not only the
    /// cycle members (Copilot #865): `a↔b` is the 2-cycle; `d` depends on it and is
    /// also un-emittable, so all three are reported.
    #[test]
    fn the_unorderable_set_includes_blocked_dependents() {
        let m = [spec("a", &["b"]), spec("b", &["a"]), spec("d", &["a"])];
        match resolve_startup_order(&m) {
            Err(ManifestError::Unorderable(tasks)) => {
                for t in ["a", "b", "d"] {
                    assert!(tasks.contains(&t.to_string()), "{t} must be in the unorderable set");
                }
            }
            other => panic!("expected an unorderable error, got {other:?}"),
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

    // --- WP-20 slice 2b: manifest-driven spawn dispatch ---

    /// A mock spawn context that records the ORDER its actions fire in, so the
    /// dispatch order can be asserted without a runtime.
    type Recorder = RefCell<Vec<&'static str>>;

    fn record(name: &'static str) -> impl Fn(&Recorder) {
        move |rec: &Recorder| rec.borrow_mut().push(name)
    }

    #[test]
    fn dispatch_fires_covered_actions_in_the_manifests_resolved_order() {
        // a → {b, c}; cover b and c only (a is "spawned elsewhere"). b and c fire in
        // the resolved order, AFTER a's position, regardless of registration order.
        let m = [spec("a", &[]), spec("b", &["a"]), spec("c", &["a"])];
        let mut reg = SpawnRegistry::<Recorder>::new();
        reg.register("c", record("c")); // registered c FIRST…
        reg.register("b", record("b"));
        let rec = Recorder::default();
        let dispatched = dispatch_in_order(&reg, &m, &["b", "c"], &rec).unwrap();
        // …but dispatch follows the manifest's resolved order (b before c).
        assert_eq!(dispatched, vec!["b", "c"]);
        assert_eq!(*rec.borrow(), vec!["b", "c"], "actions fired in resolved order, not registration order");
    }

    #[test]
    fn dispatch_is_fail_closed_on_registry_manifest_drift() {
        let m = [spec("a", &[]), spec("b", &[])];

        // A covered task with no spawner.
        let reg = SpawnRegistry::<Recorder>::new();
        assert_eq!(
            dispatch_in_order(&reg, &m, &["a"], &Recorder::default()),
            Err(DispatchError::MissingSpawner("a".into()))
        );

        // A spawner for a task not in `cover` (registry↔cover drift).
        let mut reg = SpawnRegistry::<Recorder>::new();
        reg.register("a", record("a"));
        reg.register("b", record("b"));
        assert_eq!(
            dispatch_in_order(&reg, &m, &["a"], &Recorder::default()),
            Err(DispatchError::SpawnerNotCovered("b".into()))
        );

        // A covered task that is not in the manifest.
        let mut reg = SpawnRegistry::<Recorder>::new();
        reg.register("ghost", record("ghost"));
        assert_eq!(
            dispatch_in_order(&reg, &m, &["ghost"], &Recorder::default()),
            Err(DispatchError::UnknownTask("ghost".into()))
        );

        // A duplicate registration.
        let mut reg = SpawnRegistry::<Recorder>::new();
        reg.register("a", record("a"));
        reg.register("a", record("a"));
        assert_eq!(
            dispatch_in_order(&reg, &m, &["a"], &Recorder::default()),
            Err(DispatchError::DuplicateSpawner("a".into()))
        );

        // A duplicate `cover` entry.
        let mut reg = SpawnRegistry::<Recorder>::new();
        reg.register("a", record("a"));
        assert_eq!(
            dispatch_in_order(&reg, &m, &["a", "a"], &Recorder::default()),
            Err(DispatchError::DuplicateCover("a".into()))
        );
    }

    #[test]
    fn dispatch_propagates_an_unresolvable_manifest() {
        let m = [spec("a", &["b"]), spec("b", &["a"])];
        let mut reg = SpawnRegistry::<Recorder>::new();
        reg.register("a", record("a"));
        reg.register("b", record("b"));
        assert!(matches!(
            dispatch_in_order(&reg, &m, &["a", "b"], &Recorder::default()),
            Err(DispatchError::Manifest(ManifestError::Unorderable(_)))
        ));
    }

    #[test]
    fn manifest_task_names_lists_every_task() {
        assert_eq!(manifest_task_names().len(), TASK_MANIFEST.len());
        assert!(manifest_task_names().contains(&"posture_engine_worker"));
    }

    // --- WP-20 slice 2c: deadline-miss observability ---

    #[test]
    fn deadline_registry_covers_only_budgeted_manifest_tasks() {
        let reg = DeadlineRegistry::from_manifest(TASK_MANIFEST);
        // telemetry_watchdog declares deadline_ms = Some(100); the rest are None.
        assert!(reg.stats("telemetry_watchdog").is_some(), "the watchdog is budgeted");
        assert!(reg.stats("posture_engine_worker").is_none(), "an unbudgeted task is not tracked");
        assert!(reg.stats("not_a_task").is_none());
    }

    #[test]
    fn deadline_registry_records_only_budgeted_tasks() {
        let reg = DeadlineRegistry::from_manifest(TASK_MANIFEST);
        // Under budget, then over budget (deadline is 100).
        assert!(!reg.record("telemetry_watchdog", 50));
        assert!(reg.record("telemetry_watchdog", 150), "over budget is a miss");
        let s = reg.stats("telemetry_watchdog").unwrap();
        assert_eq!(s.cycles(), 2);
        assert_eq!(s.misses(), 1);
        // Recording an unbudgeted / unknown task is a harmless no-op.
        assert!(!reg.record("posture_engine_worker", 9_999));
        assert!(!reg.record("ghost", 9_999));
    }

    #[test]
    fn deadline_registry_prometheus_has_labelled_counters() {
        let reg = DeadlineRegistry::from_manifest(TASK_MANIFEST);
        reg.record("telemetry_watchdog", 150); // one miss
        let mut out = String::new();
        reg.append_prometheus(&mut out);
        assert!(out.contains("# TYPE kirra_task_deadline_cycles_total counter"));
        assert!(out.contains("kirra_task_deadline_cycles_total{task=\"telemetry_watchdog\"} 1"));
        assert!(out.contains("kirra_task_deadline_misses_total{task=\"telemetry_watchdog\"} 1"));
    }
}
