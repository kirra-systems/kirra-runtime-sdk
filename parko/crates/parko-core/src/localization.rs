// parko-core/src/localization.rs
//
// SG2 / SG5 — localization-integrity gate over the map-anchored checks (#123,
// runtime half).
//
// The G2 ASSUMPTION-OF-USE (OCCY_SAFETY_GOALS.md, ~line 102, ref
// KIRRA-OCCY-SG2-MARGIN-001) is that the integrator's localization holds a 95th-
// percentile lateral error of ≤ 0.10 m. EVERY map-anchored trust in this crate —
// the SG5 commit-zone gate (a mapped rail crossing / box junction), the SG4
// `MapKnownSafe` water earn-back — is only as sound as that pose. This module is
// the RUNTIME COMPLEMENT to that static AoU: when the integrator's localization-
// integrity reporting says the assumption does NOT currently hold (error over
// bound, stale, or simply unreported), every MAP-DERIVED trust degrades fail-
// closed.
//
// The formal AoU REGISTER clause is a separate docs PR; this file is the
// executable gate only (doc-comments excepted).
//
// SOURCING NOTE: deriving [`LocalizationIntegrity::Reported`] from the
// integrator's signals (NDT/ICP match scores, pose covariance, RTK fix status)
// is integrator/ingestion territory and is DEFERRED — exactly like the agent
// set, water, occlusion, and commit-zone scenes are supplied check INPUTS.

use crate::commit_zone::CommitZoneScene;
use crate::water::{TraversalEvidence, WaterScene};

/// What the integrator's localization-integrity channel reports this tick.
/// Mirrors the established ABSENT-vs-KNOWN discipline (cf. `WaterScene`,
/// `CommitZoneScene`, `OcclusionScene`): an ABSENT report is NOT a healthy pose.
#[derive(Debug, Clone, Copy)]
pub enum LocalizationIntegrity {
    /// No integrity report this tick → NOT trusted. DISTINCT from a healthy
    /// report (the #238 absent-vs-known trap): a missing pose-quality signal is
    /// not "the pose is fine".
    Unknown,
    /// The integrator reported a pose-quality estimate.
    Reported {
        /// 95th-percentile lateral position error (m). Compared against the G2
        /// AoU bound. Non-finite → NOT trusted.
        lateral_error_95_m: f64,
        /// Age (ms) of this integrity snapshot vs now. Above `max_age_ms` →
        /// stale → NOT trusted.
        age_ms: u64,
    },
}

/// Config for the localization-integrity gate.
#[derive(Debug, Clone, Copy)]
pub struct LocalizationCfg {
    /// The G2 AoU bound: maximum 95th-pct lateral error (m) for the pose to be
    /// trusted for MAP-ANCHORED reasoning. Default `0.10` m — the value the
    /// OCCY_SAFETY_GOALS.md G2 assumption-of-use is written against
    /// (KIRRA-OCCY-SG2-MARGIN-001). NOT a free placeholder: this couples to the
    /// documented safety-goal margin.
    pub max_lateral_error_95_m: f64,
    /// Maximum acceptable staleness (ms) of an integrity report. VALIDATION-
    /// PENDING conservative default — tie to the per-cycle FTTI on integration.
    pub max_age_ms: u64,
}

impl Default for LocalizationCfg {
    fn default() -> Self {
        Self {
            max_lateral_error_95_m: 0.10, // G2 AoU bound — KIRRA-OCCY-SG2-MARGIN-001
            max_age_ms: 500,              // VALIDATION-PENDING conservative default
        }
    }
}

/// SG2/SG5 — is the integrator's localization currently trustworthy for MAP-
/// ANCHORED reasoning? Mirrors `CommitZoneMap::is_healthy`'s conservative,
/// finite-checked semantics.
///
/// * `Unknown` → `false` (absent ≠ healthy).
/// * `Reported` → `true` IFF `lateral_error_95_m` is FINITE **and** ≤ the bound
///   **and** `age_ms` ≤ `max_age_ms`. A non-finite error fails closed (an
///   unverifiable pose is NO pose).
// SAFETY: SG2 SG5 | REQ: localization-integrity-gate | TEST: test_loc_bound_boundary,test_loc_just_over_bound_not_trusted,test_loc_stale_not_trusted,test_loc_nonfinite_not_trusted,test_loc_unknown_not_trusted,test_loc_healthy_trusted
pub fn localization_trusted(integrity: &LocalizationIntegrity, cfg: &LocalizationCfg) -> bool {
    match *integrity {
        LocalizationIntegrity::Unknown => false,
        LocalizationIntegrity::Reported {
            lateral_error_95_m,
            age_ms,
        } => {
            lateral_error_95_m.is_finite()
                && lateral_error_95_m <= cfg.max_lateral_error_95_m
                && age_ms <= cfg.max_age_ms
        }
    }
}

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

    fn cfg() -> LocalizationCfg {
        LocalizationCfg::default() // bound 0.10 m, max_age 500 ms
    }
    fn reported(err: f64, age_ms: u64) -> LocalizationIntegrity {
        LocalizationIntegrity::Reported {
            lateral_error_95_m: err,
            age_ms,
        }
    }

    // ───────────────────────── localization_trusted ────────────────────────

    /// Boundary: exactly at the 0.10 m bound (and fresh) → trusted (inclusive).
    #[test]
    fn test_loc_bound_boundary() {
        assert!(
            localization_trusted(&reported(0.10, 50), &cfg()),
            "error exactly at the bound must be trusted (inclusive)"
        );
    }

    /// Just over the bound → NOT trusted.
    #[test]
    fn test_loc_just_over_bound_not_trusted() {
        assert!(
            !localization_trusted(&reported(0.10 + 1e-9, 50), &cfg()),
            "error just over the bound must not be trusted"
        );
    }

    /// A stale report (age over max) → NOT trusted, even with a good error.
    #[test]
    fn test_loc_stale_not_trusted() {
        assert!(
            !localization_trusted(&reported(0.01, 1_000), &cfg()),
            "a stale integrity report must not be trusted"
        );
    }

    /// A non-finite error → NOT trusted (an unverifiable pose is no pose).
    #[test]
    fn test_loc_nonfinite_not_trusted() {
        for bad in [f64::NAN, f64::INFINITY] {
            assert!(
                !localization_trusted(&reported(bad, 50), &cfg()),
                "non-finite error must not be trusted ({bad})"
            );
        }
    }

    /// Unknown (no report) is NOT trusted, and is DISTINCT from a healthy report.
    #[test]
    fn test_loc_unknown_not_trusted() {
        assert!(
            !localization_trusted(&LocalizationIntegrity::Unknown, &cfg()),
            "an absent integrity report must not be trusted (absent != healthy)"
        );
        assert_ne!(
            localization_trusted(&LocalizationIntegrity::Unknown, &cfg()),
            localization_trusted(&reported(0.05, 50), &cfg()),
            "Unknown and a healthy report must differ"
        );
    }

    /// A fresh, in-bound report → trusted.
    #[test]
    fn test_loc_healthy_trusted() {
        assert!(localization_trusted(&reported(0.05, 50), &cfg()));
    }

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
