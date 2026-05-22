// src/verifier.rs

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use crate::verifier_store::VerifierStore;

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
    /// Timestamp of the most recent trust-state change (0 if never attested).
    pub last_trust_update_ms: u64,
    /// AK public key in PEM format. Populated on registration when provided;
    /// reserved for future TPM quote verification.
    pub ak_public_pem: Option<String>,
    /// Expected SHA-256 hex digest of PCR16 at attestation time.
    pub expected_pcr16_digest_hex: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ChallengeEntry {
    pub nonce: u64,
    pub expires_at_ms: u64,
}

/// Flap-detection result for a node over the last 5 minutes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlapStatus {
    pub node_id: String,
    /// True when ≥3 posture events were recorded in the last 300 000 ms.
    pub flapping: bool,
    pub event_count_5m: u64,
}

/// Determines whether this instance accepts mutations or is read-only.
///
/// Active     — normal operation; all mutation routes are open (subject to auth).
/// PassiveStandby — HA hot-spare; mutation routes return 503 to prevent split-brain.
///
/// Configured via AEGIS_VERIFIER_MODE env var.  Anything other than
/// "passive", "passive_standby", or "standby" (case-insensitive) → Active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerifierOperationMode {
    Active,
    PassiveStandby,
}

impl VerifierOperationMode {
    pub fn from_env() -> Self {
        match std::env::var("AEGIS_VERIFIER_MODE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "passive" | "passive_standby" | "standby" => Self::PassiveStandby,
            _ => Self::Active,
        }
    }

    pub fn allows_mutation(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// Liveness/readiness probe response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
}

/// Full state snapshot for backup and HA replication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupExport {
    pub exported_at_ms: u64,
    pub nodes: Vec<RegisteredNode>,
    pub dependencies: std::collections::HashMap<String, Vec<String>>,
    pub posture_events: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetNodePosture {
    pub node_id: String,
    pub local_status: NodeTrustState,
    pub propagated_status: FleetPosture,
    pub blocked_by: Vec<String>,
}

/// Capacity of the bounded broadcast channel for posture stream events.
/// A slow subscriber that falls this many events behind is dropped rather than
/// stalling mutation handlers.
pub const POSTURE_BROADCAST_CAPACITY: usize = 1024;

#[derive(Debug, Clone, Serialize)]
pub struct PostureStreamEvent {
    pub event_type: String,
    pub node_id: Option<String>,
    pub emitted_at_ms: u64,
    pub posture: Option<FleetNodePosture>,
}

/// Controls whether the `require_client_identity` middleware enforces the
/// `x-aegis-client-id` header (or a configured alternative).
/// Fail-closed: if `trusted_ingress_mode` is false, the check always denies.
#[derive(Debug, Clone)]
pub struct TransportIdentityConfig {
    pub trusted_ingress_mode: bool,
    pub client_id_header: String,
}

impl TransportIdentityConfig {
    pub fn from_env() -> Self {
        Self {
            trusted_ingress_mode: std::env::var("AEGIS_TRUSTED_INGRESS_MODE")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            client_id_header: std::env::var("AEGIS_CLIENT_ID_HEADER")
                .unwrap_or_else(|_| "x-aegis-client-id".to_string()),
        }
    }
}

/// Pure boundary check — no side effects, no state allocation.
/// Returns `true` only when `trusted_ingress_mode` is enabled AND the designated
/// header is present and contains a non-blank value.
pub fn validate_client_identity_headers(
    trusted_ingress_mode: bool,
    client_id_header: &str,
    headers: &axum::http::HeaderMap,
) -> bool {
    if !trusted_ingress_mode {
        return false;
    }
    let Some(value) = headers.get(client_id_header) else {
        return false;
    };
    let Ok(client_id) = value.to_str() else {
        return false;
    };
    !client_id.trim().is_empty()
}

pub struct AppState {
    pub nodes: DashMap<String, RegisteredNode>,
    pub dependency_graph: DashMap<String, Vec<String>>,
    /// Volatile in-memory challenge map — nonces are never persisted to SQLite.
    pub pending_challenges: DashMap<String, ChallengeEntry>,
    /// Durable store for nodes and dependency graph (write-through, read on boot).
    pub store: Arc<Mutex<VerifierStore>>,
    /// Runtime-mutable operational mode.
    /// true = Active (accepts mutations); false = PassiveStandby (read-only).
    /// Promotion from PassiveStandby → Active uses compare_exchange on this atomic.
    pub mode_active: Arc<AtomicBool>,
    /// Bounded broadcast channel for real-time posture stream subscribers.
    pub posture_tx: broadcast::Sender<PostureStreamEvent>,
    /// Transport identity enforcement config — reads from env at startup.
    pub transport_identity: TransportIdentityConfig,
}

impl AppState {
    pub fn new(store: VerifierStore, mode: VerifierOperationMode) -> Self {
        let (posture_tx, _) = broadcast::channel(POSTURE_BROADCAST_CAPACITY);
        Self {
            nodes: DashMap::new(),
            dependency_graph: DashMap::new(),
            pending_challenges: DashMap::new(),
            store: Arc::new(Mutex::new(store)),
            mode_active: Arc::new(AtomicBool::new(mode == VerifierOperationMode::Active)),
            posture_tx,
            transport_identity: TransportIdentityConfig::from_env(),
        }
    }

    /// Returns true if this instance is currently Active (accepting mutations).
    /// Reads the atomic — reflects runtime promotion that occurred after startup.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.mode_active.load(Ordering::SeqCst)
    }

    /// Returns the current VerifierOperationMode derived from the atomic.
    pub fn current_mode(&self) -> VerifierOperationMode {
        if self.is_active() {
            VerifierOperationMode::Active
        } else {
            VerifierOperationMode::PassiveStandby
        }
    }

    /// Persist node to SQLite then update in-memory map (fail-closed: disk before memory).
    pub fn persist_and_insert_node(&self, node: RegisteredNode) -> Result<(), ()> {
        self.store.lock()
            .map_err(|_| ())?
            .save_node(&node)
            .map_err(|_| ())?;
        self.nodes.insert(node.node_id.clone(), node);
        Ok(())
    }

    /// Persist dependency list to SQLite then update in-memory graph (fail-closed).
    pub fn persist_and_insert_deps(&self, node_id: &str, deps: Vec<String>) -> Result<(), ()> {
        self.store.lock()
            .map_err(|_| ())?
            .save_dependencies(node_id, &deps)
            .map_err(|_| ())?;
        self.dependency_graph.insert(node_id.to_string(), deps);
        Ok(())
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
        if let Some(cached) = black.get(node_id) {
            return cached.clone();
        }

        // Gray: node is currently on the active call stack — back-edge (cycle).
        // Depth limit guards against stack overflow on very deep acyclic graphs.
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

#[cfg(test)]
mod transport_identity_tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn test_disabled_ingress_rejects_even_with_valid_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-aegis-client-id", HeaderValue::from_static("edge-gateway-01"));
        assert!(!validate_client_identity_headers(false, "x-aegis-client-id", &headers));
    }

    #[test]
    fn test_enabled_ingress_missing_header_rejects() {
        let headers = HeaderMap::new();
        assert!(!validate_client_identity_headers(true, "x-aegis-client-id", &headers));
    }

    #[test]
    fn test_enabled_ingress_blank_header_rejects() {
        let mut headers = HeaderMap::new();
        headers.insert("x-aegis-client-id", HeaderValue::from_static("     "));
        assert!(!validate_client_identity_headers(true, "x-aegis-client-id", &headers));
    }

    #[test]
    fn test_enabled_ingress_valid_header_accepts() {
        let mut headers = HeaderMap::new();
        headers.insert("x-aegis-client-id", HeaderValue::from_static("trusted-mesh-sidecar"));
        assert!(validate_client_identity_headers(true, "x-aegis-client-id", &headers));
    }

    #[test]
    fn test_custom_header_name_is_respected() {
        let mut headers = HeaderMap::new();
        headers.insert("x-custom-identity", HeaderValue::from_static("fleet-controller"));
        assert!(validate_client_identity_headers(true, "x-custom-identity", &headers));
        assert!(!validate_client_identity_headers(true, "x-aegis-client-id", &headers));
    }
}
