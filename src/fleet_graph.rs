//! ADR-0035 Stage 3 (slice 3k) — the in-memory fleet trust graph, lifted off the
//! `AppState` god-object into a cohesive field façade (the 3c/3e/3f/3g/3h/3i/3j
//! pattern), byte-identical.
//!
//! Unlike the peripheral plumbing slices (writer handles, observability counters),
//! these two DashMaps are the CORE fleet-legitimacy state the gray/black DAG
//! traversal reads TOGETHER on every posture recalculation, so they earn a named
//! home rather than hanging loose off the god-object:
//!
//! - `nodes` — the registered-node registry (`node_id` → [`RegisteredNode`]:
//!   trust state, AK PEM, PCR16). The in-memory mirror of the durable `nodes`
//!   table; hydrated on boot and kept write-through (disk before memory,
//!   INVARIANT #12).
//! - `dependency_graph` — the fleet dependency edges (`node_id` →
//!   `Vec<dependent_node_id>`). The adjacency list the
//!   `kirra_safety_authority::dag::recursive_calculate` gray/black traversal walks.
//!
//! Both are `DashMap`s (lock-free interior mutability, shared via the outer
//! `Arc<AppState>`), so the move is a pure relocation — no `&mut self`, no ordering
//! change, no behaviour change. Grouped in a ROOT-crate leaf (not
//! `kirra-safety-authority`: the traversal ALGORITHM already lives there per ADR-0035
//! Stage 3d; this is the mutable state it reads, which stays with the control plane).
//! Embedded on `AppState` as `app.fleet`; reached as `app.fleet.nodes` /
//! `app.fleet.dependency_graph`. The persist-then-insert ordering (INVARIANT #12) and
//! the C2 (#1031) shard-locked read-modify-write are unchanged — they operate on
//! `self.fleet.nodes` exactly as they did on `self.nodes`.

use dashmap::DashMap;

use kirra_core::RegisteredNode;

/// The in-memory fleet trust graph (ADR-0035 slice 3k): the registered-node
/// registry + the dependency adjacency list the DAG traversal reads together.
#[derive(Debug, Default)]
pub struct FleetGraph {
    /// Registered-node registry: `node_id` → [`RegisteredNode`]. In-memory mirror
    /// of the durable `nodes` table; write-through (disk before memory,
    /// INVARIANT #12). Field semantics UNCHANGED from the prior `app.nodes`.
    pub nodes: DashMap<String, RegisteredNode>,
    /// Fleet dependency edges: `node_id` → `Vec<dependent_node_id>`. The adjacency
    /// list the gray/black DAG traversal walks. Field semantics UNCHANGED from the
    /// prior `app.dependency_graph`.
    pub dependency_graph: DashMap<String, Vec<String>>,
}

impl FleetGraph {
    /// Construct an empty fleet graph — byte-identical to the prior two inline
    /// `DashMap::new()` field initializers in `AppState::new`.
    pub fn new() -> Self {
        Self {
            nodes: DashMap::new(),
            dependency_graph: DashMap::new(),
        }
    }
}
