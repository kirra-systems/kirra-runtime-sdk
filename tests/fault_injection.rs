// tests/fault_injection.rs
//
// CERT-004 — Fault injection / safe-state verification suite.
//
// Each test exercises a documented safety goal from
// docs/safety/SAFETY_GOALS.md by injecting the fault condition and
// asserting the system's safe-state response per
// docs/safety/SAFE_STATE_SPECIFICATION.md.
//
// This suite originally closed the first CERT-004 fault-injection slice
// (SG-006, SG-014, SG-016). The broader CERT-003 RTM reconciliation has since
// replaced the old ignored placeholders in `tests/cert_003_rtm_gap_stubs.rs`
// with executable tests or precise pointers to in-crate / binary tests for
// private seams. Keep this file focused on externally-drivable safe-state
// injections.
//
// Tracking: CERT-004, ADL-010 / ADL-011 in work/decisions.md.

use kirra_verifier::federation::{
    evaluate_federated_report, FederatedTrustReport, FEDERATION_REPLAY_WINDOW_MS,
};
use kirra_verifier::gateway::policy::OperationalCommand;
use kirra_verifier::posture_cache::{should_route_command, CachedFleetPosture};
use kirra_verifier::verifier::FleetPosture;

// ============================================================================
// SG-006 (ASIL D) — Unknown command denial in all posture states
//
// Property: `should_route_command` returns false for
// `OperationalCommand::Unknown` unconditionally — before any posture
// evaluation, in every posture state, and even when no cache is present.
// Safe state: SS-001 — per-request denial; posture unaffected.
// ============================================================================

#[test]
fn test_safety_goal_sg_006_unknown_command_denial() {
    let now_ms: u64 = 1_000_000;

    for posture in [
        FleetPosture::Nominal,
        FleetPosture::Degraded,
        FleetPosture::LockedOut,
    ] {
        // Cache generated at now_ms → not stale; isolates the Unknown
        // early-return from the staleness fail-closed path.
        let cache = Some(CachedFleetPosture::new_with_generation(posture, 0, now_ms));

        let routed = should_route_command(&cache, now_ms, OperationalCommand::Unknown);
        assert!(
            !routed,
            "SG-006: OperationalCommand::Unknown must be denied in posture {:?}",
            posture
        );
    }

    // Also confirm fail-closed when no cache is present.
    let no_cache: Option<CachedFleetPosture> = None;
    let routed = should_route_command(&no_cache, now_ms, OperationalCommand::Unknown);
    assert!(
        !routed,
        "SG-006: Unknown must be denied even when posture cache is absent"
    );

    // Cross-check the precondition that the same routing function DOES
    // permit a known command in Nominal — confirms our setup is not
    // accidentally fail-closing on every command.
    let nominal_cache = Some(CachedFleetPosture::new_with_generation(
        FleetPosture::Nominal,
        0,
        now_ms,
    ));
    assert!(
        should_route_command(&nominal_cache, now_ms, OperationalCommand::ReadTelemetry),
        "Sanity check: ReadTelemetry must be permitted under Nominal with fresh cache"
    );
}

// ============================================================================
// SG-014 (ASIL B) — Federation report replay prevention
//
// Property: `evaluate_federated_report` rejects reports outside the
// replay window, future-dated reports, and expired reports. The
// nonce-burn component of replay prevention is enforced at the
// persistence layer (`has_seen_federation_nonce`) and is out of scope
// for this in-memory unit test, which exercises the time-window
// component documented in SG-014.
// Safe state: SS-001 — per-request rejection; posture unchanged.
// ============================================================================

fn make_valid_report(now_ms: u64) -> FederatedTrustReport {
    FederatedTrustReport {
        source_controller_id: "ctrl-1".to_string(),
        asset_id: "asset-1".to_string(),
        posture: FleetPosture::Nominal,
        issued_at_ms: now_ms - 1_000,
        expires_at_ms: now_ms + 60_000,
        nonce_hex: "deadbeef".to_string(),
        signature_b64: "X".to_string(), // signature verification is a separate pipeline stage
    }
}

#[test]
fn test_safety_goal_sg_014_federation_report_replay_prevention() {
    let now_ms: u64 = 1_000_000;

    // Baseline: a fresh, within-window report is accepted.
    let valid = make_valid_report(now_ms);
    let result = evaluate_federated_report(&valid, now_ms);
    assert!(
        result.accepted,
        "Fresh report within replay window must be accepted (reason: {})",
        result.reason
    );

    // Replay attack: report issued more than FEDERATION_REPLAY_WINDOW_MS ago.
    let replayed = FederatedTrustReport {
        issued_at_ms: now_ms - FEDERATION_REPLAY_WINDOW_MS - 1_000,
        ..valid.clone()
    };
    let result = evaluate_federated_report(&replayed, now_ms);
    assert!(
        !result.accepted,
        "Report older than FEDERATION_REPLAY_WINDOW_MS ({} ms) must be rejected",
        FEDERATION_REPLAY_WINDOW_MS
    );
    assert!(
        result.reason.contains("REPLAY_WINDOW") || result.reason.contains("OUTSIDE"),
        "Rejection should reference replay window; got {:?}",
        result.reason
    );

    // Timeline-invalid: future-dated report.
    let future_dated = FederatedTrustReport {
        issued_at_ms: now_ms + 1_000,
        ..valid.clone()
    };
    let result = evaluate_federated_report(&future_dated, now_ms);
    assert!(
        !result.accepted,
        "Future-issued report must be rejected (clock skew / forgery defense)"
    );
    assert!(
        result.reason.contains("FUTURE"),
        "Rejection should reference timeline; got {:?}",
        result.reason
    );

    // Expired report.
    let expired = FederatedTrustReport {
        issued_at_ms: now_ms - 1_000,
        expires_at_ms: now_ms - 100,
        ..valid
    };
    let result = evaluate_federated_report(&expired, now_ms);
    assert!(
        !result.accepted,
        "Report whose expires_at_ms has passed must be rejected"
    );
    assert!(
        result.reason.contains("STALE") || result.reason.contains("EXPIRED"),
        "Rejection should reference expiry; got {:?}",
        result.reason
    );
}

// ============================================================================
// SG-016 (ASIL C) — DDS actuator topic volatile durability
//
// Property: the actuator-topic QoS the bridge actually constructs uses
// `DdsDurability::Volatile`, and the publish seam REFUSES to emit a frame under
// any non-Volatile profile. A `TransientLocal` actuator topic could replay stale
// commands to reconnecting subscribers, violating the safety property.
// Safe state: SS-004 — startup_sentinel aborts on TransientLocal.
//
// #1047 (CI-Honesty): this test used to `read_to_string("src/dds_bridge.rs")` and
// grep the SOURCE TEXT for `durability: DdsDurability::Volatile`. That check was
// defeated by any formatting/refactor (a field split across lines, a builder call,
// a `const` alias) and asserted nothing about RUNTIME behaviour. It now constructs
// the real `critical_actuator_profile()` and exercises the runtime QoS-admissibility
// decision — the actual thing that gates a publish — so a refactor that keeps the
// text but breaks the behaviour (or vice-versa) can no longer pass vacuously.
// ============================================================================

#[test]
fn test_safety_goal_sg_016_dds_actuator_volatile_durability() {
    use kirra_verifier::dds_bridge::{
        DdsDurability, DdsPublisherBridge, DdsQosProfile, DdsQosViolation,
    };

    // 1. The frozen actuator profile the bridge actually builds is Volatile, and
    //    it is admissible + publishes a frame.
    let profile = DdsQosProfile::critical_actuator_profile();
    assert_eq!(
        profile.durability,
        DdsDurability::Volatile,
        "SG-016: the constructed actuator QoS profile MUST be Volatile"
    );
    assert!(
        profile.actuator_admissibility().is_ok(),
        "SG-016: the frozen actuator profile must pass runtime QoS admissibility"
    );
    assert!(
        DdsPublisherBridge::publish_actuator_command(&[0xAA], &profile).is_ok(),
        "SG-016: a Volatile actuator topic must publish"
    );

    // 2. Flipping durability to TransientLocal is REFUSED at the runtime publish
    //    seam (fail-closed) — not merely absent from the source text.
    let mut transient = profile;
    transient.durability = DdsDurability::TransientLocal;
    assert_eq!(
        transient.actuator_admissibility(),
        Err(DdsQosViolation::NonVolatileActuatorTopic),
        "SG-016: a TransientLocal actuator topic must be inadmissible"
    );
    assert_eq!(
        DdsPublisherBridge::publish_actuator_command(&[0xAA], &transient),
        Err(DdsQosViolation::NonVolatileActuatorTopic),
        "SG-016: the publish seam MUST refuse a TransientLocal actuator topic — it \
         could replay stale commands to a reconnecting subscriber (INV-10)"
    );
}

// ============================================================================
// SG-007 (ASIL D) — Cross-asset fleet lockout propagation
//
// Property: when a convoy LEADER transitions to LockedOut, every Nominal
// convoy FOLLOWER is degraded within a single synchronous fabric pass
// (`FabricRouter::update_asset_posture_and_propagate`). The "≤ 500 ms one
// fabric tick" budget holds BY CONSTRUCTION — propagation runs inline within
// the call, with no async hop to time — so the bound is argued structurally
// rather than by sleeping.
//
// Mechanism: src/fabric/router.rs — register_asset / update_asset_posture /
// update_asset_posture_and_propagate / fabric_state, driven by
// propagate_cross_asset_trust Rule 2 (the `convoy_role` metadata key).
// Safe state: dependent assets fail toward Degraded when a depended-upon
// asset is lost.
//
// SCOPE NOTE (honest gap): the RTM also names
// `test_causal_log_records_propagation_event`, but the current FabricRouter
// does NOT record propagation events to any causal log — the FabricCausalLog
// lives on ServiceState and is not wired into propagate_cross_asset_trust.
// That assertion is therefore intentionally NOT made here (a green test must
// reflect the mechanism as it exists); it remains an explicit stub in
// tests/cert_003_rtm_gap_stubs.rs pending propagation→causal-log wiring.
// ============================================================================

#[test]
fn test_safety_goal_sg_007_cross_asset_lockout_propagation() {
    use kirra_verifier::fabric::asset::{
        AssetPosture, AssetType, FabricAsset, KinematicProfileType,
    };
    use kirra_verifier::fabric::router::FabricRouter;
    use std::collections::HashMap;

    fn convoy_asset(id: &str, role: &str) -> FabricAsset {
        let mut metadata = HashMap::new();
        metadata.insert("convoy_role".to_string(), role.to_string());
        FabricAsset {
            asset_id: id.to_string(),
            asset_type: AssetType::AutonomousVehicle,
            display_name: id.to_string(),
            kinematic_profile: KinematicProfileType::AutomotiveNominal,
            registered_at_ms: 1_000,
            last_seen_ms: 1_000,
            metadata,
        }
    }
    fn posture_at(id: &str, p: FleetPosture, generation: u64) -> AssetPosture {
        AssetPosture {
            asset_id: id.to_string(),
            posture: p,
            generation,
            computed_at_ms: 2_000,
            contributing_nodes: vec![],
            blocked_by: vec![],
        }
    }

    let router = FabricRouter::new();
    router.register_asset(&convoy_asset("leader01", "leader"));
    router.register_asset(&convoy_asset("follower01", "follower"));
    router.register_asset(&convoy_asset("follower02", "follower"));

    // register_asset seeds Degraded; force every asset Nominal so the
    // degrade TRANSITION produced by propagation is observable.
    for id in ["leader01", "follower01", "follower02"] {
        router.update_asset_posture(id, posture_at(id, FleetPosture::Nominal, 1));
    }

    // Precondition cross-check: a non-LockedOut (Nominal) leader must NOT
    // propagate — Rule 2 fires ONLY from a LockedOut leader.
    router.update_asset_posture_and_propagate(
        "leader01",
        posture_at("leader01", FleetPosture::Nominal, 2),
    );
    for a in router.fabric_state().assets {
        if a.asset_id.starts_with("follower") {
            assert_eq!(
                a.posture, FleetPosture::Nominal,
                "SG-007 precondition: follower {} must stay Nominal while the leader is not LockedOut",
                a.asset_id
            );
        }
    }

    // Leader → LockedOut: one synchronous propagation pass must degrade every
    // Nominal follower (the safety property).
    router.update_asset_posture_and_propagate(
        "leader01",
        posture_at("leader01", FleetPosture::LockedOut, 3),
    );

    let followers: Vec<(String, FleetPosture)> = router
        .fabric_state()
        .assets
        .into_iter()
        .filter(|a| a.asset_id.starts_with("follower"))
        .map(|a| (a.asset_id, a.posture))
        .collect();
    assert_eq!(followers.len(), 2, "SG-007: both followers must be present");
    for (id, posture) in followers {
        assert_eq!(
            posture,
            FleetPosture::Degraded,
            "SG-007: convoy follower {id} must degrade when the leader is LockedOut"
        );
    }
}
