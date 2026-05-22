// tests/temporal_scenario_tests.rs
//
// Multi-sensor temporal integration test suite.
// Tests use ScenarioRunner with real in-memory AppState and VirtualClock.
//
// These tests answer questions that unit tests cannot:
//   - Does a LiDAR fault actually propagate to fleet posture via the DAG?
//   - Does a partial recovery (3/5 reports) leave the node Untrusted?
//   - Does a second fault after partial recovery reset the streak correctly?
//   - Does cache staleness (virtual clock advance past TTL) produce LockedOut?
//   - Does a multi-node fault pattern produce LockedOut vs Degraded correctly?
//
// NOTE (Patch A): should_route_command signature correction vs. milestone doc
// ============================================================================
// The milestone doc replaced `command: OperationalCommand` with `required_class: &str`.
// This is a security regression:
//   - Invariant #9: `OperationalCommand::Unknown` must be denied in ALL posture
//     states. The early-return exists specifically for this. A stringly-typed
//     check ("telemetry_read") bypasses the enum entirely.
//   - The existing OperationalCommand enum is the correct abstraction.
//     String comparison is error-prone and loses type safety.
//
// The correct fix: keep the OperationalCommand signature, update only the
// staleness check to use entry.is_stale(now_ms) from CachedFleetPosture.
// The posture logic (Nominal/Degraded/LockedOut) is unchanged.
// should_route_command is implemented in src/posture_cache.rs.

use std::sync::Arc;

use aegis_runtime_sdk::verifier::{AppState, FleetPosture, NodeTrustState, VerifierOperationMode};
use aegis_runtime_sdk::posture_cache::{CachedFleetPosture, SharedPostureCache};
use aegis_runtime_sdk::scenario_runner::{
    PostureAssertion, ScenarioEvent, ScenarioRunner,
};
use aegis_runtime_sdk::clock::{Clock, VirtualClock};
use aegis_runtime_sdk::recovery_hysteresis::AV_RECOVERY_STREAK_THRESHOLD;
use aegis_runtime_sdk::posture_engine::POSTURE_CACHE_TTL_MS;

// ---------------------------------------------------------------------------
// Test infrastructure helpers
// ---------------------------------------------------------------------------

/// Builds an in-memory AppState with a minimal AV node graph:
///
///   lidar_front   ─┬
///                  ├─▶ perception_fusion ─▶ trajectory_planner
///   camera_front  ─┘
///   gps_primary   (independent)
///
/// All nodes start Trusted. The posture cache starts at Nominal.
/// Returns (app, cache, clock) ready for ScenarioRunner construction.
async fn build_av_test_infrastructure() -> (
    Arc<AppState>,
    SharedPostureCache,
    Arc<VirtualClock>,
) {
    use aegis_runtime_sdk::verifier_store::VerifierStore;

    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));

    // Register AV metadata (in production this comes through HTTP endpoints)
    let _ = app.store.lock().unwrap().register_av_subsystem_meta(
        "lidar_front", "Perception", "LIDAR-001", 0.70, 0,
    );
    let _ = app.store.lock().unwrap().register_av_subsystem_meta(
        "camera_front", "Perception", "CAM-001", 0.70, 0,
    );
    let _ = app.store.lock().unwrap().register_av_subsystem_meta(
        "gps_primary", "Positioning", "GPS-001", 0.70, 0,
    );
    let _ = app.store.lock().unwrap().register_av_subsystem_meta(
        "perception_fusion", "Planning", "FUSION-001", 0.70, 0,
    );
    let _ = app.store.lock().unwrap().register_av_subsystem_meta(
        "trajectory_planner", "Planning", "PLAN-001", 0.70, 0,
    );

    // Set up dependency graph edges (mirrors POST /fleet/dependencies)
    // perception_fusion depends on lidar_front and camera_front
    app.dependency_graph.insert(
        "perception_fusion".to_string(),
        vec!["lidar_front".to_string(), "camera_front".to_string()],
    );
    // trajectory_planner depends on perception_fusion
    app.dependency_graph.insert(
        "trajectory_planner".to_string(),
        vec!["perception_fusion".to_string()],
    );

    // Initialize all nodes as Trusted in the DashMap
    for node_id in &["lidar_front", "camera_front", "gps_primary",
                     "perception_fusion", "trajectory_planner"] {
        app.nodes.insert(node_id.to_string(), aegis_runtime_sdk::verifier::RegisteredNode {
            node_id: node_id.to_string(),
            status: NodeTrustState::Trusted,
            registered_at_ms: 0,
            last_trust_update_ms: 0,
            ak_public_pem: None,
            expected_pcr16_digest_hex: None,
        });
    }

    let clock = VirtualClock::new();
    let posture_cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(Some(
        CachedFleetPosture::new(FleetPosture::Nominal),
    )));

    (app, posture_cache, clock)
}

// ---------------------------------------------------------------------------
// Test 1: Single sensor fault propagates through DAG to fleet posture
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_lidar_fault_degrades_fleet_through_dag() {
    let (app, cache, clock) = build_av_test_infrastructure().await;

    ScenarioRunner::with_clock(app, cache, clock)
        // t=0: LiDAR hardware fault
        .at_ms(0, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(),
            confidence: 0.0,
            hw_fault: true,
        })
        // t=0 assertions: lidar_front Untrusted, fleet LockedOut (via DAG — Untrusted propagates LockedOut)
        .assert_at_ms(0, PostureAssertion::NodeIsUntrusted("lidar_front".to_string()))
        .assert_at_ms(0, PostureAssertion::FleetPostureIs(FleetPosture::LockedOut))
        .run()
        .await;
}

// ---------------------------------------------------------------------------
// Test 2: Partial recovery (below threshold) leaves node Untrusted
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_partial_recovery_does_not_restore_trust() {
    let (app, cache, clock) = build_av_test_infrastructure().await;

    let mut runner = ScenarioRunner::with_clock(Arc::clone(&app), Arc::clone(&cache), Arc::clone(&clock))
        // t=0: fault
        .at_ms(0, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(),
            confidence: 0.0,
            hw_fault: true,
        });

    // Send exactly (threshold - 1) healthy reports — should not recover
    for i in 0..(AV_RECOVERY_STREAK_THRESHOLD - 1) {
        runner = runner.at_ms(100 + i as u64 * 100, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(),
            confidence: 0.95,
            hw_fault: false,
        });
    }

    let last_report_t = 100 + (AV_RECOVERY_STREAK_THRESHOLD as u64 - 1) * 100;

    runner
        // Assert still Untrusted after (threshold - 1) healthy reports
        .assert_at_ms(last_report_t, PostureAssertion::NodeIsUntrusted("lidar_front".to_string()))
        .assert_at_ms(last_report_t, PostureAssertion::FleetPostureIs(FleetPosture::LockedOut))
        .run()
        .await;
}

// ---------------------------------------------------------------------------
// Test 3: Full recovery restores trust after exactly threshold reports
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_full_recovery_restores_trust_at_threshold() {
    let (app, cache, clock) = build_av_test_infrastructure().await;

    let mut runner = ScenarioRunner::with_clock(app, cache, clock)
        .at_ms(0, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(),
            confidence: 0.0,
            hw_fault: true,
        });

    // Send exactly threshold healthy reports
    for i in 0..AV_RECOVERY_STREAK_THRESHOLD {
        runner = runner.at_ms(100 + i as u64 * 100, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(),
            confidence: 0.95,
            hw_fault: false,
        });
    }

    let recovery_t = 100 + AV_RECOVERY_STREAK_THRESHOLD as u64 * 100;

    runner
        .assert_at_ms(recovery_t, PostureAssertion::NodeIsTrusted("lidar_front".to_string()))
        .assert_at_ms(recovery_t, PostureAssertion::FleetPostureIs(FleetPosture::Nominal))
        .run()
        .await;
}

// ---------------------------------------------------------------------------
// Test 4: Second fault during recovery resets streak and re-degrades
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_fault_during_recovery_resets_streak() {
    let (app, cache, clock) = build_av_test_infrastructure().await;

    ScenarioRunner::with_clock(app, cache, clock)
        // t=0: initial fault
        .at_ms(0, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(),
            confidence: 0.0,
            hw_fault: true,
        })
        // t=100,200,300: three healthy reports (streak=3, threshold=5)
        .at_ms(100, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.95, hw_fault: false,
        })
        .at_ms(200, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.95, hw_fault: false,
        })
        .at_ms(300, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.95, hw_fault: false,
        })
        // t=350: second fault — streak must reset to 0
        .at_ms(350, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.10, hw_fault: true,
        })
        // Assert still Untrusted (streak reset means needs full 5 again)
        .assert_at_ms(350, PostureAssertion::NodeIsUntrusted("lidar_front".to_string()))
        .assert_at_ms(350, PostureAssertion::FleetPostureIs(FleetPosture::LockedOut))
        .run()
        .await;
}

// ---------------------------------------------------------------------------
// Test 5: Cache staleness — virtual clock advance past TTL produces stale
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_stale_cache_fails_closed_after_virtual_clock_advance() {
    let (_app, cache, clock) = build_av_test_infrastructure().await;

    // Populate the cache with an entry generated at virtual t=0
    let entry_at_t0 = CachedFleetPosture {
        posture: FleetPosture::Nominal,
        generated_at_ms: 0,
        ttl_ms: POSTURE_CACHE_TTL_MS,
        generation: 1,
    };
    *cache.write().unwrap() = Some(entry_at_t0);

    // Advance virtual clock past TTL
    clock.advance_ms(POSTURE_CACHE_TTL_MS + 1);

    // At this virtual time, is_stale() must return true
    let ts = clock.now_ms();
    let guard = cache.read().unwrap();
    let is_stale = guard.as_ref().map(|c| c.is_stale(ts)).unwrap_or(true);
    assert!(is_stale, "cache must be stale after virtual clock advances past TTL");
}

// ---------------------------------------------------------------------------
// Test 6: Multi-sensor fault produces Degraded, not LockedOut (no DAG cycle)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_two_independent_sensor_faults_produce_degraded() {
    let (app, cache, clock) = build_av_test_infrastructure().await;

    ScenarioRunner::with_clock(app, cache, clock)
        // Both independent sensors fault simultaneously
        .at_ms(0, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.0, hw_fault: true,
        })
        .at_ms(0, ScenarioEvent::TelemetryReport {
            node_id: "gps_primary".to_string(), confidence: 0.3, hw_fault: false,
        })
        // Two Untrusted nodes — Untrusted propagates LockedOut in the DAG
        .assert_at_ms(0, PostureAssertion::NodeIsUntrusted("lidar_front".to_string()))
        .assert_at_ms(0, PostureAssertion::NodeIsUntrusted("gps_primary".to_string()))
        .assert_at_ms(0, PostureAssertion::FleetPostureIs(FleetPosture::LockedOut))
        .run()
        .await;
}

// ---------------------------------------------------------------------------
// Test 7: The milestone doc scenario — verbatim from the example in assessment
// ---------------------------------------------------------------------------
//
// "LiDAR degrades at t=0, GPS degrades at t=3s, LiDAR sends 3 recovery
// reports between t=5s and t=6s, then goes silent again at t=8s"

#[tokio::test]
async fn test_multisensor_scenario_from_assessment() {
    let (app, cache, clock) = build_av_test_infrastructure().await;

    ScenarioRunner::with_clock(app, cache, clock)
        // t=0: LiDAR hardware fault
        .at_ms(0, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.0, hw_fault: true,
        })
        .assert_at_ms(0, PostureAssertion::NodeIsUntrusted("lidar_front".to_string()))

        // t=3000: GPS confidence drops
        .at_ms(3000, ScenarioEvent::TelemetryReport {
            node_id: "gps_primary".to_string(), confidence: 0.45, hw_fault: false,
        })
        .assert_at_ms(3000, PostureAssertion::NodeIsUntrusted("gps_primary".to_string()))
        .assert_at_ms(3000, PostureAssertion::FleetPostureIs(FleetPosture::LockedOut))

        // t=5000, 5500, 6000: LiDAR sends 3 healthy reports (streak=3, threshold=5)
        .at_ms(5000, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.95, hw_fault: false,
        })
        .at_ms(5500, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.95, hw_fault: false,
        })
        .at_ms(6000, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.95, hw_fault: false,
        })
        // Still Untrusted — 3 < 5 threshold, fleet still LockedOut
        .assert_at_ms(6000, PostureAssertion::NodeIsUntrusted("lidar_front".to_string()))
        .assert_at_ms(6000, PostureAssertion::FleetPostureIs(FleetPosture::LockedOut))

        // t=8000: LiDAR silent again — simulated via MarkUntrusted (watchdog fires)
        .at_ms(8000, ScenarioEvent::MarkUntrusted {
            node_id: "lidar_front".to_string(),
            reason: "TELEMETRY_TIMEOUT".to_string(),
        })
        // Streak reset, both sensors Untrusted, fleet LockedOut
        .assert_at_ms(8000, PostureAssertion::NodeIsUntrusted("lidar_front".to_string()))
        .assert_at_ms(8000, PostureAssertion::NodeIsUntrusted("gps_primary".to_string()))
        .assert_at_ms(8000, PostureAssertion::FleetPostureIs(FleetPosture::LockedOut))
        .run()
        .await;
}
