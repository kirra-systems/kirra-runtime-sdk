// src/shared_store.rs
//
// #1030 stage 2 (ADR-0038) — the SHARED-tier dispatch seam.
//
// The hybrid design splits the store into two tiers:
//
//   * the SHARED control-plane state (node registry + dependency graph, HA
//     epoch fence + lease, engine state/generation, federation registry +
//     anti-replay, operators, API/cert principals, fabric assets, OTA
//     campaigns + adoption, AV subsystem meta, attestation policy, clearance
//     grants) — servable by EITHER the local SQLite `VerifierStore` (the
//     default, byte-identical path) or live Postgres (`KIRRA_DB_URL`, the
//     `postgres` build feature);
//
//   * the LOCAL tamper-evident ledger (hash-chained audit log, posture-event
//     history, causal log, signing-key ledger) — ALWAYS the per-instance
//     SQLite store, never routed (the ADR's tamper-evidence rationale).
//
// `SharedOps` is the seam: every shared-tier operation the service performs
// goes through a method here, which dispatches on the configured backend.
// Two variants, exhaustive match — an enum, not a trait object, so the
// dispatch is static-friendly and the Local arm is transparently the SAME
// writer connection `StoreHandle` already serializes (no second connection,
// no behavior change when `KIRRA_DB_URL` is unset).
//
// FUSED (chained) operations: on SQLite, campaign lifecycle updates,
// clearance-grant creation/outcomes, and federated-report saves fuse the row
// mutation with the hash-chained audit append in ONE transaction — that
// fusion is preserved verbatim on the Local arm (the original methods run
// unchanged). On the Pg arm the same operations decompose per the ADR:
// the LOCAL chained audit append first (the ledger is the pessimistic
// record — INVARIANT #12 extended), then the PG row mutation; a PG failure
// after the ledger append surfaces as an error to the caller (retryable),
// never a silent divergence.
//
// FAIL-CLOSED: every Pg-arm error surfaces as `SharedError` — there is NO
// fallback from Pg to Local at runtime (a split brain between backends is
// worse than an outage).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use kirra_persistence::{FenceError, VerifierStore};

#[cfg(feature = "postgres")]
use kirra_persistence::postgres::{PgDurableWriteError, PgStoreError, PgVerifierStore};

/// Which backend serves the SHARED control-plane tiers (ADR-0038).
pub enum SharedBackend {
    /// Shared tiers served by the local SQLite writer (default;
    /// byte-identical to the pre-#1030 service).
    Local,
    /// Shared tiers served by live Postgres (`KIRRA_DB_URL`).
    #[cfg(feature = "postgres")]
    Pg(Arc<Mutex<PgVerifierStore>>),
}

impl SharedBackend {
    /// A short label for startup logs / diagnostics.
    pub fn label(&self) -> &'static str {
        match self {
            SharedBackend::Local => "local-sqlite",
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(_) => "postgres",
        }
    }
}

/// A shared-tier operation failed. Unifies the two backends' error types so
/// handlers never match on `rusqlite::Error` / `PgStoreError` directly —
/// backend-portable predicates (`is_unique_violation`, `is_not_found`) are
/// the only introspection surface.
#[derive(Debug)]
pub enum SharedError {
    Sqlite(rusqlite::Error),
    #[cfg(feature = "postgres")]
    Pg(PgStoreError),
    /// An epoch-fenced write was refused (superseded / unreadable). Nothing
    /// was written. Fail-closed.
    Fenced(FenceError),
    /// Federation: the report nonce was already burned (H1 replay). The
    /// handler maps this to the clean FEDERATED_NONCE_REPLAY rejection.
    NonceReplay,
    /// Federation: same-epoch generation regress/replay (Item 20).
    GenerationRegress {
        found: u64,
        high_water: u64,
    },
    /// Federation: effective-epoch regress or omission-downgrade (#791 I1).
    EpochRegress {
        found: u64,
        high_water: u64,
    },
}

impl SharedError {
    /// Backend-portable UNIQUE-constraint predicate (the campaigns /
    /// principals "already exists → 409" arms).
    pub fn is_unique_violation(&self) -> bool {
        match self {
            SharedError::Sqlite(rusqlite::Error::SqliteFailure(f, _)) => {
                f.code == rusqlite::ffi::ErrorCode::ConstraintViolation
            }
            SharedError::Sqlite(_) => false,
            #[cfg(feature = "postgres")]
            SharedError::Pg(e) => e.is_unique_violation(),
            _ => false,
        }
    }

    /// Backend-portable no-such-row predicate (the "phantom mutation" arms).
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            SharedError::Sqlite(rusqlite::Error::QueryReturnedNoRows)
        )
    }
}

impl std::fmt::Display for SharedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SharedError::Sqlite(e) => write!(f, "shared store (sqlite): {e}"),
            #[cfg(feature = "postgres")]
            SharedError::Pg(e) => write!(f, "shared store (postgres): {e}"),
            SharedError::Fenced(e) => write!(f, "shared store fence: {e:?}"),
            SharedError::NonceReplay => f.write_str("federation nonce replay"),
            SharedError::GenerationRegress { found, high_water } => write!(
                f,
                "federation generation regress: found {found}, high-water {high_water}"
            ),
            SharedError::EpochRegress { found, high_water } => write!(
                f,
                "federation epoch regress: found {found}, high-water {high_water}"
            ),
        }
    }
}

impl std::error::Error for SharedError {}

impl From<rusqlite::Error> for SharedError {
    fn from(e: rusqlite::Error) -> Self {
        SharedError::Sqlite(e)
    }
}

#[cfg(feature = "postgres")]
impl From<PgStoreError> for SharedError {
    fn from(e: PgStoreError) -> Self {
        SharedError::Pg(e)
    }
}

#[cfg(feature = "postgres")]
impl From<PgDurableWriteError> for SharedError {
    fn from(e: PgDurableWriteError) -> Self {
        map_federation_pg(e)
    }
}

/// Map a LOCAL fenced-write error. Only the fence/db arms can arise from the
/// node/campaign fenced writes routed through this facade; the
/// federation-specific arms (`NonceReplay` / regressions) belong to the
/// federation tier's dedicated composition and are mapped there.
fn map_local_fenced(e: kirra_persistence::DurableWriteError) -> SharedError {
    match e {
        kirra_persistence::DurableWriteError::Fenced(f) => SharedError::Fenced(f),
        kirra_persistence::DurableWriteError::Db(d) => SharedError::Sqlite(d),
        // Unreachable for the node/campaign fenced writes (only the federation
        // save can produce the replay/regress arms); fail-closed if it ever
        // appears rather than panicking on the store path.
        _other => SharedError::Fenced(FenceError::EpochUnreadable),
    }
}

pub type SharedResult<T> = Result<T, SharedError>;

/// Lock the PG store (poison-tolerant, matching the writer's discipline). The
/// PG client is additionally serialized internally; this outer mutex exists so
/// the facade can reach the store's `&mut self` trait methods through an
/// `Arc` without per-method `&self` mirrors.
#[cfg(feature = "postgres")]
fn pg_lock(m: &Mutex<PgVerifierStore>) -> std::sync::MutexGuard<'_, PgVerifierStore> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The shared-tier operations facade. Cheaply cloneable (`Arc`s inside);
/// `'static`, so it moves into `spawn_blocking` closures freely.
#[derive(Clone)]
pub struct SharedOps {
    /// The local writer — the Local arm's target, and the LOCAL LEDGER for
    /// the decomposed fused operations on the Pg arm.
    pub(crate) writer: Arc<Mutex<VerifierStore>>,
    pub(crate) backend: Arc<SharedBackend>,
}

impl SharedOps {
    pub(crate) fn new(writer: Arc<Mutex<VerifierStore>>, backend: Arc<SharedBackend>) -> Self {
        Self { writer, backend }
    }

    /// Run a closure against the LOCAL writer (poison-tolerant, matching
    /// `StoreHandle`). Used by the Local arm and by the local-ledger halves
    /// of decomposed fused operations.
    fn local<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut VerifierStore) -> R,
    {
        let mut guard = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut guard)
    }

    pub fn backend_label(&self) -> &'static str {
        self.backend.label()
    }
}

// ---------------------------------------------------------------------------
// Tier: node registry + dependency graph + attestation policy
// ---------------------------------------------------------------------------

impl SharedOps {
    pub fn save_node(&self, node: &kirra_core::RegisteredNode) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.save_node(node)).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::NodeStore;
                pg_lock(pg).save_node(node).map_err(Into::into)
            }
        }
    }

    pub fn save_node_epoch_fenced(
        &self,
        node: &kirra_core::RegisteredNode,
        held_epoch: u64,
    ) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.save_node_epoch_fenced(node, held_epoch))
                .map_err(map_local_fenced),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg_lock(pg)
                .save_node_epoch_fenced(node, held_epoch)
                .map_err(Into::into),
        }
    }

    pub fn save_node_with_policy_epoch_fenced(
        &self,
        node: &kirra_core::RegisteredNode,
        require_tpm_quote: bool,
        held_epoch: u64,
    ) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| {
                    s.save_node_with_policy_epoch_fenced(node, require_tpm_quote, held_epoch)
                })
                .map_err(map_local_fenced),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg_lock(pg)
                .save_node_with_policy_epoch_fenced(node, require_tpm_quote, held_epoch)
                .map_err(Into::into),
        }
    }

    pub fn load_node(&self, node_id: &str) -> SharedResult<Option<kirra_core::RegisteredNode>> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.load_node(node_id)).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::NodeStore;
                pg_lock(pg).load_node(node_id).map_err(Into::into)
            }
        }
    }

    pub fn load_nodes(&self) -> SharedResult<Vec<kirra_core::RegisteredNode>> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.load_nodes()).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::NodeStore;
                pg_lock(pg).load_nodes().map_err(Into::into)
            }
        }
    }

    pub fn node_exists(&self, node_id: &str) -> SharedResult<bool> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.node_exists(node_id)).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::NodeStore;
                pg_lock(pg).node_exists(node_id).map_err(Into::into)
            }
        }
    }

    pub fn count_nodes(&self) -> SharedResult<i64> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.count_nodes()).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::NodeStore;
                pg_lock(pg).count_nodes().map_err(Into::into)
            }
        }
    }

    pub fn node_requires_tpm_quote(&self, node_id: &str) -> SharedResult<bool> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.node_requires_tpm_quote(node_id))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg_lock(pg)
                .node_requires_tpm_quote(node_id)
                .map_err(Into::into),
        }
    }

    pub fn save_node_with_policy(
        &self,
        node: &kirra_core::RegisteredNode,
        require_tpm_quote: bool,
    ) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.save_node_with_policy(node, require_tpm_quote))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg_lock(pg)
                .save_node_with_policy(node, require_tpm_quote)
                .map_err(Into::into),
        }
    }

    pub fn save_dependencies_epoch_fenced(
        &self,
        node_id: &str,
        deps: &[String],
        held_epoch: u64,
    ) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.save_dependencies_epoch_fenced(node_id, deps, held_epoch))
                .map_err(map_local_fenced),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg_lock(pg)
                .save_dependencies_epoch_fenced(node_id, deps, held_epoch)
                .map_err(Into::into),
        }
    }

    pub fn save_dependencies(&self, node_id: &str, deps: &[String]) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.save_dependencies(node_id, deps))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg_lock(pg)
                .save_dependencies(node_id, deps)
                .map_err(Into::into),
        }
    }

    pub fn load_dependencies(&self) -> SharedResult<HashMap<String, Vec<String>>> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.load_dependencies()).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg_lock(pg).load_dependencies().map_err(Into::into),
        }
    }
}

// ---------------------------------------------------------------------------
// Tier: HA epoch fence + lease
// ---------------------------------------------------------------------------

impl SharedOps {
    pub fn current_epoch(&self) -> SharedResult<u64> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.current_epoch()).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::EpochFence;
                pg_lock(pg).current_epoch().map_err(Into::into)
            }
        }
    }

    pub fn current_active_holder(&self) -> SharedResult<(u64, Option<String>)> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.current_active_holder())
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::EpochFence;
                pg_lock(pg).current_active_holder().map_err(Into::into)
            }
        }
    }

    pub fn try_claim_epoch(
        &self,
        observed: u64,
        instance_id: &str,
        now_ms: u64,
    ) -> SharedResult<Option<u64>> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.try_claim_epoch(observed, instance_id, now_ms))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                // `EpochFence::try_claim_epoch` takes `&mut self`; the PG store
                // serializes internally, and the inherent CAS is on `&self`
                // via the connection lock — call through the store's own
                // interior mutability.
                pg_lock(pg)
                    .try_claim_epoch_shared(observed, instance_id, now_ms)
                    .map_err(Into::into)
            }
        }
    }

    pub fn assert_actuator_epoch_held(&self, held_epoch: u64) -> Result<(), FenceError> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.assert_actuator_epoch_held(held_epoch)),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg_lock(pg).assert_actuator_epoch_held_shared(held_epoch),
        }
    }

    pub fn renew_lease(
        &self,
        instance_id: &str,
        held_epoch: u64,
        now_ms: u64,
    ) -> SharedResult<bool> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.renew_lease(instance_id, held_epoch, now_ms))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg_lock(pg)
                .renew_lease(instance_id, held_epoch, now_ms)
                .map_err(Into::into),
        }
    }

    pub fn read_ha_lease(&self) -> SharedResult<kirra_persistence::HaLease> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.read_ha_lease()).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg_lock(pg).read_ha_lease().map_err(Into::into),
        }
    }
}

// ---------------------------------------------------------------------------
// Tier: posture-engine state (KV + monotonic generation high-water)
// ---------------------------------------------------------------------------

impl SharedOps {
    pub fn load_engine_state(&self, key: &str) -> SharedResult<Option<String>> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.load_engine_state(key)).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::PostureEngineStateStore;
                pg_lock(pg).load_engine_state(key).map_err(Into::into)
            }
        }
    }

    pub fn save_engine_state(&self, key: &str, value: &str) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.save_engine_state(key, value))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::PostureEngineStateStore;
                pg_lock(pg)
                    .save_engine_state(key, value)
                    .map_err(Into::into)
            }
        }
    }

    pub fn load_last_generation(&self) -> SharedResult<u64> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.load_last_generation()).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::PostureEngineStateStore;
                pg_lock(pg).load_last_generation().map_err(Into::into)
            }
        }
    }

    pub fn save_last_generation(&self, generation: u64) -> SharedResult<bool> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.save_last_generation(generation))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::PostureEngineStateStore;
                pg_lock(pg)
                    .save_last_generation(generation)
                    .map_err(Into::into)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tier: federation registry + anti-replay + reports
// ---------------------------------------------------------------------------

impl SharedOps {
    pub fn save_trusted_federation_controller(
        &self,
        controller_id: &str,
        public_key_b64: &str,
        registered_at_ms: u64,
    ) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| {
                    s.save_trusted_federation_controller(
                        controller_id,
                        public_key_b64,
                        registered_at_ms,
                    )
                })
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::FederationStore;
                pg_lock(pg)
                    .save_trusted_federation_controller(
                        controller_id,
                        public_key_b64,
                        registered_at_ms,
                    )
                    .map_err(Into::into)
            }
        }
    }

    pub fn load_trusted_federation_controller_key(
        &self,
        controller_id: &str,
    ) -> SharedResult<Option<String>> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.load_trusted_federation_controller_key(controller_id))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::FederationStore;
                pg_lock(pg)
                    .load_trusted_federation_controller_key(controller_id)
                    .map_err(Into::into)
            }
        }
    }

    pub fn has_seen_federation_nonce(&self, nonce_hex: &str) -> SharedResult<bool> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.has_seen_federation_nonce(nonce_hex))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::FederationStore;
                pg_lock(pg)
                    .has_seen_federation_nonce(nonce_hex)
                    .map_err(Into::into)
            }
        }
    }

    pub fn industrial_seq_check_and_advance(
        &self,
        source_id: &str,
        sequence: u64,
        now_ms: u64,
    ) -> SharedResult<bool> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.industrial_seq_check_and_advance(source_id, sequence, now_ms))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::FederationStore;
                pg_lock(pg)
                    .industrial_seq_check_and_advance(source_id, sequence, now_ms)
                    .map_err(Into::into)
            }
        }
    }

    /// The FUSED federated-report accept. Local: the SQLite single-transaction
    /// save runs UNCHANGED (row + nonce burn + chained audit, atomic). Pg: the
    /// gated PG transaction commits the shared halves (gates + high-water +
    /// report + nonce burn), then the SAME audit events are appended to the
    /// LOCAL ledger. Shared-write-first is REQUIRED here (the gates may
    /// refuse — a refused report must never be ledgered as accepted); the
    /// crash window (PG committed, ledger append missed) is recorded in
    /// ADR-0038 and surfaces as a WARN, never a silent divergence.
    #[allow(clippy::too_many_arguments)]
    pub fn save_federated_report_chained(
        &self,
        report: &kirra_fleet_types::federation::FederatedTrustReport,
        source_generation: Option<u64>,
        source_epoch: Option<u64>,
        received_at_ms: u64,
        held_epoch: u64,
    ) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| {
                    s.save_federated_report_chained(
                        report,
                        source_generation,
                        source_epoch,
                        received_at_ms,
                        held_epoch,
                    )
                })
                .map_err(map_federation_dwe),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                let posture_json = serde_json::to_string(&report.posture)
                    .map_err(|_| SharedError::Sqlite(rusqlite::Error::InvalidQuery))?;
                let outcome = pg_lock(pg)
                    .save_federated_report_row_gated(
                        &report.source_controller_id,
                        &report.asset_id,
                        &posture_json,
                        report.issued_at_ms,
                        report.expires_at_ms,
                        &report.nonce_hex,
                        source_generation,
                        source_epoch,
                        received_at_ms,
                        held_epoch,
                    )
                    .map_err(map_federation_pg)?;
                // LOCAL ledger — the same events the SQLite fused save appends.
                let accepted = serde_json::json!({
                    "source_controller_id": report.source_controller_id,
                    "asset_id": report.asset_id,
                    "posture": posture_json,
                    "issued_at_ms": report.issued_at_ms,
                    "expires_at_ms": report.expires_at_ms,
                    "nonce_hex": report.nonce_hex,
                    "received_at_ms": received_at_ms,
                });
                self.ledger_append(
                    "FEDERATED_TRUST_REPORT_ACCEPTED",
                    &accepted.to_string(),
                    received_at_ms,
                );
                if let (Some(hw), Some(gen)) = (outcome.gap_from, source_generation) {
                    let gap = serde_json::json!({
                        "source_controller_id": report.source_controller_id,
                        "asset_id": report.asset_id,
                        "last_accepted_generation": hw,
                        "observed_generation": gen,
                        "missing_from_generation": hw + 1,
                        "missing_through_generation": gen - 1,
                        "skipped_generations": gen - hw - 1,
                    });
                    self.ledger_append(
                        "FEDERATION_GENERATION_GAP",
                        &gap.to_string(),
                        received_at_ms,
                    );
                }
                if let (Some(prev), Some(gen)) = (outcome.epoch_advance_from, source_generation) {
                    let adv = serde_json::json!({
                        "source_controller_id": report.source_controller_id,
                        "asset_id": report.asset_id,
                        "previous_epoch": prev,
                        "observed_epoch": outcome.eff_epoch,
                        "observed_generation": gen,
                    });
                    self.ledger_append(
                        "FEDERATION_EPOCH_ADVANCE",
                        &adv.to_string(),
                        received_at_ms,
                    );
                }
                Ok(())
            }
        }
    }

    pub fn load_federated_reports_for_asset(
        &self,
        asset_id: &str,
    ) -> SharedResult<Vec<serde_json::Value>> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.load_federated_reports_for_asset(asset_id))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg_lock(pg)
                .load_federated_reports_for_asset(asset_id)
                .map_err(Into::into),
        }
    }

    pub fn load_federated_report_v2s_for_asset(
        &self,
        asset_id: &str,
    ) -> SharedResult<Vec<kirra_fleet_types::federation_reconciliation::FederatedTrustReportV2>>
    {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.load_federated_report_v2s_for_asset(asset_id))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg_lock(pg)
                .load_federated_report_v2s_for_asset(asset_id)
                .map_err(Into::into),
        }
    }
}

// ---------------------------------------------------------------------------
// Tier: operators + clearance grants
// ---------------------------------------------------------------------------

impl SharedOps {
    pub fn register_operator(
        &self,
        operator_id: &str,
        pubkey_pem: &str,
        now_ms: u64,
    ) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.register_operator(operator_id, pubkey_pem, now_ms))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::OperatorStore;
                pg_lock(pg)
                    .register_operator(operator_id, pubkey_pem, now_ms)
                    .map_err(Into::into)
            }
        }
    }

    pub fn revoke_operator(&self, operator_id: &str, now_ms: u64) -> SharedResult<bool> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.revoke_operator(operator_id, now_ms))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::OperatorStore;
                pg_lock(pg)
                    .revoke_operator(operator_id, now_ms)
                    .map_err(Into::into)
            }
        }
    }

    pub fn load_operator(
        &self,
        operator_id: &str,
    ) -> SharedResult<Option<kirra_persistence::OperatorRecord>> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.load_operator(operator_id))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::OperatorStore;
                pg_lock(pg).load_operator(operator_id).map_err(Into::into)
            }
        }
    }

    pub fn load_operators(&self) -> SharedResult<Vec<kirra_persistence::OperatorRecord>> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.load_operators()).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::OperatorStore;
                pg_lock(pg).load_operators().map_err(Into::into)
            }
        }
    }

    /// FUSED grant-create. Local: the SQLite chained insert runs unchanged.
    /// Pg: the PG grant row lands first (it can fail; a failed insert must
    /// not be ledgered), then the SAME OperatorClearanceGrantIssued payload
    /// is appended to the LOCAL ledger.
    pub fn save_clearance_grant_chained_with_auth(
        &self,
        node_id: &str,
        operator_id: &str,
        granted_at_ms: u64,
        auth_method: &str,
        operator_key_fingerprint: Option<&str>,
    ) -> SharedResult<i64> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| {
                    s.save_clearance_grant_chained_with_auth(
                        node_id,
                        operator_id,
                        granted_at_ms,
                        auth_method,
                        operator_key_fingerprint,
                    )
                })
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                let id = pg_lock(pg).insert_clearance_grant_row(
                    node_id,
                    operator_id,
                    granted_at_ms,
                    granted_at_ms,
                    auth_method,
                    operator_key_fingerprint,
                )?;
                let payload = serde_json::json!({
                    "node_id": node_id,
                    "operator_id": operator_id,
                    "granted_at_ms": granted_at_ms,
                    "delivery": "PENDING-NODE-TRANSPORT",
                    "auth_method": auth_method,
                    "operator_key_fingerprint": operator_key_fingerprint,
                });
                self.ledger_append(
                    "OperatorClearanceGrantIssued",
                    &payload.to_string(),
                    granted_at_ms,
                );
                Ok(id)
            }
        }
    }

    pub fn take_pending_clearance_grant(
        &self,
        node_id: &str,
        now_ms: u64,
    ) -> SharedResult<Option<kirra_persistence::PendingClearanceGrant>> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.take_pending_clearance_grant(node_id, now_ms))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg_lock(pg)
                .take_pending_clearance_grant(node_id, now_ms)
                .map_err(Into::into),
        }
    }

    /// FUSED outcome record. Local: unchanged (row + chained event, one tx).
    /// Pg: row first (phantom check — a missing grant must not be ledgered),
    /// then the SAME ClearanceDelivered/ClearanceDeliveryRejected event on
    /// the LOCAL ledger.
    pub fn record_grant_outcome(
        &self,
        grant_rowid: i64,
        outcome: &str,
        detail: Option<&str>,
        now_ms: u64,
    ) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.record_grant_outcome(grant_rowid, outcome, detail, now_ms))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                let found = pg_lock(pg).record_grant_outcome_row(grant_rowid, outcome, detail)?;
                if !found {
                    return Err(SharedError::Sqlite(rusqlite::Error::QueryReturnedNoRows));
                }
                let event_type = if outcome == "Cleared" {
                    "ClearanceDelivered"
                } else {
                    "ClearanceDeliveryRejected"
                };
                let payload = serde_json::json!({
                    "grant_rowid": grant_rowid,
                    "outcome": outcome,
                    "detail": detail,
                });
                self.ledger_append(event_type, &payload.to_string(), now_ms);
                Ok(())
            }
        }
    }

    pub fn latest_clearance_grant(
        &self,
        node_id: &str,
    ) -> SharedResult<Option<kirra_persistence::ClearanceGrantState>> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.latest_clearance_grant(node_id))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg_lock(pg)
                .latest_clearance_grant(node_id)
                .map_err(Into::into),
        }
    }
}

// ---------------------------------------------------------------------------
// Tier: API principals + cert principals
// ---------------------------------------------------------------------------

impl SharedOps {
    pub fn register_api_principal(
        &self,
        principal_id: &str,
        token_sha256: &str,
        role: &str,
        now_ms: u64,
    ) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.register_api_principal(principal_id, token_sha256, role, now_ms))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::PrincipalStore;
                pg_lock(pg)
                    .register_api_principal(principal_id, token_sha256, role, now_ms)
                    .map_err(Into::into)
            }
        }
    }

    pub fn revoke_api_principal(&self, principal_id: &str, now_ms: u64) -> SharedResult<bool> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.revoke_api_principal(principal_id, now_ms))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::PrincipalStore;
                pg_lock(pg)
                    .revoke_api_principal(principal_id, now_ms)
                    .map_err(Into::into)
            }
        }
    }

    pub fn load_api_principal_by_token_hash(
        &self,
        token_sha256: &str,
    ) -> SharedResult<Option<kirra_persistence::ApiPrincipalRecord>> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.load_api_principal_by_token_hash(token_sha256))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::PrincipalStore;
                pg_lock(pg)
                    .load_api_principal_by_token_hash(token_sha256)
                    .map_err(Into::into)
            }
        }
    }

    pub fn load_api_principals(&self) -> SharedResult<Vec<kirra_persistence::ApiPrincipalRecord>> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.load_api_principals()).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::PrincipalStore;
                pg_lock(pg).load_api_principals().map_err(Into::into)
            }
        }
    }

    pub fn register_cert_principal(
        &self,
        principal_id: &str,
        cert_sha256: &str,
        role: &str,
        not_after_ms: Option<u64>,
        now_ms: u64,
    ) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| {
                    s.register_cert_principal(principal_id, cert_sha256, role, not_after_ms, now_ms)
                })
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::CertPrincipalStore;
                pg_lock(pg)
                    .register_cert_principal(principal_id, cert_sha256, role, not_after_ms, now_ms)
                    .map_err(Into::into)
            }
        }
    }

    pub fn revoke_cert_principal(&self, principal_id: &str, now_ms: u64) -> SharedResult<bool> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.revoke_cert_principal(principal_id, now_ms))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::CertPrincipalStore;
                pg_lock(pg)
                    .revoke_cert_principal(principal_id, now_ms)
                    .map_err(Into::into)
            }
        }
    }

    pub fn load_cert_principal_by_fingerprint(
        &self,
        cert_sha256: &str,
    ) -> SharedResult<Option<kirra_persistence::CertPrincipalRecord>> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.load_cert_principal_by_fingerprint(cert_sha256))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::CertPrincipalStore;
                pg_lock(pg)
                    .load_cert_principal_by_fingerprint(cert_sha256)
                    .map_err(Into::into)
            }
        }
    }

    pub fn load_cert_principals(
        &self,
    ) -> SharedResult<Vec<kirra_persistence::CertPrincipalRecord>> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.load_cert_principals()).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::CertPrincipalStore;
                pg_lock(pg).load_cert_principals().map_err(Into::into)
            }
        }
    }

    pub fn cert_expiry_summary(
        &self,
        now_ms: u64,
        warn_window_ms: u64,
    ) -> SharedResult<kirra_persistence::CertExpirySummary> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.cert_expiry_summary(now_ms, warn_window_ms))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg_lock(pg)
                .cert_expiry_summary(now_ms, warn_window_ms)
                .map_err(Into::into),
        }
    }
}

// ---------------------------------------------------------------------------
// Tier: fabric assets + OTA campaigns + AV subsystem meta
// ---------------------------------------------------------------------------

impl SharedOps {
    pub fn save_fabric_asset(
        &self,
        asset: &kirra_fabric_types::asset::FabricAsset,
    ) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.save_fabric_asset(asset))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::FabricAssetStore;
                pg_lock(pg).save_fabric_asset(asset).map_err(Into::into)
            }
        }
    }

    pub fn load_fabric_assets(&self) -> SharedResult<Vec<kirra_fabric_types::asset::FabricAsset>> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.load_fabric_assets()).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::FabricAssetStore;
                pg_lock(pg).load_fabric_assets().map_err(Into::into)
            }
        }
    }

    pub fn insert_campaign(&self, campaign: &kirra_ota_campaign::Campaign) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.insert_campaign(campaign))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::OtaCampaignStore;
                pg_lock(pg).insert_campaign(campaign).map_err(Into::into)
            }
        }
    }

    pub fn load_campaign(
        &self,
        campaign_id: &str,
    ) -> SharedResult<Option<kirra_ota_campaign::Campaign>> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.load_campaign(campaign_id))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::OtaCampaignStore;
                pg_lock(pg).load_campaign(campaign_id).map_err(Into::into)
            }
        }
    }

    pub fn load_campaigns(&self) -> SharedResult<Vec<kirra_ota_campaign::Campaign>> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.load_campaigns()).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::OtaCampaignStore;
                pg_lock(pg).load_campaigns().map_err(Into::into)
            }
        }
    }

    pub fn load_active_campaigns(&self) -> SharedResult<Vec<kirra_ota_campaign::Campaign>> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.load_active_campaigns())
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::OtaCampaignStore;
                pg_lock(pg).load_active_campaigns().map_err(Into::into)
            }
        }
    }

    /// FUSED campaign lifecycle update. Local: the SQLite single-transaction
    /// row-update + chained audit runs UNCHANGED (incl. the phantom-mutation
    /// refusal). Pg: the row update commits first (a phantom must never be
    /// ledgered — the same rule the SQLite transaction enforces), then the
    /// SAME R156-shaped payload is appended to the LOCAL ledger.
    pub fn update_campaign(
        &self,
        campaign: &kirra_ota_campaign::Campaign,
        event_type: &str,
    ) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.update_campaign(campaign, event_type))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                if !pg_lock(pg).update_campaign_row(campaign)? {
                    return Err(SharedError::Sqlite(rusqlite::Error::QueryReturnedNoRows));
                }
                let payload = kirra_persistence::campaign_audit_payload(campaign, event_type);
                self.ledger_append(event_type, &payload, campaign.updated_at_ms);
                Ok(())
            }
        }
    }

    /// [`Self::update_campaign`], fenced on the caller's held epoch (#1093).
    pub fn update_campaign_epoch_fenced(
        &self,
        campaign: &kirra_ota_campaign::Campaign,
        event_type: &str,
        held_epoch: u64,
    ) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.update_campaign_epoch_fenced(campaign, event_type, held_epoch))
                .map_err(map_local_fenced),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                if !pg_lock(pg).update_campaign_row_epoch_fenced(campaign, held_epoch)? {
                    return Err(SharedError::Sqlite(rusqlite::Error::QueryReturnedNoRows));
                }
                let payload = kirra_persistence::campaign_audit_payload(campaign, event_type);
                self.ledger_append(event_type, &payload, campaign.updated_at_ms);
                Ok(())
            }
        }
    }

    pub fn upsert_node_artifact_status(
        &self,
        st: &kirra_ota_campaign::NodeArtifactStatus,
    ) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.upsert_node_artifact_status(st))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::OtaCampaignStore;
                pg_lock(pg)
                    .upsert_node_artifact_status(st)
                    .map_err(Into::into)
            }
        }
    }

    pub fn load_node_artifact_statuses(
        &self,
    ) -> SharedResult<Vec<kirra_ota_campaign::NodeArtifactStatus>> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.load_node_artifact_statuses())
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::OtaCampaignStore;
                pg_lock(pg)
                    .load_node_artifact_statuses()
                    .map_err(Into::into)
            }
        }
    }

    pub fn register_av_subsystem_meta(
        &self,
        node_id: &str,
        subsystem_type: &str,
        hardware_id: &str,
        confidence_floor: f64,
        initial_telemetry_ms: u64,
    ) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| {
                    s.register_av_subsystem_meta(
                        node_id,
                        subsystem_type,
                        hardware_id,
                        confidence_floor,
                        initial_telemetry_ms,
                    )
                })
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::AvSubsystemStore;
                pg_lock(pg)
                    .register_av_subsystem_meta(
                        node_id,
                        subsystem_type,
                        hardware_id,
                        confidence_floor,
                        initial_telemetry_ms,
                    )
                    .map_err(Into::into)
            }
        }
    }

    pub fn load_av_confidence_floor(&self, node_id: &str) -> SharedResult<Option<f64>> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.load_av_confidence_floor(node_id))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::AvSubsystemStore;
                pg_lock(pg)
                    .load_av_confidence_floor(node_id)
                    .map_err(Into::into)
            }
        }
    }

    pub fn touch_av_telemetry_timestamp(&self, node_id: &str, now_ms: u64) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.touch_av_telemetry_timestamp(node_id, now_ms))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::AvSubsystemStore;
                pg_lock(pg)
                    .touch_av_telemetry_timestamp(node_id, now_ms)
                    .map_err(Into::into)
            }
        }
    }

    pub fn get_last_telemetry_timestamp(&self, node_id: &str) -> SharedResult<u64> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.get_last_telemetry_timestamp(node_id))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::AvSubsystemStore;
                pg_lock(pg)
                    .get_last_telemetry_timestamp(node_id)
                    .map_err(Into::into)
            }
        }
    }

    pub fn load_all_registered_av_node_ids(&self) -> SharedResult<Vec<String>> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.load_all_registered_av_node_ids())
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::AvSubsystemStore;
                pg_lock(pg)
                    .load_all_registered_av_node_ids()
                    .map_err(Into::into)
            }
        }
    }

    pub fn load_av_subsystems(&self) -> SharedResult<Vec<kirra_persistence::AvSubsystemRecord>> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.load_av_subsystems()).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::AvSubsystemStore;
                pg_lock(pg).load_av_subsystems().map_err(Into::into)
            }
        }
    }

    pub fn load_recovery_streak(&self, node_id: &str) -> SharedResult<(u32, u64)> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.load_recovery_streak(node_id))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::AvSubsystemStore;
                pg_lock(pg)
                    .load_recovery_streak(node_id)
                    .map_err(Into::into)
            }
        }
    }

    pub fn reset_recovery_streak(&self, node_id: &str, now_ms: u64) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.reset_recovery_streak(node_id, now_ms))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::AvSubsystemStore;
                pg_lock(pg)
                    .reset_recovery_streak(node_id, now_ms)
                    .map_err(Into::into)
            }
        }
    }

    pub fn reset_recovery_streak_preserving_telemetry(&self, node_id: &str) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.reset_recovery_streak_preserving_telemetry(node_id))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::AvSubsystemStore;
                pg_lock(pg)
                    .reset_recovery_streak_preserving_telemetry(node_id)
                    .map_err(Into::into)
            }
        }
    }

    pub fn increment_recovery_streak(&self, node_id: &str, now_ms: u64) -> SharedResult<u32> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.increment_recovery_streak(node_id, now_ms))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::AvSubsystemStore;
                pg_lock(pg)
                    .increment_recovery_streak(node_id, now_ms)
                    .map_err(Into::into)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Support: the local-ledger append + &mut adapters + federation error maps
// ---------------------------------------------------------------------------

impl SharedOps {
    /// Append a hash-chained event to the LOCAL ledger and PROPAGATE failure —
    /// for sweeps whose audit entry IS the product (the cert-expiry census,
    /// Copilot #857), where dropping the append would let callers treat a
    /// missing entry as evidence. The ledger is per-instance local SQLite on
    /// BOTH backends (ADR-0038).
    pub fn ledger_append_checked(
        &self,
        event_type: &str,
        payload_json: &str,
        at_ms: u64,
    ) -> SharedResult<()> {
        self.local(|s| s.append_clearance_audit_event(event_type, payload_json, at_ms))?;
        Ok(())
    }

    /// Append a hash-chained event to the LOCAL ledger (the Pg-arm half of the
    /// decomposed fused operations). A ledger-append failure after a committed
    /// shared write is LOUD (tracing::error) but does not roll back the shared
    /// state — the divergence window is recorded in ADR-0038 and is
    /// reconcilable by cross-checking the shared rows against the chain.
    #[cfg(feature = "postgres")]
    fn ledger_append(&self, event_type: &str, payload_json: &str, at_ms: u64) {
        let r = self.local(|s| s.append_clearance_audit_event(event_type, payload_json, at_ms));
        if let Err(e) = r {
            tracing::error!(
                event_type,
                error = %e,
                "LOCAL LEDGER APPEND FAILED after a committed shared-state write — \
                 the chain is missing an event the shared backend carries; reconcile \
                 via the shared rows (ADR-0038 crash-window)"
            );
        }
    }
}

/// The federation-specific error maps: surface the distinct rejection arms the
/// handler turns into clean HTTP rejections (replay / regress), instead of
/// flattening them into a 500.
fn map_federation_dwe(e: kirra_persistence::DurableWriteError) -> SharedError {
    use kirra_persistence::DurableWriteError as D;
    match e {
        D::Fenced(f) => SharedError::Fenced(f),
        D::NonceReplay => SharedError::NonceReplay,
        D::GenerationRegress { found, high_water } => {
            SharedError::GenerationRegress { found, high_water }
        }
        D::EpochRegress { found, high_water } => SharedError::EpochRegress { found, high_water },
        D::Db(d) => SharedError::Sqlite(d),
    }
}

#[cfg(feature = "postgres")]
fn map_federation_pg(e: PgDurableWriteError) -> SharedError {
    use kirra_persistence::postgres::PgDurableWriteError as P;
    match e {
        P::Fenced(f) => SharedError::Fenced(f),
        P::NonceReplay => SharedError::NonceReplay,
        P::GenerationRegress { found, high_water } => {
            SharedError::GenerationRegress { found, high_water }
        }
        P::EpochRegress { found, high_water } => SharedError::EpochRegress { found, high_water },
        P::Db(d) => SharedError::Pg(d),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops() -> SharedOps {
        let store = VerifierStore::new(":memory:").expect("in-memory store");
        SharedOps::new(Arc::new(Mutex::new(store)), Arc::new(SharedBackend::Local))
    }

    #[test]
    fn local_arm_is_the_same_writer() {
        let o = ops();
        assert_eq!(o.backend_label(), "local-sqlite");
        assert_eq!(o.count_nodes().unwrap(), 0);
        o.save_node(&kirra_core::RegisteredNode {
            node_id: "n1".to_string(),
            status: kirra_core::NodeTrustState::Unknown,
            registered_at_ms: 1,
            last_trust_update_ms: 1,
            ak_public_pem: None,
            expected_pcr16_digest_hex: None,
            site: None,
            firmware_version: None,
        })
        .unwrap();
        assert_eq!(o.count_nodes().unwrap(), 1);
        assert!(o.node_exists("n1").unwrap());
    }

    #[test]
    fn unified_error_predicates_cover_sqlite() {
        let nf = SharedError::Sqlite(rusqlite::Error::QueryReturnedNoRows);
        assert!(nf.is_not_found());
        assert!(!nf.is_unique_violation());
    }

    #[test]
    fn fence_and_lease_round_trip_on_local() {
        let o = ops();
        let e = o.try_claim_epoch(0, "inst-A", 5).unwrap().unwrap();
        assert_eq!(o.current_epoch().unwrap(), e);
        assert!(o.renew_lease("inst-A", e, 9).unwrap());
        let lease = o.read_ha_lease().unwrap();
        assert_eq!(lease.holder.as_deref(), Some("inst-A"));
        assert!(o.assert_actuator_epoch_held(e).is_ok());
        assert!(o.assert_actuator_epoch_held(e + 1).is_err());
    }
}
