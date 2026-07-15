//! ADR-0035 Stage 3 (slice 3d) — the gray/black two-set dependency-DAG posture
//! traversal (INVARIANT #4), lifted VERBATIM out of `AppState` into the reviewable
//! safety-authority crate.
//!
//! The algorithm is BYTE-IDENTICAL to its prior `AppState` form — it is never
//! mocked (INVARIANT #4). It was already pure with respect to `AppState`: it reads
//! only the node registry (`nodes`) and the dependency edges (`dependency_graph`),
//! so the extraction takes those two maps as parameters and `AppState` keeps the
//! `calculate_posture*` / `calculate_fleet_posture` methods as thin delegators
//! (every existing `app.calculate_*` caller is unchanged). The fields themselves
//! stay on `AppState` for now — this slice moves the ALGORITHM (the safety-critical
//! core, and the thing the Kani proofs + `shared_memo_equivalence_tests` pin), not
//! the storage.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dashmap::DashMap;
use kirra_core::{FleetPosture, NodeTrustState, RegisteredNode};

use crate::FleetNodePosture;

/// Maximum recursion depth for dependency graph traversal.
/// Prevents stack overflow on pathologically deep (but acyclic) graphs.
pub const MAX_DEPENDENCY_DEPTH: usize = 10;

/// Single-root posture with a fresh memo.
pub fn calculate_posture(
    nodes: &DashMap<String, RegisteredNode>,
    dependency_graph: &DashMap<String, Vec<String>>,
    node_id: &str,
) -> FleetNodePosture {
    let mut black: HashMap<Arc<str>, Arc<FleetNodePosture>> = HashMap::new();
    calculate_posture_memoized(nodes, dependency_graph, node_id, &mut black)
}

/// As [`calculate_posture`], but reuses a CALLER-OWNED `black` memo across
/// many roots (review P3). A node's fully-evaluated posture is
/// root-INDEPENDENT (it is a property of the node + its dependency subgraph,
/// not of which root reached it), so the whole-fleet recalc can share ONE
/// memo: a node depended on by K others is traversed ONCE (the first root to
/// reach it) and then black-hit by the rest — turning the fleet recalc from
/// O(N·(N+E)) into ~O(N+E). The gray (cycle-detection) set is still FRESH per
/// call: it tracks the CURRENT root's active call stack, and the cycle /
/// depth sentinels are deliberately NOT memoized (never inserted into
/// `black`), so sharing `black` only ever reuses fully-resolved verdicts.
/// The memo stores `Arc<FleetNodePosture>` so a hit is an `Arc::clone`
/// (refcount bump) rather than a deep clone of the node id + `blocked_by`
/// vector. Node ids are interned as `Arc<str>` (review P5): the memo key, the
/// gray set, every `node_id` field and every `blocked_by` entry share one
/// allocation per distinct id, so a hot dependency referenced by K parents
/// costs one id allocation rather than K+ `String`s. `Arc<str>: Borrow<str>`
/// lets both maps be probed with a plain `&str` (no allocation on a lookup).
pub fn calculate_posture_memoized(
    nodes: &DashMap<String, RegisteredNode>,
    dependency_graph: &DashMap<String, Vec<String>>,
    node_id: &str,
    black: &mut HashMap<Arc<str>, Arc<FleetNodePosture>>,
) -> FleetNodePosture {
    let mut gray: HashSet<Arc<str>> = HashSet::new();
    // Recursion bound for the stack-overflow backstop below. The gray set
    // already makes a repeated node on the active path impossible without a
    // cycle, so the longest *acyclic* path visits at most as many DISTINCT
    // ids as exist in the graph. That universe is NOT just `nodes` (B8):
    // `dependency_graph` may carry edges to ids never registered as nodes (a
    // dangling/forward dependency), and the traversal recurses into them
    // (they resolve to `Unknown`). A chain of such unregistered ids can be
    // LONGER than `nodes.len()`, so the old `nodes.len()` bound could fire on
    // a perfectly acyclic graph and spuriously report LockedOut. Bound by the
    // full id universe instead: `nodes.len() + dependency_graph.len()`
    // over-approximates the distinct-id count (every id with an outgoing edge
    // is a dependency_graph key; a leaf id has no edge and cannot extend a
    // path), so the depth check CANNOT fire on a valid acyclic graph,
    // registered or not — keeping it a pure stack-safety guard rather than a
    // (traversal-order-dependent) semantic verdict. Floored at
    // MAX_DEPENDENCY_DEPTH so a tiny fleet still carries the documented guard.
    let max_depth = (nodes.len() + dependency_graph.len()).max(MAX_DEPENDENCY_DEPTH);
    let posture = recursive_calculate(
        nodes,
        dependency_graph,
        node_id,
        &mut gray,
        black,
        0,
        max_depth,
    );
    (*posture).clone()
}

/// Whole-fleet per-node posture in ONE pass with a SHARED `black` memo
/// (review P3): O(N+E) instead of the O(N·(N+E)) of calling
/// [`calculate_posture`] once per node (each with a fresh memo). Result is
/// IDENTICAL to mapping `calculate_posture` over every registered node — the
/// per-node verdict is root-independent, so one shared memo only ever reuses
/// fully-resolved verdicts (proven in `shared_memo_equivalence_tests`).
///
/// The node-id set is snapshotted FIRST (the `nodes` shard guards are dropped
/// before any traversal), so the re-entrant `nodes.get(...)` inside the DAG
/// walk cannot deadlock against a held `nodes.iter()` guard — the same B1
/// hazard the posture-engine recalc already avoids.
pub fn calculate_fleet_posture(
    nodes: &DashMap<String, RegisteredNode>,
    dependency_graph: &DashMap<String, Vec<String>>,
) -> Vec<FleetNodePosture> {
    // SAFETY: SG-RED-2 — snapshot iteration prevents nested DashMap locks.
    // SAFETY: SG-RED-3 — posture DAG recalculation must be deadlock-free.
    let ids: Vec<String> = nodes.iter().map(|e| e.key().clone()).collect();
    let mut black: HashMap<Arc<str>, Arc<FleetNodePosture>> = HashMap::new();
    ids.iter()
        .map(|id| calculate_posture_memoized(nodes, dependency_graph, id.as_str(), &mut black))
        .collect()
}

/// The gray/black two-set DFS (INVARIANT #4). BYTE-IDENTICAL to the prior
/// `AppState::recursive_calculate` — self-recursive, reads only `nodes` +
/// `dependency_graph`.
#[allow(clippy::too_many_arguments)]
pub fn recursive_calculate(
    nodes: &DashMap<String, RegisteredNode>,
    dependency_graph: &DashMap<String, Vec<String>>,
    node_id: &str,
    gray: &mut HashSet<Arc<str>>,
    black: &mut HashMap<Arc<str>, Arc<FleetNodePosture>>,
    depth: usize,
    max_depth: usize,
) -> Arc<FleetNodePosture> {
    // Black: node already fully evaluated in this pass — reuse without
    // re-traversal. `Arc::clone` is a refcount bump, not a deep copy (P5).
    if let Some(cached) = black.get(node_id) {
        return Arc::clone(cached);
    }

    // Gray: node is currently on the active call stack — a back-edge, i.e. a
    // genuine cycle. A circular dependency has no well-defined posture →
    // fail-closed LockedOut (tagged CYCLE_DETECTED). Deterministic: any
    // traversal that re-enters the cycle hits a gray node regardless of the
    // entry path, so this is a true property of the graph, not the walk order.
    // NOT memoized (not inserted into `black`) — a transient per-DFS sentinel,
    // which is what makes sharing `black` across roots sound (P3).
    if gray.contains(node_id) {
        return Arc::new(FleetNodePosture {
            node_id: Arc::from(node_id),
            local_status: NodeTrustState::Unknown,
            propagated_status: FleetPosture::LockedOut,
            blocked_by: vec![Arc::from("CYCLE_DETECTED")],
        });
    }

    // Depth backstop — a stack-overflow guard, NOT a semantic verdict. Because
    // the gray set bounds any acyclic path to the number of distinct ids in the
    // graph and `max_depth` covers that whole id universe (registered nodes +
    // dependency-graph edges, see above), this branch is unreachable on a valid
    // acyclic graph (a cycle is always caught above first). It exists only so a
    // pathological graph degrades to fail-closed LockedOut instead of
    // overflowing the stack. The prior `depth >= MAX_DEPENDENCY_DEPTH` fixed cap
    // conflated this with graph validity: because the sentinel was not memoized,
    // a node reachable both within and beyond 10 hops resolved to LockedOut or
    // Nominal depending on which path the DFS reached it by FIRST — so the whole
    // fleet's posture depended on dependency *insertion order* rather than the
    // graph's trust state. Bounding by the id-universe count removes that flip
    // entirely, including for chains of unregistered dependency ids (B8).
    if depth >= max_depth {
        return Arc::new(FleetNodePosture {
            node_id: Arc::from(node_id),
            local_status: NodeTrustState::Unknown,
            propagated_status: FleetPosture::LockedOut,
            blocked_by: vec![Arc::from("MAX_DEPTH_EXCEEDED")],
        });
    }

    // Mint the interned id ONCE for this node; every subsequent use (gray
    // set, the result's `node_id`, the `black` key, and each parent's
    // `blocked_by`) is an `Arc::clone` refcount bump, not a new allocation.
    let id: Arc<str> = Arc::from(node_id);
    gray.insert(Arc::clone(&id));

    let local_status = nodes
        .get(node_id)
        .map(|n| n.status.clone())
        .unwrap_or(NodeTrustState::Unknown);

    let deps = dependency_graph
        .get(node_id)
        .map(|d| d.value().clone())
        .unwrap_or_default();

    let mut blocked_by: Vec<Arc<str>> = Vec::new();
    let mut has_locked_out_dep = false;

    for dep_id in &deps {
        let dep_posture = recursive_calculate(
            nodes,
            dependency_graph,
            dep_id,
            gray,
            black,
            depth + 1,
            max_depth,
        );
        match &dep_posture.propagated_status {
            FleetPosture::LockedOut => {
                // Share the dep's interned id rather than re-allocating it.
                blocked_by.push(Arc::clone(&dep_posture.node_id));
                has_locked_out_dep = true;
            }
            FleetPosture::Degraded => {
                blocked_by.push(Arc::clone(&dep_posture.node_id));
            }
            FleetPosture::Nominal => {}
        }
    }

    let propagated_status = match &local_status {
        NodeTrustState::Untrusted(_) => FleetPosture::LockedOut,
        _ if has_locked_out_dep => FleetPosture::LockedOut,
        _ if !blocked_by.is_empty() => FleetPosture::Degraded,
        NodeTrustState::Unknown => FleetPosture::Degraded,
        NodeTrustState::Trusted => FleetPosture::Nominal,
    };

    let posture = Arc::new(FleetNodePosture {
        node_id: Arc::clone(&id),
        local_status,
        propagated_status,
        blocked_by,
    });

    gray.remove(node_id);
    black.insert(id, Arc::clone(&posture));

    posture
}
