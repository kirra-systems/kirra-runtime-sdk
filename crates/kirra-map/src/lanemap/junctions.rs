//! `LaneGraph` — right-of-way & junction context methods (de-monolith split of lanemap.rs).
//!
//! Additional `impl LaneGraph` block; behaviour unchanged. Shared internals are
//! `pub(crate)` in the parent module.

use super::*;

impl LaneGraph {
    /// Record that `priority_lane` has right-of-way over `yielding_lane` — traffic in
    /// the yielding lane must cede to traffic in the priority lane at their conflict.
    pub fn add_right_of_way(&mut self, priority_lane: u64, yielding_lane: u64) {
        self.priority_over
            .entry(priority_lane)
            .or_default()
            .insert(yielding_lane);
    }

    /// Builder form of [`add_right_of_way`](Self::add_right_of_way).
    #[must_use]
    pub fn with_right_of_way(mut self, priority_lane: u64, yielding_lane: u64) -> Self {
        self.add_right_of_way(priority_lane, yielding_lane);
        self
    }

    /// **Derive** junction right-of-way from each approach lane's traffic **control**, populating
    /// the same `priority_over` relation [`add_right_of_way`](Self::add_right_of_way) writes — so
    /// the cede list falls out of the map's signs instead of being integrator-supplied (the
    /// roadmap-#4 follow-up: the upstream right-of-way *derivation* from the lane graph + controls).
    ///
    /// Rule (MUTCD / Vienna Convention, the uncontrolled-vs-controlled core): where two **crossing**
    /// approaches share a junction (their termini within [`JUNCTION_CONFLICT_RADIUS_M`], headings at
    /// least [`ROW_CROSSING_MIN_RAD`] apart), an approach carrying a STOP or YIELD control yields to
    /// a conflicting approach with **no** control — the uncontrolled (through) road has priority.
    /// Only this unambiguous case asserts a relation; every ambiguous one is left **unasserted**, so
    /// the ego then yields to that agent and KIRRA's RSS backstops regardless (fail-safe):
    ///
    /// * both uncontrolled, or both controlled (an all-way stop — first-come, not a static relation): none.
    /// * a TRAFFIC LIGHT on either approach: none — its priority is the live signal state each tick,
    ///   not static map structure (handled by the signal path).
    /// * parallel (same / opposing) approaches: none — a following / head-on relation, not RoW.
    ///
    /// Additive and road-correct: it only **adds** priority assertions the rules grant; an
    /// integrator-set relation is kept. Degenerate / non-finite-terminus lanes are skipped.
    // SAFETY: SG5 | REQ: junction-right-of-way-derived-from-controls | TEST: through_road_has_priority_over_a_stop_controlled_side_road,two_uncontrolled_approaches_assert_no_priority,an_all_way_stop_asserts_no_priority,a_traffic_light_defers_to_the_signal_state_not_static_priority,parallel_approaches_get_no_right_of_way,distinct_junctions_do_not_interact,derivation_is_additive_to_a_hand_set_relation,junction_context_falls_out_of_derived_right_of_way
    pub fn derive_right_of_way_from_controls(&mut self) {
        // Snapshot the approach geometry first (immutable borrow), then mutate `priority_over`.
        struct Approach {
            id: u64,
            tx: f64,
            ty: f64,
            heading: f64,
            control: Option<LaneControl>,
        }
        let approaches: Vec<Approach> = self
            .lanes
            .values()
            .filter_map(|l| {
                let t = l.centerline.last()?;
                (t.x_m.is_finite() && t.y_m.is_finite()).then_some(Approach {
                    id: l.id,
                    tx: t.x_m,
                    ty: t.y_m,
                    heading: l.heading_rad,
                    control: l.control,
                })
            })
            .collect();

        let mut grants: Vec<(u64, u64)> = Vec::new(); // (priority_lane, yielding_lane)
        for i in 0..approaches.len() {
            for j in (i + 1)..approaches.len() {
                let (a, b) = (&approaches[i], &approaches[j]);
                // Same junction? (termini within the junction-box radius)
                let (dx, dy) = (a.tx - b.tx, a.ty - b.ty);
                if dx.hypot(dy) > JUNCTION_CONFLICT_RADIUS_M {
                    continue;
                }
                // Crossing? (not parallel same / opposing)
                let dh = wrap_pi(a.heading - b.heading).abs();
                if !(ROW_CROSSING_MIN_RAD..=std::f64::consts::PI - ROW_CROSSING_MIN_RAD)
                    .contains(&dh)
                {
                    continue;
                }
                // Uncontrolled beats stop/yield-controlled. A traffic light (or matching control
                // classes) asserts nothing.
                let give = |c: Option<LaneControl>| {
                    matches!(c, Some(LaneControl::Stop | LaneControl::Yield))
                };
                if a.control.is_none() && give(b.control) {
                    grants.push((a.id, b.id));
                } else if b.control.is_none() && give(a.control) {
                    grants.push((b.id, a.id));
                }
            }
        }
        for (priority, yielding) in grants {
            self.add_right_of_way(priority, yielding);
        }
    }

    /// Builder form of [`derive_right_of_way_from_controls`](Self::derive_right_of_way_from_controls).
    #[must_use]
    pub fn with_derived_right_of_way(mut self) -> Self {
        self.derive_right_of_way_from_controls();
        self
    }

    /// The lanes that must yield to `priority_lane` (empty if it has no asserted priority).
    pub fn lanes_yielding_to(&self, priority_lane: u64) -> impl Iterator<Item = u64> + '_ {
        self.priority_over
            .get(&priority_lane)
            .into_iter()
            .flatten()
            .copied()
    }

    /// Derive the **`cedes_to_ego_ids`** list for an ego in `ego_lane`: every perceived
    /// object currently in a lane that yields to the ego's lane (per the map's
    /// right-of-way). This closes the gap where `cedes_to_ego_ids` was
    /// integrator-supplied — it now falls out of the Lanelet2 right-of-way relations.
    /// An object off the mapped road, or in a non-yielding lane, is **not** included
    /// (fail-safe: the ego asserts priority only where the map grants it; KIRRA still
    /// backstops every crossing agent regardless).
    #[must_use]
    pub fn cedes_to_ego(&self, ego_lane: u64, objects: &[PerceivedObject]) -> Vec<u64> {
        let Some(yielding) = self.priority_over.get(&ego_lane) else {
            return Vec::new();
        };
        objects
            .iter()
            .filter(|o| self.lane_at(o.pos).is_some_and(|l| yielding.contains(&l)))
            .map(|o| o.id)
            .collect()
    }

    /// The lanes that have right-of-way **over** `ego_lane` — i.e. the lanes the ego
    /// must yield to. The inverse of [`lanes_yielding_to`](Self::lanes_yielding_to),
    /// read from the **same** `priority_over` map: lane `p` is returned iff
    /// `ego_lane ∈ priority_over[p]`.
    pub fn lanes_with_priority_over(&self, ego_lane: u64) -> impl Iterator<Item = u64> + '_ {
        self.priority_over
            .iter()
            .filter(move |(_, yields)| yields.contains(&ego_lane))
            .map(|(p, _)| *p)
    }

    /// The counterpart to [`cedes_to_ego`](Self::cedes_to_ego): the perceived objects
    /// the ego must **yield to / wait for** at a junction — those in a lane that has
    /// right-of-way over `ego_lane`. This is the map-derived **non-yielding** set
    /// (Parko's SG5 `NonYieldingScene` / the Occy junction-negotiation "still yield to
    /// everyone not on the cede list"), produced from the *same* `priority_over`
    /// relation as `cedes_to_ego` — so the two are **consistent by construction**: no
    /// agent can be both "cedes to me" and "I yield to it" unless the map itself
    /// asserts mutual priority (a map error). Fail-safe: an off-map object is excluded;
    /// KIRRA backstops every crossing agent regardless.
    ///
    /// Cross-stack note: Parko (a separate workspace) owns the runtime
    /// `NonYieldingScene` / `CommitZoneMap` veto; this method is the map-side *source*
    /// either path consumes, mapped to that path's types at the deployment boundary.
    #[must_use]
    pub fn non_yielding_to_ego(&self, ego_lane: u64, objects: &[PerceivedObject]) -> Vec<u64> {
        let priority_lanes: BTreeSet<u64> = self.lanes_with_priority_over(ego_lane).collect();
        if priority_lanes.is_empty() {
            return Vec::new();
        }
        objects
            .iter()
            .filter(|o| {
                self.lane_at(o.pos)
                    .is_some_and(|l| priority_lanes.contains(&l))
            })
            .map(|o| o.id)
            .collect()
    }

    /// **The junction integration entry point.** Resolve the ego's lane from its
    /// *pose* (the input a deployment actually has — not a lane id) and derive BOTH
    /// consumer-ready sets from the one right-of-way map, in a single pass:
    ///
    /// * `cedes_to_ego` → drop straight into Occy's `PlanInput.cedes_to_ego_ids`;
    /// * `must_yield_to` → map to Parko SG5's `NonYieldingScene` at the parko boundary.
    ///
    /// This is the deployment-integration step packaged: it turns *ego pose + perceived
    /// objects* into the two junction sets either path consumes, consistent by
    /// construction (both from `priority_over`). Fail-safe: an ego off the mapped road
    /// yields empty sets (no `ego_lane`) — the path's own gates (Occy's snapshot yield,
    /// Parko's fail-closed veto, KIRRA's RSS) still apply.
    #[must_use]
    pub fn junction_context(&self, ego_pos: Point, objects: &[PerceivedObject]) -> JunctionContext {
        match self.lane_at(ego_pos) {
            Some(ego_lane) => JunctionContext {
                ego_lane: Some(ego_lane),
                cedes_to_ego: self.cedes_to_ego(ego_lane, objects),
                must_yield_to: self.non_yielding_to_ego(ego_lane, objects),
            },
            None => JunctionContext::default(),
        }
    }
}
