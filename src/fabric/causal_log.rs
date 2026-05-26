use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use crate::posture_cache::now_ms;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalLogEntry {
    pub entry_id: String,
    pub timestamp_ms: u64,
    pub asset_id: String,
    pub event_type: String,
    pub payload: String,
    pub caused_by: Vec<String>,
    pub affects_assets: Vec<String>,
    pub fabric_generation: u64,
    pub signature_b64: Option<String>,
}

pub struct FabricCausalLog {
    entries: Arc<Mutex<Vec<CausalLogEntry>>>,
    signing_key: Option<ed25519_dalek::SigningKey>,
}

fn generate_entry_id(asset_id: &str, event_type: &str, timestamp_ms: u64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(asset_id.as_bytes());
    hasher.update(event_type.as_bytes());
    hasher.update(timestamp_ms.to_le_bytes());
    // Add sub-millisecond entropy from the entry count placeholder
    hasher.update(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos()
        .to_le_bytes());
    hex::encode(&hasher.finalize()[..16])
}

impl FabricCausalLog {
    pub fn new(signing_key: Option<ed25519_dalek::SigningKey>) -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
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

        let signature_b64 = self.signing_key.as_ref().map(|sk| {
            use ed25519_dalek::Signer;
            let msg = format!("{entry_id}:{asset_id}:{event_type}:{timestamp_ms}:{payload}");
            let sig = sk.sign(msg.as_bytes());
            use base64::{engine::general_purpose::STANDARD as b64, Engine as _};
            b64.encode(sig.to_bytes())
        });

        let entry = CausalLogEntry {
            entry_id: entry_id.clone(),
            timestamp_ms,
            asset_id: asset_id.to_string(),
            event_type: event_type.to_string(),
            payload: payload.to_string(),
            caused_by,
            affects_assets,
            fabric_generation,
            signature_b64,
        };

        if let Ok(mut entries) = self.entries.lock() {
            entries.push(entry);
        }

        entry_id
    }

    pub fn causal_chain(&self, entry_id: &str) -> Vec<CausalLogEntry> {
        let entries = match self.entries.lock() {
            Ok(e) => e.clone(),
            Err(_) => return vec![],
        };

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
        let entries = match self.entries.lock() {
            Ok(e) => e.clone(),
            Err(_) => return vec![],
        };
        let mut result: Vec<CausalLogEntry> = entries.into_iter()
            .filter(|e| e.timestamp_ms >= from_ms && e.timestamp_ms <= to_ms)
            .collect();
        result.sort_by_key(|e| e.timestamp_ms);
        result
    }

    pub fn len(&self) -> usize {
        self.entries.lock().map(|e| e.len()).unwrap_or(0)
    }
}

impl Default for FabricCausalLog {
    fn default() -> Self { Self::new(None) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_entry_with_causal_predecessors() {
        let log = FabricCausalLog::new(None);
        let id = log.record("asset_01", "FAULT_DETECTED", "{}", vec![], vec!["asset_02".to_string()], 1);
        assert!(!id.is_empty());
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn test_causal_chain_traversal() {
        let log = FabricCausalLog::new(None);
        let root_id = log.record("gcs01", "GCS_FAULT", "{}", vec![], vec!["drone01".to_string()], 1);
        let child_id = log.record("drone01", "DRONE_DEGRADED", "{}", vec![root_id.clone()], vec![], 1);

        let chain = log.causal_chain(&child_id);
        let ids: Vec<&str> = chain.iter().map(|e| e.entry_id.as_str()).collect();
        assert!(ids.contains(&root_id.as_str()), "chain must contain root: {ids:?}");
        assert!(ids.contains(&child_id.as_str()), "chain must contain child: {ids:?}");
    }

    #[test]
    fn test_causal_chain_stops_at_root() {
        let log = FabricCausalLog::new(None);
        let root = log.record("a01", "ROOT", "{}", vec![], vec![], 1);
        let chain = log.causal_chain(&root);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].entry_id, root);
    }

    #[test]
    fn test_export_by_time_window() {
        let log = FabricCausalLog::new(None);
        log.record("a01", "EVT_A", "{}", vec![], vec![], 1);
        // Export a future window — should get nothing
        let future = now_ms() + 10_000;
        let entries = log.export(future, future + 1000);
        assert!(entries.is_empty());
    }

    #[test]
    fn test_cross_asset_causality_recorded() {
        let log = FabricCausalLog::new(None);
        let id = log.record(
            "infra01", "INFRA_LOCKOUT", "{}",
            vec![], vec!["robot01".to_string(), "robot02".to_string()], 5
        );
        let entries = log.export(0, u64::MAX);
        let entry = entries.iter().find(|e| e.entry_id == id).unwrap();
        assert!(entry.affects_assets.contains(&"robot01".to_string()));
        assert!(entry.affects_assets.contains(&"robot02".to_string()));
        assert_eq!(entry.fabric_generation, 5);
    }
}
