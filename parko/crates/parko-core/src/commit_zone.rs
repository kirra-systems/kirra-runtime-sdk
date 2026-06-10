// parko-core/src/commit_zone.rs
//
// SG5 — map-anchored COMMIT_ZONE_BLOCKED veto (foundation brick under EPIC #106).
//
// SG5 (OCCY_SAFETY_GOALS.md H5, ASIL B): the system shall NOT enter a
// high-consequence commit zone (rail crossing, box junction, narrow bridge)
// without confirmed clearance and a verified exit, and shall not stop within
// one. Safe state: STOP SHORT of the zone, rejecting ≥ ~94 m ahead at the cap.
//
// LOAD-BEARING ROBUSTNESS PROPERTY — "Reject fires from MAP ALONE": a KNOWN zone
// with degraded/absent inputs blocks WITHOUT needing live perception of the
// hazard. The veto is anchored on a map prior, so a perception miss of the
// crossing/junction cannot defeat it.
//
// LAYERING: this brick is the zone model + map-anchored fail-closed gate + entry
// veto over SUPPLIED clearance/exit signals. `clearance_confirmed` and
// `exit_verified` are INPUTS here (synthetic in tests). #107 derives
// exit-clearance from geometry/kinematics (and adds stop-inside-zone
// prevention); #108 derives train / non-yielding-agent conflict. Both replace
// the supplied booleans with computed logic ON TOP of this foundation.
//
// The INPUT health-gating mirrors the gateway SG2 containment `Corridor`
// (confidence / staleness / finiteness → fail-closed degraded). The VERDICT
// lives in parko's own vocabulary (like water / occlusion / impact) — the
// gateway `DenyCode` enum is inside the FROZEN talisman and is never touched.

/// A map prior describing a commit zone on the ego path. The health fields
/// mirror the SG2 containment `Corridor`: an absent / stale / low-confidence /
/// non-finite map is treated as UNHEALTHY (fail-closed), never as "clear".
#[derive(Debug, Clone, Copy)]
pub struct CommitZoneMap {
    /// A mapped commit zone intersects the ego path within the look-ahead
    /// horizon (a MAP prior, not a perception of the hazard).
    pub zone_ahead: bool,
    /// Distance along the ego path to the zone entry (m). Non-finite → veto.
    pub distance_to_zone_m: f64,
    /// Source confidence in `[0.0, 1.0]`. Below `min_confidence` → unhealthy.
    pub confidence: f32,
    /// Age (ms) of the map snapshot vs now. Above `max_age_ms` → unhealthy.
    pub age_ms: u64,
    /// Minimum acceptable confidence for the map to be considered healthy.
    pub min_confidence: f32,
    /// Maximum acceptable staleness (ms) — tied to the per-cycle FTTI.
    pub max_age_ms: u64,
}

impl CommitZoneMap {
    /// True iff the map prior is present, fresh, plausible, and finite —
    /// matching `Corridor::is_healthy`'s conservative semantics. Failure → the
    /// commit-zone gate fails closed (veto).
    pub fn is_healthy(&self) -> bool {
        self.confidence >= self.min_confidence
            && self.age_ms <= self.max_age_ms
            && self.confidence.is_finite()
            && self.distance_to_zone_m.is_finite()
    }
}

/// The commit-zone scene the governor sees this tick. Mirrors the established
/// ABSENT-vs-KNOWN discipline (cf. `WaterScene`, `OcclusionScene`): an absent
/// map source is NOT "no zone".
#[derive(Debug, Clone, Copy)]
pub enum CommitZoneScene {
    /// A healthy map reports no commit zone on the path → no veto.
    NoZone,
    /// A mapped commit zone is ahead, with the (supplied) clearance / exit
    /// confirmations for it. Entry requires BOTH on a HEALTHY map. Additionally
    /// the proposed plan must not STOP within the zone (SG5 clause 3).
    ZoneAhead {
        map: CommitZoneMap,
        /// Clearance into the zone is confirmed (no conflicting traffic / train).
        /// (#108 derives this from non-yielding-agent arrival.)
        clearance_confirmed: bool,
        /// A clear exit beyond the zone is verified (won't get stuck inside).
        /// This boolean is now DERIVED by [`exit_clearance_verified`] (the
        /// box-junction receiving-space rule); it stays a field so #108 still
        /// composes through it.
        exit_verified: bool,
        /// Along-path length of the commit zone (m). Used by the stop-inside
        /// clause. Non-finite → veto (when a stop is proposed).
        zone_length_m: f64,
        /// The proposal's implied stop point, as a distance ahead of ego (m).
        /// `None` = the proposal does not plan a stop within the horizon (the
        /// stop-inside clause is then inert).
        proposed_stop_distance_m: Option<f64>,
    },
    /// The map source is absent / unhealthy this tick → fail-closed VETO.
    /// DISTINCT from `NoZone`: an absent map is not "no zone" (the #238 trap and
    /// the literal "Reject fires from map alone" requirement).
    Unknown,
}

/// Config for the commit-zone gate. `look_ahead_m` is a PARAMETER with a
/// conservative VALIDATION-PENDING default tied to the SG5 ≈ 94 m basis
/// (SSD = v·t_react + v²/2a at the 22.35 m/s cap) — NOT a certified constant
/// (same honesty as #98's water thresholds). It derates with the cap under
/// degraded conditions (handled upstream).
#[derive(Debug, Clone, Copy)]
pub struct CommitZoneCfg {
    /// Actionable look-ahead (m): a zone farther than this is not yet a decision.
    pub look_ahead_m: f64,
    /// Ego vehicle length (m). parko has NO vehicle model (that lives in the
    /// frozen gateway contract), so this is config. VALIDATION-PENDING — a
    /// conservative placeholder, not a certified value.
    pub vehicle_length_m: f64,
    /// Extra receiving-space margin (m) required beyond the vehicle length for a
    /// downstream exit to count as verified. VALIDATION-PENDING.
    pub exit_margin_m: f64,
}

impl Default for CommitZoneCfg {
    fn default() -> Self {
        // VALIDATION-PENDING placeholders (not certified values):
        Self {
            look_ahead_m: 94.0,    // SG5 / SG4 ≈ 94 m look-ahead basis
            vehicle_length_m: 4.5, // a passenger-vehicle-class default
            exit_margin_m: 1.0,    // a small standoff beyond the vehicle length
        }
    }
}

/// Evidence for the box-junction / queue-spillback exit-clearance rule.
/// Synthetic in tests; perception/map ingestion of the receiving space is
/// DEFERRED (as with the agent-set / water / occlusion ingestion).
#[derive(Debug, Clone, Copy)]
pub struct ExitClearanceEvidence {
    /// Measured clear receiving space (m) beyond the zone's FAR edge — the room
    /// to fully exit without stopping inside (the box-junction measure).
    pub downstream_clear_m: f64,
}

/// SG5 — derive `exit_verified`: is there enough downstream receiving space to
/// fully clear the zone (no spillback / no stuck-inside)?
///
/// `true` iff `downstream_clear_m` is finite AND ≥ `vehicle_length_m +
/// exit_margin_m`. A non-finite measurement is fail-closed (an unverifiable exit
/// is NO exit). Callers use this to POPULATE `ZoneAhead.exit_verified` — the
/// boolean is now DERIVED, not asserted.
// SAFETY: SG5 | REQ: commit-zone-exit-clearance | TEST: test_exit_clearance_queue_spillback_blocks,test_exit_clearance_ample_space_verified,test_exit_clearance_nonfinite_not_verified,test_exit_clearance_boundary
pub fn exit_clearance_verified(evidence: &ExitClearanceEvidence, cfg: &CommitZoneCfg) -> bool {
    evidence.downstream_clear_m.is_finite()
        && evidence.downstream_clear_m >= cfg.vehicle_length_m + cfg.exit_margin_m
}

/// SG5 — must the governor BLOCK entry to this commit zone (stop short)?
///
/// `true`  = veto (COMMIT_ZONE_BLOCKED; the governor stops short of the zone);
/// `false` = no veto (the planner proceeds — no zone, or entry is permitted).
///
/// Lattice:
///   * `NoZone`   → `false` (healthy map, no zone).
///   * `Unknown`  → `true`  (fail-closed; absent map ≠ no zone — Reject from map alone).
///   * `ZoneAhead`→ a non-finite distance vetoes. The STOP-INSIDE clause (SG5
///     "shall not stop within one") vetoes — REGARDLESS of clearance/exit and of
///     the horizon — when a proposed stop falls within the zone interval
///     `[distance_to_zone_m, distance_to_zone_m + zone_length_m]` (inclusive;
///     non-finite `zone_length_m` / `d_stop` → veto). A stop SHORT of the zone is
///     the safe state and never vetoes. Otherwise: a zone BEYOND the look-ahead
///     horizon is not yet actionable (no veto); a zone WITHIN the horizon is
///     blocked UNLESS the map is HEALTHY **and** `clearance_confirmed` **and**
///     `exit_verified`. (Health gates the confirmations.)
// SAFETY: SG5 | REQ: commit-zone-map-anchored-block,commit-zone-stop-inside | TEST: test_map_prior_perception_miss_unknown_vetoes,test_gate_down_clearance_unconfirmed_vetoes,test_no_verified_exit_vetoes,test_both_confirmed_healthy_no_veto,test_unhealthy_map_with_confirmations_still_vetoes,test_no_zone_distinct_from_unknown,test_nonfinite_distance_vetoes,test_horizon_boundary,test_beyond_horizon_no_veto,test_stop_inside_vetoes_despite_confirmations,test_stop_short_of_zone_no_veto,test_stop_beyond_far_edge_no_veto,test_stop_inside_interval_boundaries,test_stop_none_clause_inert,test_nonfinite_zone_length_or_stop_vetoes
pub fn commit_zone_blocked(scene: &CommitZoneScene, cfg: &CommitZoneCfg) -> bool {
    match *scene {
        CommitZoneScene::NoZone => false,
        CommitZoneScene::Unknown => true,
        CommitZoneScene::ZoneAhead {
            map,
            clearance_confirmed,
            exit_verified,
            zone_length_m,
            proposed_stop_distance_m,
        } => {
            // A non-finite distance can never be trusted as "beyond horizon".
            if !map.distance_to_zone_m.is_finite() {
                return true;
            }

            // STOP-INSIDE clause (SG5 "shall not stop within one") — independent
            // of clearance/exit AND of the horizon: a plan that stops inside the
            // zone interval is a violation. Checked before the horizon gate so a
            // planned stop inside even a far zone is rejected now.
            if let Some(d_stop) = proposed_stop_distance_m {
                if !zone_length_m.is_finite() || !d_stop.is_finite() {
                    return true; // NaN discipline — fail closed
                }
                let zone_start = map.distance_to_zone_m;
                let zone_end = map.distance_to_zone_m + zone_length_m;
                // Inclusive bounds — stopping exactly on either edge is stopping
                // in the zone. A stop SHORT (d_stop < zone_start) is the safe
                // state and must NOT veto.
                if d_stop >= zone_start && d_stop <= zone_end {
                    return true;
                }
            }

            // Beyond the actionable horizon → not yet a decision (no veto).
            if map.distance_to_zone_m > cfg.look_ahead_m {
                return false;
            }
            // Within horizon: entry requires a HEALTHY map AND both confirmations.
            !(map.is_healthy() && clearance_confirmed && exit_verified)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy_map(distance_m: f64) -> CommitZoneMap {
        CommitZoneMap {
            zone_ahead: true,
            distance_to_zone_m: distance_m,
            confidence: 0.95,
            age_ms: 50,
            min_confidence: 0.5,
            max_age_ms: 1_000,
        }
    }

    fn cfg() -> CommitZoneCfg {
        CommitZoneCfg::default() // look_ahead = 94.0 m
    }

    /// Confirmed entry on a healthy map within the horizon → permitted.
    /// Benign stop-inside inputs (`None`) so the stop clause is inert here.
    fn confirmed_zone(distance_m: f64) -> CommitZoneScene {
        CommitZoneScene::ZoneAhead {
            map: healthy_map(distance_m),
            clearance_confirmed: true,
            exit_verified: true,
            zone_length_m: 30.0,
            proposed_stop_distance_m: None,
        }
    }

    /// "Reject fires from MAP ALONE": an absent / unhealthy map source vetoes —
    /// no live perception of the crossing needed.
    #[test]
    fn test_map_prior_perception_miss_unknown_vetoes() {
        assert!(commit_zone_blocked(&CommitZoneScene::Unknown, &cfg()),
            "an absent/unhealthy map must veto (Reject from map alone)");
    }

    /// Gate-down: clearance not confirmed → veto, even with a verified exit.
    #[test]
    fn test_gate_down_clearance_unconfirmed_vetoes() {
        let s = CommitZoneScene::ZoneAhead {
            map: healthy_map(50.0), clearance_confirmed: false, exit_verified: true,
            zone_length_m: 30.0, proposed_stop_distance_m: None,
        };
        assert!(commit_zone_blocked(&s, &cfg()), "unconfirmed clearance must veto");
    }

    /// No verified exit → veto (the no-stuck-inside guard at entry).
    #[test]
    fn test_no_verified_exit_vetoes() {
        let s = CommitZoneScene::ZoneAhead {
            map: healthy_map(50.0), clearance_confirmed: true, exit_verified: false,
            zone_length_m: 30.0, proposed_stop_distance_m: None,
        };
        assert!(commit_zone_blocked(&s, &cfg()), "no verified exit must veto");
    }

    /// Both confirmed on a healthy map within horizon → NO veto (no over-block).
    #[test]
    fn test_both_confirmed_healthy_no_veto() {
        assert!(!commit_zone_blocked(&confirmed_zone(50.0), &cfg()),
            "a healthy, clearance-confirmed, exit-verified zone permits entry");
    }

    /// Health gates the confirmations: a degraded map with BOTH confirmations
    /// STILL vetoes (a degraded map cannot earn entry).
    #[test]
    fn test_unhealthy_map_with_confirmations_still_vetoes() {
        // low confidence
        let low_conf = CommitZoneScene::ZoneAhead {
            map: CommitZoneMap { confidence: 0.1, ..healthy_map(50.0) },
            clearance_confirmed: true, exit_verified: true,
            zone_length_m: 30.0, proposed_stop_distance_m: None,
        };
        assert!(commit_zone_blocked(&low_conf, &cfg()), "low-confidence map must veto despite confirmations");
        // stale
        let stale = CommitZoneScene::ZoneAhead {
            map: CommitZoneMap { age_ms: 999_999, ..healthy_map(50.0) },
            clearance_confirmed: true, exit_verified: true,
            zone_length_m: 30.0, proposed_stop_distance_m: None,
        };
        assert!(commit_zone_blocked(&stale, &cfg()), "stale map must veto despite confirmations");
    }

    /// NoZone and Unknown are DISTINCT outcomes.
    #[test]
    fn test_no_zone_distinct_from_unknown() {
        assert!(!commit_zone_blocked(&CommitZoneScene::NoZone, &cfg()));
        assert!(commit_zone_blocked(&CommitZoneScene::Unknown, &cfg()));
        assert_ne!(
            commit_zone_blocked(&CommitZoneScene::NoZone, &cfg()),
            commit_zone_blocked(&CommitZoneScene::Unknown, &cfg()),
            "NoZone (healthy, clear) and Unknown (absent map) must differ"
        );
    }

    /// A non-finite distance vetoes (NaN discipline, as in #98/#102).
    #[test]
    fn test_nonfinite_distance_vetoes() {
        for bad in [f64::NAN, f64::INFINITY] {
            let s = confirmed_zone(bad);
            assert!(commit_zone_blocked(&s, &cfg()), "non-finite distance must veto ({bad})");
        }
    }

    /// Hand-checked horizon boundary (look_ahead = 94.0): a confirmed zone
    /// EXACTLY at the horizon is within (decision made → permitted because
    /// confirmed); the SAME distance unconfirmed vetoes; just beyond is not yet
    /// actionable.
    #[test]
    fn test_horizon_boundary() {
        // exactly at horizon, confirmed → within horizon, permitted (no veto).
        assert!(!commit_zone_blocked(&confirmed_zone(94.0), &cfg()),
            "a confirmed zone exactly at the horizon is actionable and permitted");
        // exactly at horizon, unconfirmed → within horizon → veto.
        let at_unconfirmed = CommitZoneScene::ZoneAhead {
            map: healthy_map(94.0), clearance_confirmed: false, exit_verified: true,
            zone_length_m: 30.0, proposed_stop_distance_m: None,
        };
        assert!(commit_zone_blocked(&at_unconfirmed, &cfg()),
            "an unconfirmed zone exactly at the horizon must veto (within horizon)");
    }

    /// A zone just beyond the horizon is not yet a decision (no veto), even
    /// unconfirmed.
    #[test]
    fn test_beyond_horizon_no_veto() {
        let beyond = CommitZoneScene::ZoneAhead {
            map: healthy_map(94.0 + 1e-6), clearance_confirmed: false, exit_verified: false,
            zone_length_m: 30.0, proposed_stop_distance_m: None,
        };
        assert!(!commit_zone_blocked(&beyond, &cfg()),
            "a zone beyond the look-ahead horizon is not yet actionable");
    }

    // ───────────────────────── #107 exit-clearance derivation ──────────────

    /// Queue spillback: too little downstream receiving space → exit NOT verified
    /// (the box-junction rule rejects entry that would strand the ego inside).
    #[test]
    fn test_exit_clearance_queue_spillback_blocks() {
        let c = cfg(); // needs >= 4.5 + 1.0 = 5.5 m
        let ev = ExitClearanceEvidence { downstream_clear_m: 3.0 };
        assert!(!exit_clearance_verified(&ev, &c),
            "insufficient downstream space must NOT verify the exit");
    }

    /// Ample receiving space → exit verified.
    #[test]
    fn test_exit_clearance_ample_space_verified() {
        let c = cfg();
        let ev = ExitClearanceEvidence { downstream_clear_m: 20.0 };
        assert!(exit_clearance_verified(&ev, &c),
            "ample downstream space must verify the exit");
    }

    /// Non-finite measurement is fail-closed (an unverifiable exit is NO exit).
    #[test]
    fn test_exit_clearance_nonfinite_not_verified() {
        let c = cfg();
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let ev = ExitClearanceEvidence { downstream_clear_m: bad };
            assert!(!exit_clearance_verified(&ev, &c),
                "non-finite downstream space must NOT verify ({bad})");
        }
    }

    /// Boundary: exactly `vehicle_length_m + exit_margin_m` (5.5 m) verifies;
    /// one ULP short does not.
    #[test]
    fn test_exit_clearance_boundary() {
        let c = cfg();
        let threshold = c.vehicle_length_m + c.exit_margin_m; // 5.5
        let at = ExitClearanceEvidence { downstream_clear_m: threshold };
        assert!(exit_clearance_verified(&at, &c), "exactly at threshold verifies");
        let below = ExitClearanceEvidence { downstream_clear_m: threshold - 1e-9 };
        assert!(!exit_clearance_verified(&below, &c), "just below threshold does not verify");
    }

    // ───────────────────────── #107 stop-inside clause ─────────────────────

    /// Confirmed, healthy, in-horizon zone — but the plan STOPS inside it →
    /// veto regardless of clearance/exit (SG5 "shall not stop within one").
    #[test]
    fn test_stop_inside_vetoes_despite_confirmations() {
        // zone [50, 80]; stop at 65 is inside.
        let s = CommitZoneScene::ZoneAhead {
            map: healthy_map(50.0), clearance_confirmed: true, exit_verified: true,
            zone_length_m: 30.0, proposed_stop_distance_m: Some(65.0),
        };
        assert!(commit_zone_blocked(&s, &cfg()),
            "a stop inside the zone must veto even with both confirmations");
    }

    /// A stop SHORT of the zone is the safe state → no veto (with entry confirmed).
    #[test]
    fn test_stop_short_of_zone_no_veto() {
        // zone [50, 80]; stop at 40 is short.
        let s = CommitZoneScene::ZoneAhead {
            map: healthy_map(50.0), clearance_confirmed: true, exit_verified: true,
            zone_length_m: 30.0, proposed_stop_distance_m: Some(40.0),
        };
        assert!(!commit_zone_blocked(&s, &cfg()),
            "a stop short of the zone is the safe state and must not veto");
    }

    /// A stop BEYOND the far edge (fully clears) → no veto (with entry confirmed).
    #[test]
    fn test_stop_beyond_far_edge_no_veto() {
        // zone [50, 80]; stop at 90 is beyond.
        let s = CommitZoneScene::ZoneAhead {
            map: healthy_map(50.0), clearance_confirmed: true, exit_verified: true,
            zone_length_m: 30.0, proposed_stop_distance_m: Some(90.0),
        };
        assert!(!commit_zone_blocked(&s, &cfg()),
            "a stop beyond the far edge fully clears and must not veto");
    }

    /// Inclusive interval: stopping EXACTLY on either edge is stopping in the zone.
    #[test]
    fn test_stop_inside_interval_boundaries() {
        // near edge (zone_start = 50)
        let at_start = CommitZoneScene::ZoneAhead {
            map: healthy_map(50.0), clearance_confirmed: true, exit_verified: true,
            zone_length_m: 30.0, proposed_stop_distance_m: Some(50.0),
        };
        assert!(commit_zone_blocked(&at_start, &cfg()),
            "a stop exactly on the near edge is stopping in the zone");
        // far edge (zone_end = 80)
        let at_end = CommitZoneScene::ZoneAhead {
            map: healthy_map(50.0), clearance_confirmed: true, exit_verified: true,
            zone_length_m: 30.0, proposed_stop_distance_m: Some(80.0),
        };
        assert!(commit_zone_blocked(&at_end, &cfg()),
            "a stop exactly on the far edge is stopping in the zone");
    }

    /// `None` proposed stop → the stop-inside clause is inert (no veto on its
    /// account); entry still permitted when confirmed/healthy.
    #[test]
    fn test_stop_none_clause_inert() {
        let s = CommitZoneScene::ZoneAhead {
            map: healthy_map(50.0), clearance_confirmed: true, exit_verified: true,
            zone_length_m: 30.0, proposed_stop_distance_m: None,
        };
        assert!(!commit_zone_blocked(&s, &cfg()),
            "no proposed stop → the stop-inside clause must not veto");
    }

    /// The stop-inside clause vetoes even for a zone BEYOND the horizon: a planned
    /// stop inside a far zone is still a violation (clause precedes the horizon
    /// gate).
    #[test]
    fn test_stop_inside_vetoes_beyond_horizon() {
        // zone start 200 (> 94 horizon), length 30 → [200, 230]; stop at 210 inside.
        let s = CommitZoneScene::ZoneAhead {
            map: healthy_map(200.0), clearance_confirmed: true, exit_verified: true,
            zone_length_m: 30.0, proposed_stop_distance_m: Some(210.0),
        };
        assert!(commit_zone_blocked(&s, &cfg()),
            "a planned stop inside a far zone must still veto (clause precedes horizon)");
    }

    /// NaN discipline on the stop-inside inputs: a non-finite `zone_length_m` or
    /// `proposed_stop_distance_m` fails closed.
    #[test]
    fn test_nonfinite_zone_length_or_stop_vetoes() {
        for bad in [f64::NAN, f64::INFINITY] {
            let bad_len = CommitZoneScene::ZoneAhead {
                map: healthy_map(50.0), clearance_confirmed: true, exit_verified: true,
                zone_length_m: bad, proposed_stop_distance_m: Some(60.0),
            };
            assert!(commit_zone_blocked(&bad_len, &cfg()),
                "non-finite zone_length_m with a proposed stop must veto ({bad})");
            let bad_stop = CommitZoneScene::ZoneAhead {
                map: healthy_map(50.0), clearance_confirmed: true, exit_verified: true,
                zone_length_m: 30.0, proposed_stop_distance_m: Some(bad),
            };
            assert!(commit_zone_blocked(&bad_stop, &cfg()),
                "non-finite proposed stop must veto ({bad})");
        }
    }

    /// End-to-end: a DERIVED unverified exit (queue spillback) feeds the gate and
    /// vetoes entry — the boolean is computed, not asserted.
    #[test]
    fn test_derived_exit_clearance_feeds_gate() {
        let c = cfg();
        let exit = exit_clearance_verified(
            &ExitClearanceEvidence { downstream_clear_m: 2.0 }, &c); // < 5.5 → false
        let s = CommitZoneScene::ZoneAhead {
            map: healthy_map(50.0), clearance_confirmed: true, exit_verified: exit,
            zone_length_m: 30.0, proposed_stop_distance_m: None,
        };
        assert!(commit_zone_blocked(&s, &c),
            "a derived unverified exit must veto entry");
    }
}
