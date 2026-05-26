use std::sync::atomic::{AtomicU64, Ordering};
use dashmap::DashMap;
use crate::fabric::asset::{AssetPosture, AssetType, FabricAsset, FabricState};
use crate::fabric::governor::AssetGovernor;
use crate::gateway::kinematics_contract::{EnforceAction, ProposedVehicleCommand};
use crate::verifier::FleetPosture;
use crate::posture_cache::now_ms;

#[derive(Debug)]
pub enum FabricError {
    AssetNotFound(String),
    GovernorError(String),
    PostureUnavailable(String),
}

impl std::fmt::Display for FabricError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AssetNotFound(id) => write!(f, "Asset not found: {id}"),
            Self::GovernorError(msg) => write!(f, "Governor error: {msg}"),
            Self::PostureUnavailable(id) => write!(f, "Posture unavailable for: {id}"),
        }
    }
}

pub struct FabricRouter {
    governors: DashMap<String, AssetGovernor>,
    assets: DashMap<String, FabricAsset>,
    asset_postures: DashMap<String, AssetPosture>,
    fabric_generation: AtomicU64,
}

impl FabricRouter {
    pub fn new() -> Self {
        Self {
            governors: DashMap::new(),
            assets: DashMap::new(),
            asset_postures: DashMap::new(),
            fabric_generation: AtomicU64::new(1),
        }
    }

    pub fn register_asset(&self, asset: &FabricAsset) {
        let governor = AssetGovernor::new(
            asset.asset_id.clone(),
            asset.kinematic_profile.clone(),
        );
        self.governors.insert(asset.asset_id.clone(), governor);
        self.assets.insert(asset.asset_id.clone(), asset.clone());

        // Initialize with Nominal posture until first real posture update
        let initial_posture = AssetPosture {
            asset_id: asset.asset_id.clone(),
            posture: FleetPosture::Nominal,
            generation: 1,
            computed_at_ms: now_ms(),
            contributing_nodes: vec![],
            blocked_by: vec![],
        };
        self.asset_postures.entry(asset.asset_id.clone()).or_insert(initial_posture);
    }

    pub fn route_command(
        &self,
        asset_id: &str,
        cmd: &ProposedVehicleCommand,
    ) -> Result<EnforceAction, FabricError> {
        let governor = self.governors.get(asset_id)
            .ok_or_else(|| FabricError::AssetNotFound(asset_id.to_string()))?;

        let posture = self.asset_postures.get(asset_id)
            .map(|p| p.posture.clone())
            .unwrap_or(FleetPosture::LockedOut);  // fail-closed if posture unknown

        Ok(governor.evaluate_command(cmd, &posture))
    }

    pub fn update_asset_posture(&self, asset_id: &str, posture: AssetPosture) {
        self.asset_postures.insert(asset_id.to_string(), posture);
        self.fabric_generation.fetch_add(1, Ordering::SeqCst);
    }

    pub fn fabric_state(&self) -> FabricState {
        let now = now_ms();
        let gen = self.fabric_generation.load(Ordering::SeqCst);

        let mut assets: Vec<AssetPosture> = self.asset_postures.iter()
            .map(|r| r.value().clone())
            .collect();
        assets.sort_by(|a, b| a.asset_id.cmp(&b.asset_id));

        let nominal_count = assets.iter().filter(|a| a.posture == FleetPosture::Nominal).count();
        let degraded_count = assets.iter().filter(|a| a.posture == FleetPosture::Degraded).count();
        let locked_out_count = assets.iter().filter(|a| a.posture == FleetPosture::LockedOut).count();

        FabricState {
            total_assets: assets.len(),
            nominal_count,
            degraded_count,
            locked_out_count,
            assets,
            fabric_generation: gen,
            computed_at_ms: now,
        }
    }

    /// Cross-asset trust propagation rules.
    /// Returns a list of (asset_id, forced_posture) pairs to apply.
    pub fn propagate_cross_asset_trust(&self) -> Vec<(String, FleetPosture)> {
        let mut changes: Vec<(String, FleetPosture)> = Vec::new();

        // Collect current postures and asset metadata
        let all_assets: Vec<(String, AssetType, AssetPosture, std::collections::HashMap<String, String>)> =
            self.assets.iter().filter_map(|a| {
                let posture = self.asset_postures.get(&a.asset_id as &str)?.clone();
                Some((
                    a.asset_id.clone(),
                    a.asset_type.clone(),
                    posture,
                    a.metadata.clone(),
                ))
            }).collect();

        // Rule 1: Drone depends on ground control station (IndustrialController)
        let ground_stations_locked: bool = all_assets.iter().any(|(_, at, ap, _)|
            *at == AssetType::IndustrialController && ap.posture == FleetPosture::LockedOut
        );
        if ground_stations_locked {
            for (id, at, ap, _) in &all_assets {
                if *at == AssetType::Drone && ap.posture == FleetPosture::Nominal {
                    changes.push((id.clone(), FleetPosture::Degraded));
                }
            }
        }

        // Rule 2: Convoy follower degrades when leader is LockedOut
        let leader_locked: bool = all_assets.iter().any(|(_, _, ap, meta)|
            meta.get("convoy_role").map(|r| r == "leader").unwrap_or(false)
                && ap.posture == FleetPosture::LockedOut
        );
        if leader_locked {
            for (id, _, ap, meta) in &all_assets {
                if meta.get("convoy_role").map(|r| r == "follower").unwrap_or(false)
                    && ap.posture == FleetPosture::Nominal
                {
                    changes.push((id.clone(), FleetPosture::Degraded));
                }
            }
        }

        // Rule 3: Infrastructure lockout degrades dependents
        let infra_locked: bool = all_assets.iter().any(|(_, at, ap, _)|
            *at == AssetType::Infrastructure && ap.posture == FleetPosture::LockedOut
        );
        if infra_locked {
            for (id, _, ap, meta) in &all_assets {
                if meta.get("depends_on_infrastructure").map(|v| v == "true").unwrap_or(false)
                    && ap.posture == FleetPosture::Nominal
                {
                    changes.push((id.clone(), FleetPosture::Degraded));
                }
            }
        }

        // Rule 4: Warehouse lockout degrades all registered robots
        let locked_warehouses: Vec<String> = all_assets.iter()
            .filter(|(_, at, ap, _)| *at == AssetType::Warehouse && ap.posture == FleetPosture::LockedOut)
            .map(|(id, _, _, _)| id.clone())
            .collect();
        if !locked_warehouses.is_empty() {
            for (id, at, ap, meta) in &all_assets {
                if *at == AssetType::Robot && ap.posture == FleetPosture::Nominal {
                    let robot_warehouse = meta.get("warehouse_id").map(|s| s.as_str()).unwrap_or("");
                    if locked_warehouses.iter().any(|w| w == robot_warehouse) {
                        changes.push((id.clone(), FleetPosture::Degraded));
                    }
                }
            }
        }

        changes
    }

    pub fn asset_count(&self) -> usize {
        self.assets.len()
    }

    pub fn list_assets(&self) -> Vec<FabricAsset> {
        let mut assets: Vec<FabricAsset> = self.assets.iter().map(|r| r.value().clone()).collect();
        assets.sort_by(|a, b| a.asset_id.cmp(&b.asset_id));
        assets
    }
}

impl Default for FabricRouter {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use crate::fabric::asset::{AssetType, FabricAsset, KinematicProfileType};
    use std::collections::HashMap;

    fn make_asset(id: &str, asset_type: AssetType, profile: KinematicProfileType) -> FabricAsset {
        FabricAsset {
            asset_id: id.to_string(),
            asset_type,
            display_name: id.to_string(),
            kinematic_profile: profile,
            registered_at_ms: 1000,
            last_seen_ms: 1000,
            metadata: HashMap::new(),
        }
    }

    fn make_asset_with_meta(id: &str, asset_type: AssetType, meta: Vec<(&str, &str)>) -> FabricAsset {
        let mut asset = make_asset(id, asset_type, KinematicProfileType::RobotNominal);
        for (k, v) in meta {
            asset.metadata.insert(k.to_string(), v.to_string());
        }
        asset
    }

    fn safe_cmd() -> ProposedVehicleCommand {
        ProposedVehicleCommand {
            linear_velocity_mps: 0.1,
            current_velocity_mps: 0.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        }
    }

    #[test]
    fn test_route_command_to_correct_asset_governor() {
        let router = FabricRouter::new();
        router.register_asset(&make_asset("r01", AssetType::Robot, KinematicProfileType::RobotNominal));
        let result = router.route_command("r01", &safe_cmd());
        assert!(result.is_ok());
    }

    #[test]
    fn test_unknown_asset_returns_error() {
        let router = FabricRouter::new();
        let result = router.route_command("nonexistent", &safe_cmd());
        assert!(matches!(result, Err(FabricError::AssetNotFound(_))));
    }

    #[test]
    fn test_fabric_state_aggregates_all_assets() {
        let router = FabricRouter::new();
        router.register_asset(&make_asset("r01", AssetType::Robot, KinematicProfileType::RobotNominal));
        router.register_asset(&make_asset("r02", AssetType::Robot, KinematicProfileType::RobotNominal));
        let state = router.fabric_state();
        assert_eq!(state.total_assets, 2);
    }

    #[test]
    fn test_cross_asset_propagation_drone_depends_on_ground_station() {
        let router = FabricRouter::new();
        router.register_asset(&make_asset("gcs01", AssetType::IndustrialController, KinematicProfileType::IndustrialNominal));
        router.register_asset(&make_asset("drone01", AssetType::Drone, KinematicProfileType::DroneNominal));

        // Lock out the ground control station
        router.update_asset_posture("gcs01", AssetPosture {
            asset_id: "gcs01".to_string(),
            posture: FleetPosture::LockedOut,
            generation: 2,
            computed_at_ms: 1000,
            contributing_nodes: vec![],
            blocked_by: vec!["gcs_sensor_01".to_string()],
        });

        let changes = router.propagate_cross_asset_trust();
        assert!(changes.iter().any(|(id, p)| id == "drone01" && *p == FleetPosture::Degraded),
            "drone01 must degrade when ground station is locked out; changes={changes:?}");
    }

    #[test]
    fn test_cross_asset_propagation_convoy_follower_degrades_with_leader() {
        let router = FabricRouter::new();
        router.register_asset(&make_asset_with_meta("leader01", AssetType::AutonomousVehicle,
            vec![("convoy_role", "leader")]));
        router.register_asset(&make_asset_with_meta("follower01", AssetType::AutonomousVehicle,
            vec![("convoy_role", "follower")]));

        router.update_asset_posture("leader01", AssetPosture {
            asset_id: "leader01".to_string(),
            posture: FleetPosture::LockedOut,
            generation: 2,
            computed_at_ms: 1000,
            contributing_nodes: vec![],
            blocked_by: vec!["lidar_01".to_string()],
        });

        let changes = router.propagate_cross_asset_trust();
        assert!(changes.iter().any(|(id, p)| id == "follower01" && *p == FleetPosture::Degraded),
            "follower must degrade when leader is locked out");
    }

    #[test]
    fn test_warehouse_lockout_degrades_all_robots() {
        let router = FabricRouter::new();
        router.register_asset(&make_asset("wh01", AssetType::Warehouse, KinematicProfileType::IndustrialNominal));
        router.register_asset(&make_asset_with_meta("robot01", AssetType::Robot, vec![("warehouse_id", "wh01")]));
        router.register_asset(&make_asset_with_meta("robot02", AssetType::Robot, vec![("warehouse_id", "wh01")]));

        router.update_asset_posture("wh01", AssetPosture {
            asset_id: "wh01".to_string(),
            posture: FleetPosture::LockedOut,
            generation: 2,
            computed_at_ms: 1000,
            contributing_nodes: vec![],
            blocked_by: vec!["access_sensor".to_string()],
        });

        let changes = router.propagate_cross_asset_trust();
        assert!(changes.iter().any(|(id, p)| id == "robot01" && *p == FleetPosture::Degraded));
        assert!(changes.iter().any(|(id, p)| id == "robot02" && *p == FleetPosture::Degraded));
    }

    #[test]
    fn test_concurrent_command_routing_thread_safe() {
        use std::thread;
        let router = Arc::new(FabricRouter::new());
        router.register_asset(&make_asset("r01", AssetType::Robot, KinematicProfileType::RobotNominal));

        let handles: Vec<_> = (0..10).map(|_| {
            let r = Arc::clone(&router);
            thread::spawn(move || {
                for _ in 0..100 {
                    let _ = r.route_command("r01", &ProposedVehicleCommand {
                        linear_velocity_mps: 0.1,
                        current_velocity_mps: 0.0,
                        delta_time_s: 0.1,
                        steering_angle_deg: 0.0,
                        current_steering_angle_deg: 0.0,
                    });
                }
            })
        }).collect();
        for h in handles { h.join().unwrap(); }
    }
}
