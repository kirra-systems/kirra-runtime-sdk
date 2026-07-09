use crate::fabric::asset::AssetType;
use crate::posture_cache::now_ms;
use crate::verifier::FleetPosture;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetTelemetrySnapshot {
    pub asset_id: String,
    pub asset_type: AssetType,
    pub posture: FleetPosture,
    pub node_count: usize,
    pub trusted_nodes: usize,
    pub untrusted_nodes: usize,
    pub last_command_ms: u64,
    pub last_command_action: String,
    pub commands_per_minute: f64,
    pub denial_rate: f64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FabricTelemetrySummary {
    pub total_assets: usize,
    pub active_assets: usize,
    pub total_commands_per_minute: f64,
    pub fabric_denial_rate: f64,
    pub assets_by_type: HashMap<String, usize>,
    pub assets_by_posture: HashMap<String, usize>,
    pub highest_denial_asset: Option<String>,
    pub computed_at_ms: u64,
}

pub struct FabricTelemetry {
    snapshots: DashMap<String, AssetTelemetrySnapshot>,
}

impl FabricTelemetry {
    pub fn new() -> Self {
        Self {
            snapshots: DashMap::new(),
        }
    }

    pub fn update(&self, snapshot: AssetTelemetrySnapshot) {
        self.snapshots.insert(snapshot.asset_id.clone(), snapshot);
    }

    pub fn asset_snapshot(&self, asset_id: &str) -> Option<AssetTelemetrySnapshot> {
        self.snapshots.get(asset_id).map(|s| s.clone())
    }

    /// #396 console analytics — all per-asset telemetry snapshots. Read-only
    /// clone of the live map for the console's interventions-by-asset rollup.
    pub fn all_snapshots(&self) -> Vec<AssetTelemetrySnapshot> {
        self.snapshots.iter().map(|r| r.value().clone()).collect()
    }

    pub fn summary(&self) -> FabricTelemetrySummary {
        let now = now_ms();
        let active_threshold_ms = 30_000u64;

        let all: Vec<AssetTelemetrySnapshot> =
            self.snapshots.iter().map(|r| r.value().clone()).collect();

        let total_assets = all.len();
        let active_assets = all
            .iter()
            .filter(|s| now.saturating_sub(s.updated_at_ms) < active_threshold_ms)
            .count();

        let total_cpm: f64 = all.iter().map(|s| s.commands_per_minute).sum();

        let total_commands: f64 = all
            .iter()
            .map(|s| s.commands_per_minute)
            .sum::<f64>()
            .max(1.0);
        let weighted_denial: f64 = all
            .iter()
            .map(|s| s.denial_rate * s.commands_per_minute)
            .sum::<f64>();
        let fabric_denial_rate = weighted_denial / total_commands;

        let mut assets_by_type: HashMap<String, usize> = HashMap::new();
        let mut assets_by_posture: HashMap<String, usize> = HashMap::new();
        for s in &all {
            *assets_by_type
                .entry(format!("{:?}", s.asset_type))
                .or_insert(0) += 1;
            *assets_by_posture
                .entry(format!("{:?}", s.posture))
                .or_insert(0) += 1;
        }

        let highest_denial_asset = all
            .iter()
            .max_by(|a, b| {
                a.denial_rate
                    .partial_cmp(&b.denial_rate)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .filter(|s| s.denial_rate > 0.0)
            .map(|s| s.asset_id.clone());

        FabricTelemetrySummary {
            total_assets,
            active_assets,
            total_commands_per_minute: total_cpm,
            fabric_denial_rate,
            assets_by_type,
            assets_by_posture,
            highest_denial_asset,
            computed_at_ms: now,
        }
    }
}

impl Default for FabricTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fabric::asset::AssetType;

    fn snap(
        id: &str,
        cpm: f64,
        denial: f64,
        posture: FleetPosture,
        stale: bool,
    ) -> AssetTelemetrySnapshot {
        let now = now_ms();
        AssetTelemetrySnapshot {
            asset_id: id.to_string(),
            asset_type: AssetType::Robot,
            posture,
            node_count: 4,
            trusted_nodes: 4,
            untrusted_nodes: 0,
            last_command_ms: now,
            last_command_action: "Allow".to_string(),
            commands_per_minute: cpm,
            denial_rate: denial,
            updated_at_ms: if stale {
                now.saturating_sub(60_000)
            } else {
                now
            },
        }
    }

    #[test]
    fn test_denial_rate_computed_correctly() {
        let t = FabricTelemetry::new();
        t.update(snap("r01", 60.0, 0.5, FleetPosture::Nominal, false));
        t.update(snap("r02", 60.0, 0.0, FleetPosture::Nominal, false));
        let s = t.summary();
        // weighted: (0.5*60 + 0.0*60) / 120 = 0.25
        assert!((s.fabric_denial_rate - 0.25).abs() < 1e-9);
    }

    #[test]
    fn test_active_assets_excludes_stale() {
        let t = FabricTelemetry::new();
        t.update(snap("r01", 10.0, 0.0, FleetPosture::Nominal, false));
        t.update(snap("r02", 10.0, 0.0, FleetPosture::Nominal, true)); // stale
        let s = t.summary();
        assert_eq!(s.active_assets, 1);
    }

    #[test]
    fn test_fabric_summary_aggregates_correctly() {
        let t = FabricTelemetry::new();
        t.update(snap("r01", 30.0, 0.1, FleetPosture::Nominal, false));
        t.update(snap("r02", 30.0, 0.0, FleetPosture::Degraded, false));
        let s = t.summary();
        assert_eq!(s.total_assets, 2);
        assert_eq!(s.total_commands_per_minute, 60.0);
    }
}
