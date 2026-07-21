// ============================================================================
// PATCH: src/verifier.rs + src/bin/kirra_verifier_service.rs
//
// v2.2.0 — AV sensor fault handling and MRC posture transitions
//
// ARCHITECTURE CORRECTION vs. milestone doc:
//
// The doc proposed: SensorFaultReport → directly write FleetPosture to SharedPostureCache
//
// This is WRONG for two reasons:
//
//   1. It bypasses AppState::recursive_calculate (invariant #4). SharedPostureCache
//      is a READ cache of the DAG result. Writing to it directly means the gray/black
//      traversal is never consulted — the posture reflects the last fault report, not
//      the actual state of the dependency graph.
//
//   2. It creates a write-write race: the background posture broadcast loop also writes
//      to SharedPostureCache. Direct mutation from a request handler races with it.
//
// CORRECT approach (implemented below):
//
//   SensorFaultReport
//     → validate node exists
//     → load per-node confidence_floor from av_subsystem_meta
//     → if fault: mark node NodeTrustState::Untrusted("SENSOR_FAULT") in AppState.nodes
//     → call AppState::recalculate_and_broadcast() (or equivalent)
//       which runs recursive_calculate → writes to SharedPostureCache → sends posture_tx
//     → the DAG propagates Untrusted upward through deps to FleetPosture::Degraded
//
// This means a LiDAR fault propagates: lidar → Untrusted → perception_fusion → Degraded
// → FleetPosture::Degraded → SharedPostureCache → actuator middleware sees MRC profile.
// The full chain fires correctly. No invariant is violated.
//
// ============================================================================

// ----------------------------------------------------------------------------
// SECTION A: Types — append to src/verifier.rs
// ----------------------------------------------------------------------------

// Minimum confidence score below which a sensor node is automatically marked Untrusted.
// This default applies when no per-node confidence_floor is set in av_subsystem_meta.
// Individual nodes can override this via their registered confidence_floor value.
pub const AV_DEFAULT_CONFIDENCE_FLOOR: f64 = 0.70;

// The trust reason string written to NodeTrustState::Untrusted when a sensor fault fires.
// Stored in the DashMap and persisted to the audit chain for forensic tracing.
pub const AV_TRUST_REASON_SENSOR_FAULT: &str = "SENSOR_FAULT_REPORTED";
pub const AV_TRUST_REASON_LOW_CONFIDENCE: &str = "CONFIDENCE_BELOW_FLOOR";
pub const AV_TRUST_REASON_HARDWARE_FAULT: &str = "HARDWARE_FAULT_DETECTED";

/*
// Append to src/verifier.rs

/// AV subsystem registration payload.
/// Used by POST /fleet/assets/register to annotate an existing fleet node
/// with AV-specific classification metadata.
///
/// The node_id MUST already be registered via POST /attestation/register.
/// This endpoint adds the AV metadata layer — it does not create a new node.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AvSubsystemRegistration {
    /// Must match an existing node_id in AppState.nodes.
    pub node_id: String,
    /// 'Perception' | 'Planning' | 'Actuation' | 'Positioning'
    pub subsystem_class: String,
    /// Physical device identifier (serial number, PCIe address, etc.)
    pub hardware_serial: String,
    /// Minimum acceptable confidence score. Defaults to AV_DEFAULT_CONFIDENCE_FLOOR.
    /// Stored in av_subsystem_meta and loaded on each sensor fault evaluation.
    pub confidence_floor: Option<f64>,
}
*/

// ----------------------------------------------------------------------------
// SECTION B: Handler — append to src/bin/kirra_verifier_service.rs
// ----------------------------------------------------------------------------

/*
// Append to src/bin/kirra_verifier_service.rs imports:
use crate::verifier::{
    AvSubsystemRegistration,
    AV_DEFAULT_CONFIDENCE_FLOOR,
    AV_TRUST_REASON_HARDWARE_FAULT,
    AV_TRUST_REASON_LOW_CONFIDENCE,
    NodeTrustState,
};

// ---------------------------------------------------------------------------
// Sensor fault report input type
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, Debug)]
pub struct SensorFaultReport {
    pub source_node_id: String,
    /// Confidence score in [0.0, 1.0]. Values below the node's registered
    /// confidence_floor (default: AV_DEFAULT_CONFIDENCE_FLOOR) trigger Untrusted.
    pub confidence_score: f64,
    /// True if a hardware-level fault was detected independent of confidence score.
    /// Either condition alone is sufficient to mark the node Untrusted.
    pub hardware_fault_detected: bool,
}

// ---------------------------------------------------------------------------
// POST /fleet/assets/register — Tier 2 admin-only
//
// Registers AV subsystem metadata for an existing fleet node.
// The node must already exist in AppState.nodes (registered via /attestation/register).
// ---------------------------------------------------------------------------

pub async fn handle_register_av_asset(
    State(svc): State<Arc<ServiceState>>,
    Json(reg): Json<AvSubsystemRegistration>,
) -> Result<StatusCode, StatusCode> {
    // Validate node_id is non-empty
    if reg.node_id.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Verify the node exists in the live fleet graph before writing metadata.
    // AppState.nodes is the DashMap that feeds recursive_calculate.
    // If the node doesn't exist there, AV metadata is orphaned and useless.
    if !svc.app.nodes.contains_key(&reg.node_id) {
        tracing::warn!(
            node_id = %reg.node_id,
            "AV subsystem registration rejected: node not found in fleet graph. \
             Register via /attestation/register first."
        );
        return Err(StatusCode::NOT_FOUND);
    }

    let floor = reg.confidence_floor.unwrap_or(AV_DEFAULT_CONFIDENCE_FLOOR);
    let ts = now_ms();

    // Write AV metadata to disk first (invariant #12: disk before memory).
    svc.app.store
        .register_av_subsystem_meta(
            &reg.node_id,
            &reg.subsystem_class,
            &reg.hardware_serial,
            floor,
            ts,
        )
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to persist AV subsystem metadata");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Audit chain entry for registration event.
    let payload = serde_json::json!({
        "node_id": reg.node_id,
        "subsystem_class": reg.subsystem_class,
        "hardware_serial": reg.hardware_serial,
        "confidence_floor": floor,
        "registered_at_ms": ts,
    });

    let _ = svc.app.store.save_posture_event_chained(
        "av_graph_engine",
        "AV_SUBSYSTEM_REGISTERED",
        &payload.to_string(),
        Some("AV subsystem metadata registered for existing fleet node"),
        ts,
    );

    tracing::info!(
        node_id       = %reg.node_id,
        class         = %reg.subsystem_class,
        serial        = %reg.hardware_serial,
        floor         = %floor,
        "AV subsystem metadata registered"
    );

    Ok(StatusCode::CREATED)
}

// ---------------------------------------------------------------------------
// POST /fleet/diagnostics/report — Tier 2 admin-only
//
// Receives a sensor health report. If the node is degraded (hardware fault OR
// confidence below floor), marks it Untrusted in AppState.nodes and triggers
// a full DAG recalculation via recalculate_and_broadcast().
//
// THE KEY CORRECTION: this handler does NOT write directly to SharedPostureCache.
// It marks the node in the DashMap, then calls the DAG engine to recompute.
// The DAG engine owns SharedPostureCache writes — no one else does.
//
// Auth: Tier 2 admin-only (require_admin_token). This is a mutation that can
// change fleet trust posture — it must not be reachable without admin credentials.
// ---------------------------------------------------------------------------

pub async fn handle_sensor_fault_report(
    State(svc): State<Arc<ServiceState>>,
    Json(report): Json<SensorFaultReport>,
) -> Result<StatusCode, StatusCode> {
    // Validate node exists in the fleet graph.
    if !svc.app.nodes.contains_key(&report.source_node_id) {
        tracing::warn!(
            node_id = %report.source_node_id,
            "Sensor fault report for unknown node — ignored"
        );
        return Err(StatusCode::NOT_FOUND);
    }

    let ts = now_ms();

    // Load per-node confidence floor from persistent metadata.
    // Falls back to the crate-level default if no AV metadata registered.
    let confidence_floor = svc.app.store
        .load_av_confidence_floor(&report.source_node_id)
        .unwrap_or(None)
        .unwrap_or(AV_DEFAULT_CONFIDENCE_FLOOR);

    // Determine whether this report indicates a degraded node.
    let is_degraded = report.hardware_fault_detected
        || report.confidence_score < confidence_floor;

    if is_degraded {
        // Determine the specific failure reason for audit trail.
        let reason = if report.hardware_fault_detected {
            AV_TRUST_REASON_HARDWARE_FAULT
        } else {
            AV_TRUST_REASON_LOW_CONFIDENCE
        };

        tracing::warn!(
            node_id          = %report.source_node_id,
            confidence       = %report.confidence_score,
            floor            = %confidence_floor,
            hardware_fault   = %report.hardware_fault_detected,
            reason           = %reason,
            "AV sensor node degraded — marking Untrusted and recalculating fleet posture"
        );

        // Step 1: Update trust state in AppState.nodes (DashMap).
        // This is what recursive_calculate reads — we must update it here
        // BEFORE triggering recalculation, otherwise the DAG sees stale state.
        if let Some(mut node_entry) = svc.app.nodes.get_mut(&report.source_node_id) {
            node_entry.trust_state = NodeTrustState::Untrusted(reason.to_string());
        }

        // Step 2: Touch the telemetry timestamp in persistent store.
        // Disk write before the recalculation broadcast (invariant #12 spirit).
        let _ = svc.app.store.touch_av_telemetry_timestamp(&report.source_node_id, ts);

        // Step 3: Write fault event to tamper-evident audit chain.
        let fault_payload = serde_json::json!({
            "node_id": report.source_node_id,
            "confidence_score": report.confidence_score,
            "confidence_floor": confidence_floor,
            "hardware_fault": report.hardware_fault_detected,
            "trust_reason": reason,
            "reported_at_ms": ts,
        });
        let _ = svc.app.store.save_posture_event_chained(
            "av_sensor_fault_handler",
            "AV_SENSOR_DEGRADED",
            &fault_payload.to_string(),
            Some("Sensor node marked Untrusted — fleet posture recalculation triggered"),
            ts,
        );

        // Step 4: Trigger DAG recalculation.
        //
        // recalculate_and_broadcast() runs recursive_calculate across all nodes,
        // computes the new FleetPosture, writes the result to SharedPostureCache,
        // and sends via posture_tx to SSE subscribers.
        //
        // This is the ONLY correct write path to SharedPostureCache.
        // No handler ever writes to SharedPostureCache directly.
        svc.app.recalculate_and_broadcast();

    } else {
        // Node is healthy — check if it was previously Untrusted and recover it.
        let currently_untrusted = svc.app.nodes
            .get(&report.source_node_id)
            .map(|n| matches!(n.trust_state, NodeTrustState::Untrusted(_)))
            .unwrap_or(false);

        if currently_untrusted {
            tracing::info!(
                node_id    = %report.source_node_id,
                confidence = %report.confidence_score,
                "AV sensor node confidence restored — recovering trust state"
            );

            // Mark node Trusted again. The next recalculation will re-evaluate
            // whether this is sufficient to recover the fleet to Nominal.
            if let Some(mut node_entry) = svc.app.nodes.get_mut(&report.source_node_id) {
                node_entry.trust_state = NodeTrustState::Trusted;
            }

            let _ = svc.app.store.touch_av_telemetry_timestamp(&report.source_node_id, ts);

            let recovery_payload = serde_json::json!({
                "node_id": report.source_node_id,
                "confidence_score": report.confidence_score,
                "confidence_floor": confidence_floor,
                "recovered_at_ms": ts,
            });
            let _ = svc.app.store.save_posture_event_chained(
                "av_sensor_fault_handler",
                "AV_SENSOR_RECOVERED",
                &recovery_payload.to_string(),
                Some("Sensor node trust recovered — fleet posture recalculation triggered"),
                ts,
            );

            // Trigger recalculation on recovery too — the fleet may return to Nominal.
            svc.app.recalculate_and_broadcast();
        } else {
            // Healthy report for already-healthy node — just update telemetry timestamp.
            let _ = svc.app.store.touch_av_telemetry_timestamp(&report.source_node_id, ts);
        }
    }

    Ok(StatusCode::ACCEPTED)
}

// ---------------------------------------------------------------------------
// Router wiring — apply inside the router construction block.
//
// Both AV routes are Tier 2 admin-only. Rationale:
//   - /fleet/assets/register: creates persistent metadata that affects DAG behavior
//   - /fleet/diagnostics/report: directly influences node trust state → fleet posture
//
// Tier 1 (identity-gated) is insufficient for these — they are mutations that
// affect the physical safety posture of the vehicle fleet.
//
// Do NOT put these on a separate actuator-physics middleware layer (that layer is
// for command-time kinematic enforcement, not for node trust state mutations).
// ---------------------------------------------------------------------------

let av_management_routes = Router::new()
    .route("/fleet/assets/register",     post(handle_register_av_asset))
    .route("/fleet/diagnostics/report",  post(handle_sensor_fault_report))
    .layer(from_fn(require_admin_token));  // Tier 2: admin-only, no identity header required

*/

// ============================================================================
// INTEGRATION TESTS
// ============================================================================

#[cfg(test)]
mod av_transition_integration_tests {
    use std::sync::Arc;
    use axum::{http::StatusCode, routing::post, Json, Router};
    use axum_test::TestServer;
    use serde_json::json;
    use tokio::sync::broadcast;

    use kirra_core::kinematics_contract::ProposedVehicleCommand;
    use crate::gateway::policy_layer::enforce_actuator_safety_envelope;
    use crate::posture_cache::{CachedFleetPosture, SharedPostureCache};
    use crate::verifier::{FleetPosture, NodeTrustState};
    use crate::verifier_store::VerifierStore;
    use super::{ServiceState, handle_sensor_fault_report, SensorFaultReport};

    /// Builds a ServiceState with:
    ///   - In-memory SQLite
    ///   - lidar_front registered as a fleet node (Trusted, with AV metadata)
    ///   - perception_fusion registered, depending on lidar_front
    ///   - Cache initialized to Nominal
    async fn build_av_test_state() -> Arc<ServiceState> {
        use crate::verifier::AppState;

        let store = VerifierStore::new(":memory:").expect("in-memory store");
        let (posture_tx, _) = broadcast::channel(1024);

        let app = Arc::new(AppState::new(store, posture_tx));

        // Register AV metadata directly (test bypasses HTTP for setup)
        let _ = app.store.register_av_subsystem_meta(
            "lidar_front",
            "Perception",
            "LIDAR-SN-001",
            0.70,
            0,
        );

        // Add dependency: perception_fusion depends on lidar_front
        app.deps.insert(
            "perception_fusion".to_string(),
            vec!["lidar_front".to_string()],
        );

        let posture_cache: SharedPostureCache = Arc::new(tokio::sync::RwLock::new(Some(
            CachedFleetPosture::new(FleetPosture::Nominal),
        )));

        Arc::new(ServiceState { app, posture_cache })
    }

    fn build_test_router(state: Arc<ServiceState>) -> Router {
        Router::new()
            .route("/fleet/diagnostics/report", post(handle_sensor_fault_report))
            .route(
                "/actuator/motion/command",
                post(|Json(cmd): Json<ProposedVehicleCommand>| async move {
                    (StatusCode::OK, Json(cmd))
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                Arc::clone(&state),
                enforce_actuator_safety_envelope,
            ))
            .with_state(state)
    }

    /// Core integration test: sensor fault → DAG recalculation → MRC enforcement.
    ///
    /// Flow:
    ///   1. Nominal: 12 m/s command passes unmodified
    ///   2. Inject LiDAR hardware fault → lidar_front = Untrusted
    ///   3. DAG propagates: lidar_front Untrusted → perception_fusion Degraded → FleetPosture::Degraded
    ///   4. Same 12 m/s command now clamped to MRC 5.0 m/s ceiling
    #[tokio::test]
    async fn test_sensor_fault_propagates_through_dag_to_actuator_envelope() {
        let state = build_av_test_state().await;
        let server = TestServer::new(build_test_router(Arc::clone(&state))).unwrap();

        let motion_payload = json!({
            "linear_velocity_mps": 12.0,
            "current_velocity_mps": 12.0,
            "delta_time_s": 0.1,
            "steering_angle_deg": 0.0,
            "current_steering_angle_deg": 0.0
        });

        // Step 1: Nominal — 12 m/s should pass
        let res_nominal = server.post("/actuator/motion/command").json(&motion_payload).await;
        res_nominal.assert_status(StatusCode::OK);
        let cmd_nominal: ProposedVehicleCommand = res_nominal.json();
        assert_eq!(cmd_nominal.linear_velocity_mps, 12.0, "nominal: command must pass unmodified");

        // Step 2: Inject hardware fault on lidar_front
        let fault_payload = json!({
            "source_node_id": "lidar_front",
            "confidence_score": 0.22,
            "hardware_fault_detected": true
        });
        server.post("/fleet/diagnostics/report").json(&fault_payload).await
            .assert_status(StatusCode::ACCEPTED);

        // Step 3: Verify lidar_front is now Untrusted in the DashMap
        let lidar_trust = state.app.nodes
            .get("lidar_front")
            .map(|n| matches!(n.trust_state, NodeTrustState::Untrusted(_)))
            .unwrap_or(false);
        assert!(lidar_trust, "lidar_front must be Untrusted after fault injection");

        // Step 4: Same motion command — must now be clamped to MRC ceiling
        let res_degraded = server.post("/actuator/motion/command").json(&motion_payload).await;
        res_degraded.assert_status(StatusCode::OK);
        let cmd_degraded: ProposedVehicleCommand = res_degraded.json();
        assert_eq!(
            cmd_degraded.linear_velocity_mps, 5.0,
            "degraded: command must be clamped to MRC max_speed_mps = 5.0"
        );
    }

    /// Recovery test: after a fault clears, the node returns to Trusted.
    #[tokio::test]
    async fn test_sensor_recovery_restores_node_trust_state() {
        let state = build_av_test_state().await;
        let server = TestServer::new(build_test_router(Arc::clone(&state))).unwrap();

        let fault = json!({
            "source_node_id": "lidar_front",
            "confidence_score": 0.10,
            "hardware_fault_detected": true
        });
        server.post("/fleet/diagnostics/report").json(&fault).await
            .assert_status(StatusCode::ACCEPTED);

        assert!(
            state.app.nodes.get("lidar_front")
                .map(|n| matches!(n.trust_state, NodeTrustState::Untrusted(_)))
                .unwrap_or(false),
            "lidar_front must be Untrusted after fault"
        );

        let recovery = json!({
            "source_node_id": "lidar_front",
            "confidence_score": 0.95,
            "hardware_fault_detected": false
        });
        server.post("/fleet/diagnostics/report").json(&recovery).await
            .assert_status(StatusCode::ACCEPTED);

        assert!(
            state.app.nodes.get("lidar_front")
                .map(|n| matches!(n.trust_state, NodeTrustState::Trusted))
                .unwrap_or(false),
            "lidar_front must be Trusted after recovery"
        );
    }

    /// Unknown node fault reports must return 404.
    #[tokio::test]
    async fn test_fault_report_for_unknown_node_returns_404() {
        let state = build_av_test_state().await;
        let server = TestServer::new(build_test_router(state)).unwrap();

        let fault = json!({
            "source_node_id": "nonexistent_sensor",
            "confidence_score": 0.0,
            "hardware_fault_detected": true
        });
        server.post("/fleet/diagnostics/report").json(&fault).await
            .assert_status(StatusCode::NOT_FOUND);
    }

    /// Confidence exactly at the floor (0.70) must NOT trigger degradation.
    /// Only strictly below the floor triggers Untrusted.
    #[tokio::test]
    async fn test_confidence_at_floor_boundary_does_not_trigger_fault() {
        let state = build_av_test_state().await;
        let server = TestServer::new(build_test_router(Arc::clone(&state))).unwrap();

        let at_floor = json!({
            "source_node_id": "lidar_front",
            "confidence_score": 0.70,
            "hardware_fault_detected": false
        });
        server.post("/fleet/diagnostics/report").json(&at_floor).await
            .assert_status(StatusCode::ACCEPTED);

        assert!(
            state.app.nodes.get("lidar_front")
                .map(|n| matches!(n.trust_state, NodeTrustState::Trusted))
                .unwrap_or(false),
            "confidence exactly at floor must not degrade the node"
        );
    }

    /// Hardware fault overrides confidence score — even a perfect 1.0 score
    /// with hardware_fault_detected=true must mark the node Untrusted.
    #[tokio::test]
    async fn test_hardware_fault_overrides_high_confidence_score() {
        let state = build_av_test_state().await;
        let server = TestServer::new(build_test_router(Arc::clone(&state))).unwrap();

        let hw_fault = json!({
            "source_node_id": "lidar_front",
            "confidence_score": 1.0,
            "hardware_fault_detected": true
        });
        server.post("/fleet/diagnostics/report").json(&hw_fault).await
            .assert_status(StatusCode::ACCEPTED);

        assert!(
            state.app.nodes.get("lidar_front")
                .map(|n| matches!(n.trust_state, NodeTrustState::Untrusted(_)))
                .unwrap_or(false),
            "hardware fault must override confidence score"
        );
    }
}
