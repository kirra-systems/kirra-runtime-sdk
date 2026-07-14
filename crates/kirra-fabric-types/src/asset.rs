use kirra_core::FleetPosture;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AssetType {
    AutonomousVehicle,
    Robot,
    Drone,
    IndustrialController,
    Warehouse,
    Infrastructure,
    Unknown,
}

impl std::fmt::Display for AssetType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::AutonomousVehicle => "autonomous_vehicle",
            Self::Robot => "robot",
            Self::Drone => "drone",
            Self::IndustrialController => "industrial_controller",
            Self::Warehouse => "warehouse",
            Self::Infrastructure => "infrastructure",
            Self::Unknown => "unknown",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum KinematicProfileType {
    AutomotiveNominal,
    AutomotiveMRC,
    RobotNominal,
    DroneNominal,
    IndustrialNominal,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FabricAsset {
    pub asset_id: String,
    pub asset_type: AssetType,
    pub display_name: String,
    pub kinematic_profile: KinematicProfileType,
    pub registered_at_ms: u64,
    pub last_seen_ms: u64,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetPosture {
    pub asset_id: String,
    pub posture: FleetPosture,
    pub generation: u64,
    pub computed_at_ms: u64,
    pub contributing_nodes: Vec<String>,
    pub blocked_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FabricState {
    pub total_assets: usize,
    pub nominal_count: usize,
    pub degraded_count: usize,
    pub locked_out_count: usize,
    pub assets: Vec<AssetPosture>,
    pub fabric_generation: u64,
    pub computed_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_asset_type_serialization_round_trip() {
        for at in [
            AssetType::AutonomousVehicle,
            AssetType::Robot,
            AssetType::Drone,
            AssetType::IndustrialController,
            AssetType::Warehouse,
            AssetType::Infrastructure,
            AssetType::Unknown,
        ] {
            let json = serde_json::to_string(&at).unwrap();
            let rt: AssetType = serde_json::from_str(&json).unwrap();
            assert_eq!(at, rt);
        }
    }

    #[test]
    fn test_fabric_asset_serializes() {
        let asset = FabricAsset {
            asset_id: "drone_01".to_string(),
            asset_type: AssetType::Drone,
            display_name: "Survey Drone 01".to_string(),
            kinematic_profile: KinematicProfileType::DroneNominal,
            registered_at_ms: 1000,
            last_seen_ms: 2000,
            metadata: HashMap::new(),
        };
        let json = serde_json::to_string(&asset).unwrap();
        let rt: FabricAsset = serde_json::from_str(&json).unwrap();
        assert_eq!(rt.asset_id, "drone_01");
        assert_eq!(rt.asset_type, AssetType::Drone);
    }
}
