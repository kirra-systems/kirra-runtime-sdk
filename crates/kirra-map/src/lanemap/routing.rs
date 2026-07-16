//! `LaneGraph` — routing & corridor construction methods (de-monolith split of lanemap.rs).
//!
//! Additional `impl LaneGraph` block; behaviour unchanged. Shared internals are
//! `pub(crate)` in the parent module.

use super::*;

impl LaneGraph {
    /// Shortest lane **route** from `from` to `to` (inclusive) over the connectivity
    /// graph: longitudinal **successors** (cost 1) and lateral **neighbors** (cost 3,
    /// so the router prefers driving forward and only changes lanes when the route
    /// requires it). This is the lane *selection* a Lanelet2 routing graph provides —
    /// the sequence whose drivable span [`corridor_over`](Self::corridor_over) /
    /// per-lane [`corridor`](Lane::corridor) then materialize.
    ///
    /// Returns the lane-id sequence, or `None` if either endpoint is unknown, `to` is
    /// unreachable, or the route would exceed [`MAX_ROUTE_LANES`] (fail-closed).
    /// Deterministic — ties are broken by lane id.
    #[must_use]
    pub fn route(&self, from: u64, to: u64) -> Option<Vec<u64>> {
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;

        self.lane(from)?;
        self.lane(to)?;
        if from == to {
            return Some(vec![from]);
        }

        const SUCCESSOR_COST: u32 = 1;
        const LANE_CHANGE_COST: u32 = 3;

        let mut dist: BTreeMap<u64, u32> = BTreeMap::new();
        let mut prev: BTreeMap<u64, u64> = BTreeMap::new();
        let mut heap: BinaryHeap<Reverse<(u32, u64)>> = BinaryHeap::new();
        dist.insert(from, 0);
        heap.push(Reverse((0, from)));

        while let Some(Reverse((cost, lane_id))) = heap.pop() {
            if lane_id == to {
                break;
            }
            if cost > *dist.get(&lane_id).unwrap_or(&u32::MAX) {
                continue; // a stale heap entry superseded by a cheaper path
            }
            let Some(lane) = self.lane(lane_id) else {
                continue;
            };
            let edges: Vec<(u64, u32)> = lane
                .successors()
                .map(|s| (s, SUCCESSOR_COST))
                .chain(lane.left_neighbor().map(|n| (n, LANE_CHANGE_COST)))
                .chain(lane.right_neighbor().map(|n| (n, LANE_CHANGE_COST)))
                .collect();
            for (next, w) in edges {
                if self.lane(next).is_none() {
                    continue; // dangling edge — ignore (fail-closed: never invents a lane)
                }
                let nd = cost.saturating_add(w);
                if nd < *dist.get(&next).unwrap_or(&u32::MAX) {
                    dist.insert(next, nd);
                    prev.insert(next, lane_id);
                    heap.push(Reverse((nd, next)));
                }
            }
        }

        if !dist.contains_key(&to) {
            return None;
        }
        let mut route = vec![to];
        let mut cur = to;
        while cur != from {
            cur = *prev.get(&cur)?;
            route.push(cur);
            if route.len() > MAX_ROUTE_LANES {
                return None; // pathological length → fail closed
            }
        }
        route.reverse();
        Some(route)
    }

    /// Route from `from` to the lane containing world point `goal` (resolved via
    /// [`lane_at`](Self::lane_at)). `None` if the goal is off the mapped road or
    /// unreachable.
    #[must_use]
    pub fn route_to_point(&self, from: u64, goal: Point) -> Option<Vec<u64>> {
        let to = self.lane_at(goal)?;
        self.route(from, to)
    }

    /// The geometric path an object at `start` would trace if it **FOLLOWS its lane** (and the
    /// lowest-id successor chain at each junction) for `length_m` of travel — the **map-intention**
    /// prediction hypothesis. The path begins at the projection of `start` onto the lane
    /// centerline and extends forward along the road. `None` if `start` is off the mapped road.
    ///
    /// This is the third predicted mode (alongside the kinematic CV / CTRV rollouts): an object on
    /// a CURVING lane traces the curve — which a constant-velocity / constant-turn-rate predictor
    /// cannot know — so a vehicle that will follow a bend INTO the ego's path is caught; and one
    /// keeping its own (diverging) lane stays in it, suppressing a spurious cut-in yield. The
    /// planner's predictive yield worst-cases over the modes; KIRRA still backstops.
    ///
    /// Bounded: walks at most [`MAX_ROUTE_LANES`] lanes of successor chain, then truncates once the
    /// accumulated forward length reaches `length_m`. Returns `None` if the result degenerates to
    /// fewer than two vertices.
    #[must_use]
    pub fn lane_follow_path(&self, start: Point, length_m: f64) -> Option<Vec<Point>> {
        let lane_id = self.lane_at(start)?;
        let chain = self.forward_centerline(lane_id);
        if chain.len() < 2 {
            return None;
        }
        let (seg, proj) = project_onto_polyline(&chain, start);
        let mut out = vec![proj];
        let mut acc = 0.0;
        let mut prev = proj;
        for p in chain.iter().skip(seg + 1) {
            acc += (p.x_m - prev.x_m).hypot(p.y_m - prev.y_m);
            out.push(*p);
            prev = *p;
            if acc >= length_m.max(0.0) {
                break;
            }
        }
        (out.len() >= 2).then_some(out)
    }

    /// The centerline of `lane_id` concatenated with the lowest-id successor chain (seam-deduped),
    /// bounded by [`MAX_ROUTE_LANES`] and cycle-guarded — the forward road geometry a lane-following
    /// object would trace through junctions.
    fn forward_centerline(&self, lane_id: u64) -> Vec<Point> {
        let mut chain: Vec<Point> = Vec::new();
        let mut visited: BTreeSet<u64> = BTreeSet::new();
        let mut cur = lane_id;
        while visited.insert(cur) && visited.len() <= MAX_ROUTE_LANES {
            let Some(lane) = self.lanes.get(&cur) else {
                break;
            };
            concat_dedup(&mut chain, &lane.centerline);
            match lane.successors().min() {
                Some(n) if !visited.contains(&n) => cur = n,
                _ => break,
            }
        }
        chain
    }

    /// Derive the drivable [`LaneCorridor`] spanning the given laterally-adjacent
    /// lanes — the **outer envelope**: the left boundary of the leftmost lane and
    /// the right boundary of the rightmost lane. This is what makes a commanded
    /// lane change checkable: the corridor covers the source *and* target lanes, so
    /// the shifted trajectory stays contained. Returns `None` if any id is unknown
    /// or the slice is empty.
    #[must_use]
    pub fn corridor_over(
        &self,
        lane_ids: &[u64],
        confidence: f32,
        age_ms: u64,
    ) -> Option<LaneCorridor> {
        let lanes = self.resolve(lane_ids)?;
        // Leftmost = greatest mean-y; rightmost = least mean-y.
        let leftmost = lanes
            .iter()
            .copied()
            .max_by(|a, b| a.mean_y().total_cmp(&b.mean_y()))?;
        let rightmost = lanes
            .iter()
            .copied()
            .min_by(|a, b| a.mean_y().total_cmp(&b.mean_y()))?;
        Some(LaneCorridor {
            left: offset_polyline(&leftmost.centerline, leftmost.half_width_m),
            right: offset_polyline(&rightmost.centerline, -rightmost.half_width_m),
            confidence,
            age_ms,
        })
    }

    /// Materialize a **route** (a longitudinal lane-id sequence from
    /// [`route`](Self::route)) into one continuous drivable [`LaneCorridor`] that
    /// FOLLOWS the route through any turns — the longitudinal counterpart to
    /// [`corridor_over`](Self::corridor_over) (which spans laterally-adjacent lanes for
    /// a road *cross-section*). Each route lane's centerline is offset to its own
    /// ±`half_width_m`, and the per-lane left/right boundary polylines are concatenated
    /// end-to-end, so the corridor **curves through a junction** exactly as the route
    /// lanes do. This is the handle the planner follows and KIRRA re-reads for an
    /// intersection turn (a route `[ego_lane → junction_lane → exit_lane]`).
    ///
    /// Seam dedup: where one lane's offset boundary ends at the next lane's start (a
    /// shared junction node), the duplicated vertex is dropped so the polyline carries
    /// no zero-length segment. Returns `None` if `route` is empty, any id is unknown, or
    /// the result degenerates to fewer than two vertices a side (fail-closed).
    ///
    /// **Geometry assumption (stated, not hidden):** the route lanes are expected to
    /// connect end-to-end (lane *i*'s centerline terminus ≈ lane *i+1*'s start), as a
    /// Lanelet2 successor chain does, and a junction *turn* lane carries a smooth **arc**
    /// centerline so the offset stays kink-free — a hard L-corner would spike the
    /// implied steering rate, which KIRRA would then (correctly) clamp/refuse. The
    /// materializer trusts the map's geometry: it concatenates, it does not re-fit
    /// corners. Typed lane boundaries along a route (for a mid-turn lane change) are a
    /// tracked follow-up; a turn follows the route centerline and needs only the corridor.
    #[must_use]
    pub fn route_corridor(
        &self,
        route: &[u64],
        confidence: f32,
        age_ms: u64,
    ) -> Option<LaneCorridor> {
        let lanes = self.resolve(route)?;
        let mut left: Vec<Point> = Vec::new();
        let mut right: Vec<Point> = Vec::new();
        for lane in lanes {
            concat_dedup(
                &mut left,
                &offset_polyline(&lane.centerline, lane.half_width_m),
            );
            concat_dedup(
                &mut right,
                &offset_polyline(&lane.centerline, -lane.half_width_m),
            );
        }
        (left.len() >= 2 && right.len() >= 2).then_some(LaneCorridor {
            left,
            right,
            confidence,
            age_ms,
        })
    }

    /// Materialize a **wide** route corridor — the route's lanes PLUS their direct lateral
    /// neighbors — as the *drivable area* a turn may borrow (the longitudinal counterpart to
    /// [`corridor_over`], and the wide sibling of [`route_corridor`]). Per route lane the
    /// outer envelope takes the LEFT edge of its left neighbor (if any, else its own) and the
    /// RIGHT edge of its right neighbor (if any, else its own); the per-lane outer boundaries
    /// are concatenated longitudinally (seam-deduped) so the area follows the route at full
    /// width through any turns.
    ///
    /// This is what lets the planner route-around an obstacle or lane-change ACROSS a
    /// crossable divider *within* a turn: [`route_corridor`] is the reference path (`map`);
    /// this is the `drivable` width; the typed lines come from [`boundaries_relative_to`]
    /// over the same lane + neighbors. Returns `None` if `route` is empty, any id is unknown,
    /// or the result degenerates. **Scope:** one level of lateral neighbor each side (covers
    /// a two-wide turn); the same smooth-arc geometry caveat as `route_corridor` applies.
    #[must_use]
    pub fn route_drivable(
        &self,
        route: &[u64],
        confidence: f32,
        age_ms: u64,
    ) -> Option<LaneCorridor> {
        let lanes = self.resolve(route)?;
        let mut left: Vec<Point> = Vec::new();
        let mut right: Vec<Point> = Vec::new();
        for lane in lanes {
            let left_lane = lane
                .left_neighbor()
                .and_then(|n| self.lanes.get(&n))
                .unwrap_or(lane);
            let right_lane = lane
                .right_neighbor()
                .and_then(|n| self.lanes.get(&n))
                .unwrap_or(lane);
            concat_dedup(
                &mut left,
                &offset_polyline(&left_lane.centerline, left_lane.half_width_m),
            );
            concat_dedup(
                &mut right,
                &offset_polyline(&right_lane.centerline, -right_lane.half_width_m),
            );
        }
        (left.len() >= 2 && right.len() >= 2).then_some(LaneCorridor {
            left,
            right,
            confidence,
            age_ms,
        })
    }

    /// Derive the typed lane boundaries across a span of lanes, expressed as
    /// lateral offsets **relative to `ego_lane`'s centerline** (the frame Occy's
    /// `lane_boundaries` input uses). Boundaries shared between adjacent lanes are
    /// deduplicated by lateral position. Returns `None` if any id is unknown.
    #[must_use]
    pub fn boundaries_relative_to(
        &self,
        ego_lane: u64,
        lane_ids: &[u64],
    ) -> Option<Vec<LaneBoundary>> {
        let ego_y = self.lanes.get(&ego_lane)?.mean_y();
        let lanes = self.resolve(lane_ids)?;
        let mut out: Vec<LaneBoundary> = Vec::new();
        for lane in lanes {
            let c = lane.mean_y() - ego_y;
            for b in [
                LaneBoundary {
                    y_m: c + lane.half_width_m,
                    line: lane.left_line,
                },
                LaneBoundary {
                    y_m: c - lane.half_width_m,
                    line: lane.right_line,
                },
            ] {
                // Dedup a boundary already present at this lateral position (shared
                // divider between two adjacent lanes appears on both).
                if !out.iter().any(|e| (e.y_m - b.y_m).abs() <= 1e-6) {
                    out.push(b);
                }
            }
        }
        Some(out)
    }

    /// Like [`boundaries_relative_to`](Self::boundaries_relative_to), but **curve-correct**: the
    /// lateral offsets are measured in the EGO's Frenet frame at its current station (the
    /// projection of `ego_pos` onto the ego lane's centerline), not from each lane's GLOBAL
    /// `mean_y`. On a straight lane the two agree; on a CURVING lane `mean_y` averages the whole
    /// arc and mis-places a neighbor's boundary relative to where the ego actually is — which can
    /// mis-gate a lateral maneuver (admit crossing a solid line, or block a legal cross). This
    /// measures each boundary's signed perpendicular offset from the ego station along the local
    /// lane normal, so the lane-line crossing rules see the boundary where it really is.
    ///
    /// Returns `None` if any id is unknown or the ego centerline is degenerate. The same
    /// gently-curved / roughly-parallel-lanes assumption as the rest of the lane geometry applies
    /// (the nearest-point projection approximates the Frenet lateral for parallel lanes).
    #[must_use]
    pub fn boundaries_relative_to_at(
        &self,
        ego_lane: u64,
        lane_ids: &[u64],
        ego_pos: Point,
    ) -> Option<Vec<LaneBoundary>> {
        let ego = self.lanes.get(&ego_lane)?;
        if ego.centerline.len() < 2 {
            return None;
        }
        // Ego station: the projection of the ego onto its centerline, and the LEFT normal of the
        // local tangent there (the Frenet lateral axis, +y to the ego's left).
        let (seg, e) = project_onto_polyline(&ego.centerline, ego_pos);
        let a = ego.centerline[seg];
        let b = ego.centerline[seg + 1];
        let (tx, ty) = (b.x_m - a.x_m, b.y_m - a.y_m);
        let tlen = tx.hypot(ty).max(1e-9);
        let (nx, ny) = (-ty / tlen, tx / tlen); // left normal

        let lanes = self.resolve(lane_ids)?;
        let mut out: Vec<LaneBoundary> = Vec::new();
        for lane in lanes {
            for (bd, line) in [
                (
                    offset_polyline(&lane.centerline, lane.half_width_m),
                    lane.left_line,
                ),
                (
                    offset_polyline(&lane.centerline, -lane.half_width_m),
                    lane.right_line,
                ),
            ] {
                if bd.len() < 2 {
                    continue;
                }
                let (_, nearest) = project_onto_polyline(&bd, e);
                // Signed lateral offset of the boundary from the ego station, along the ego normal.
                let y = (nearest.x_m - e.x_m) * nx + (nearest.y_m - e.y_m) * ny;
                if !out.iter().any(|x| (x.y_m - y).abs() <= 1e-6) {
                    out.push(LaneBoundary { y_m: y, line });
                }
            }
        }
        Some(out)
    }

    /// Synthesize a two-lane **undivided** road from a single wide drivable
    /// `corridor` — the unmarked-road / dirt-road case. On a road with no painted
    /// centerline, perception (`kirra_taj`) reports
    /// one wide corridor, and "follow the centerline" would drive the ego **down
    /// the middle** — wrong, and unsafe w.r.t. oncoming traffic. The US rule of the
    /// road (keep **right**, UVC §11-301) still applies even with no paint.
    ///
    /// This applies that rule **structurally**: split the road at its midline into
    /// a right-half **ego** lane and a left-half **oncoming** lane, divided by an
    /// [`LineType::Unmarked`] centerline (crossable — you may use the other half to
    /// pass when clear). The ego then "keeps right" simply by following its
    /// synthesized lane's corridor ([`Lane::corridor`]) — no special biasing logic,
    /// it reuses the same lane-following the marked-road case uses.
    ///
    /// **Honest scope:** the *travel direction* of the oncoming lane (it runs the
    /// opposite way) is not yet encoded — the oncoming lane is marked only
    /// structurally (the left neighbor). Directionality is needed for the head-on
    /// RSS check (the oncoming-traffic collision bound) and lands with it.
    ///
    /// Returns `None` if the corridor boundaries are empty/degenerate or not a
    /// `+y`-left / `-y`-right road.
    #[must_use]
    pub fn from_undivided_corridor(
        corridor: &dyn CorridorSource,
        ego_lane_id: u64,
        oncoming_lane_id: u64,
    ) -> Option<Self> {
        let left = corridor.left_boundary();
        let right = corridor.right_boundary();
        if left.len() < 2 || right.len() < 2 {
            return None;
        }
        let left_y = mean_y_of(left);
        let right_y = mean_y_of(right);
        if left_y <= right_y {
            return None; // not a +y-left / -y-right road
        }
        let (x0, x1) = x_extent(left, right)?;
        let mid = 0.5 * (left_y + right_y);
        let lane_half = 0.25 * (left_y - right_y); // a quarter of the total width
        let ego_c = 0.5 * (mid + right_y); // center of the right half
        let onc_c = 0.5 * (left_y + mid); // center of the left half

        let mut g = LaneGraph::new();
        // Ego (right half): unmarked centerline on the LEFT, road edge on the right.
        g.add_lane(
            Lane::straight(
                ego_lane_id,
                ego_c,
                x0,
                x1,
                lane_half,
                LineType::Unmarked,
                LineType::Solid,
            )
            .with_edge(LaneEdge::LeftNeighbor {
                to: oncoming_lane_id,
            }),
        );
        // Oncoming (left half): road edge on the left, unmarked centerline on the
        // right, and travel OPPOSING the ego (heading π) — the head-on direction.
        g.add_lane(
            Lane::straight(
                oncoming_lane_id,
                onc_c,
                x0,
                x1,
                lane_half,
                LineType::Solid,
                LineType::Unmarked,
            )
            .with_heading(std::f64::consts::PI)
            .with_edge(LaneEdge::RightNeighbor { to: ego_lane_id }),
        );
        Some(g)
    }

    /// Resolve a slice of ids to lane refs, or `None` if any is missing/empty.
    fn resolve(&self, lane_ids: &[u64]) -> Option<Vec<&Lane>> {
        if lane_ids.is_empty() {
            return None;
        }
        lane_ids.iter().map(|id| self.lanes.get(id)).collect()
    }
}
