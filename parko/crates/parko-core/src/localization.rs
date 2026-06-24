// parko-core/src/localization.rs
//
// SG4 / SG5 — the bool-driven map-anchored scene gates.
//
// The localization-integrity TYPE and its trust resolver were unified into
// kirra-core (`kirra_core::frame_integrity`: FrameIntegrity / FrameTrust /
// resolve_frame_trust) in Stage S-FI1c, so a SINGLE canonical frame-trust
// verdict drives both the SG2 containment margin (graduated) and these discrete
// map-anchored vetoes (strict — Trusted only). parko-core stays kirra-core-free:
// it keeps only the bool-driven gates below; parko-kirra resolves the verdict
// (strict view) and passes the bool in. See
// docs/safety/STAGE_S-FI1_FRAME_INTEGRITY_GATE.md.
//
// EVERY map-anchored trust here — the SG5 commit-zone gate (a mapped rail
// crossing / box junction), the SG4 `MapKnownSafe` water earn-back — is only as
// sound as the ego pose; under an untrusted pose every MAP-DERIVED trust
// degrades fail-closed. The G2 AoU (≤ 0.10 m 95th-pct lateral) is
// AOU-LOCALIZATION-001.

use crate::commit_zone::CommitZoneScene;
use crate::water::{TraversalEvidence, WaterScene};

// LocalizationIntegrity / LocalizationCfg / localization_trusted were relocated
// to kirra-core `frame_integrity` (FrameIntegrity / FrameIntegrityCfg /
// FrameTrust / resolve_frame_trust) in Stage S-FI1c — the single canonical
// frame-trust verdict. parko-kirra resolves it (strict `Trusted`-only view) and
// passes the resulting `bool` into the gates below. The boundary coverage that
// `test_loc_*` provided is now in kirra-core's `frame_integrity` tests.

/// SG5 coupling — degrade the commit-zone scene under an UNTRUSTED pose.
///
/// `trusted` → the scene is returned UNCHANGED. NOT trusted →
/// [`CommitZoneScene::Unknown`] REGARDLESS of the input — `NoZone` INCLUDED: "the
/// map says no commit zone here" is exactly the claim a wrong pose poisons (the
/// vehicle could be over an unmapped crossing). `Unknown` already vetoes via the
/// #260 machinery, so this reuses that fail-closed path — no new veto logic.
// SAFETY: SG5 | REQ: localization-integrity-gate | TEST: test_gate_cz_untrusted_nozone_to_unknown,test_gate_cz_untrusted_zoneahead_to_unknown,test_gate_cz_trusted_identity
pub fn gate_commit_zone_scene(scene: CommitZoneScene, trusted: bool) -> CommitZoneScene {
    if trusted {
        scene
    } else {
        CommitZoneScene::Unknown
    }
}

/// SG4 coupling — strip MAP-DERIVED water trust under an UNTRUSTED pose.
///
/// `trusted` → UNCHANGED. NOT trusted → strip ONLY the map-frame-dependent trust:
///   * `EarnedTraversable { MapKnownSafe }` → [`WaterScene::Unknown`]: a mapped
///     ford is anchored to the map frame, so a misplaced pose cannot vouch for it
///     — fall back to the fail-closed veto state (we do not retain the pre-earn
///     detection, so the conservative `Unknown` is the correct floor).
///   * `EarnedTraversable { OperatorAuthorized }` → UNCHANGED. **The asymmetry is
///     the design's crux:** operator authority is a human grant for THIS physical
///     spot, not a map-frame claim, so it survives a localization fault.
///   * `Clear` / `Detected` / `Unknown` → UNCHANGED: these are perception-derived
///     (the sensor sees water or doesn't), not map-anchored.
// SAFETY: SG4 | REQ: localization-integrity-gate | TEST: test_gate_water_untrusted_strips_mapknownsafe,test_gate_water_operator_authorized_survives,test_gate_water_perception_states_untouched,test_gate_water_trusted_identity
pub fn gate_water_scene(scene: WaterScene, trusted: bool) -> WaterScene {
    if trusted {
        return scene;
    }
    match scene {
        WaterScene::EarnedTraversable {
            evidence: TraversalEvidence::MapKnownSafe,
        } => WaterScene::Unknown,
        // Operator authority is not map-frame-dependent → survives.
        WaterScene::EarnedTraversable {
            evidence: TraversalEvidence::OperatorAuthorized,
        } => scene,
        // Perception-derived states are untouched by a localization fault.
        WaterScene::Clear | WaterScene::Detected { .. } | WaterScene::Unknown => scene,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit_zone::{CommitZoneMap, CommitZoneScene};
    use crate::water::{water_untraversable_veto, TraversalEvidence, WaterScene, WaterVetoConfig};

    // The localization_trusted boundary tests (test_loc_*) moved with the
    // resolver to kirra-core `frame_integrity`. The strict `Trusted`-only view
    // parko applies is exercised end-to-end by the parko-kirra wrapper tests.

    // ─────────────────────── gate_commit_zone_scene ────────────────────────

    fn healthy_map() -> CommitZoneMap {
        CommitZoneMap {
            zone_ahead: false,
            distance_to_zone_m: 50.0,
            confidence: 0.95,
            age_ms: 50,
            min_confidence: 0.5,
            max_age_ms: 1_000,
        }
    }

    /// THE HEADLINE: an untrusted pose turns a clean `NoZone` into `Unknown` — a
    /// "no commit zone here" reading under a bad pose is NOT clean.
    #[test]
    fn test_gate_cz_untrusted_nozone_to_unknown() {
        let gated = gate_commit_zone_scene(CommitZoneScene::NoZone, false);
        assert!(
            matches!(gated, CommitZoneScene::Unknown),
            "untrusted localization must turn NoZone into Unknown"
        );
    }

    /// A `ZoneAhead` under untrusted pose also collapses to `Unknown`.
    #[test]
    fn test_gate_cz_untrusted_zoneahead_to_unknown() {
        let zone = CommitZoneScene::ZoneAhead {
            map: healthy_map(),
            clearance_confirmed: true,
            exit_verified: true,
            zone_length_m: 30.0,
            proposed_stop_distance_m: None,
        };
        assert!(matches!(
            gate_commit_zone_scene(zone, false),
            CommitZoneScene::Unknown
        ));
    }

    /// Trusted → identity (the scene is returned unchanged).
    #[test]
    fn test_gate_cz_trusted_identity() {
        assert!(matches!(
            gate_commit_zone_scene(CommitZoneScene::NoZone, true),
            CommitZoneScene::NoZone
        ));
        let zone = CommitZoneScene::ZoneAhead {
            map: healthy_map(),
            clearance_confirmed: true,
            exit_verified: true,
            zone_length_m: 30.0,
            proposed_stop_distance_m: None,
        };
        assert!(matches!(
            gate_commit_zone_scene(zone, true),
            CommitZoneScene::ZoneAhead { .. }
        ));
    }

    // ───────────────────────── gate_water_scene ────────────────────────────

    fn wcfg() -> WaterVetoConfig {
        WaterVetoConfig {
            max_exit_distance_m: 5.0,
            max_puddle_extent_m: 5.0,
        }
    }

    /// Untrusted pose strips a `MapKnownSafe` earn-back → the gated scene now
    /// VETOES (it became `Unknown`).
    #[test]
    fn test_gate_water_untrusted_strips_mapknownsafe() {
        let earned = WaterScene::EarnedTraversable {
            evidence: TraversalEvidence::MapKnownSafe,
        };
        // Pre-gate: the earn-back does NOT veto.
        assert!(!water_untraversable_veto(&earned, &wcfg()));
        // Post-gate under untrusted pose: it does.
        let gated = gate_water_scene(earned, false);
        assert!(matches!(gated, WaterScene::Unknown));
        assert!(
            water_untraversable_veto(&gated, &wcfg()),
            "a stripped MapKnownSafe must veto under untrusted localization"
        );
    }

    /// OperatorAuthorized SURVIVES an untrusted pose (the asymmetry crux) — still
    /// an earn-back, still no veto.
    #[test]
    fn test_gate_water_operator_authorized_survives() {
        let earned = WaterScene::EarnedTraversable {
            evidence: TraversalEvidence::OperatorAuthorized,
        };
        let gated = gate_water_scene(earned, false);
        assert!(matches!(
            gated,
            WaterScene::EarnedTraversable {
                evidence: TraversalEvidence::OperatorAuthorized
            }
        ));
        assert!(
            !water_untraversable_veto(&gated, &wcfg()),
            "operator authority is not map-frame-dependent — it must survive"
        );
    }

    /// Perception-derived states (Clear / Detected / Unknown) are UNTOUCHED by a
    /// localization fault.
    #[test]
    fn test_gate_water_perception_states_untouched() {
        assert!(matches!(
            gate_water_scene(WaterScene::Clear, false),
            WaterScene::Clear
        ));
        assert!(matches!(
            gate_water_scene(WaterScene::Unknown, false),
            WaterScene::Unknown
        ));
        let detected = WaterScene::Detected {
            extent_m: 2.0,
            exit_distance_m: Some(1.0),
            flow_detected: false,
            geometry_confirmed: true,
        };
        assert!(matches!(
            gate_water_scene(detected, false),
            WaterScene::Detected { .. }
        ));
    }

    /// Trusted → identity for the water gate.
    #[test]
    fn test_gate_water_trusted_identity() {
        let earned = WaterScene::EarnedTraversable {
            evidence: TraversalEvidence::MapKnownSafe,
        };
        assert!(matches!(
            gate_water_scene(earned, true),
            WaterScene::EarnedTraversable {
                evidence: TraversalEvidence::MapKnownSafe
            }
        ));
    }
}
