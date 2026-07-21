// src/verifier.rs

use crate::security::constant_time_compare;
use crate::verifier_store::VerifierStore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

// ADR-0035 Stage 3 (slice 3d): the gray/black DAG traversal + its depth bound
// moved to `kirra_safety_authority::dag`; re-exported so every
// `crate::verifier::MAX_DEPENDENCY_DEPTH` path resolves unchanged.
pub use kirra_safety_authority::dag::MAX_DEPENDENCY_DEPTH;

/// Nonces expire after 30 seconds — long enough for a challenged node to respond,
/// short enough to limit the replay window if a response is intercepted.
const CHALLENGE_TTL_MS: u64 = 30_000;

// `FleetPosture` / `NodeTrustState` moved to the lean `kirra-core` crate (de-monolith
// Stage 1) so the governor/contract surface need not pull this heavy module. Re-exported
// here so every existing `crate::verifier::FleetPosture` path keeps the same type.
// `RegisteredNode` moved to `kirra-core` alongside `NodeTrustState` (ADR-0035 — the
// `kirra-persistence` enabling work: the persistence layer must name it without the
// verifier service tree). Re-exported here so every existing
// `crate::verifier::RegisteredNode` path keeps the same type.
pub use kirra_core::{FleetPosture, NodeTrustState, RegisteredNode};

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

// ADR-0035 Stage 3 (slice 3a): `FleetNodePosture` moved to the lean
// `kirra-safety-authority` crate (the pure safety-decision surface). Re-exported
// here so every `crate::verifier::FleetNodePosture` path (the DAG traversal that
// mints it, SSE payloads, tests) resolves unchanged.
pub use kirra_safety_authority::FleetNodePosture;

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
/// **Sec1 (#1044) — two tiers, connection first.** When in-process TLS is active
/// (`KIRRA_TLS_CERT_PATH`/`KEY_PATH`), the gate derives "secure" from the ACTUAL
/// connection: `serve_tls` injects a server-side `ServerTerminatedTls` request
/// extension (unspoofable — a client cannot set a request extension), which
/// `request_transport_is_secure` trusts directly. The `X-Forwarded-Proto` header
/// is consulted ONLY as the fallback for the plaintext-listener-behind-a-proxy
/// topology.
///
/// **AOU-TRANSPORT-PROXY-001:** in that header-fallback topology the trusted
/// proxy/mesh MUST set — overwriting any client-supplied value — the
/// forwarded-proto header; a directly-reachable (un-proxied, no in-process TLS)
/// verifier would let a client spoof it. The startup sentinel WARNS when the gate
/// is enabled without in-process TLS, naming this obligation (the same class of
/// assumption that backs `KIRRA_TRUSTED_INGRESS_MODE` / `x-kirra-client-id`).
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
/// admit (backward-compatible, byte-identical to before). When ON:
///
///   1. If `connection_is_tls` — the request arrived over the in-process
///      rustls-terminated listener (a TRUSTED, server-side signal injected by
///      `serve_tls`, not a client header) — admit. This is the ground truth and
///      cannot be spoofed (Sec1 · #1044).
///   2. Otherwise fall back to the `X-Forwarded-Proto` header (the
///      plaintext-listener-behind-an-external-TLS-proxy topology): admit ONLY if
///      it is present, readable, and its ORIGINAL-client value (the FIRST entry
///      of a possibly comma-listed `client,proxy,…` chain) is `https`
///      (case-insensitive). Absent / unreadable / non-`https` → reject.
///
/// The header path is trustworthy ONLY behind a proxy that unconditionally
/// overwrites `X-Forwarded-Proto` (an operator obligation — AOU-TRANSPORT-PROXY-001,
/// warned at startup by the sentinel when the gate is on without in-process TLS).
/// A client on a plaintext leg that forges the header can no longer be admitted
/// on a TLS-terminating deployment, because tier 1 already answered from the real
/// connection.
pub fn request_transport_is_secure(
    require_secure_transport: bool,
    connection_is_tls: bool,
    forwarded_proto_header: &str,
    headers: &axum::http::HeaderMap,
) -> bool {
    if !require_secure_transport {
        return true;
    }
    // Tier 1: the real connection is in-process TLS (server-derived, unspoofable).
    if connection_is_tls {
        return true;
    }
    // Tier 2: external-proxy topology — consult the forwarded-proto header.
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

/// ADR-0035 Stage 3 (slice 3h) — the two transport-layer enforcement configs
/// grouped into one `AppState` field façade (`app.transport`), the same shape as
/// the earlier slices. Both are read from env at startup and consulted by the
/// request middleware (`identity` gates the `x-kirra-client-id` header; `security`
/// fail-closes a request not asserted to have arrived over TLS). A root-crate
/// config grouping (not a safety-decision surface). Per-field semantics UNCHANGED.
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// Transport identity enforcement config — reads from env at startup.
    pub identity: TransportIdentityConfig,
    /// Transport SECURITY (TLS-required) enforcement config (#G7) — reads from env
    /// at startup. When enabled, the `require_secure_transport` middleware
    /// fail-closes a request not asserted to have arrived over TLS.
    pub security: TransportSecurityConfig,
}

impl TransportConfig {
    /// Read both transport configs from env — byte-identical to the prior two
    /// inline `*::from_env()` calls in `AppState::new`.
    pub fn from_env() -> Self {
        Self {
            identity: TransportIdentityConfig::from_env(),
            security: TransportSecurityConfig::from_env(),
        }
    }
}

// ADR-0035 Stage 3 (slice 3a): `RssRecoveryStreak` moved to
// `kirra-safety-authority`; re-exported so `crate::verifier::RssRecoveryStreak`
// (the `AppState.escalation.rss_recovery_streak` field type) resolves unchanged.
pub use kirra_safety_authority::RssRecoveryStreak;

pub struct AppState {
    /// ADR-0035 Stage 3 (slice 3k): the in-memory fleet trust graph — the
    /// registered-node registry (`nodes`) and the dependency adjacency list
    /// (`dependency_graph`) the gray/black DAG traversal reads TOGETHER — grouped
    /// VERBATIM onto `crate::fleet_graph::FleetGraph` (per-field semantics and field
    /// names UNCHANGED, documented on the struct). Reached as `app.fleet.nodes` /
    /// `app.fleet.dependency_graph`; both are `DashMap` (lock-free interior
    /// mutability), so the move is pure relocation — no `&mut self`, no ordering
    /// change. The persist-then-insert ordering (INVARIANT #12) and the C2 (#1031)
    /// shard-locked RMW operate on `self.fleet.nodes` exactly as before.
    pub fleet: crate::fleet_graph::FleetGraph,
    /// ADR-0035 Stage 3 (slice 3i): the volatile challenge/nonce state — the
    /// attestation-challenge map (`pending_challenges`, INVARIANT #5), the
    /// operator-clearance map (`pending_clearance_challenges`), and the Bug 3
    /// attestation-challenge rate limiter (`challenge_rate_limiter`) — grouped
    /// VERBATIM onto `crate::challenge_state::ChallengeState` (per-field semantics
    /// and field names UNCHANGED, documented on the struct). Reached as
    /// `app.challenges.<field>`; a pure relocation (never persisted, off the
    /// verdict path), no `&mut self`, no behaviour change. The `pending_challenges`
    /// field declaration is kept byte-identical so INVARIANT #5's grep resolves.
    pub challenges: crate::challenge_state::ChallengeState,
    /// Durable store for nodes and dependency graph (write-through, read on boot).
    /// Accessed through the `StoreHandle` seam (`with` / `call`) — never a raw
    /// lock. Phase 2 of the DB-actor migration swaps the handle's internals for a
    /// dedicated-thread connection owner.
    pub store: crate::store_handle::StoreHandle,
    /// ADR-0035 Stage 3 (slice 3f): the HA split-brain fence state — the
    /// `mode_active` (Active vs PassiveStandby) + `held_epoch` / `cached_db_epoch`
    /// durable-epoch fencing atomics — lifted VERBATIM onto
    /// `kirra_safety_authority::HaFenceState` (per-field semantics UNCHANGED,
    /// documented on the struct in the safety-authority crate). Reached as
    /// `app.ha_fence.<field>`; `is_active()` / `current_mode()` stay as delegators
    /// so their callers are unchanged. All are `Arc<Atomic*>` interior-mutable, so
    /// the move is pure relocation — no `&mut self`, no ordering change.
    pub ha_fence: kirra_safety_authority::HaFenceState,
    /// ADR-0035 Stage 3 (slice 3g): the off-verdict-path async writer handles —
    /// the audit-writer + learning-capture mpsc Senders (`OnceLock`, None-fallback)
    /// and the monotonic capture join-key sequence — grouped VERBATIM onto
    /// `crate::writer_handles::WriterHandles` (per-field semantics UNCHANGED,
    /// documented on the struct). Reached as `app.writers.<field>`;
    /// `install_audit_writer` / `install_capture_writer` stay as delegators so
    /// their callers are unchanged. Off-verdict-path async plumbing, not a
    /// safety-decision surface — grouped in a root leaf, not kirra-safety-authority.
    pub writers: crate::writer_handles::WriterHandles,
    /// Bounded broadcast channel for real-time posture stream subscribers.
    pub posture_tx: broadcast::Sender<PostureStreamEvent>,
    /// ADR-0035 Stage 3 (slice 3h): the two transport-layer enforcement configs
    /// (`identity` + `security`) grouped onto `TransportConfig`. Reached as
    /// `app.transport.identity` / `app.transport.security`; per-field semantics
    /// UNCHANGED (documented on the struct). A root-crate config grouping.
    pub transport: TransportConfig,
    /// ADR-0035 Stage 3 (slice 3c): the fleet-escalation / hysteresis state —
    /// the RSS / flood / supervisor-trip / frame-integrity (S-FI1d) / governor-
    /// divergence (S-DG1) flags + their recovery/untrusted streaks, plus the H-3
    /// `av_registry_dirty` watchdog flag — lifted VERBATIM onto
    /// `kirra_safety_authority::EscalationState`. Reached as
    /// `app.escalation.<field>`; each field's semantics are UNCHANGED (documented
    /// on the struct in the safety-authority crate). All are `Arc<…>` interior-
    /// mutable, so the move is pure relocation — no `&mut self`, no behaviour change.
    pub escalation: kirra_safety_authority::EscalationState,
    /// #104 — the currently-open post-incident forensic sequence (correlation id
    /// + ordinal), or `None` when no incident is open. Volatile; the durable
    /// forensic record lives in the signed audit chain.
    pub current_incident: Arc<Mutex<Option<crate::post_incident::IncidentState>>>,
    /// ADR-0035 Stage 3 (slice 3e): the off-verdict-path write-failure / drop
    /// observability counters — `post_incident_write_failures`,
    /// `incident_durability_failures`, `command_source_write_failures`,
    /// `audit_write_drops`, `capture_drops` — lifted VERBATIM onto
    /// `kirra_safety_authority::OffPathWriteCounters` (per-field semantics
    /// UNCHANGED, documented on the struct in the safety-authority crate). Reached
    /// as `app.off_path_writes.<field>`. Each MUST be 0 in a healthy deployment and
    /// NONE ever gates the verdict path. All are `Arc<AtomicU64>` interior-mutable,
    /// so the move is pure relocation — no `&mut self`, no behaviour change.
    pub off_path_writes: kirra_safety_authority::OffPathWriteCounters,
    /// ADR-0035 Stage 3 (slice 3j): the two lock-free fleet-observability
    /// registries — the WS-0.5 `fleet_metrics` (posture/denial/HA-promotion
    /// counters + WP-05 latency histograms) and the WP-20 (G-11) per-task
    /// `deadline_registry` — grouped VERBATIM onto
    /// `crate::observability_state::ObservabilityState`. Reached as
    /// `app.observability.fleet_metrics` / `app.observability.deadline_registry`;
    /// per-field semantics UNCHANGED (documented on the struct). Both are read only
    /// by `GET /metrics` and NONE ever gates the verdict path. Interior-mutable
    /// (`AtomicU64` / `Arc<DeadlineRegistry>`), so the move is pure relocation — no
    /// `&mut self`, no behaviour change.
    pub observability: crate::observability_state::ObservabilityState,
}

impl AppState {
    pub fn new(store: VerifierStore, mode: VerifierOperationMode) -> Self {
        let (posture_tx, _) = broadcast::channel(POSTURE_BROADCAST_CAPACITY);
        // Pass B1 cache seed (S3 / #115): read the current durable epoch
        // before moving the store into the handle so the gate has a fresh
        // value before any request lands. Unreadable → 0 (gate falls through).
        let initial_db_epoch = store.current_epoch().unwrap_or(0);
        Self {
            // ADR-0035 Stage 3k: the node registry + dependency adjacency list now
            // live on FleetGraph; identical initial state (two empty DashMaps).
            fleet: crate::fleet_graph::FleetGraph::new(),
            // ADR-0035 Stage 3i: the two nonce maps + the challenge rate limiter
            // now live on ChallengeState; identical initial state (empty maps +
            // clock-free-seeded limiter).
            challenges: crate::challenge_state::ChallengeState::new(),
            store: crate::store_handle::StoreHandle::new(store),
            // ADR-0035 Stage 3f: the mode/epoch fence atomics now live on
            // HaFenceState; identical initial state (Active flag + held=0 +
            // cached_db_epoch seeded from the store's current durable epoch).
            ha_fence: kirra_safety_authority::HaFenceState::new(
                mode == VerifierOperationMode::Active,
                initial_db_epoch,
            ),
            // ADR-0035 Stage 3g: the audit/capture Senders + capture join-key
            // sequence now live on WriterHandles; identical initial state
            // (both writers uninstalled, capture sequence at 0).
            writers: crate::writer_handles::WriterHandles::new(),
            posture_tx,
            // ADR-0035 Stage 3h: the two transport configs now live on
            // TransportConfig; identical initial state (both read from env).
            transport: TransportConfig::from_env(),
            // ADR-0035 Stage 3c: the escalation/hysteresis flags + streaks (incl.
            // av_registry_dirty) now live on EscalationState; identical initial state.
            escalation: kirra_safety_authority::EscalationState::new(),
            current_incident: Arc::new(Mutex::new(None)),
            // ADR-0035 Stage 3e: the five off-path counters now live on
            // OffPathWriteCounters; identical initial state (all 0).
            off_path_writes: kirra_safety_authority::OffPathWriteCounters::new(),
            // ADR-0035 Stage 3j: the fleet-observability registries (fleet_metrics
            // + deadline_registry) now live on ObservabilityState; identical
            // initial state (fresh counters + a from-manifest deadline registry).
            observability: crate::observability_state::ObservabilityState::new(),
        }
    }

    /// Returns true if this instance is currently Active (accepting mutations).
    /// Reads the atomic — reflects runtime promotion that occurred after startup.
    /// ADR-0035 Stage 3f: thin delegator to `HaFenceState::is_active` (the atomic
    /// moved to `app.ha_fence`); every `app.is_active()` caller is unchanged.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.ha_fence.is_active()
    }

    /// Install the audit-writer mpsc Sender. Called once at startup, after
    /// `audit_writer::spawn_audit_writer`. Subsequent calls are ignored
    /// (OnceLock semantics) and logged as a duplicate-install warning.
    /// ADR-0035 Stage 3g: thin delegator to `WriterHandles::install_audit_writer`
    /// (the Sender moved to `app.writers`); every caller is unchanged.
    pub fn install_audit_writer(
        &self,
        tx: tokio::sync::mpsc::Sender<crate::audit_writer::AuditWriteJob>,
    ) {
        self.writers.install_audit_writer(tx);
    }

    /// Install the capture-writer mpsc Sender (learning-loop Phase 1, #190).
    /// Called once at startup, after `capture::spawn_capture_writer`, and only
    /// when `capture::capture_enabled()`. Mirrors `install_audit_writer`.
    /// ADR-0035 Stage 3g: thin delegator to `WriterHandles::install_capture_writer`.
    pub fn install_capture_writer(
        &self,
        tx: tokio::sync::mpsc::Sender<kirra_core::capture::CaptureRecord>,
    ) {
        self.writers.install_capture_writer(tx);
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
    //
    // C2 (#1031): the disk write and the memory write both happen while the
    // per-key DashMap entry (shard) lock is HELD, so a concurrent same-node
    // mutator (a watchdog downgrade racing this re-registration) can never
    // interleave between them. Previously these were two unsynchronized ops
    // (`store.with(save_node)` then `nodes.insert`), so an interleaving could
    // leave disk=Untrusted / memory=Trusted → a restart hydrating from disk would
    // resurrect revoked trust the running system believed gone. Disk before
    // memory within the lock (invariant #12). No store closure ever touches
    // `self.fleet.nodes`, so holding the shard lock across `store.with` cannot deadlock.
    #[allow(clippy::result_unit_err)]
    pub fn persist_and_insert_node(&self, node: RegisteredNode) -> Result<(), ()> {
        use dashmap::mapref::entry::Entry;
        match self.fleet.nodes.entry(node.node_id.clone()) {
            Entry::Occupied(mut occ) => {
                self.persist_node_row(&node)?;
                occ.insert(node);
            }
            Entry::Vacant(vac) => {
                self.persist_node_row(&node)?;
                vac.insert(node);
            }
        }
        Ok(())
    }

    /// C5 (#1036): route the durable node upsert through the epoch-fenced write
    /// when this process holds a claimed HA epoch (`held != 0`), else the plain
    /// write. A never-claimed process (`held == 0`: an in-memory test store, or a
    /// node that never became Active) has no superseded-primary scenario to guard
    /// against; a claimed Active always fences, so a just-superseded primary
    /// (whose stale `held_epoch != durable`) is rejected inside the write
    /// transaction before it can overwrite a row the new Active just changed.
    /// Mirrors the request-path `mutation_fence_verdict`'s `held_epoch != 0` guard.
    fn persist_node_row(&self, node: &RegisteredNode) -> Result<(), ()> {
        let held = self
            .ha_fence
            .held_epoch
            .load(std::sync::atomic::Ordering::SeqCst);
        self.store.with(|store| {
            if held == 0 {
                store.save_node(node).map_err(|_| ())
            } else {
                store.save_node_epoch_fenced(node, held).map_err(|_| ())
            }
        })
    }

    /// Sec9 (#1050): persist a node record AND its TPM-quote attestation policy in
    /// ONE durable transaction, then insert into the in-memory registry (disk
    /// before memory, invariant #12). Atomic registration — a crash can no longer
    /// leave a policy row without its node. Same held-epoch fence dispatch as
    /// `persist_node_row` (a superseded primary is rejected before either row is
    /// written). `Ok(())` only after both durable writes commit AND the memory
    /// insert lands.
    #[allow(clippy::result_unit_err)]
    pub fn persist_and_insert_node_with_policy(
        &self,
        node: RegisteredNode,
        require_tpm_quote: bool,
    ) -> Result<(), ()> {
        use dashmap::mapref::entry::Entry;
        let held = self
            .ha_fence
            .held_epoch
            .load(std::sync::atomic::Ordering::SeqCst);
        let write = |store: &mut VerifierStore| {
            if held == 0 {
                store
                    .save_node_with_policy(&node, require_tpm_quote)
                    .map_err(|_| ())
            } else {
                store
                    .save_node_with_policy_epoch_fenced(&node, require_tpm_quote, held)
                    .map_err(|_| ())
            }
        };
        match self.fleet.nodes.entry(node.node_id.clone()) {
            Entry::Occupied(mut occ) => {
                self.store.with(write)?;
                occ.insert(node);
            }
            Entry::Vacant(vac) => {
                self.store.with(write)?;
                vac.insert(node);
            }
        }
        Ok(())
    }

    /// Atomically read-modify-write a REGISTERED node's record under the per-key
    /// entry (shard) lock (C2 #1031). `f` receives the current record and returns
    /// its replacement; the read, the disk write and the memory write ALL happen
    /// while the lock is held, so a concurrent same-node mutator (a watchdog
    /// downgrade vs a re-registration / recovery re-trust) can never interleave
    /// and lose an update or invert disk/memory. Disk before memory (invariant #12).
    ///
    /// Returns `Ok(true)` if the node existed and was updated, `Ok(false)` if no
    /// such node is registered (the caller fail-closes on this), `Err(())` on a
    /// store failure — in which case MEMORY IS LEFT UNCHANGED (the durable state
    /// stays authoritative; we never advance memory past a failed disk write).
    #[allow(clippy::result_unit_err)] // intentional fail-closed `()` error; see persist_and_insert_node.
    pub fn update_node_atomic<F>(&self, node_id: &str, f: F) -> Result<bool, ()>
    where
        F: FnOnce(&RegisteredNode) -> RegisteredNode,
    {
        use dashmap::mapref::entry::Entry;
        match self.fleet.nodes.entry(node_id.to_string()) {
            Entry::Occupied(mut occ) => {
                let updated = f(occ.get());
                self.store
                    .with(|store| store.save_node(&updated))
                    .map_err(|_| ())?;
                occ.insert(updated);
                Ok(true)
            }
            Entry::Vacant(_) => Ok(false),
        }
    }

    /// Mark a registered node `Untrusted` (e.g. a CANopen NMT node-offline,
    /// #84) so the next DAG recalc reflects it. Disk-first (invariant #12).
    ///
    /// C2 (#1031): the get→build→persist→insert is now ONE atomic critical
    /// section via [`update_node_atomic`](Self::update_node_atomic) — previously
    /// the read (`nodes.get`) and the write were unsynchronized, so a concurrent
    /// re-registration could clobber this downgrade (fail-open).
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
        self.update_node_atomic(node_id, |existing| RegisteredNode {
            status: NodeTrustState::Untrusted(reason.to_string()),
            last_trust_update_ms: now_ms,
            ..existing.clone()
        })
    }

    /// Persist dependency list to SQLite then update in-memory graph (fail-closed).
    #[allow(clippy::result_unit_err)] // intentional fail-closed `()` error; see persist_and_insert_node.
    pub fn persist_and_insert_deps(&self, node_id: &str, deps: Vec<String>) -> Result<(), ()> {
        // C5 (#1036): fence the dependency-graph write on the held HA epoch, same
        // dispatch as `persist_node_row` (see there for the `held == 0` rationale).
        let held = self
            .ha_fence
            .held_epoch
            .load(std::sync::atomic::Ordering::SeqCst);
        self.store.with(|store| {
            if held == 0 {
                store.save_dependencies(node_id, &deps).map_err(|_| ())
            } else {
                store
                    .save_dependencies_epoch_fenced(node_id, &deps, held)
                    .map_err(|_| ())
            }
        })?;
        self.fleet
            .dependency_graph
            .insert(node_id.to_string(), deps);
        Ok(())
    }

    /// Single-root posture with a fresh memo. Delegates to the gray/black DAG
    /// traversal in `kirra_safety_authority::dag` (ADR-0035 slice 3d) — the
    /// algorithm is byte-identical and never mocked (INVARIANT #4).
    pub fn calculate_posture(&self, node_id: &str) -> FleetNodePosture {
        kirra_safety_authority::dag::calculate_posture(
            &self.fleet.nodes,
            &self.fleet.dependency_graph,
            node_id,
        )
    }

    /// Whole-fleet-shared-memo posture (see `dag::calculate_posture_memoized`).
    pub fn calculate_posture_memoized(
        &self,
        node_id: &str,
        black: &mut HashMap<Arc<str>, Arc<FleetNodePosture>>,
    ) -> FleetNodePosture {
        kirra_safety_authority::dag::calculate_posture_memoized(
            &self.fleet.nodes,
            &self.fleet.dependency_graph,
            node_id,
            black,
        )
    }

    /// Whole-fleet per-node posture in ONE pass with a shared memo
    /// (see `dag::calculate_fleet_posture`). Snapshot-then-traverse, deadlock-free.
    pub fn calculate_fleet_posture(&self) -> Vec<FleetNodePosture> {
        kirra_safety_authority::dag::calculate_fleet_posture(
            &self.fleet.nodes,
            &self.fleet.dependency_graph,
        )
    }
    /// Consume a challenge nonce. Returns false if nonce is absent, expired, or mismatched.
    pub fn consume_challenge(&self, node_id: &str, nonce: u64, now_ms: u64) -> bool {
        let entry = match self.challenges.pending_challenges.remove(node_id) {
            Some((_, e)) => e,
            None => return false,
        };
        if now_ms > entry.expires_at_ms {
            return false;
        }
        // Sec2 (#1050): constant-time compare for INVARIANT #2 consistency. There
        // is no secret to leak here (the nonce is public, single-use, TTL-bound,
        // and the caller verifies the Ed25519 signature FIRST), but every
        // security-critical byte comparison uses `constant_time_compare` — never
        // `==` — so the discipline holds uniformly across the challenge paths
        // (mirrors `consume_clearance_challenge`, which already does).
        constant_time_compare(&entry.nonce.to_le_bytes(), &nonce.to_le_bytes())
    }

    /// Issue a fresh challenge nonce for the given node. Overwrites any prior pending challenge.
    pub fn issue_challenge(&self, node_id: &str, nonce: u64, now_ms: u64) {
        // Store hygiene (#147): prune expired pending challenges so stale
        // entries for nodes that never re-attested do not linger. The map is
        // already bounded (keyed by node_id, per-node overwrite); this only
        // drops timed-out entries — it never introduces unbounded growth.
        self.challenges
            .pending_challenges
            .retain(|_, e| now_ms <= e.expires_at_ms);
        self.challenges.pending_challenges.insert(
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
        self.challenges
            .pending_clearance_challenges
            .retain(|_, e| now_ms <= e.expires_at_ms);
        self.challenges.pending_clearance_challenges.insert(
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
        let entry = match self.challenges.pending_clearance_challenges.remove(key) {
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
/// Fail-closed: if the OS CSPRNG is unavailable we return `Err` rather than fall
/// back to a weak/predictable source — no secure nonce can be issued without
/// entropy. Sec3 (#1050): the caller (an UNAUTHENTICATED challenge route) maps
/// the error to a 503 instead of aborting the process — a transient entropy
/// stall must not take the verifier down. Mirrors `authz::generate_api_token`.
pub fn generate_challenge_nonce() -> Result<u64, getrandom::Error> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
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
            false,
            "x-forwarded-proto",
            &HeaderMap::new()
        ));
        assert!(request_transport_is_secure(
            false,
            false,
            "x-forwarded-proto",
            &with_proto("http")
        ));
    }

    #[test]
    fn enabled_requires_https_assertion() {
        assert!(request_transport_is_secure(
            true,
            false,
            "x-forwarded-proto",
            &with_proto("https")
        ));
        assert!(
            request_transport_is_secure(true, false, "x-forwarded-proto", &with_proto("HTTPS")),
            "case-insensitive"
        );
        assert!(
            request_transport_is_secure(true, false, "x-forwarded-proto", &with_proto(" https ")),
            "trimmed"
        );
    }

    #[test]
    fn enabled_rejects_insecure_or_absent_fail_closed() {
        assert!(
            !request_transport_is_secure(true, false, "x-forwarded-proto", &HeaderMap::new()),
            "absent header → deny"
        );
        assert!(
            !request_transport_is_secure(true, false, "x-forwarded-proto", &with_proto("http")),
            "plaintext → deny"
        );
        assert!(
            !request_transport_is_secure(true, false, "x-forwarded-proto", &with_proto("")),
            "empty → deny"
        );
    }

    #[test]
    fn enabled_uses_original_client_protocol_from_a_proxy_chain() {
        // X-Forwarded-Proto lists client,proxy,...: the FIRST (client) leg governs.
        assert!(request_transport_is_secure(
            true,
            false,
            "x-forwarded-proto",
            &with_proto("https, http")
        ));
        assert!(
            !request_transport_is_secure(
                true,
                false,
                "x-forwarded-proto",
                &with_proto("http, https")
            ),
            "a plaintext ORIGINAL client leg must deny even if a later hop is https"
        );
    }

    #[test]
    fn custom_header_name_is_respected() {
        let mut h = HeaderMap::new();
        h.insert("x-mesh-proto", HeaderValue::from_static("https"));
        assert!(request_transport_is_secure(true, false, "x-mesh-proto", &h));
        assert!(
            !request_transport_is_secure(true, false, "x-forwarded-proto", &h),
            "wrong header name → deny"
        );
    }

    #[test]
    fn connection_derived_tls_is_trusted_over_the_header() {
        // Sec1 (#1044): a GENUINE in-process-TLS connection (the server-injected
        // `ServerTerminatedTls` marker → connection_is_tls=true) is secure even
        // when a spoofed forwarded-proto header lies `http`. Tier 1 answers from
        // the real connection and the header is never consulted — so a client on a
        // plaintext leg forging the header cannot be admitted on a TLS-terminating
        // deployment.
        assert!(
            request_transport_is_secure(true, true, "x-forwarded-proto", &with_proto("http")),
            "a real TLS connection admits regardless of a lying header"
        );
        assert!(
            request_transport_is_secure(true, true, "x-forwarded-proto", &HeaderMap::new()),
            "a real TLS connection needs no header at all"
        );
        // The off-switch and the header-fallback path are unchanged.
        assert!(request_transport_is_secure(
            false,
            false,
            "x-forwarded-proto",
            &with_proto("http")
        ));
        assert!(
            request_transport_is_secure(true, false, "x-forwarded-proto", &with_proto("https")),
            "no in-process TLS → fall back to the (proxy-set) header"
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
            app.fleet.nodes.get("robot-01").unwrap().status,
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

    // C5 (#1036): a superseded primary (its stale `held_epoch` trails the durable
    // `ha_state` epoch) must have its node registration + dependency write fenced
    // at the delegator — neither the durable row nor the in-memory map is mutated.
    #[test]
    fn superseded_primary_registration_is_fenced_at_the_delegator() {
        use std::sync::atomic::Ordering::SeqCst;
        let mut store = VerifierStore::new(":memory:").expect("in-memory store");
        // Durable epoch advances 0 -> 1 -> 2; the process that claimed 1 is stale.
        store.try_claim_epoch(0, "A", 1).unwrap();
        store.try_claim_epoch(1, "B", 2).unwrap();
        let app = AppState::new(store, VerifierOperationMode::Active);

        // Holder (held == durable == 2) registers normally.
        app.ha_fence.held_epoch.store(2, SeqCst);
        app.persist_and_insert_node(trusted_node("holder")).unwrap();
        app.persist_and_insert_deps("holder", vec![]).unwrap();
        assert!(app.fleet.nodes.get("holder").is_some());

        // Superseded primary (held == 1 < durable == 2) is rejected, fail-closed.
        app.ha_fence.held_epoch.store(1, SeqCst);
        assert!(
            app.persist_and_insert_node(trusted_node("stale")).is_err(),
            "a superseded primary's registration must fail closed"
        );
        assert!(
            app.fleet.nodes.get("stale").is_none(),
            "the in-memory map must not be mutated when the durable write is fenced (disk before memory)"
        );
        assert!(
            app.persist_and_insert_deps("holder", vec!["stale".into()])
                .is_err(),
            "a superseded primary's dependency write must fail closed too"
        );
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
        let a = generate_challenge_nonce().expect("CSPRNG");
        let b = generate_challenge_nonce().expect("CSPRNG");
        let c = generate_challenge_nonce().expect("CSPRNG");
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
            !app.challenges.pending_challenges.contains_key("stale"),
            "expired entry pruned on issue"
        );
        assert!(
            app.challenges.pending_challenges.contains_key("fresh"),
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
        let mut ids: Vec<String> = app.fleet.nodes.iter().map(|e| e.key().clone()).collect();
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
        app.fleet
            .dependency_graph
            .insert("a".to_string(), vec!["u1".to_string()]);
        for i in 1..N {
            app.fleet
                .dependency_graph
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
