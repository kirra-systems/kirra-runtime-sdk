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
    Pg(Arc<PgVerifierStore>),
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
            SharedError::Fenced(_) => false,
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
        match e {
            PgDurableWriteError::Fenced(f) => SharedError::Fenced(f),
            PgDurableWriteError::Db(d) => SharedError::Pg(d),
        }
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
                pg.save_node(node).map_err(Into::into)
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
            SharedBackend::Pg(pg) => pg
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
            SharedBackend::Pg(pg) => pg
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
                pg.load_node(node_id).map_err(Into::into)
            }
        }
    }

    pub fn load_nodes(&self) -> SharedResult<Vec<kirra_core::RegisteredNode>> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.load_nodes()).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::NodeStore;
                pg.load_nodes().map_err(Into::into)
            }
        }
    }

    pub fn node_exists(&self, node_id: &str) -> SharedResult<bool> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.node_exists(node_id)).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::NodeStore;
                pg.node_exists(node_id).map_err(Into::into)
            }
        }
    }

    pub fn count_nodes(&self) -> SharedResult<i64> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.count_nodes()).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::NodeStore;
                pg.count_nodes().map_err(Into::into)
            }
        }
    }

    pub fn node_requires_tpm_quote(&self, node_id: &str) -> SharedResult<bool> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.node_requires_tpm_quote(node_id))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg.node_requires_tpm_quote(node_id).map_err(Into::into),
        }
    }

    pub fn save_dependencies(&self, node_id: &str, deps: &[String]) -> SharedResult<()> {
        match &*self.backend {
            SharedBackend::Local => self
                .local(|s| s.save_dependencies(node_id, deps))
                .map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg.save_dependencies(node_id, deps).map_err(Into::into),
        }
    }

    pub fn load_dependencies(&self) -> SharedResult<HashMap<String, Vec<String>>> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.load_dependencies()).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg.load_dependencies().map_err(Into::into),
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
                pg.current_epoch().map_err(Into::into)
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
                pg.current_active_holder().map_err(Into::into)
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
                pg.try_claim_epoch_shared(observed, instance_id, now_ms)
                    .map_err(Into::into)
            }
        }
    }

    pub fn assert_actuator_epoch_held(&self, held_epoch: u64) -> Result<(), FenceError> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.assert_actuator_epoch_held(held_epoch)),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg.assert_actuator_epoch_held_shared(held_epoch),
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
            SharedBackend::Pg(pg) => pg
                .renew_lease(instance_id, held_epoch, now_ms)
                .map_err(Into::into),
        }
    }

    pub fn read_ha_lease(&self) -> SharedResult<kirra_persistence::HaLease> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.read_ha_lease()).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => pg.read_ha_lease().map_err(Into::into),
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
                pg.load_engine_state(key).map_err(Into::into)
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
                pg.save_engine_state(key, value).map_err(Into::into)
            }
        }
    }

    pub fn load_last_generation(&self) -> SharedResult<u64> {
        match &*self.backend {
            SharedBackend::Local => self.local(|s| s.load_last_generation()).map_err(Into::into),
            #[cfg(feature = "postgres")]
            SharedBackend::Pg(pg) => {
                use kirra_persistence::PostureEngineStateStore;
                pg.load_last_generation().map_err(Into::into)
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
                pg.save_last_generation(generation).map_err(Into::into)
            }
        }
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
