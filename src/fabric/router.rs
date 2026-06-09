use std::sync::atomic::{AtomicU64, Ordering};
use dashmap::DashMap;
use crate::fabric::asset::{AssetPosture, AssetType, FabricAsset, FabricState};
use crate::fabric::governor::AssetGovernor;
use crate::fabric::causal_log::FabricCausalLog;
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

/// One trigger-tagged cross-asset trust propagation (SG-007): the LockedOut
/// `trigger_asset` degraded `follower` to `new_posture`. Internal — the public
/// `propagate_cross_asset_trust` exposes only `(follower, new_posture)`; the
/// trigger tag is what lets `propagate_and_record` attribute the causal event.
struct TrustPropagation {
    trigger_asset: String,
    follower: String,
    new_posture: FleetPosture,
}

impl TrustPropagation {
    /// A follower degraded to `Degraded` by a LockedOut trigger (the only
    /// transition the propagation rules produce).
    fn degrade(trigger: &str, follower: &str) -> Self {
        Self {
            trigger_asset: trigger.to_string(),
            follower: follower.to_string(),
            new_posture: FleetPosture::Degraded,
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

        // INTERIM seed: Degraded.
        //
        // The strict fail-closed default would be LockedOut — but with no
        // production feed from the verifier's real FleetPosture into fabric
        // asset postures (follow-up #3), LockedOut would brick every
        // registered asset until an operator manually pushed a posture.
        // The registration route (/fabric/assets/register) is admin-token
        // gated, so the registrant is a trusted operator: Degraded grants
        // limited, MRC-envelope motion until the first real posture lands.
        // `evaluate_command` already dispatches Degraded to the asset's
        // per-profile `mrc_contract()` (RobotNominal→robot MRC,
        // DroneNominal→drone MRC, etc.) — no flat 5 m/s cap is imposed
        // here.
        //
        // generation: 0 is the never-yet-computed sentinel (identical
        // convention to `CachedFleetPosture::new` for fleet posture).
        // The first real engine-driven update supersedes it.
        //
        // END STATE (follow-up #3): seed LockedOut and rely on the
        // verifier→fabric posture feed to lift verified assets to
        // Nominal. Do NOT make Degraded the permanent default.
        let initial_posture = AssetPosture {
            asset_id: asset.asset_id.clone(),
            posture: FleetPosture::Degraded,
            generation: 0,
            computed_at_ms: now_ms(),
            contributing_nodes: vec![],
            blocked_by: vec!["UNVERIFIED_PENDING_FIRST_POSTURE".to_string()],
        };
        self.asset_postures.entry(asset.asset_id.clone()).or_insert(initial_posture);
    }

    // `effective_perception_cap` (KIRRA-OCCY-PMON-002): the perception-derate
    // cap the caller resolved from the `SharedPerceptionCap` (the caller holds
    // `ServiceState`; the router just threads the scalar through to the
    // governor's Nominal arm). `None` when the monitor is disabled.
    pub fn route_command(
        &self,
        asset_id: &str,
        cmd: &ProposedVehicleCommand,
        effective_perception_cap: Option<f64>,
    ) -> Result<EnforceAction, FabricError> {
        let governor = self.governors.get(asset_id)
            .ok_or_else(|| FabricError::AssetNotFound(asset_id.to_string()))?;

        let posture = self.asset_postures.get(asset_id)
            .map(|p| p.posture.clone())
            .unwrap_or(FleetPosture::LockedOut);  // fail-closed if posture unknown

        Ok(governor.evaluate_command(cmd, &posture, effective_perception_cap))
    }

    /// Low-level posture write. Used by:
    ///   - the propagation apply-loop (write-back of derived dependent
    ///     postures), which MUST NOT recurse, and
    ///   - any caller that legitimately needs a posture update with no
    ///     side effects.
    /// For external/manual posture changes that should trigger dependent
    /// propagation, use `update_asset_posture_and_propagate`.
    pub fn update_asset_posture(&self, asset_id: &str, posture: AssetPosture) {
        self.asset_postures.insert(asset_id.to_string(), posture);
        self.fabric_generation.fetch_add(1, Ordering::SeqCst);
    }

    /// External-entry posture update with one bounded propagation pass.
    ///
    /// Bounded + non-recursive: `propagate_cross_asset_trust` rules fire
    /// only when the SOURCE asset is `LockedOut` and the changes they
    /// produce only ever set dependents to `Degraded`. An applied change
    /// can therefore never become a new propagation source, so one pass
    /// suffices and cannot recurse.
    ///
    /// The apply-loop deliberately writes via the BARE `update_asset_posture`
    /// — NOT this method — so a propagation pass can never re-trigger
    /// itself. (a)/(b) already guarantee termination via idempotence; this
    /// guarantees each external update yields exactly one bounded pass.
    pub fn update_asset_posture_and_propagate(&self, asset_id: &str, posture: AssetPosture) {
        self.update_asset_posture(asset_id, posture);

        let changes = self.propagate_cross_asset_trust();
        for (dependent_id, forced) in changes {
            if let Some(existing) = self.asset_postures.get(&dependent_id).map(|r| r.clone()) {
                if existing.posture != forced {
                    let next_gen = existing.generation.saturating_add(1);
                    let now = now_ms();
                    let updated = AssetPosture {
                        asset_id: dependent_id.clone(),
                        posture: forced,
                        generation: next_gen,
                        computed_at_ms: now,
                        contributing_nodes: existing.contributing_nodes.clone(),
                        blocked_by: vec![
                            "CROSS_ASSET_PROPAGATION_FROM_LOCKED_DEPENDENCY".to_string(),
                        ],
                    };
                    // Bare write — propagation must not re-enter.
                    self.update_asset_posture(&dependent_id, updated);
                }
            }
        }
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
    ///
    /// Thin map over [`FabricRouter::evaluate_cross_asset_trust`] — the returned
    /// `(follower, posture)` pairs (and their order) are byte-identical to the
    /// pre-SG-007-recording behavior. The richer trigger-tagged result is used
    /// only by [`FabricRouter::propagate_and_record`] for causal-log recording.
    ///
    // Verifies: SG-007 — a LockedOut leader degrades dependent followers within
    // one synchronous fabric pass (tests/fault_injection.rs
    // test_safety_goal_sg_007_cross_asset_lockout_propagation).
    pub fn propagate_cross_asset_trust(&self) -> Vec<(String, FleetPosture)> {
        self.evaluate_cross_asset_trust()
            .into_iter()
            .map(|p| (p.follower, p.new_posture))
            .collect()
    }

    /// Cross-asset trust propagation + causal-log recording (SG-007).
    ///
    /// Runs the SAME evaluation as [`FabricRouter::propagate_cross_asset_trust`]
    /// (the propagation DECISIONS are unchanged) and additionally records one
    /// causal event per rule-firing (per LockedOut trigger asset) into `log`,
    /// tagging the followers it degraded as `affects_assets`. Returns the same
    /// flat `(follower, posture)` Vec the public fn returns.
    ///
    // Verifies: SG-007 (causal-log sub-gap) — propagation events are recorded to
    // the FabricCausalLog, not just applied.
    pub fn propagate_and_record(
        &self,
        log: &FabricCausalLog,
        fabric_generation: u64,
    ) -> Vec<(String, FleetPosture)> {
        let propagations = self.evaluate_cross_asset_trust();

        // Group followers by their triggering LockedOut asset, preserving
        // first-seen trigger order (one causal event per rule-firing/trigger).
        let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
        for p in &propagations {
            if let Some(entry) = grouped.iter_mut().find(|(t, _)| *t == p.trigger_asset) {
                entry.1.push(p.follower.clone());
            } else {
                grouped.push((p.trigger_asset.clone(), vec![p.follower.clone()]));
            }
        }
        for (trigger, followers) in &grouped {
            let payload = format!(
                "LockedOut asset '{trigger}' degraded {} dependent follower(s) to Degraded \
                 (SG-007 cross-asset trust propagation)",
                followers.len()
            );
            // caused_by empty (the lockout is the root cause); affects_assets =
            // the followers this trigger degraded. Mirrors the record(...) usage
            // in kirra_verifier_service.rs.
            log.record(
                trigger,
                "cross_asset_trust_degrade",
                &payload,
                vec![],
                followers.clone(),
                fabric_generation,
            );
        }

        propagations
            .into_iter()
            .map(|p| (p.follower, p.new_posture))
            .collect()
    }

    /// Internal: evaluate the 4 cross-asset trust rules into trigger-tagged
    /// propagations (one entry per degraded follower, tagged with the LockedOut
    /// asset that triggered it). Single source of truth for both
    /// `propagate_cross_asset_trust` (decision only) and `propagate_and_record`
    /// (decision + causal-log recording). The rule conditions and the set/order
    /// of degraded followers are identical to the original inline logic; the
    /// only addition is capturing the trigger asset id per firing.
    fn evaluate_cross_asset_trust(&self) -> Vec<TrustPropagation> {
        let mut out: Vec<TrustPropagation> = Vec::new();

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

        // Rule 1: Drone depends on ground control station (IndustrialController).
        // Trigger = a LockedOut ground station (deterministic representative: the
        // lexicographically-smallest locked id). `Some` iff the original `any`.
        let locked_ground = all_assets.iter()
            .filter(|(_, at, ap, _)| *at == AssetType::IndustrialController && ap.posture == FleetPosture::LockedOut)
            .map(|(id, _, _, _)| id.clone())
            .min();
        if let Some(trigger) = locked_ground {
            for (id, at, ap, _) in &all_assets {
                if *at == AssetType::Drone && ap.posture == FleetPosture::Nominal {
                    out.push(TrustPropagation::degrade(&trigger, id));
                }
            }
        }

        // Rule 2: Convoy follower degrades when leader is LockedOut.
        let locked_leader = all_assets.iter()
            .filter(|(_, _, ap, meta)|
                meta.get("convoy_role").map(|r| r == "leader").unwrap_or(false)
                    && ap.posture == FleetPosture::LockedOut)
            .map(|(id, _, _, _)| id.clone())
            .min();
        if let Some(trigger) = locked_leader {
            for (id, _, ap, meta) in &all_assets {
                if meta.get("convoy_role").map(|r| r == "follower").unwrap_or(false)
                    && ap.posture == FleetPosture::Nominal
                {
                    out.push(TrustPropagation::degrade(&trigger, id));
                }
            }
        }

        // Rule 3: Infrastructure lockout degrades dependents.
        let locked_infra = all_assets.iter()
            .filter(|(_, at, ap, _)| *at == AssetType::Infrastructure && ap.posture == FleetPosture::LockedOut)
            .map(|(id, _, _, _)| id.clone())
            .min();
        if let Some(trigger) = locked_infra {
            for (id, _, ap, meta) in &all_assets {
                if meta.get("depends_on_infrastructure").map(|v| v == "true").unwrap_or(false)
                    && ap.posture == FleetPosture::Nominal
                {
                    out.push(TrustPropagation::degrade(&trigger, id));
                }
            }
        }

        // Rule 4: Warehouse lockout degrades registered robots in that warehouse.
        // Trigger is per-follower (the robot's own locked warehouse).
        let locked_warehouses: Vec<String> = all_assets.iter()
            .filter(|(_, at, ap, _)| *at == AssetType::Warehouse && ap.posture == FleetPosture::LockedOut)
            .map(|(id, _, _, _)| id.clone())
            .collect();
        if !locked_warehouses.is_empty() {
            for (id, at, ap, meta) in &all_assets {
                if *at == AssetType::Robot && ap.posture == FleetPosture::Nominal {
                    let robot_warehouse = meta.get("warehouse_id").map(|s| s.as_str()).unwrap_or("");
                    if let Some(w) = locked_warehouses.iter().find(|w| w.as_str() == robot_warehouse) {
                        out.push(TrustPropagation::degrade(w, id));
                    }
                }
            }
        }

        out
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
        let result = router.route_command("r01", &safe_cmd(), None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_unknown_asset_returns_error() {
        let router = FabricRouter::new();
        let result = router.route_command("nonexistent", &safe_cmd(), None);
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

    fn nominal_posture(id: &str) -> AssetPosture {
        AssetPosture {
            asset_id: id.to_string(),
            posture: FleetPosture::Nominal,
            generation: 1,
            computed_at_ms: 500,
            contributing_nodes: vec![],
            blocked_by: vec![],
        }
    }

    #[test]
    fn test_cross_asset_propagation_drone_depends_on_ground_station() {
        let router = FabricRouter::new();
        router.register_asset(&make_asset("gcs01", AssetType::IndustrialController, KinematicProfileType::IndustrialNominal));
        router.register_asset(&make_asset("drone01", AssetType::Drone, KinematicProfileType::DroneNominal));
        // Registration seeds Degraded; the propagation rule fires only on
        // a Nominal dependent (a Degraded one needs no further transition).
        // Push the drone to Nominal so the rule has something to transition.
        router.update_asset_posture("drone01", nominal_posture("drone01"));

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
        // Push follower to Nominal so the rule has something to transition.
        router.update_asset_posture("follower01", nominal_posture("follower01"));

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
        // Push both robots to Nominal so the propagation rule transitions
        // them down — they are seeded Degraded by registration.
        router.update_asset_posture("robot01", nominal_posture("robot01"));
        router.update_asset_posture("robot02", nominal_posture("robot02"));

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

    // FIX 1 — registration seed.
    // A freshly-registered asset MUST start at the Degraded (MRC envelope)
    // posture and cannot be commanded at the full nominal envelope before
    // a real posture lands. The Degraded seed grants the per-profile MRC
    // contract via `AssetGovernor::evaluate_command`, not a flat cap.
    #[test]
    fn test_newly_registered_asset_seeded_degraded_with_mrc_envelope() {
        let router = FabricRouter::new();
        router.register_asset(&make_asset("r01", AssetType::Robot, KinematicProfileType::RobotNominal));

        let posture = router.asset_postures.get("r01").map(|p| p.clone())
            .expect("registration must seed a posture");
        assert_eq!(posture.posture, FleetPosture::Degraded,
            "registration must seed Degraded (MRC envelope) — not Nominal, not LockedOut");
        assert_eq!(posture.generation, 0,
            "fresh registration uses generation: 0 sentinel for never-yet-computed");
        assert!(posture.blocked_by.iter().any(|s| s == "UNVERIFIED_PENDING_FIRST_POSTURE"),
            "blocked_by must surface the unverified state, got {:?}", posture.blocked_by);

        // Issue #70: a freshly-registered asset is seeded Degraded =
        // decel-to-stop-and-HOLD. Holding at a standstill is allowed; the
        // governor will NOT autonomously re-initiate motion from rest.
        let hold = ProposedVehicleCommand {
            linear_velocity_mps: 0.0,
            current_velocity_mps: 0.0,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        let result = router.route_command("r01", &hold, None)
            .expect("route_command should not error");
        assert!(matches!(result, EnforceAction::Allow),
            "holding at a standstill must be allowed on a freshly registered (Degraded) asset, got {result:?}");

        // A re-initiation command from rest (the `safe_cmd` 0.1 m/s crawl that
        // the old MRC-crawl behavior admitted) must now be DENIED — no
        // autonomous re-initiation of motion under Degraded.
        let result = router.route_command("r01", &safe_cmd(), None)
            .expect("route_command should not error");
        assert!(matches!(result, EnforceAction::DenyBreach(_)),
            "re-initiation from a stop must be denied on a Degraded asset, got {result:?}");

        // A decelerating command (moving → slower, within the MRC envelope)
        // IS admitted — the asset may bleed speed to a controlled stop.
        let decel = ProposedVehicleCommand {
            linear_velocity_mps: 0.2,
            current_velocity_mps: 0.4,
            delta_time_s: 0.1,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        let result = router.route_command("r01", &decel, None).expect("route_command should not error");
        assert!(!matches!(result, EnforceAction::DenyBreach(_)),
            "a decelerating within-MRC command must be admitted on a Degraded asset, got {result:?}");
    }

    // FIX 2 — auto-propagation.
    // A single call to update_asset_posture_and_propagate on a LockedOut
    // source produces the dependent's Degraded transition WITHOUT a
    // separate propagate_cross_asset_trust call.
    #[test]
    fn test_lockout_auto_propagates_to_dependents_on_update() {
        let router = FabricRouter::new();
        router.register_asset(&make_asset("gcs01", AssetType::IndustrialController, KinematicProfileType::IndustrialNominal));
        router.register_asset(&make_asset("drone01", AssetType::Drone, KinematicProfileType::DroneNominal));
        // Elevate drone to Nominal so the propagation rule has work to do.
        router.update_asset_posture("drone01", nominal_posture("drone01"));

        router.update_asset_posture_and_propagate("gcs01", AssetPosture {
            asset_id: "gcs01".to_string(),
            posture: FleetPosture::LockedOut,
            generation: 5,
            computed_at_ms: 2000,
            contributing_nodes: vec![],
            blocked_by: vec!["sensor_x".to_string()],
        });

        // Drone is now Degraded — no manual propagate call was needed.
        let drone = router.asset_postures.get("drone01").map(|p| p.clone()).expect("drone present");
        assert_eq!(drone.posture, FleetPosture::Degraded,
            "drone must auto-degrade on a single update_asset_posture_and_propagate call");
        assert!(drone.blocked_by.iter().any(|s| s.contains("CROSS_ASSET_PROPAGATION")),
            "blocked_by must indicate propagation source, got {:?}", drone.blocked_by);
    }

    // FIX 2 — termination/idempotence.
    // A second propagating call (with no external state change) must NOT
    // produce further cascading changes. Properties (a) and (b) guarantee
    // this: rules fire only on LockedOut sources, changes are always
    // Degraded, so an applied change cannot become a new source.
    #[test]
    fn test_propagation_pass_terminates_and_is_idempotent() {
        let router = FabricRouter::new();
        router.register_asset(&make_asset("gcs01", AssetType::IndustrialController, KinematicProfileType::IndustrialNominal));
        router.register_asset(&make_asset("drone01", AssetType::Drone, KinematicProfileType::DroneNominal));
        router.update_asset_posture("drone01", nominal_posture("drone01"));

        let locking = AssetPosture {
            asset_id: "gcs01".to_string(),
            posture: FleetPosture::LockedOut,
            generation: 5,
            computed_at_ms: 2000,
            contributing_nodes: vec![],
            blocked_by: vec!["sensor_x".to_string()],
        };
        router.update_asset_posture_and_propagate("gcs01", locking.clone());
        let drone_after_first = router.asset_postures.get("drone01").map(|p| p.clone()).unwrap();
        assert_eq!(drone_after_first.posture, FleetPosture::Degraded);
        let drone_gen_after_first = drone_after_first.generation;

        // Second call with the same posture: drone is already Degraded,
        // so the propagation pass produces no NEW transition for it. The
        // drone's generation must not advance.
        router.update_asset_posture_and_propagate("gcs01", locking);
        let drone_after_second = router.asset_postures.get("drone01").map(|p| p.clone()).unwrap();
        assert_eq!(drone_after_second.posture, FleetPosture::Degraded);
        assert_eq!(drone_after_second.generation, drone_gen_after_first,
            "second propagation pass must NOT re-degrade an already-Degraded dependent \
             (generation must not advance)");
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
                    }, None);
                }
            })
        }).collect();
        for h in handles { h.join().unwrap(); }
    }
}
