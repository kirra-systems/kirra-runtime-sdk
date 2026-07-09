// src/verifier.rs

use crate::security::constant_time_compare;
use crate::verifier_store::VerifierStore;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// Maximum recursion depth for dependency graph traversal.
/// Prevents stack overflow on pathologically deep (but acyclic) graphs.
pub const MAX_DEPENDENCY_DEPTH: usize = 10;

/// Nonces expire after 30 seconds — long enough for a challenged node to respond,
/// short enough to limit the replay window if a response is intercepted.
const CHALLENGE_TTL_MS: u64 = 30_000;

// `FleetPosture` / `NodeTrustState` moved to the lean `kirra-core` crate (de-monolith
// Stage 1) so the governor/contract surface need not pull this heavy module. Re-exported
// here so every existing `crate::verifier::FleetPosture` path keeps the same type.
pub use kirra_core::{FleetPosture, NodeTrustState};

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
    /// #397 console — optional site/location label for fleet rollups. NULLABLE;
    /// captured at registration. Never gates trust/posture.
    pub site: Option<String>,
    /// #398 console — optional firmware version label for version rollups.
    /// NULLABLE; captured at registration. Never gates trust/posture.
    pub firmware_version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ChallengeEntry {
    pub nonce: u64,
    pub expires_at_ms: u64,
}

/// Volatile operator clearance-challenge entry (#314 Phase 1). The human analogue
/// of [`ChallengeEntry`]: a one-time nonce an operator must sign to prove key
/// possession before a grant is recorded. The nonce is a HEX STRING (not a u64) so
/// the in-browser WebCrypto flow never loses precision on a > 2^53 value. Same
/// volatility discipline as `pending_challenges` (INVARIANT #5): never persisted,
/// TTL-bounded, single-use.
#[derive(Debug, Clone)]
pub struct ClearanceChallengeEntry {
    pub nonce_hex: String,
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
    /// Interned node id (review P5). One `Arc<str>` per distinct id is minted
    /// per whole-fleet recalc (in `recursive_calculate`) and shared by
    /// `Arc::clone` into the gray set, the `black` memo key, this field, and
    /// every parent's `blocked_by` — so a node depended on by K others costs one
    /// id allocation, not K+. Serializes/compares exactly like the prior
    /// `String` (it derefs to `str`).
    pub node_id: Arc<str>,
    pub local_status: NodeTrustState,
    pub propagated_status: FleetPosture,
    /// Interned blocking-dependency ids — each is an `Arc::clone` of that dep's
    /// own `node_id`, not a fresh allocation (review P5).
    pub blocked_by: Vec<Arc<str>>,
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

/// Controls the `require_secure_transport` middleware (#G7 — the mesh-mTLS option
/// of "TLS on the verifier OR mandated mesh with enforcement check"). In the
/// standard mesh deployment the verifier binds plaintext over loopback to a trusted
/// sidecar that performs (m)TLS with peers; the sidecar asserts the client leg was
/// TLS via a forwarded-proto header. When `require_secure_transport` is on, a
/// request that does NOT carry that assertion is rejected (fail-closed).
///
/// **AOU-TRANSPORT-TLS-001:** the trusted proxy/mesh MUST set — overwriting any
/// client-supplied value — the forwarded-proto header. A directly-reachable
/// (un-proxied) verifier would let a client spoof it, so this enforcement is only
/// sound behind a trusted proxy (the same assumption that backs
/// `KIRRA_TRUSTED_INGRESS_MODE` / `x-kirra-client-id`). In-process TLS termination
/// on the verifier itself is the alternative (tracked separately).
#[derive(Debug, Clone)]
pub struct TransportSecurityConfig {
    pub require_secure_transport: bool,
    pub forwarded_proto_header: String,
}

impl TransportSecurityConfig {
    pub fn from_env() -> Self {
        Self {
            // Case-insensitive + trimmed (Copilot #805): for a SECURITY toggle,
            // `TRUE`/`True`/` true ` from an env file must ENABLE enforcement, not
            // silently leave it off (a fail-open relative to operator intent).
            require_secure_transport: std::env::var("KIRRA_REQUIRE_SECURE_TRANSPORT")
                .map(|v| {
                    let v = v.trim();
                    v == "1" || v.eq_ignore_ascii_case("true")
                })
                .unwrap_or(false),
            // Trim + lowercase the header name (Copilot #805): trailing whitespace
            // would make it an invalid `HeaderName` and deny ALL requests; an empty
            // value falls back to the default rather than breaking the lookup.
            forwarded_proto_header: std::env::var("KIRRA_FORWARDED_PROTO_HEADER")
                .ok()
                .map(|v| v.trim().to_ascii_lowercase())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "x-forwarded-proto".to_string()),
        }
    }
}

/// Pure boundary check — fail-closed. When `require_secure_transport` is OFF,
/// admit (backward-compatible, byte-identical to before). When ON, admit ONLY if
/// the forwarded-proto header is present, readable, and its ORIGINAL-client value
/// (the FIRST entry of a possibly comma-listed `client,proxy,…` chain — the
/// standard `X-Forwarded-Proto` semantics) is `https` (case-insensitive). An
/// absent header, an unreadable value, or a non-`https` client protocol is rejected.
pub fn request_transport_is_secure(
    require_secure_transport: bool,
    forwarded_proto_header: &str,
    headers: &axum::http::HeaderMap,
) -> bool {
    if !require_secure_transport {
        return true;
    }
    let Some(value) = headers.get(forwarded_proto_header) else {
        return false;
    };
    let Ok(proto) = value.to_str() else {
        return false;
    };
    // `X-Forwarded-Proto: <client>,<proxy1>,…` — the leftmost is the protocol the
    // ORIGINAL client used, which is what "did the client connect over TLS" asks.
    match proto.split(',').next() {
        Some(first) => first.trim().eq_ignore_ascii_case("https"),
        None => false,
    }
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
    /// Volatile operator clearance-challenge map (#314 Phase 1) — keyed by
    /// `"{operator_id}|{node_id}"`. Same volatility discipline as
    /// `pending_challenges` (INVARIANT #5): never persisted, TTL-bounded, single-use.
    pub pending_clearance_challenges: DashMap<String, ClearanceChallengeEntry>,
    /// Durable store for nodes and dependency graph (write-through, read on boot).
    /// Accessed through the `StoreHandle` seam (`with` / `call`) — never a raw
    /// lock. Phase 2 of the DB-actor migration swaps the handle's internals for a
    /// dedicated-thread connection owner.
    pub store: crate::store_handle::StoreHandle,
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
    /// Transport SECURITY (TLS-required) enforcement config (#G7) — reads from env
    /// at startup. When enabled, the `require_secure_transport` middleware
    /// fail-closes a request not asserted to have arrived over TLS.
    pub transport_security: TransportSecurityConfig,
    /// True while an RSS safe-distance violation is active (recalculate elevates to Degraded).
    pub rss_active_violation: Arc<AtomicBool>,
    /// #99 — true while flood conditions are present. Read by the posture engine
    /// to escalate Nominal → Degraded (SG4 operational layer), exactly like
    /// `rss_active_violation`. The SETTER (a flood detector, or a bridge from
    /// sustained #98 WATER_UNTRAVERSABLE vetoes) is a deferred cross-subsystem
    /// follow-up; this flag is read-only in the current code (defaults false).
    pub flood_condition_active: Arc<AtomicBool>,
    /// C2 supervisor trip flag (review finding C2). Set by `supervisor::spawn_supervised`
    /// when a CRITICAL background safety loop (the telemetry watchdog, the posture
    /// engine worker, the HA heartbeat writer / promotion monitor) crashes past its
    /// restart budget. The posture engine reads it and forces `FleetPosture::LockedOut`
    /// unconditionally (highest priority, overriding the DAG), so a wedged safety loop
    /// fails the whole fleet closed instead of silently leaving actuators live. Sticky:
    /// once tripped it stays tripped until process restart (a recovered loop within the
    /// restart window clears the supervisor's local counter but NOT this flag — recovery
    /// from a forced fleet lockout is an explicit human/HA action, matching LockedOut's
    /// human-reset semantics). Defaults false.
    pub supervisor_tripped: Arc<AtomicBool>,
    /// Recovery streak for clearing an active RSS violation.
    pub rss_recovery_streak: Arc<Mutex<RssRecoveryStreak>>,
    /// S-FI1d — true while frame/localization integrity is below full `Trusted`
    /// (a `Degraded` *or* `Untrusted` verdict). Read by the posture engine to
    /// escalate Nominal → Degraded, exactly like `rss_active_violation`. Set
    /// IMMEDIATELY on the first sub-trusted tick (fail-closed-immediately, no
    /// grace period); cleared by an `AV_RECOVERY_STREAK_THRESHOLD`-long run of
    /// `Trusted` ticks (auto-recovery). Defaults false. (AOU-LOCALIZATION-001.)
    pub frame_degraded_active: Arc<AtomicBool>,
    /// S-FI1d — true once frame integrity has been `Untrusted` for a SUSTAINED
    /// run (an inverted streak): a transient localization loss is the
    /// frame-trust-minimal Degraded MRC (decel-to-stop, auto-recovering), but a
    /// sustained / repeated fault is a genuine failure (sensor death, possible
    /// GNSS spoofing) and escalates to `LockedOut`. STICKY like
    /// `supervisor_tripped` — recovery is an explicit human/HA reset, matching
    /// LockedOut semantics. Defaults false.
    pub frame_lockout_active: Arc<AtomicBool>,
    /// Recovery streak for clearing `frame_degraded_active` (consecutive
    /// `Trusted` ticks within the recovery window).
    pub frame_recovery_streak: Arc<Mutex<RssRecoveryStreak>>,
    /// Inverted streak counting consecutive `Untrusted` ticks toward the
    /// `frame_lockout_active` escalation (sustained-fault detection).
    pub frame_untrusted_streak: Arc<Mutex<RssRecoveryStreak>>,
    /// S-DG1 — a posture-significant governor divergence is ACTIVE (the parko
    /// comparator's leaky-bucket accumulator crossed its significance
    /// threshold): two independently-derived safety governors disagree and we
    /// cannot tell which is wrong. Escalates Nominal → Degraded
    /// (decel-to-stop MRC) immediately; auto-recovers via the recovery
    /// streak once agreeing ticks resume. Defaults false (inert until the
    /// comparator's `PostureSignalSink` is wired).
    pub divergence_degraded_active: Arc<AtomicBool>,
    /// S-DG1 — the comparator's own sustained-divergence escalation
    /// (`escalated_to_lockout`) was reported: a persistent disagreement is a
    /// genuine fault (a real governor bug or corrupted input), not a
    /// transient. STICKY like `supervisor_tripped` / `frame_lockout_active` —
    /// recovery is an explicit human/HA reset. Defaults false.
    pub divergence_lockout_active: Arc<AtomicBool>,
    /// Recovery streak for clearing `divergence_degraded_active` (consecutive
    /// agreeing ticks within the recovery window).
    pub divergence_recovery_streak: Arc<Mutex<RssRecoveryStreak>>,
    /// #104 — the currently-open post-incident forensic sequence (correlation id
    /// + ordinal), or `None` when no incident is open. Volatile; the durable
    /// forensic record lives in the signed audit chain.
    pub current_incident: Arc<Mutex<Option<crate::post_incident::IncidentState>>>,
    /// #104 — operator-observable count of post-incident audit writes that were
    /// detected but could not be durably recorded (#245/#247 pattern). MUST be 0
    /// in a healthy deployment; never gates the verdict path.
    pub post_incident_write_failures: Arc<AtomicU64>,
    /// WS-0.3 / #772 F3 — operator-observable count of INCIDENT-CLASS posture
    /// transitions whose hard-power-loss-durable (FULL-connection) write failed
    /// and fell back to the checkpoint-bounded NORMAL write. The row IS in the
    /// chain (durable to the next checkpoint), only its at-write-time power-loss
    /// durability was degraded — DISTINCT from `post_incident_write_failures`
    /// (row MISSING from the chain). MUST be 0 in a healthy deployment; never
    /// gates the verdict path (a durability fault must not suppress an escalation).
    pub incident_durability_failures: Arc<AtomicU64>,
    /// #112 — operator-observable count of command-source handoff audit writes
    /// that were detected but could not be durably recorded (#245/#247 pattern).
    /// MUST be 0 in a healthy deployment; never gates the verdict path.
    pub command_source_write_failures: Arc<AtomicU64>,
    /// A3 — operator-observable count of kinematic-DenyBreach AUDIT records that
    /// were dropped because the bounded audit-writer channel was Full/Closed
    /// (drop-on-full, INV-4: safety never waits). Drops were previously LOGGED
    /// only; this counter makes the loss-rate observable (a non-zero / rising
    /// value = the audit chain has sequence gaps — under-provisioned channel or a
    /// dead writer). Never gates the verdict path.
    pub audit_write_drops: Arc<AtomicU64>,
    /// A3 — operator-observable count of learning-capture verdict records dropped
    /// on a Full/Closed capture channel. Non-safety (capture is an off-verdict-path
    /// side channel); surfaced so an integrator can see how much training data the
    /// channel sizing is shedding.
    pub capture_drops: Arc<AtomicU64>,
    /// H-3 — set when an AV subsystem is (de)registered, so the telemetry watchdog
    /// refreshes its watched-node list on the NEXT sweep instead of waiting up to
    /// `AV_WATCHDOG_NODE_REFRESH_MS` (30 s). Without this a node registered just
    /// after a refresh was unmonitored for ~28 s — a fail-OPEN window where a
    /// freshly-registered sensor could go silent/faulty undetected, breaking the
    /// SG-003 detection-latency bound (TIMEOUT + one sweep). The watchdog swaps it
    /// back to false when it refreshes. Defaults false.
    pub av_registry_dirty: Arc<AtomicBool>,
    /// WS-0.5 — fleet-safety Prometheus counters (posture transitions, gate
    /// denials, HA promotions), exported by `GET /metrics` on the verifier
    /// binary. Lock-free; incremented on the observed paths, never gating
    /// them. Lives here (not on `ServiceState`) so the posture engine, the
    /// routing gate, and the HA promotion path can all reach it.
    pub fleet_metrics: crate::metrics::FleetSafetyMetrics,

    /// WP-20 (G-11) per-task deadline-miss counters for the supervised loops that
    /// declare a `deadline_ms` budget in `execution_manager::TASK_MANIFEST` (today:
    /// the telemetry watchdog). The task loop records each cycle; `GET /metrics`
    /// exports `kirra_task_deadline_*`. Lock-free; observability only.
    pub deadline_registry: Arc<crate::execution_manager::DeadlineRegistry>,
}

impl AppState {
    pub fn new(store: VerifierStore, mode: VerifierOperationMode) -> Self {
        let (posture_tx, _) = broadcast::channel(POSTURE_BROADCAST_CAPACITY);
        // Pass B1 cache seed (S3 / #115): read the current durable epoch
        // before moving the store into the handle so the gate has a fresh
        // value before any request lands. Unreadable → 0 (gate falls through).
        let initial_db_epoch = store.current_epoch().unwrap_or(0);
        Self {
            nodes: DashMap::new(),
            dependency_graph: DashMap::new(),
            pending_challenges: DashMap::new(),
            pending_clearance_challenges: DashMap::new(),
            store: crate::store_handle::StoreHandle::new(store),
            mode_active: Arc::new(AtomicBool::new(mode == VerifierOperationMode::Active)),
            held_epoch: Arc::new(AtomicU64::new(0)),
            cached_db_epoch: Arc::new(AtomicU64::new(initial_db_epoch)),
            audit_writer_tx: std::sync::OnceLock::new(),
            capture_writer_tx: std::sync::OnceLock::new(),
            capture_decision_seq: Arc::new(AtomicU64::new(0)),
            posture_tx,
            transport_identity: TransportIdentityConfig::from_env(),
            transport_security: TransportSecurityConfig::from_env(),
            rss_active_violation: Arc::new(AtomicBool::new(false)),
            flood_condition_active: Arc::new(AtomicBool::new(false)),
            supervisor_tripped: Arc::new(AtomicBool::new(false)),
            rss_recovery_streak: Arc::new(Mutex::new(RssRecoveryStreak {
                count: 0,
                start_ms: 0,
            })),
            frame_degraded_active: Arc::new(AtomicBool::new(false)),
            frame_lockout_active: Arc::new(AtomicBool::new(false)),
            frame_recovery_streak: Arc::new(Mutex::new(RssRecoveryStreak {
                count: 0,
                start_ms: 0,
            })),
            frame_untrusted_streak: Arc::new(Mutex::new(RssRecoveryStreak {
                count: 0,
                start_ms: 0,
            })),
            divergence_degraded_active: Arc::new(AtomicBool::new(false)),
            divergence_lockout_active: Arc::new(AtomicBool::new(false)),
            divergence_recovery_streak: Arc::new(Mutex::new(RssRecoveryStreak {
                count: 0,
                start_ms: 0,
            })),
            current_incident: Arc::new(Mutex::new(None)),
            post_incident_write_failures: Arc::new(AtomicU64::new(0)),
            incident_durability_failures: Arc::new(AtomicU64::new(0)),
            command_source_write_failures: Arc::new(AtomicU64::new(0)),
            audit_write_drops: Arc::new(AtomicU64::new(0)),
            capture_drops: Arc::new(AtomicU64::new(0)),
            av_registry_dirty: Arc::new(AtomicBool::new(false)),
            fleet_metrics: crate::metrics::FleetSafetyMetrics::new(),
            deadline_registry: Arc::new(crate::execution_manager::DeadlineRegistry::from_manifest(
                crate::execution_manager::TASK_MANIFEST,
            )),
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
            tracing::warn!("audit writer Sender already installed — ignoring duplicate install");
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
            tracing::warn!("capture writer Sender already installed — ignoring duplicate install");
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
    // `Result<_, ()>` is intentional: the caller fail-closes on ANY error without
    // needing a typed reason (the detail is logged at the store layer).
    #[allow(clippy::result_unit_err)]
    pub fn persist_and_insert_node(&self, node: RegisteredNode) -> Result<(), ()> {
        self.store
            .with(|store| store.save_node(&node))
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
    #[allow(clippy::result_unit_err)] // intentional fail-closed `()` error; see persist_and_insert_node.
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
    #[allow(clippy::result_unit_err)] // intentional fail-closed `()` error; see persist_and_insert_node.
    pub fn persist_and_insert_deps(&self, node_id: &str, deps: Vec<String>) -> Result<(), ()> {
        self.store
            .with(|store| store.save_dependencies(node_id, &deps))
            .map_err(|_| ())?;
        self.dependency_graph.insert(node_id.to_string(), deps);
        Ok(())
    }

    pub fn calculate_posture(&self, node_id: &str) -> FleetNodePosture {
        let mut black: HashMap<Arc<str>, Arc<FleetNodePosture>> = HashMap::new();
        self.calculate_posture_memoized(node_id, &mut black)
    }

    /// As `calculate_posture`, but reuses a CALLER-OWNED `black` memo across
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
        &self,
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
        let max_depth = (self.nodes.len() + self.dependency_graph.len()).max(MAX_DEPENDENCY_DEPTH);
        let posture = self.recursive_calculate(node_id, &mut gray, black, 0, max_depth);
        (*posture).clone()
    }

    /// Whole-fleet per-node posture in ONE pass with a SHARED `black` memo
    /// (review P3): O(N+E) instead of the O(N·(N+E)) of calling
    /// [`calculate_posture`](Self::calculate_posture) once per node (each with a
    /// fresh memo). Result is IDENTICAL to mapping `calculate_posture` over every
    /// registered node — the per-node verdict is root-independent, so one shared
    /// memo only ever reuses fully-resolved verdicts (proven in
    /// `shared_memo_equivalence_tests`).
    ///
    /// The node-id set is snapshotted FIRST (the `nodes` shard guards are dropped
    /// before any traversal), so the re-entrant `nodes.get(...)` inside the DAG
    /// walk cannot deadlock against a held `nodes.iter()` guard — the same B1
    /// hazard the posture-engine recalc already avoids. (The previous
    /// `/fleet/posture` handler iterated `nodes` while calling `calculate_posture`
    /// per entry, holding the iter guard across re-entrant gets.)
    pub fn calculate_fleet_posture(&self) -> Vec<FleetNodePosture> {
        // SAFETY: SG-RED-2 — snapshot iteration prevents nested DashMap locks.
        // SAFETY: SG-RED-3 — posture DAG recalculation must be deadlock-free.
        let ids: Vec<String> = self.nodes.iter().map(|e| e.key().clone()).collect();
        let mut black: HashMap<Arc<str>, Arc<FleetNodePosture>> = HashMap::new();
        ids.iter()
            .map(|id| self.calculate_posture_memoized(id.as_str(), &mut black))
            .collect()
    }

    fn recursive_calculate(
        &self,
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

        let local_status = self
            .nodes
            .get(node_id)
            .map(|n| n.status.clone())
            .unwrap_or(NodeTrustState::Unknown);

        let deps = self
            .dependency_graph
            .get(node_id)
            .map(|d| d.value().clone())
            .unwrap_or_default();

        let mut blocked_by: Vec<Arc<str>> = Vec::new();
        let mut has_locked_out_dep = false;

        for dep_id in &deps {
            let dep_posture = self.recursive_calculate(dep_id, gray, black, depth + 1, max_depth);
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

    /// Consume a challenge nonce. Returns false if nonce is absent, expired, or mismatched.
    pub fn consume_challenge(&self, node_id: &str, nonce: u64, now_ms: u64) -> bool {
        let entry = match self.pending_challenges.remove(node_id) {
            Some((_, e)) => e,
            None => return false,
        };
        if now_ms > entry.expires_at_ms {
            return false;
        }
        entry.nonce == nonce
    }

    /// Issue a fresh challenge nonce for the given node. Overwrites any prior pending challenge.
    pub fn issue_challenge(&self, node_id: &str, nonce: u64, now_ms: u64) {
        // Store hygiene (#147): prune expired pending challenges so stale
        // entries for nodes that never re-attested do not linger. The map is
        // already bounded (keyed by node_id, per-node overwrite); this only
        // drops timed-out entries — it never introduces unbounded growth.
        self.pending_challenges
            .retain(|_, e| now_ms <= e.expires_at_ms);
        self.pending_challenges.insert(
            node_id.to_string(),
            ChallengeEntry {
                nonce,
                expires_at_ms: now_ms + CHALLENGE_TTL_MS,
            },
        );
    }

    /// Issue an operator clearance-challenge nonce, keyed by `(operator_id,
    /// node_id)` (#314 Phase 1). Mirrors [`issue_challenge`](Self::issue_challenge):
    /// prunes expired entries, per-key overwrite, TTL-bounded.
    pub fn issue_clearance_challenge(&self, key: &str, nonce_hex: String, now_ms: u64) {
        self.pending_clearance_challenges
            .retain(|_, e| now_ms <= e.expires_at_ms);
        self.pending_clearance_challenges.insert(
            key.to_string(),
            ClearanceChallengeEntry {
                nonce_hex,
                expires_at_ms: now_ms + CHALLENGE_TTL_MS,
            },
        );
    }

    /// Consume an operator clearance-challenge nonce — the VERIFY-THEN-CONSUME
    /// half (the caller verifies the signature FIRST). Atomic `remove` so a
    /// replay finds nothing. Returns false if absent, expired, or mismatched.
    pub fn consume_clearance_challenge(&self, key: &str, nonce_hex: &str, now_ms: u64) -> bool {
        let entry = match self.pending_clearance_challenges.remove(key) {
            Some((_, e)) => e,
            None => return false,
        };
        if now_ms > entry.expires_at_ms {
            return false;
        }
        constant_time_compare(entry.nonce_hex.as_bytes(), nonce_hex.as_bytes())
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
    getrandom::fill(&mut bytes)
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
        headers.insert(
            "x-kirra-client-id",
            HeaderValue::from_static("edge-gateway-01"),
        );
        assert!(!validate_client_identity_headers(
            false,
            "x-kirra-client-id",
            &headers
        ));
    }

    #[test]
    fn test_enabled_ingress_missing_header_rejects() {
        let headers = HeaderMap::new();
        assert!(!validate_client_identity_headers(
            true,
            "x-kirra-client-id",
            &headers
        ));
    }

    #[test]
    fn test_enabled_ingress_blank_header_rejects() {
        let mut headers = HeaderMap::new();
        headers.insert("x-kirra-client-id", HeaderValue::from_static("     "));
        assert!(!validate_client_identity_headers(
            true,
            "x-kirra-client-id",
            &headers
        ));
    }

    #[test]
    fn test_enabled_ingress_valid_header_accepts() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-kirra-client-id",
            HeaderValue::from_static("trusted-mesh-sidecar"),
        );
        assert!(validate_client_identity_headers(
            true,
            "x-kirra-client-id",
            &headers
        ));
    }

    #[test]
    fn test_custom_header_name_is_respected() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-custom-identity",
            HeaderValue::from_static("fleet-controller"),
        );
        assert!(validate_client_identity_headers(
            true,
            "x-custom-identity",
            &headers
        ));
        assert!(!validate_client_identity_headers(
            true,
            "x-kirra-client-id",
            &headers
        ));
    }
}

#[cfg(test)]
mod transport_security_tests {
    use super::request_transport_is_secure;
    use axum::http::{HeaderMap, HeaderValue};

    fn with_proto(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-proto", HeaderValue::from_str(v).unwrap());
        h
    }

    #[test]
    fn disabled_admits_everything_backward_compatible() {
        // require off → admit regardless of header (byte-identical to before).
        assert!(request_transport_is_secure(
            false,
            "x-forwarded-proto",
            &HeaderMap::new()
        ));
        assert!(request_transport_is_secure(
            false,
            "x-forwarded-proto",
            &with_proto("http")
        ));
    }

    #[test]
    fn enabled_requires_https_assertion() {
        assert!(request_transport_is_secure(
            true,
            "x-forwarded-proto",
            &with_proto("https")
        ));
        assert!(
            request_transport_is_secure(true, "x-forwarded-proto", &with_proto("HTTPS")),
            "case-insensitive"
        );
        assert!(
            request_transport_is_secure(true, "x-forwarded-proto", &with_proto(" https ")),
            "trimmed"
        );
    }

    #[test]
    fn enabled_rejects_insecure_or_absent_fail_closed() {
        assert!(
            !request_transport_is_secure(true, "x-forwarded-proto", &HeaderMap::new()),
            "absent header → deny"
        );
        assert!(
            !request_transport_is_secure(true, "x-forwarded-proto", &with_proto("http")),
            "plaintext → deny"
        );
        assert!(
            !request_transport_is_secure(true, "x-forwarded-proto", &with_proto("")),
            "empty → deny"
        );
    }

    #[test]
    fn enabled_uses_original_client_protocol_from_a_proxy_chain() {
        // X-Forwarded-Proto lists client,proxy,...: the FIRST (client) leg governs.
        assert!(request_transport_is_secure(
            true,
            "x-forwarded-proto",
            &with_proto("https, http")
        ));
        assert!(
            !request_transport_is_secure(true, "x-forwarded-proto", &with_proto("http, https")),
            "a plaintext ORIGINAL client leg must deny even if a later hop is https"
        );
    }

    #[test]
    fn custom_header_name_is_respected() {
        let mut h = HeaderMap::new();
        h.insert("x-mesh-proto", HeaderValue::from_static("https"));
        assert!(request_transport_is_secure(true, "x-mesh-proto", &h));
        assert!(
            !request_transport_is_secure(true, "x-forwarded-proto", &h),
            "wrong header name → deny"
        );
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
            site: None,
            firmware_version: None,
        }
    }

    // #84: marking the resolved fleet node offline must be EFFECTFUL — the DAG
    // recalc flips the node's posture from Nominal to LockedOut.
    #[test]
    fn marking_registered_node_untrusted_is_effectful() {
        let app = app();
        app.persist_and_insert_node(trusted_node("robot-01"))
            .unwrap();
        assert_eq!(
            app.calculate_posture("robot-01").propagated_status,
            FleetPosture::Nominal,
            "a Trusted node is Nominal before the offline"
        );

        let updated = app
            .mark_node_untrusted("robot-01", "CANOPEN_NMT_OFFLINE", 1_000)
            .unwrap();
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
        assert!(!app
            .mark_node_untrusted("ghost", "CANOPEN_NMT_OFFLINE", 1)
            .unwrap());
    }
}

#[cfg(test)]
mod nonce_lifecycle_tests {
    use super::*;
    use crate::verifier_store::VerifierStore;

    fn app() -> AppState {
        AppState::new(
            VerifierStore::new(":memory:").expect("in-memory store"),
            VerifierOperationMode::Active,
        )
    }

    // #147 HEADLINE: nonces are CSPRNG-sourced, not wall-clock-derived.
    #[test]
    fn nonce_is_csprng_unpredictable_not_time_derived() {
        let a = generate_challenge_nonce();
        let b = generate_challenge_nonce();
        let c = generate_challenge_nonce();
        assert!(
            !(a == b && b == c),
            "successive CSPRNG nonces must not all be identical"
        );

        // A wall-clock-nanos nonce would land within ~1s of `now`; a CSPRNG u64
        // landing that close to a ~1.8e18 timestamp has probability ~5e-11/value.
        let now_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        for n in [a, b, c] {
            assert!(
                n.abs_diff(now_nanos) > 1_000_000_000,
                "nonce {n} is suspiciously close to the wall clock — not CSPRNG-sourced?"
            );
        }
    }

    // SINGLE-USE: replay of a consumed nonce is rejected.
    #[test]
    fn replay_of_consumed_nonce_is_rejected() {
        let app = app();
        app.issue_challenge("n1", 42, 1_000);
        assert!(
            app.consume_challenge("n1", 42, 1_100),
            "first consume succeeds"
        );
        assert!(
            !app.consume_challenge("n1", 42, 1_100),
            "replay of a consumed nonce is rejected"
        );
    }

    // TTL-BOUNDED: an expired nonce is rejected.
    #[test]
    fn expired_nonce_is_rejected() {
        let app = app();
        app.issue_challenge("n1", 7, 1_000); // expires at 1_000 + CHALLENGE_TTL_MS
        let after_expiry = 1_000 + CHALLENGE_TTL_MS + 1;
        assert!(
            !app.consume_challenge("n1", 7, after_expiry),
            "expired nonce is rejected"
        );
    }

    // NODE-BOUND: a nonce issued for node A cannot be consumed for node B.
    #[test]
    fn nonce_is_bound_to_its_node() {
        let app = app();
        app.issue_challenge("node-a", 99, 1_000);
        assert!(
            !app.consume_challenge("node-b", 99, 1_100),
            "another node cannot consume A's nonce"
        );
        assert!(
            app.consume_challenge("node-a", 99, 1_100),
            "A's own nonce remains consumable"
        );
    }

    // STORE HYGIENE: issuing prunes expired entries (bounded, no lingering stale nonces).
    #[test]
    fn issue_prunes_expired_entries() {
        let app = app();
        app.issue_challenge("stale", 1, 1_000);
        let later = 1_000 + CHALLENGE_TTL_MS + 1;
        app.issue_challenge("fresh", 2, later);
        assert!(
            !app.pending_challenges.contains_key("stale"),
            "expired entry pruned on issue"
        );
        assert!(
            app.pending_challenges.contains_key("fresh"),
            "fresh entry retained"
        );
    }

    #[test]
    fn clearance_nonce_exact_match_is_accepted_once() {
        let app = app();
        let key = "alice|robot-01";
        app.issue_clearance_challenge(key, "abcdef0123456789".to_string(), 1_000);

        assert!(
            app.consume_clearance_challenge(key, "abcdef0123456789", 1_100),
            "exact clearance nonce must be accepted"
        );
        assert!(
            !app.consume_clearance_challenge(key, "abcdef0123456789", 1_100),
            "clearance nonce must be single-use"
        );
    }

    #[test]
    fn clearance_nonce_mismatch_is_rejected_and_consumed() {
        let app = app();
        let key = "alice|robot-01";
        app.issue_clearance_challenge(key, "abcdef0123456789".to_string(), 1_000);

        assert!(
            !app.consume_clearance_challenge(key, "abcdef0123456788", 1_100),
            "mismatched clearance nonce must be rejected"
        );
        assert!(
            !app.consume_clearance_challenge(key, "abcdef0123456789", 1_100),
            "a rejected clearance nonce attempt must still burn the challenge"
        );
    }

    #[test]
    fn expired_clearance_nonce_is_rejected() {
        let app = app();
        let key = "alice|robot-01";
        app.issue_clearance_challenge(key, "abcdef0123456789".to_string(), 1_000);

        assert!(
            !app.consume_clearance_challenge(key, "abcdef0123456789", 1_000 + CHALLENGE_TTL_MS + 1),
            "expired clearance nonce must be rejected"
        );
    }

    #[test]
    fn long_clearance_nonces_sharing_prefix_are_distinguished() {
        let app = app();
        let key = "alice|robot-01";
        let prefix = "a".repeat(64);
        let stored = format!("{prefix}1111111111111111");
        let provided = format!("{prefix}2222222222222222");
        app.issue_clearance_challenge(key, stored, 1_000);

        assert!(
            !app.consume_clearance_challenge(key, &provided, 1_100),
            "clearance nonce comparison must distinguish bytes beyond a 64-byte shared prefix"
        );
    }
}

// ---------------------------------------------------------------------------
// P3/P5 — the shared `black` memo must produce results IDENTICAL to the
// per-call memo for every node (critical-invariant #4: the gray/black traversal
// is unchanged; only the memo's lifetime + storage type changed).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod shared_memo_equivalence_tests {
    use super::*;
    use crate::verifier_store::VerifierStore;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn app() -> AppState {
        AppState::new(
            VerifierStore::new(":memory:").expect("in-memory store"),
            VerifierOperationMode::Active,
        )
    }

    fn node(id: &str, status: NodeTrustState) -> RegisteredNode {
        RegisteredNode {
            node_id: id.to_string(),
            status,
            registered_at_ms: 1,
            last_trust_update_ms: 1,
            ak_public_pem: None,
            expected_pcr16_digest_hex: None,
            site: None,
            firmware_version: None,
        }
    }

    /// Assert that, for every registered node, resolving with a FRESH per-call
    /// memo equals resolving the whole fleet through ONE shared memo (in id-sorted
    /// order). This pins the P3/P5 change as result-preserving.
    fn assert_shared_equals_per_call(app: &AppState) {
        let mut ids: Vec<String> = app.nodes.iter().map(|e| e.key().clone()).collect();
        ids.sort();
        let mut shared: HashMap<Arc<str>, Arc<FleetNodePosture>> = HashMap::new();
        for id in &ids {
            let per_call = app.calculate_posture(id);
            let shared_res = app.calculate_posture_memoized(id, &mut shared);
            // FleetNodePosture is not PartialEq; its Debug captures every field
            // (node_id, local_status, propagated_status, blocked_by) faithfully.
            assert_eq!(
                format!("{per_call:?}"),
                format!("{shared_res:?}"),
                "shared-memo result for {id} must equal the per-call result"
            );
        }
    }

    #[test]
    fn diamond_dag_with_a_faulted_shared_dep_is_memo_equivalent() {
        // d -> {b, c} -> a ; plus c -> x (Untrusted). `a` is the SHARED dep that
        // the shared memo evaluates once and reuses; `x`'s LockedOut propagates up
        // through c and d. Both resolution strategies must agree on every node.
        let app = app();
        app.persist_and_insert_node(node("a", NodeTrustState::Trusted))
            .unwrap();
        app.persist_and_insert_node(node("b", NodeTrustState::Trusted))
            .unwrap();
        app.persist_and_insert_node(node("c", NodeTrustState::Trusted))
            .unwrap();
        app.persist_and_insert_node(node("d", NodeTrustState::Trusted))
            .unwrap();
        app.persist_and_insert_node(node("x", NodeTrustState::Untrusted("fault".into())))
            .unwrap();
        app.persist_and_insert_deps("b", vec!["a".into()]).unwrap();
        app.persist_and_insert_deps("c", vec!["a".into(), "x".into()])
            .unwrap();
        app.persist_and_insert_deps("d", vec!["b".into(), "c".into()])
            .unwrap();

        // Spot-check the expected verdicts, then prove equivalence.
        assert_eq!(
            app.calculate_posture("a").propagated_status,
            FleetPosture::Nominal
        );
        assert_eq!(
            app.calculate_posture("x").propagated_status,
            FleetPosture::LockedOut
        );
        assert_eq!(
            app.calculate_posture("c").propagated_status,
            FleetPosture::LockedOut,
            "c depends on the faulted x → LockedOut"
        );
        assert_eq!(
            app.calculate_posture("d").propagated_status,
            FleetPosture::LockedOut,
            "d inherits c's LockedOut"
        );
        assert_shared_equals_per_call(&app);
    }

    /// M2: `calculate_fleet_posture` (the shared-memo whole-fleet pass that the
    /// `/fleet/posture` handler now uses) must yield, for every registered node,
    /// the SAME `FleetNodePosture` as the per-node `calculate_posture`, and cover
    /// exactly the registered id set.
    #[test]
    fn calculate_fleet_posture_matches_per_node_m2() {
        let app = app();
        app.persist_and_insert_node(node("a", NodeTrustState::Trusted))
            .unwrap();
        app.persist_and_insert_node(node("b", NodeTrustState::Trusted))
            .unwrap();
        app.persist_and_insert_node(node("x", NodeTrustState::Untrusted("fault".into())))
            .unwrap();
        app.persist_and_insert_deps("b", vec!["a".into(), "x".into()])
            .unwrap();

        let fleet = app.calculate_fleet_posture();
        assert_eq!(fleet.len(), 3, "one posture per registered node");

        for fp in &fleet {
            let per_node = app.calculate_posture(fp.node_id.as_ref());
            assert_eq!(
                format!("{fp:?}"),
                format!("{per_node:?}"),
                "fleet entry must equal per-node calculate_posture for {}",
                fp.node_id
            );
        }

        let mut got: Vec<String> = fleet.iter().map(|p| p.node_id.to_string()).collect();
        got.sort();
        assert_eq!(got, vec!["a".to_string(), "b".to_string(), "x".to_string()]);
    }

    #[test]
    fn cycle_is_memo_equivalent_and_not_poisoned_by_sharing() {
        // a -> b -> a (a cycle) and an independent Trusted node t. The cycle
        // sentinel (CYCLE_DETECTED) is NEVER memoized, so sharing the memo across
        // roots must not leak it onto t — both strategies must agree.
        let app = app();
        app.persist_and_insert_node(node("a", NodeTrustState::Trusted))
            .unwrap();
        app.persist_and_insert_node(node("b", NodeTrustState::Trusted))
            .unwrap();
        app.persist_and_insert_node(node("t", NodeTrustState::Trusted))
            .unwrap();
        app.persist_and_insert_deps("a", vec!["b".into()]).unwrap();
        app.persist_and_insert_deps("b", vec!["a".into()]).unwrap();

        assert_eq!(
            app.calculate_posture("a").propagated_status,
            FleetPosture::LockedOut,
            "a is on a cycle → LockedOut"
        );
        assert_eq!(
            app.calculate_posture("t").propagated_status,
            FleetPosture::Nominal,
            "the independent node is unaffected by the cycle"
        );
        assert_shared_equals_per_call(&app);
    }

    /// B8: a node whose dependency chain runs through ids that were never
    /// registered (a dangling/forward dependency) is still a perfectly ACYCLIC
    /// graph. The depth backstop must bound by the full id universe
    /// (`nodes.len() + dependency_graph.len()`), NOT by `nodes.len()` alone —
    /// otherwise a chain of unregistered ids longer than the node count trips
    /// MAX_DEPTH_EXCEEDED and spuriously reports LockedOut. The verdict must be
    /// driven by trust state (unregistered → Unknown → Degraded), not by depth.
    #[test]
    fn deep_chain_of_unregistered_deps_is_not_depth_locked() {
        let app = app();
        app.persist_and_insert_node(node("a", NodeTrustState::Trusted))
            .unwrap();

        // a -> u1 -> u2 -> ... -> u20, none of the u* registered as nodes. With
        // only one registered node, the old `nodes.len().max(10)` bound (=10)
        // would trip at u10; the id-universe bound clears the whole chain. Insert
        // straight into the in-memory graph (the edges intentionally reference
        // unregistered ids, which a persisted FK path would not allow).
        const N: usize = 20;
        app.dependency_graph
            .insert("a".to_string(), vec!["u1".to_string()]);
        for i in 1..N {
            app.dependency_graph
                .insert(format!("u{i}"), vec![format!("u{}", i + 1)]);
        }

        let posture = app.calculate_posture("a");
        assert_eq!(
            posture.propagated_status,
            FleetPosture::Degraded,
            "an acyclic chain through unregistered (Unknown) deps is Degraded, not depth-locked",
        );
        assert!(
            !posture
                .blocked_by
                .iter()
                .any(|b| b.as_ref() == "MAX_DEPTH_EXCEEDED"),
            "the depth backstop must not fire on a valid acyclic chain of unregistered ids",
        );
    }
}
