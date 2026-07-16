//! `LaneGraph` — occlusion / junction sight-distance methods (de-monolith split of lanemap.rs).
//!
//! Additional `impl LaneGraph` block; behaviour unchanged. Shared internals are
//! `pub(crate)` in the parent module.

use super::*;

impl LaneGraph {
    /// Record that approach `lane` has limited cross-traffic visibility — `sight_distance_m` of
    /// assured-clear sight toward the junction conflict (a blind corner). Drives the occluded-
    /// approach speed cap. A non-finite / negative distance is ignored (fail-safe: treated as no
    /// occlusion datum rather than a spurious 0-speed creep).
    pub fn set_occluded_approach(&mut self, lane: u64, sight_distance_m: f64) {
        if sight_distance_m.is_finite() && sight_distance_m >= 0.0 {
            self.occlusion_sight.insert(lane, sight_distance_m);
        }
    }

    /// Builder form of [`set_occluded_approach`](Self::set_occluded_approach).
    #[must_use]
    pub fn with_occluded_approach(mut self, lane: u64, sight_distance_m: f64) -> Self {
        self.set_occluded_approach(lane, sight_distance_m);
        self
    }

    /// The assured-clear sight distance (m) toward the junction conflict for `lane`, or `None`
    /// if the approach has an open view (no occlusion datum). The source the occluded-approach
    /// speed cap is derived from.
    #[must_use]
    pub fn sight_distance(&self, lane: u64) -> Option<f64> {
        self.occlusion_sight.get(&lane).copied()
    }

    /// **Derive** each approach lane's assured-clear sight distance from occluder geometry,
    /// populating the same `occlusion_sight` map [`set_occluded_approach`](Self::set_occluded_approach)
    /// writes — so the occluded-junction speed cap binds from the **map's footprints** instead of
    /// the hand-fed [`with_occluded_approach`](Self::with_occluded_approach) datum (the honest-scope
    /// follow-up named in ADR-0016).
    ///
    /// For every lane that advances along +x toward its terminus (the conflict line — the
    /// straight-approach frame the junction model already uses), the worst (minimum) sight over all
    /// [`corner_sight_distance`] candidates is recorded. A lane no occluder shadows is left with an
    /// open view (no cap). Fail-safe and additive: non-finite footprints are dropped, a −x /
    /// degenerate lane is skipped, and an existing hand-set datum is only **tightened** (the
    /// derivation never relaxes a stricter integrator value).
    // SAFETY: SG1 SG9 | REQ: occlusion-sight-derived-from-geometry | TEST: derived_sight_is_the_gap_from_the_corner_building_to_the_conflict_line,a_building_closer_to_the_junction_yields_less_sight,a_building_reaching_the_conflict_line_is_fully_blind,no_occluder_leaves_an_open_view,an_in_lane_or_far_lateral_box_does_not_bound_sight,the_worst_of_two_corners_wins,derivation_only_tightens_an_existing_datum_and_fails_safe,occlusion_creep_is_driven_by_map_occluder_geometry_not_a_hand_fed_datum
    pub fn derive_occluded_approaches(&mut self, occluders: &[Occluder]) {
        // Collect first (immutable borrow of lanes) then write (mutable borrow of the map).
        let mut derived: Vec<(u64, f64)> = Vec::new();
        for lane in self.lanes.values() {
            let (Some(first), Some(last)) = (lane.centerline.first(), lane.centerline.last())
            else {
                continue; // degenerate lane — no approach geometry
            };
            if last.x_m <= first.x_m {
                continue; // not a +x approach (oncoming / vertical) — outside the model, skip
            }
            let conflict_x = lane.stop_line_x();
            let lane_y = last.y_m;
            let sight = occluders
                .iter()
                .filter_map(|occ| corner_sight_distance(occ, conflict_x, lane_y, lane.half_width_m))
                .fold(f64::INFINITY, f64::min);
            if sight.is_finite() {
                derived.push((lane.id, sight));
            }
        }
        for (id, sight) in derived {
            // Tighten-only: keep the stricter of an existing datum and the derived one.
            let tightened = self
                .occlusion_sight
                .get(&id)
                .map_or(sight, |&prev| prev.min(sight));
            self.set_occluded_approach(id, tightened);
        }
    }

    /// Builder form of [`derive_occluded_approaches`](Self::derive_occluded_approaches).
    #[must_use]
    pub fn with_derived_occlusion(mut self, occluders: &[Occluder]) -> Self {
        self.derive_occluded_approaches(occluders);
        self
    }
}
