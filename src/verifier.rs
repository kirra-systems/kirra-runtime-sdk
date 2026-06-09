// src/verifier.rs

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
/// Configured via KIRRA_VERIFIER_MODE env var.  Anything other than
/// "passive", "passive_standby", or "standby" (case-insensitive) → Active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerifierOperationMode {
    Active,
    PassiveStandby,
}

impl VerifierOperationMode {
    pub fn from_env() -> Self {
        match std::env::var("KIRRA_VERIFIER_MODE")
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
/// `x-kirra-client-id` header (or a configured alternative).
/// Fail-closed: if `trusted_ingress_mode` is false, the check always denies.
#[derive(Debug, Clone)]
pub struct TransportIdentityConfig {
    pub trusted_ingress_mode: bool,
    pub client_id_header: String,
}

impl TransportIdentityConfig {
    pub fn from_env() -> Self {
        Self {
            trusted_ingress_mode: std::env::var("KIRRA_TRUSTED_INGRESS_MODE")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            client_id_header: std::env::var("KIRRA_CLIENT_ID_HEADER")
                .unwrap_or_else(|_| "x-kirra-client-id".to_string()),
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

/// In-memory streak counter for RSS recovery hysteresis.
/// Tracks consecutive safe RSS reports and the streak start timestamp.
pub struct RssRecoveryStreak {
    pub count: u32,
    pub start_ms: u64,
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
    /// LOCAL only — coordinates this process. Distributed split-brain is
    /// prevented by `held_epoch` against the durable `ha_state` row.
    pub mode_active: Arc<AtomicBool>,
    /// HA fencing token (durable epoch) currently claimed by this instance.
    /// 0 = no claim yet. The mutation gate compares this to the DB epoch on
    /// every state-mutating request; if they diverge this node has been
    /// fenced (another instance promoted) and must self-demote.
    pub held_epoch: Arc<AtomicU64>,
    /// Pass B1 cache (S3 / #115): the most recently observed durable `ha_state`
    /// epoch. The mutation gate (`policy_layer.rs::enforce_posture_routing`)
    /// reads this atomically instead of taking `store.lock()` + `current_epoch()`
    /// per request. Re-stamped by `perform_promotion` after a successful
    /// `try_claim_epoch` (Release) and by the heartbeat writer on every
    /// `HEARTBEAT_INTERVAL_MS` tick (Release). 0 = "not yet observed";
    /// the gate treats 0 the same way the previous DB-read path treated
    /// an unreadable epoch — fall through and rely on the existing
    /// `held == 0` / non-Active checks for fail-closed.
    pub cached_db_epoch: Arc<AtomicU64>,
    /// Pass B2 (S3 / #115): bounded mpsc Sender for the audit-writer task.
    /// The deny arm of the actuator-safety-envelope middleware does
    /// `audit_writer_tx.get().try_send(job)` to push the kinematic-violation
    /// audit record off the verdict path. `None` (writer not installed)
    /// causes the deny arm to fall back to the previous inline lock+save
    /// path — production main always installs the writer at startup; tests
    /// that don't may still exercise the verdict path. Use
    /// `install_audit_writer` once to install.
    pub audit_writer_tx:
        std::sync::OnceLock<tokio::sync::mpsc::Sender<crate::audit_writer::AuditWriteJob>>,
    /// Learning-loop capture channel (Phase 1, #190) — sibling of
    /// `audit_writer_tx`. The actuator gateway `try_send`s a small
    /// `CaptureRecord` here off the verdict path. `None` (writer not installed,
    /// e.g. capture disabled or tests) → the gateway emit is a pure no-op.
    /// Installed once via `install_capture_writer` at startup, only when
    /// `capture::capture_enabled()`.
    pub capture_writer_tx:
        std::sync::OnceLock<tokio::sync::mpsc::Sender<crate::capture::CaptureRecord>>,
    /// Monotonic per-decision sequence for the capture join key. Incremented at
    /// the gateway emit; non-safety (capture only).
    pub capture_decision_seq: Arc<AtomicU64>,
    /// Bounded broadcast channel for real-time posture stream subscribers.
    pub posture_tx: broadcast::Sender<PostureStreamEvent>,
    /// Transport identity enforcement config — reads from env at startup.
    pub transport_identity: TransportIdentityConfig,
    /// True while an RSS safe-distance violation is active (recalculate elevates to Degraded).
    pub rss_active_violation: Arc<AtomicBool>,
    /// Recovery streak for clearing an active RSS violation.
    pub rss_recovery_streak: Arc<Mutex<RssRecoveryStreak>>,
}

impl AppState {
    pub fn new(store: VerifierStore, mode: VerifierOperationMode) -> Self {
        let (posture_tx, _) = broadcast::channel(POSTURE_BROADCAST_CAPACITY);
        // Pass B1 cache seed (S3 / #115): read the current durable epoch
        // before wrapping the store in the Mutex so the gate has a fresh
        // value before any request lands. Unreadable → 0 (gate falls through).
        let initial_db_epoch = store.current_epoch().unwrap_or(0);
        Self {
            nodes: DashMap::new(),
            dependency_graph: DashMap::new(),
            pending_challenges: DashMap::new(),
            store: Arc::new(Mutex::new(store)),
            mode_active: Arc::new(AtomicBool::new(mode == VerifierOperationMode::Active)),
            held_epoch: Arc::new(AtomicU64::new(0)),
            cached_db_epoch: Arc::new(AtomicU64::new(initial_db_epoch)),
            audit_writer_tx: std::sync::OnceLock::new(),
            capture_writer_tx: std::sync::OnceLock::new(),
            capture_decision_seq: Arc::new(AtomicU64::new(0)),
            posture_tx,
            transport_identity: TransportIdentityConfig::from_env(),
            rss_active_violation: Arc::new(AtomicBool::new(false)),
            rss_recovery_streak: Arc::new(Mutex::new(RssRecoveryStreak { count: 0, start_ms: 0 })),
        }
    }

    /// Returns true if this instance is currently Active (accepting mutations).
    /// Reads the atomic — reflects runtime promotion that occurred after startup.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.mode_active.load(Ordering::SeqCst)
    }

    /// Install the audit-writer mpsc Sender. Called once at startup, after
    /// `audit_writer::spawn_audit_writer`. Subsequent calls are ignored
    /// (OnceLock semantics) and logged as a duplicate-install warning.
    pub fn install_audit_writer(
        &self,
        tx: tokio::sync::mpsc::Sender<crate::audit_writer::AuditWriteJob>,
    ) {
        if self.audit_writer_tx.set(tx).is_err() {
            tracing::warn!(
                "audit writer Sender already installed — ignoring duplicate install"
            );
        }
    }

    /// Install the capture-writer mpsc Sender (learning-loop Phase 1, #190).
    /// Called once at startup, after `capture::spawn_capture_writer`, and only
    /// when `capture::capture_enabled()`. Mirrors `install_audit_writer`.
    pub fn install_capture_writer(
        &self,
        tx: tokio::sync::mpsc::Sender<crate::capture::CaptureRecord>,
    ) {
        if self.capture_writer_tx.set(tx).is_err() {
            tracing::warn!(
                "capture writer Sender already installed — ignoring duplicate install"
            );
        }
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

    /// Mark a registered node `Untrusted` (e.g. a CANopen NMT node-offline,
    /// #84) so the next DAG recalc reflects it. Disk-first (invariant #12):
    /// re-persists via `persist_and_insert_node`.
    ///
    /// Returns `Ok(true)` if the node existed and was updated, `Ok(false)` if
    /// no such node is registered (the caller fail-closes on this), `Err(())`
    /// on a store failure.
    pub fn mark_node_untrusted(
        &self,
        node_id: &str,
        reason: &str,
        now_ms: u64,
    ) -> Result<bool, ()> {
        let Some(existing) = self.nodes.get(node_id).map(|n| n.clone()) else {
            return Ok(false);
        };
        let updated = RegisteredNode {
            status: NodeTrustState::Untrusted(reason.to_string()),
            last_trust_update_ms: now_ms,
            ..existing
        };
        self.persist_and_insert_node(updated)?;
        Ok(true)
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
        // Store hygiene (#147): prune expired pending challenges so stale
        // entries for nodes that never re-attested do not linger. The map is
        // already bounded (keyed by node_id, per-node overwrite); this only
        // drops timed-out entries — it never introduces unbounded growth.
        self.pending_challenges.retain(|_, e| now_ms <= e.expires_at_ms);
        self.pending_challenges.insert(node_id.to_string(), ChallengeEntry {
            nonce,
            expires_at_ms: now_ms + CHALLENGE_TTL_MS,
        });
    }
}

/// Generate a fresh, unpredictable attestation challenge nonce (#147).
///
/// Sourced from the operating-system CSPRNG (`getrandom`, the same OS entropy
/// source that backs `OsRng`) — NOT from the wall clock. A `SystemTime`-derived
/// nonce is predictable (an attacker who knows the issue time knows the nonce)
/// and can collide for two challenges issued within the same nanosecond. The
/// remaining nonce-lifecycle invariants — single-use, TTL-bounded, node-bound —
/// are enforced by the challenge store (`issue_challenge` / `consume_challenge`);
/// this function supplies the *unpredictability* half.
///
/// Fail-closed: if the OS CSPRNG is unavailable we panic rather than fall back
/// to a weak/predictable source — no secure nonce can be issued without entropy.
#[must_use]
pub fn generate_challenge_nonce() -> u64 {
    let mut bytes = [0u8; 8];
    getrandom::getrandom(&mut bytes)
        .expect("OS CSPRNG (getrandom) unavailable — cannot issue a secure attestation nonce");
    u64::from_le_bytes(bytes)
}

#[cfg(test)]
mod transport_identity_tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn test_disabled_ingress_rejects_even_with_valid_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-kirra-client-id", HeaderValue::from_static("edge-gateway-01"));
        assert!(!validate_client_identity_headers(false, "x-kirra-client-id", &headers));
    }

    #[test]
    fn test_enabled_ingress_missing_header_rejects() {
        let headers = HeaderMap::new();
        assert!(!validate_client_identity_headers(true, "x-kirra-client-id", &headers));
    }

    #[test]
    fn test_enabled_ingress_blank_header_rejects() {
        let mut headers = HeaderMap::new();
        headers.insert("x-kirra-client-id", HeaderValue::from_static("     "));
        assert!(!validate_client_identity_headers(true, "x-kirra-client-id", &headers));
    }

    #[test]
    fn test_enabled_ingress_valid_header_accepts() {
        let mut headers = HeaderMap::new();
        headers.insert("x-kirra-client-id", HeaderValue::from_static("trusted-mesh-sidecar"));
        assert!(validate_client_identity_headers(true, "x-kirra-client-id", &headers));
    }

    #[test]
    fn test_custom_header_name_is_respected() {
        let mut headers = HeaderMap::new();
        headers.insert("x-custom-identity", HeaderValue::from_static("fleet-controller"));
        assert!(validate_client_identity_headers(true, "x-custom-identity", &headers));
        assert!(!validate_client_identity_headers(true, "x-kirra-client-id", &headers));
    }
}

#[cfg(test)]
mod mark_node_untrusted_tests {
    use super::*;
    use crate::verifier_store::VerifierStore;

    fn app() -> AppState {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        AppState::new(store, VerifierOperationMode::Active)
    }

    fn trusted_node(id: &str) -> RegisteredNode {
        RegisteredNode {
            node_id: id.to_string(),
            status: NodeTrustState::Trusted,
            registered_at_ms: 1,
            last_trust_update_ms: 1,
            ak_public_pem: None,
            expected_pcr16_digest_hex: None,
        }
    }

    // #84: marking the resolved fleet node offline must be EFFECTFUL — the DAG
    // recalc flips the node's posture from Nominal to LockedOut.
    #[test]
    fn marking_registered_node_untrusted_is_effectful() {
        let app = app();
        app.persist_and_insert_node(trusted_node("robot-01")).unwrap();
        assert_eq!(
            app.calculate_posture("robot-01").propagated_status,
            FleetPosture::Nominal,
            "a Trusted node is Nominal before the offline"
        );

        let updated = app.mark_node_untrusted("robot-01", "CANOPEN_NMT_OFFLINE", 1_000).unwrap();
        assert!(updated, "an existing node is updated");

        assert!(matches!(
            app.nodes.get("robot-01").unwrap().status,
            NodeTrustState::Untrusted(_)
        ));
        assert_eq!(
            app.calculate_posture("robot-01").propagated_status,
            FleetPosture::LockedOut,
            "marking the node offline must change the recalculated posture (effectful)"
        );
    }

    // Marking a node the verifier doesn't know returns Ok(false) so the caller
    // can fail-closed (treat as an unattributed offline) rather than no-op.
    #[test]
    fn marking_unknown_node_returns_false() {
        let app = app();
        assert!(!app.mark_node_untrusted("ghost", "CANOPEN_NMT_OFFLINE", 1).unwrap());
    }
}

#[cfg(test)]
mod nonce_lifecycle_tests {
    use super::*;
    use crate::verifier_store::VerifierStore;

    fn app() -> AppState {
        AppState::new(VerifierStore::new(":memory:").expect("in-memory store"), VerifierOperationMode::Active)
    }

    // #147 HEADLINE: nonces are CSPRNG-sourced, not wall-clock-derived.
    #[test]
    fn nonce_is_csprng_unpredictable_not_time_derived() {
        let a = generate_challenge_nonce();
        let b = generate_challenge_nonce();
        let c = generate_challenge_nonce();
        assert!(!(a == b && b == c), "successive CSPRNG nonces must not all be identical");

        // A wall-clock-nanos nonce would land within ~1s of `now`; a CSPRNG u64
        // landing that close to a ~1.8e18 timestamp has probability ~5e-11/value.
        let now_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64;
        for n in [a, b, c] {
            assert!(n.abs_diff(now_nanos) > 1_000_000_000,
                "nonce {n} is suspiciously close to the wall clock — not CSPRNG-sourced?");
        }
    }

    // SINGLE-USE: replay of a consumed nonce is rejected.
    #[test]
    fn replay_of_consumed_nonce_is_rejected() {
        let app = app();
        app.issue_challenge("n1", 42, 1_000);
        assert!(app.consume_challenge("n1", 42, 1_100), "first consume succeeds");
        assert!(!app.consume_challenge("n1", 42, 1_100), "replay of a consumed nonce is rejected");
    }

    // TTL-BOUNDED: an expired nonce is rejected.
    #[test]
    fn expired_nonce_is_rejected() {
        let app = app();
        app.issue_challenge("n1", 7, 1_000); // expires at 1_000 + CHALLENGE_TTL_MS
        let after_expiry = 1_000 + CHALLENGE_TTL_MS + 1;
        assert!(!app.consume_challenge("n1", 7, after_expiry), "expired nonce is rejected");
    }

    // NODE-BOUND: a nonce issued for node A cannot be consumed for node B.
    #[test]
    fn nonce_is_bound_to_its_node() {
        let app = app();
        app.issue_challenge("node-a", 99, 1_000);
        assert!(!app.consume_challenge("node-b", 99, 1_100), "another node cannot consume A's nonce");
        assert!(app.consume_challenge("node-a", 99, 1_100), "A's own nonce remains consumable");
    }

    // STORE HYGIENE: issuing prunes expired entries (bounded, no lingering stale nonces).
    #[test]
    fn issue_prunes_expired_entries() {
        let app = app();
        app.issue_challenge("stale", 1, 1_000);
        let later = 1_000 + CHALLENGE_TTL_MS + 1;
        app.issue_challenge("fresh", 2, later);
        assert!(!app.pending_challenges.contains_key("stale"), "expired entry pruned on issue");
        assert!(app.pending_challenges.contains_key("fresh"), "fresh entry retained");
    }
}
