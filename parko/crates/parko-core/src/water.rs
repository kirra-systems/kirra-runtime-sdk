// parko-core/src/water.rs
//
// SG4 — WATER_UNTRAVERSABLE governor veto (depth-free, bounded-worst-case, #98).
//
// MOTIVATION. The May 2026 Atlanta and April 2026 San Antonio Waymo flood
// incidents: vehicles drove into flooded roadways (one was swept into a creek).
// The mitigations that failed were advisory / fleet-level (weather feeds,
// operational triggers) and were outpaced by the rapid flood. SG4's value is an
// ONBOARD, real-time governor veto that does not wait for a warning.
//
// CHECKER, NOT DRIVER. This is checker-over-doer: the independent governor
// VETOES the clearly-dangerous (unbounded) water signature and stays SILENT on
// bounded puddles — the planner handles normal puddle driving. The primitive
// answers "must the governor veto?", NOT "is this drivable?".
//
// DEPTH IS UNRANGEABLE (`docs/safety/OCCY_INDEPENDENT_DETECTOR.md`: "Water depth
// is unrangeable by anything … it does not measure depth"). This module NEVER
// takes, computes, or claims a depth value. It bounds the WORST CASE
// geometrically — the same philosophy as RSS / occlusion (bound the hazard,
// don't measure it).

/// Explicit evidence that a detected water surface is traversable — the ONLY way
/// to override the untraversable default. It is NEVER inferred from the water
/// signature itself; it must come from a map prior or a human.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraversalEvidence {
    /// The segment is a mapped, known-safe ford / crossing.
    MapKnownSafe,
    /// A human operator explicitly authorized traversal.
    OperatorAuthorized,
}

/// The water descriptor the governor sees this tick, for SG4. Mirrors
/// [`crate::rss::OcclusionScene`]'s ABSENT-vs-KNOWN discipline: a detector that
/// did not run (or is unhealthy) is **not** "clear water".
#[derive(Debug, Clone, Copy)]
pub enum WaterScene {
    /// The water detector ran and found no water in the path → no veto.
    Clear,
    /// No healthy water assessment this tick (detector unhealthy / no update) →
    /// fail-closed VETO. DISTINCT from `Clear`: a detector that did not look is
    /// not "clear water" (the #238 / #122 absent-vs-known trap).
    Unknown,
    /// A water surface is present in the path. Every field bounds the WORST CASE
    /// geometrically — **none is a depth**.
    Detected {
        /// Along-path extent of the water surface (m). Larger ⇒ less bounded.
        extent_m: f64,
        /// Distance to a VISIBLE near dry exit (m), if one is confirmed. `None`
        /// = no bounded exit visible ⇒ unbounded ⇒ veto.
        exit_distance_m: Option<f64>,
        /// A current / flow was detected on the surface — the creek-sweep guard.
        /// HARD GATE: `true` always vetoes (no config can relax it).
        flow_detected: bool,
        /// The road geometry is confirmed intact ABOVE the water (not submerged
        /// / washed out). HARD GATE: `false` always vetoes.
        geometry_confirmed: bool,
    },
    /// An EXPLICIT, evidence-backed override of the untraversable default.
    EarnedTraversable { evidence: TraversalEvidence },
}

/// Thresholds for the bounded-safe conjunction of a `Detected` scene.
///
/// **VALIDATION-PENDING.** These defaults are conservative *placeholders*, NOT
/// track-test / SOTIF-derived certified values — the same honesty as the
/// speed-cap basis (`OCCY_SPEED_CAP_VALIDATION.md`). Smaller thresholds ⇒ more
/// vetoing (more conservative). `flow_detected` and `geometry_confirmed` are
/// HARD GATES in [`water_untraversable_veto`] and are **not** configurable here.
#[derive(Debug, Clone, Copy)]
pub struct WaterVetoConfig {
    /// Max distance to the visible near dry exit for a `Detected` scene to be
    /// eligible for no-veto (m).
    pub max_exit_distance_m: f64,
    /// Max along-path extent for a `Detected` scene to be eligible for no-veto
    /// (m).
    pub max_puddle_extent_m: f64,
}

impl Default for WaterVetoConfig {
    fn default() -> Self {
        // VALIDATION-PENDING conservative placeholders (not certified values):
        // only a short puddle (≤ 3 m) with a near visible dry exit (≤ 5 m) is
        // eligible for no-veto; anything larger fails closed to a veto.
        Self {
            max_exit_distance_m: 5.0,
            max_puddle_extent_m: 3.0,
        }
    }
}

/// SG4 — must the governor VETO this water scene?
///
/// `true`  = veto (WATER_UNTRAVERSABLE; the governor forces a stop short of the
/// water); `false` = no veto (the planner's call — e.g. a bounded puddle).
///
/// Lattice:
///   * `Clear`            → `false` (detector saw no water).
///   * `Unknown`          → `true`  (fail-closed; absent ≠ clear).
///   * `EarnedTraversable`→ `false` (explicit map/operator grant overrides).
///   * `Detected`         → `false` ONLY if the conservative bounded-safe
///     CONJUNCTION holds — a near visible dry exit (`Some(d), d ≤
///     max_exit_distance_m`) AND bounded extent (`≤ max_puddle_extent_m`) AND
///     `geometry_confirmed` AND `!flow_detected`. Any single condition failing
///     → veto. Non-finite / negative geometry fails the `≤` (NaN-safe) → veto.
///
/// No depth is taken or computed anywhere.
// SAFETY: SG4 | REQ: water-untraversable-default | TEST: test_clear_no_veto,test_unknown_fail_closed_veto_not_clear,test_detected_bounded_safe_no_veto,test_detected_no_exit_veto,test_detected_large_extent_veto,test_flow_detected_always_veto,test_geometry_unconfirmed_always_veto,test_earned_traversable_overrides_vetoing_scene,test_exit_distance_boundary_inclusive
// (≅ Occy SG4 / H4 "enters standing water of unverified depth" → stop short of
//  water. Unknown / unbounded signatures fail closed to a veto.)
pub fn water_untraversable_veto(scene: &WaterScene, cfg: &WaterVetoConfig) -> bool {
    match *scene {
        WaterScene::Clear => false,
        WaterScene::Unknown => true,
        WaterScene::EarnedTraversable { .. } => false,
        WaterScene::Detected {
            extent_m,
            exit_distance_m,
            flow_detected,
            geometry_confirmed,
        } => {
            // A near, finite, non-negative, bounded visible dry exit.
            let exit_bounded = match exit_distance_m {
                Some(d) => d.is_finite() && d >= 0.0 && d <= cfg.max_exit_distance_m,
                None => false,
            };
            // Finite, non-negative, bounded along-path extent.
            let extent_bounded =
                extent_m.is_finite() && extent_m >= 0.0 && extent_m <= cfg.max_puddle_extent_m;

            // No-veto ONLY if the WHOLE bounded-safe conjunction holds. The two
            // booleans are HARD GATES (no config can disable them): a current
            // (`flow_detected`) or unconfirmed geometry always vetoes.
            let bounded_safe =
                exit_bounded && extent_bounded && geometry_confirmed && !flow_detected;
            !bounded_safe
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> WaterVetoConfig {
        WaterVetoConfig::default() // max_exit = 5.0, max_extent = 3.0
    }

    /// A bounded-safe `Detected` scene: short puddle, near visible dry exit,
    /// geometry intact, no flow. The shared "puddle the planner can drive".
    fn bounded_safe_puddle() -> WaterScene {
        WaterScene::Detected {
            extent_m: 2.0,
            exit_distance_m: Some(4.0),
            flow_detected: false,
            geometry_confirmed: true,
        }
    }

    #[test]
    fn test_clear_no_veto() {
        assert!(!water_untraversable_veto(&WaterScene::Clear, &cfg()));
    }

    /// Unknown (detector unhealthy / no update) fails closed to a veto — and is
    /// DISTINCT from Clear.
    #[test]
    fn test_unknown_fail_closed_veto_not_clear() {
        assert!(
            water_untraversable_veto(&WaterScene::Unknown, &cfg()),
            "a detector that did not look must NOT be read as clear water"
        );
        assert_ne!(
            water_untraversable_veto(&WaterScene::Unknown, &cfg()),
            water_untraversable_veto(&WaterScene::Clear, &cfg()),
            "Unknown and Clear must produce opposite verdicts"
        );
    }

    /// The bounded puddle is NOT vetoed — proves the governor does not over-stop
    /// for normal rain puddles (the planner proceeds).
    #[test]
    fn test_detected_bounded_safe_no_veto() {
        assert!(!water_untraversable_veto(&bounded_safe_puddle(), &cfg()));
    }

    /// No visible exit (unbounded) → veto.
    #[test]
    fn test_detected_no_exit_veto() {
        let s = WaterScene::Detected {
            extent_m: 2.0,
            exit_distance_m: None,
            flow_detected: false,
            geometry_confirmed: true,
        };
        assert!(
            water_untraversable_veto(&s, &cfg()),
            "no visible dry exit is unbounded → veto"
        );
    }

    /// Extent beyond the bound → veto, even with a near exit / geometry / no flow.
    #[test]
    fn test_detected_large_extent_veto() {
        let s = WaterScene::Detected {
            extent_m: 50.0,
            exit_distance_m: Some(4.0),
            flow_detected: false,
            geometry_confirmed: true,
        };
        assert!(
            water_untraversable_veto(&s, &cfg()),
            "extent beyond max_puddle_extent_m → veto"
        );
    }

    /// flow_detected ALWAYS vetoes — even an otherwise bounded-safe scene (the
    /// creek-sweep guard). No config can relax it.
    #[test]
    fn test_flow_detected_always_veto() {
        let s = WaterScene::Detected {
            extent_m: 2.0,
            exit_distance_m: Some(1.0),
            flow_detected: true,
            geometry_confirmed: true,
        };
        assert!(
            water_untraversable_veto(&s, &cfg()),
            "a detected current must always veto"
        );
    }

    /// geometry_confirmed == false ALWAYS vetoes — even otherwise bounded-safe.
    #[test]
    fn test_geometry_unconfirmed_always_veto() {
        let s = WaterScene::Detected {
            extent_m: 2.0,
            exit_distance_m: Some(1.0),
            flow_detected: false,
            geometry_confirmed: false,
        };
        assert!(
            water_untraversable_veto(&s, &cfg()),
            "unconfirmed road geometry must always veto"
        );
    }

    /// EarnedTraversable overrides an otherwise-vetoing situation (explicit
    /// map/operator grant). Both evidence kinds.
    #[test]
    fn test_earned_traversable_overrides_vetoing_scene() {
        for ev in [
            TraversalEvidence::MapKnownSafe,
            TraversalEvidence::OperatorAuthorized,
        ] {
            assert!(
                !water_untraversable_veto(&WaterScene::EarnedTraversable { evidence: ev }, &cfg()),
                "an explicit ford/operator grant overrides the untraversable default"
            );
        }
    }

    /// Hand-checked boundary at EXACTLY max_exit_distance_m (= 5.0): inclusive
    /// passes; one ulp beyond vetoes.
    #[test]
    fn test_exit_distance_boundary_inclusive() {
        let at = WaterScene::Detected {
            extent_m: 2.0,
            exit_distance_m: Some(5.0),
            flow_detected: false,
            geometry_confirmed: true,
        };
        assert!(
            !water_untraversable_veto(&at, &cfg()),
            "exit exactly at max_exit_distance_m must NOT veto (inclusive)"
        );
        let beyond = WaterScene::Detected {
            extent_m: 2.0,
            exit_distance_m: Some(5.0 + 1e-6),
            flow_detected: false,
            geometry_confirmed: true,
        };
        assert!(
            water_untraversable_veto(&beyond, &cfg()),
            "exit just beyond max_exit_distance_m must veto"
        );
    }

    /// Non-finite / negative geometry fails closed (NaN-safe `≤`).
    #[test]
    fn test_nonfinite_inputs_fail_closed() {
        let nan_exit = WaterScene::Detected {
            extent_m: 2.0,
            exit_distance_m: Some(f64::NAN),
            flow_detected: false,
            geometry_confirmed: true,
        };
        assert!(
            water_untraversable_veto(&nan_exit, &cfg()),
            "NaN exit distance must veto"
        );
        let nan_extent = WaterScene::Detected {
            extent_m: f64::NAN,
            exit_distance_m: Some(1.0),
            flow_detected: false,
            geometry_confirmed: true,
        };
        assert!(
            water_untraversable_veto(&nan_extent, &cfg()),
            "NaN extent must veto"
        );
    }
}
