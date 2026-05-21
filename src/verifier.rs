// src/verifier.rs

use std::collections::{HashMap, HashSet};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};

/// Maximum recursion depth for dependency graph traversal.
/// Prevents stack overflow on pathologically deep (but acyclic) graphs.
pub const MAX_DEPENDENCY_DEPTH: usize = 10;

/// Nonces expire after 30 seconds — long enough for a challenged node to respond,
/// short enough to limit the replay window if a response is intercepted.
const CHALLENGE_TTL_MS: u64 = 30_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NodeTrustState {
    Trusted,
    Untrusted(String),
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FleetPosture {
    Nominal,
    Degraded,
    LockedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredNode {
    pub node_id: String,
    pub status: NodeTrustState,
    pub registered_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ChallengeEntry {
    pub nonce: u64,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetNodePosture {
    pub node_id: String,
    pub local_status: NodeTrustState,
    pub propagated_status: FleetPosture,
    pub blocked_by: Vec<String>,
}

pub struct AppState {
    pub nodes: DashMap<String, RegisteredNode>,
    pub dependency_graph: DashMap<String, Vec<String>>,
    pub pending_challenges: DashMap<String, ChallengeEntry>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            nodes: DashMap::new(),
            dependency_graph: DashMap::new(),
            pending_challenges: DashMap::new(),
        }
    }

    pub fn calculate_posture(&self, node_id: &str) -> FleetNodePosture {
        let mut gray: HashSet<String> = HashSet::new();
        let mut black: HashMap<String, FleetNodePosture> = HashMap::new();
        self.recursive_calculate(node_id, &mut gray, &mut black, 0)
    }

    fn recursive_calculate(
        &self,
        node_id: &str,
        gray: &mut HashSet<String>,
        black: &mut HashMap<String, FleetNodePosture>,
        depth: usize,
    ) -> FleetNodePosture {
        // Black: node already fully evaluated in this pass — reuse without re-traversal.
        // This handles diamond DAGs: if A→B→D and A→C→D, D is computed once and
        // memoized; the second visit through C returns the cached result rather than
        // triggering a false cycle alarm.
        if let Some(cached) = black.get(node_id) {
            return cached.clone();
        }

        // Gray: node is currently on the active call stack — this is a real back-edge
        // (cycle). Depth limit guards against stack overflow on very deep acyclic graphs.
        // Both cases fail closed: LockedOut with a diagnostic tag.
        if gray.contains(node_id) || depth >= MAX_DEPENDENCY_DEPTH {
            return FleetNodePosture {
                node_id: node_id.to_string(),
                local_status: NodeTrustState::Unknown,
                propagated_status: FleetPosture::LockedOut,
                blocked_by: vec!["INVALID_GRAPH_CONFIG".to_string()],
            };
        }

        gray.insert(node_id.to_string());

        let local_status = self.nodes
            .get(node_id)
            .map(|n| n.status.clone())
            .unwrap_or(NodeTrustState::Unknown);

        let deps = self.dependency_graph
            .get(node_id)
            .map(|d| d.value().clone())
            .unwrap_or_default();

        let mut blocked_by: Vec<String> = Vec::new();
        let mut has_locked_out_dep = false;

        for dep_id in &deps {
            let dep_posture = self.recursive_calculate(dep_id, gray, black, depth + 1);
            match &dep_posture.propagated_status {
                FleetPosture::LockedOut => {
                    blocked_by.push(dep_id.clone());
                    has_locked_out_dep = true;
                }
                FleetPosture::Degraded => {
                    blocked_by.push(dep_id.clone());
                }
                FleetPosture::Nominal => {}
            }
        }

        // Severity propagation rules (in priority order):
        //   1. Local Untrusted               → LockedOut  (own node is compromised)
        //   2. Any dependency is LockedOut   → LockedOut  (do not soften to Degraded)
        //   3. Any dependency is Degraded    → Degraded
        //   4. Local Unknown                 → Degraded   (unverified = not Nominal)
        //   5. Local Trusted, all deps Nominal → Nominal
        let propagated_status = match &local_status {
            NodeTrustState::Untrusted(_) => FleetPosture::LockedOut,
            _ if has_locked_out_dep => FleetPosture::LockedOut,
            _ if !blocked_by.is_empty() => FleetPosture::Degraded,
            NodeTrustState::Unknown => FleetPosture::Degraded,
            NodeTrustState::Trusted => FleetPosture::Nominal,
        };

        let posture = FleetNodePosture {
            node_id: node_id.to_string(),
            local_status,
            propagated_status,
            blocked_by,
        };

        // Backtrack: remove from gray (no longer on call stack).
        // Memoize in black: future visits to this node return this result directly.
        gray.remove(node_id);
        black.insert(node_id.to_string(), posture.clone());

        posture
    }

    /// Consume a challenge nonce. Returns false if nonce is absent, expired, or mismatched.
    pub fn consume_challenge(&self, node_id: &str, nonce: u64, now_ms: u64) -> bool {
        let entry = match self.pending_challenges.remove(node_id) {
            Some((_, e)) => e,
            None => return false,
        };
        if now_ms > entry.expires_at_ms { return false; }
        entry.nonce == nonce
    }

    /// Issue a fresh challenge nonce for the given node. Overwrites any prior pending challenge.
    pub fn issue_challenge(&self, node_id: &str, nonce: u64, now_ms: u64) {
        self.pending_challenges.insert(node_id.to_string(), ChallengeEntry {
            nonce,
            expires_at_ms: now_ms + CHALLENGE_TTL_MS,
        });
    }
}
