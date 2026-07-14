use crate::posture_cache::now_ms;
use crate::store_handle::StoreHandle;
use crate::verifier_store::VerifierStore;
use sha2::{Digest, Sha256};

// ADR-0035 Stage 2.5 C2 slice 2: the pure `CausalLogEntry` record + the
// `CAUSAL_EXPORT_MAX_PAGE` bound were relocated to the lean `kirra-fabric-types`
// crate so `verifier_store::fabric` names them without this service-layer facade
// module. Re-exported so every existing `crate::fabric::causal_log::CausalLogEntry`
// / `CAUSAL_EXPORT_MAX_PAGE` path resolves unchanged. The hashing/signing FACADE
// (`FabricCausalLog`, over the shared `VerifierStore`) stays here.
pub use kirra_fabric_types::{CausalLogEntry, CAUSAL_EXPORT_MAX_PAGE};

/// Forensic, tamper-evident, hash-chained, signed, PERSISTED causal ledger (#87).
///
/// All entries are durably persisted to the `fabric_causal_log` SQLite table via
/// the SHARED [`VerifierStore`]; the record hash binds the causality edges
/// (`caused_by`, `affects_assets`, `fabric_generation`) so edge tampering is
/// detected. Reuses the audit-chain machinery rather than forking a weaker one.
pub struct FabricCausalLog {
    store: StoreHandle,
    signing_key: Option<ed25519_dalek::SigningKey>,
}

fn generate_entry_id(asset_id: &str, event_type: &str, timestamp_ms: u64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(asset_id.as_bytes());
    hasher.update(event_type.as_bytes());
    hasher.update(timestamp_ms.to_le_bytes());
    // Add sub-millisecond entropy from the entry count placeholder
    hasher.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
            .to_le_bytes(),
    );
    hex::encode(&hasher.finalize()[..16])
}

impl FabricCausalLog {
    /// Build over a SHARED [`StoreHandle`] so causal rows land in the same DB
    /// the rest of the service uses. Entries are durably persisted + chained.
    pub fn new(store: StoreHandle, signing_key: Option<ed25519_dalek::SigningKey>) -> Self {
        Self { store, signing_key }
    }

    /// Test/standalone constructor over a fresh in-memory store. Used where a
    /// caller has no shared store handy (tests, isolated integration cases).
    pub fn new_in_memory(signing_key: Option<ed25519_dalek::SigningKey>) -> Self {
        let store = VerifierStore::new(":memory:").expect("in-memory verifier store");
        Self {
            store: StoreHandle::new(store),
            signing_key,
        }
    }

    pub fn record(
        &self,
        asset_id: &str,
        event_type: &str,
        payload: &str,
        caused_by: Vec<String>,
        affects_assets: Vec<String>,
        fabric_generation: u64,
    ) -> String {
        let timestamp_ms = now_ms();
        let entry_id = generate_entry_id(asset_id, event_type, timestamp_ms);

        // Persist to the hash-chained forensic ledger. A persistence failure
        // (lock poison or DB error) must NOT crash the SG-007 propagation path —
        // log it and still return the entry_id.
        self.store.with(|store| {
            if let Err(e) = store.append_causal_event(
                &crate::verifier_store::CausalEventInput {
                    entry_id: &entry_id,
                    asset_id,
                    event_type,
                    payload,
                    caused_by: &caused_by,
                    affects_assets: &affects_assets,
                    fabric_generation,
                    timestamp_ms,
                },
                self.signing_key.as_ref(),
            ) {
                tracing::error!(
                    error = %e,
                    entry_id = %entry_id,
                    "failed to persist causal-log entry (#87)"
                );
            }
        });

        entry_id
    }

    pub fn causal_chain(&self, entry_id: &str) -> Vec<CausalLogEntry> {
        let entries = self
            .store
            .with(|store| store.load_causal_entries().unwrap_or_default());

        // BFS/DFS over `caused_by`, dedup via visited set, sort by timestamp.
        let mut result = Vec::new();
        let mut to_visit = vec![entry_id.to_string()];
        let mut visited = std::collections::HashSet::new();

        while let Some(current_id) = to_visit.pop() {
            if visited.contains(&current_id) {
                continue;
            }
            visited.insert(current_id.clone());

            if let Some(entry) = entries.iter().find(|e| e.entry_id == current_id) {
                for cause_id in &entry.caused_by {
                    to_visit.push(cause_id.clone());
                }
                result.push(entry.clone());
            }
        }

        result.sort_by_key(|e| e.timestamp_ms);
        result
    }

    pub fn export(&self, from_ms: u64, to_ms: u64) -> Vec<CausalLogEntry> {
        self.export_page(from_ms, to_ms, CAUSAL_EXPORT_MAX_PAGE, 0)
    }

    /// Bounded, paginated export over `[from_ms, to_ms]`. `limit` is clamped to
    /// [`CAUSAL_EXPORT_MAX_PAGE`] so the response is always bounded.
    pub fn export_page(
        &self,
        from_ms: u64,
        to_ms: u64,
        limit: u32,
        offset: u32,
    ) -> Vec<CausalLogEntry> {
        let limit = limit.min(CAUSAL_EXPORT_MAX_PAGE);
        self.store.with(|store| {
            store
                .load_causal_entries_in_range(from_ms, to_ms, limit, offset)
                .unwrap_or_default()
        })
    }

    pub fn len(&self) -> usize {
        self.store
            .with(|store| store.count_causal_entries().ok())
            .map(|n| n as usize)
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signing_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])
    }

    #[test]
    fn test_record_entry_with_causal_predecessors() {
        let log = FabricCausalLog::new_in_memory(None);
        let id = log.record(
            "asset_01",
            "FAULT_DETECTED",
            "{}",
            vec![],
            vec!["asset_02".to_string()],
            1,
        );
        assert!(!id.is_empty());
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn test_causal_chain_traversal() {
        let log = FabricCausalLog::new_in_memory(None);
        let root_id = log.record(
            "gcs01",
            "GCS_FAULT",
            "{}",
            vec![],
            vec!["drone01".to_string()],
            1,
        );
        let child_id = log.record(
            "drone01",
            "DRONE_DEGRADED",
            "{}",
            vec![root_id.clone()],
            vec![],
            1,
        );

        let chain = log.causal_chain(&child_id);
        let ids: Vec<&str> = chain.iter().map(|e| e.entry_id.as_str()).collect();
        assert!(
            ids.contains(&root_id.as_str()),
            "chain must contain root: {ids:?}"
        );
        assert!(
            ids.contains(&child_id.as_str()),
            "chain must contain child: {ids:?}"
        );
    }

    #[test]
    fn test_causal_chain_stops_at_root() {
        let log = FabricCausalLog::new_in_memory(None);
        let root = log.record("a01", "ROOT", "{}", vec![], vec![], 1);
        let chain = log.causal_chain(&root);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].entry_id, root);
    }

    #[test]
    fn test_export_by_time_window() {
        let log = FabricCausalLog::new_in_memory(None);
        log.record("a01", "EVT_A", "{}", vec![], vec![], 1);
        // Export a future window — should get nothing
        let future = now_ms() + 10_000;
        let entries = log.export(future, future + 1000);
        assert!(entries.is_empty());
    }

    #[test]
    fn test_cross_asset_causality_recorded() {
        let log = FabricCausalLog::new_in_memory(None);
        let id = log.record(
            "infra01",
            "INFRA_LOCKOUT",
            "{}",
            vec![],
            vec!["robot01".to_string(), "robot02".to_string()],
            5,
        );
        let entries = log.export(0, u64::MAX);
        let entry = entries.iter().find(|e| e.entry_id == id).unwrap();
        assert!(entry.affects_assets.contains(&"robot01".to_string()));
        assert!(entry.affects_assets.contains(&"robot02".to_string()));
        assert_eq!(entry.fabric_generation, 5);
    }

    #[test]
    fn test_chain_integrity_over_n_records_unsigned() {
        let log = FabricCausalLog::new_in_memory(None);
        for i in 0..6 {
            log.record("a", &format!("EVT_{i}"), "{}", vec![], vec![], i);
        }
        let r = log
            .store
            .with(|store| store.verify_causal_chain_integrity(None))
            .unwrap();
        assert_eq!(r.total_entries, 6);
        assert!(r.chain_intact, "unsigned chain must be intact");
        assert!(r.head_verified, "head must verify: {}", r.head_status);
    }

    #[test]
    fn test_full_record_signature_verifies_with_key() {
        let key = signing_key();
        let vk = key.verifying_key();
        let log = FabricCausalLog::new_in_memory(Some(key));
        let root = log.record("a", "ROOT", "{}", vec![], vec!["x".to_string()], 1);
        log.record("b", "CHILD", "{}", vec![root], vec![], 1);

        let r = log
            .store
            .with(|store| store.verify_causal_chain_integrity(Some(&vk)))
            .unwrap();
        assert!(r.chain_intact);
        assert!(r.signature_valid, "all signatures must verify");
        assert!(r.head_verified, "head must verify: {}", r.head_status);
        assert_eq!(r.signed_entries, 2);
        assert_eq!(r.unsigned_entries, 0);
    }
}
