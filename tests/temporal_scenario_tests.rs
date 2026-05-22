// tests/temporal_scenario_tests.rs
//
// Multi-sensor temporal integration test suite.
// Tests use ScenarioRunner with real in-memory AppState and VirtualClock.

use std::sync::Arc;

use aegis_runtime_sdk::verifier::{AppState, FleetPosture, NodeTrustState, RegisteredNode, VerifierOperationMode};
use aegis_runtime_sdk::verifier_store::VerifierStore;
use aegis_runtime_sdk::posture_cache::{CachedFleetPosture, SharedPostureCache};
use aegis_runtime_sdk::scenario_runner::{
    PostureAssertion, ScenarioEvent, ScenarioRunner,
};
use aegis_runtime_sdk::clock::VirtualClock;
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
async fn build_av_test_infrastructure() -> (
    Arc<AppState>,
    SharedPostureCache,
    Arc<VirtualClock>,
) {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));

    // Register AV metadata (lock the Mutex-wrapped store).
    {
        let guard = app.store.lock().unwrap();
        let _ = guard.register_av_subsystem_meta(
            "lidar_front", "Perception", "LIDAR-001", 0.70, 0,
        );
        let _ = guard.register_av_subsystem_meta(
            "camera_front", "Perception", "CAM-001", 0.70, 0,
        );
        let _ = guard.register_av_subsystem_meta(
            "gps_primary", "Positioning", "GPS-001", 0.70, 0,
        );
        let _ = guard.register_av_subsystem_meta(
            "perception_fusion", "Planning", "FUSION-001", 0.70, 0,
        );
        let _ = guard.register_av_subsystem_meta(
            "trajectory_planner", "Planning", "PLAN-001", 0.70, 0,
        );
    }

    // Set up dependency graph.
    app.dependency_graph.insert(
        "perception_fusion".to_string(),
        vec!["lidar_front".to_string(), "camera_front".to_string()],
    );
    app.dependency_graph.insert(
        "trajectory_planner".to_string(),
        vec!["perception_fusion".to_string()],
    );

    // Initialize all nodes as Trusted.
    let now = 0u64;
    for node_id in &["lidar_front", "camera_front", "gps_primary",
                     "perception_fusion", "trajectory_planner"] {
        app.nodes.insert(node_id.to_string(), RegisteredNode {
            node_id: node_id.to_string(),
            status: NodeTrustState::Trusted,
            registered_at_ms: now,
            last_trust_update_ms: now,
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
        .at_ms(0, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(),
            confidence: 0.0,
            hw_fault: true,
        })
        .assert_at_ms(0, PostureAssertion::NodeIsUntrusted("lidar_front".to_string()))
        .assert_at_ms(0, PostureAssertion::FleetPostureIs(FleetPosture::Degraded))
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
        .at_ms(0, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(),
            confidence: 0.0,
            hw_fault: true,
        });

    for i in 0..(AV_RECOVERY_STREAK_THRESHOLD - 1) {
        runner = runner.at_ms(100 + i as u64 * 100, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(),
            confidence: 0.95,
            hw_fault: false,
        });
    }

    let last_report_t = 100 + (AV_RECOVERY_STREAK_THRESHOLD as u64 - 1) * 100;

    runner
        .assert_at_ms(last_report_t, PostureAssertion::NodeIsUntrusted("lidar_front".to_string()))
        .assert_at_ms(last_report_t, PostureAssertion::FleetPostureIs(FleetPosture::Degraded))
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
        .at_ms(0, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(),
            confidence: 0.0,
            hw_fault: true,
        })
        .at_ms(100, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.95, hw_fault: false,
        })
        .at_ms(200, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.95, hw_fault: false,
        })
        .at_ms(300, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.95, hw_fault: false,
        })
        .at_ms(350, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.10, hw_fault: true,
        })
        .assert_at_ms(350, PostureAssertion::NodeIsUntrusted("lidar_front".to_string()))
        .assert_at_ms(350, PostureAssertion::FleetPostureIs(FleetPosture::Degraded))
        .run()
        .await;
}

// ---------------------------------------------------------------------------
// Test 5: Cache staleness — virtual clock advance past TTL produces stale
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_stale_cache_fails_closed_after_virtual_clock_advance() {
    let (app, cache, clock) = build_av_test_infrastructure().await;

    aegis_runtime_sdk::posture_engine::recalculate_and_broadcast(&app, &cache);

    clock.advance_ms(POSTURE_CACHE_TTL_MS + 1);

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
        .at_ms(0, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.0, hw_fault: true,
        })
        .at_ms(0, ScenarioEvent::TelemetryReport {
            node_id: "gps_primary".to_string(), confidence: 0.3, hw_fault: false,
        })
        .assert_at_ms(0, PostureAssertion::NodeIsUntrusted("lidar_front".to_string()))
        .assert_at_ms(0, PostureAssertion::NodeIsUntrusted("gps_primary".to_string()))
        .assert_at_ms(0, PostureAssertion::FleetPostureIs(FleetPosture::Degraded))
        .run()
        .await;
}

// ---------------------------------------------------------------------------
// Test 7: Multi-sensor scenario
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_multisensor_scenario_from_assessment() {
    let (app, cache, clock) = build_av_test_infrastructure().await;

    ScenarioRunner::with_clock(app, cache, clock)
        .at_ms(0, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.0, hw_fault: true,
        })
        .assert_at_ms(0, PostureAssertion::NodeIsUntrusted("lidar_front".to_string()))

        .at_ms(3000, ScenarioEvent::TelemetryReport {
            node_id: "gps_primary".to_string(), confidence: 0.45, hw_fault: false,
        })
        .assert_at_ms(3000, PostureAssertion::NodeIsUntrusted("gps_primary".to_string()))
        .assert_at_ms(3000, PostureAssertion::FleetPostureIs(FleetPosture::Degraded))

        .at_ms(5000, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.95, hw_fault: false,
        })
        .at_ms(5500, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.95, hw_fault: false,
        })
        .at_ms(6000, ScenarioEvent::TelemetryReport {
            node_id: "lidar_front".to_string(), confidence: 0.95, hw_fault: false,
        })
        .assert_at_ms(6000, PostureAssertion::NodeIsUntrusted("lidar_front".to_string()))
        .assert_at_ms(6000, PostureAssertion::FleetPostureIs(FleetPosture::Degraded))

        .at_ms(8000, ScenarioEvent::MarkUntrusted {
            node_id: "lidar_front".to_string(),
            reason: "TELEMETRY_TIMEOUT".to_string(),
        })
        .assert_at_ms(8000, PostureAssertion::NodeIsUntrusted("lidar_front".to_string()))
        .assert_at_ms(8000, PostureAssertion::NodeIsUntrusted("gps_primary".to_string()))
        .assert_at_ms(8000, PostureAssertion::FleetPostureIs(FleetPosture::Degraded))
        .run()
        .await;
}
