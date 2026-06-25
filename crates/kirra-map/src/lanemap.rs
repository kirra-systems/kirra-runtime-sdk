//! Occy lane-graph substrate — a parse-free **Lanelet2-lite** lane model (#90 / Occy 1.D).
//!
//! # Why this exists
//!
//! Occy's competitive-gap analysis named the **lane graph / routing** substrate
//! the single highest-leverage missing piece (see
//! `docs/COMPETITIVE_PLANNER_ANALYSIS.md` §5.1): it is what turns the planner's
//! centerline-relative lane-line rules and commanded lane changes from
//! hand-fed inputs into something *derived from a map*. Autoware uses **Lanelet2**
//! for exactly this; the adapter already has the production seam
//! (`kirra_ros2_adapter::corridor`'s feature-gated `Lanelet2CorridorSource`,
//! which deserializes a real `LaneletMapBin` through C++ `lanelet2_core`).
//!
//! # Honest scope — what is and isn't here
//!
//! This module is the **lane data model + the derivation to Occy's inputs**, with
//! **no map-file parse**. The actual Lanelet2 `.osm`/`MapBin` deserialization stays
//! behind the adapter's `lanelet2` feature gate (it needs the C++ library); this is
//! the structure that parser *populates* and the two derivations the planner
//! consumes:
//!
//! 1. [`LaneGraph::corridor_over`] → a [`LaneCorridor`] (a [`CorridorSource`]) — the
//!    drivable envelope across one or more laterally-adjacent lanes, the **same**
//!    handle the KIRRA checker re-reads. This is what makes a lane change physically
//!    checkable: the corridor spans both the source and target lanes.
//! 2. [`LaneGraph::boundaries_relative_to`] / [`Lane::lane_boundaries`] → typed
//!    [`LaneBoundary`]s at **real positions** (solid / broken at their true lateral
//!    offsets) instead of the centerline-relative literals the lane-line rules took
//!    as hand-fed inputs before.
//!
//! # Geometry assumption (stated, not hidden)
//!
//! Lanes carry a polyline **centerline** and produce real offset-polyline boundaries
//! (via the local segment normal), so [`LaneCorridor`] is correct for gently-curved
//! lanes. The *centerline-relative* `LaneBoundary` derivation (a single lateral
//! `y_m` offset) is the **Frenet projection** and is exact for **straight** lanes —
//! which matches Occy's lateral model (its `LaneBoundary` is itself a
//! centerline-relative-`y` abstraction) and the straight corridors the rest of the
//! stack produces (`TajCorridor`, `MockCorridorSource`). Full curved-lane Frenet
//! derivation is follow-up, exactly as elsewhere in the stack.

use kirra_core::corridor::{CorridorSource, Point};
use kirra_core::trajectory::PerceivedObject;
use std::collections::{BTreeMap, BTreeSet};

use crate::lane_lines::{LaneBoundary, LineType};

/// Maximum lanes a [`LaneGraph::route`] may traverse before failing closed — a
/// bounded graph walk, mirroring the verifier's `MAX_DEPENDENCY_DEPTH`. A route
/// longer than this (or a pathological graph) returns `None` rather than churning.
pub const MAX_ROUTE_LANES: usize = 64;

/// A directed connectivity edge out of a [`Lane`]. Mirrors the relations a
/// Lanelet2 routing graph carries: longitudinal **successors** (drive forward
/// into) and lateral **neighbors** (the adjacent lane on each side, the lane a
/// commanded lane change targets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneEdge {
    /// Longitudinal successor — the lane you drive forward into.
    Successor { to: u64 },
    /// Laterally-adjacent lane on the +y (left) side.
    LeftNeighbor { to: u64 },
    /// Laterally-adjacent lane on the -y (right) side.
    RightNeighbor { to: u64 },
}

/// A regulatory control the ego faces at the **end** of a lane — at the junction approach,
/// the stop line being the lane's terminus (a Lanelet2 `traffic_sign` / `traffic_light`
/// regulatory element). The static signs (`Stop` / `Yield`) carry all they need; a
/// `TrafficLight` carries only its **presence** — the live red/green/amber state is dynamic
/// (perception / V2X) and supplied at the loop boundary, defaulting fail-closed to *red*
/// (stop) when unknown. Mapped to a behavioral-layer traffic control at that boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneControl {
    /// STOP sign (MUTCD R1-1): full stop at the line, then proceed.
    Stop,
    /// YIELD / give-way (R1-2): slow, prepared to stop.
    Yield,
    /// Traffic LIGHT at the lane's stop line. Presence only — the live signal state is
    /// supplied separately each tick (unknown → red / stop, fail-closed).
    TrafficLight,
}

/// One lane: a centerline polyline, a typed boundary on each side, and its
/// connectivity. The boundary `LineType`s carry the crossing rules
/// ([`LaneBoundary::may_cross`]) that gate Occy's lateral maneuvers; the
/// centerline + `half_width_m` produce the drivable corridor.
///
/// Boundaries are stored **per-lane** (left/right `LineType`). A boundary shared
/// with a laterally-adjacent lane is modeled on both lanes; the derivations dedup
/// it by lateral position. This is the planner-facing view a Lanelet2 parser
/// (whose linestrings are shared between lanelets) populates.
#[derive(Debug, Clone)]
pub struct Lane {
    /// Stable lane id (the Lanelet2 primitive id in production).
    pub id: u64,
    /// World-frame centerline polyline (≥ 2 vertices), advancing along travel.
    pub centerline: Vec<Point>,
    /// Lateral half-width to each boundary, metres.
    pub half_width_m: f64,
    /// Marking on the +y (left) boundary — its crossing rule.
    pub left_line: LineType,
    /// Marking on the -y (right) boundary — its crossing rule.
    pub right_line: LineType,
    /// **Travel direction** of the lane, as a world-frame heading (radians). A
    /// forward `+X` lane is `0.0`; an *oncoming* lane (opposite traffic) is `π`.
    /// This is what distinguishes a same-direction lead from a head-on conflict —
    /// the input the oncoming-traffic RSS bound consumes. Independent of the
    /// centerline vertex ordering (which can advance either way).
    pub heading_rad: f64,
    /// Connectivity (successors + lateral neighbors).
    pub edges: Vec<LaneEdge>,
    /// Optional regulatory control the ego faces at this lane's END (its junction
    /// approach) — a STOP / YIELD sign whose stop line is the lane terminus. `None` = no
    /// control (the common open lane). Derived into a [`crate::behavior::TrafficControl`]
    /// at the loop boundary; a too-far / behind control is a no-op.
    pub control: Option<LaneControl>,
}

impl Lane {
    /// Build a **straight** lane of constant `y_center` running `x_start..x_end`.
    /// The convenience constructor for the straight lanes the stack uses today;
    /// curved lanes set `centerline` directly.
    #[must_use]
    pub fn straight(
        id: u64,
        y_center: f64,
        x_start: f64,
        x_end: f64,
        half_width_m: f64,
        left_line: LineType,
        right_line: LineType,
    ) -> Self {
        Self {
            id,
            centerline: vec![
                Point { x_m: x_start, y_m: y_center },
                Point { x_m: x_end, y_m: y_center },
            ],
            half_width_m,
            left_line,
            right_line,
            heading_rad: 0.0, // forward (+X) by default; oncoming lanes set π
            edges: Vec::new(),
            control: None,
        }
    }

    /// Builder: append a connectivity edge.
    #[must_use]
    pub fn with_edge(mut self, edge: LaneEdge) -> Self {
        self.edges.push(edge);
        self
    }

    /// Builder: set the regulatory control the ego faces at this lane's end.
    #[must_use]
    pub fn with_control(mut self, control: LaneControl) -> Self {
        self.control = Some(control);
        self
    }

    /// World-frame x of this lane's terminus along travel — where a junction control's stop
    /// line sits (the last centerline vertex). Straight-approach assumption, matching the
    /// behavioral layer's longitudinal (ego-frame-x) stop-line model.
    #[must_use]
    pub fn stop_line_x(&self) -> f64 {
        self.centerline.last().map_or(0.0, |p| p.x_m)
    }

    /// Builder: set the lane's travel direction (world heading, radians). Use `π`
    /// (`std::f64::consts::PI`) for an oncoming lane.
    #[must_use]
    pub fn with_heading(mut self, heading_rad: f64) -> Self {
        self.heading_rad = heading_rad;
        self
    }

    /// Does `other` carry traffic in the **opposing** direction (head-on)? True
    /// when the headings differ by more than a right angle — i.e. an oncoming
    /// lane. The discriminator between a same-direction lead (use the
    /// same-direction RSS) and a head-on conflict (needs the closing-speed bound).
    #[must_use]
    pub fn opposes(&self, other: &Lane) -> bool {
        wrap_pi(self.heading_rad - other.heading_rad).abs() > std::f64::consts::FRAC_PI_2
    }

    /// Is world point `p` inside this lane's footprint — within `±half_width_m` of the
    /// centerline **polyline**? Measured as the perpendicular distance to the nearest
    /// centerline segment (projection clamped to each segment), so it is correct for a CURVED
    /// lane as well as a straight one. (The previous `mean_y` bounding box silently excluded a
    /// turning lane's own ends — an arc from y=0 up to y=12 has `mean_y≈6`, so its box `[3,9]`
    /// missed the arc near the junction seam; that is what made `lane_at` return `None` mid-turn.)
    #[must_use]
    pub fn contains(&self, p: Point) -> bool {
        if self.centerline.len() < 2 {
            return false;
        }
        let half_sq = self.half_width_m * self.half_width_m;
        self.centerline
            .windows(2)
            .any(|w| point_segment_dist_sq(p, w[0], w[1]) <= half_sq)
    }

    /// Longitudinal successors of this lane.
    pub fn successors(&self) -> impl Iterator<Item = u64> + '_ {
        self.edges.iter().filter_map(|e| match e {
            LaneEdge::Successor { to } => Some(*to),
            _ => None,
        })
    }

    /// The lane id of the +y (left) lateral neighbor, if any.
    #[must_use]
    pub fn left_neighbor(&self) -> Option<u64> {
        self.edges.iter().find_map(|e| match e {
            LaneEdge::LeftNeighbor { to } => Some(*to),
            _ => None,
        })
    }

    /// The lane id of the -y (right) lateral neighbor, if any.
    #[must_use]
    pub fn right_neighbor(&self) -> Option<u64> {
        self.edges.iter().find_map(|e| match e {
            LaneEdge::RightNeighbor { to } => Some(*to),
            _ => None,
        })
    }

    /// Mean lateral position of the centerline (the lane's `y_center` for a
    /// straight lane).
    fn mean_y(&self) -> f64 {
        if self.centerline.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.centerline.iter().map(|p| p.y_m).sum();
        sum / self.centerline.len() as f64
    }

    /// This lane's two boundaries as centerline-relative typed [`LaneBoundary`]s:
    /// the left line at `+half_width_m`, the right line at `-half_width_m`. These
    /// feed Occy's lane-line crossing rules directly (its `LaneBoundary` is a
    /// centerline-relative-`y` abstraction). Exact for straight lanes (the Frenet
    /// projection).
    #[must_use]
    pub fn lane_boundaries(&self) -> Vec<LaneBoundary> {
        vec![
            LaneBoundary { y_m: self.half_width_m, line: self.left_line },
            LaneBoundary { y_m: -self.half_width_m, line: self.right_line },
        ]
    }

    /// Derive this single lane's drivable [`LaneCorridor`] — the left/right offset
    /// polylines at `±half_width_m`. `confidence` / `age_ms` are the map-server
    /// health the kernel's `Corridor::is_healthy` check reads.
    #[must_use]
    pub fn corridor(&self, confidence: f32, age_ms: u64) -> LaneCorridor {
        LaneCorridor {
            left: offset_polyline(&self.centerline, self.half_width_m),
            right: offset_polyline(&self.centerline, -self.half_width_m),
            confidence,
            age_ms,
        }
    }
}

/// A `CorridorSource` derived from a lane (or a span of laterally-adjacent lanes).
/// Owns the boundary polylines; the trait methods return slice borrows — the same
/// surface [`kirra_core::corridor::MockCorridorSource`] and `TajCorridor`
/// present, so the planner and KIRRA read one corridor without copying.
#[derive(Debug, Clone)]
pub struct LaneCorridor {
    left: Vec<Point>,
    right: Vec<Point>,
    confidence: f32,
    age_ms: u64,
}

impl CorridorSource for LaneCorridor {
    fn left_boundary(&self) -> &[Point] {
        &self.left
    }
    fn right_boundary(&self) -> &[Point] {
        &self.right
    }
    fn confidence(&self) -> f32 {
        self.confidence
    }
    fn age_ms(&self) -> u64 {
        self.age_ms
    }
}

/// The map-derived junction reasoning for one tick — the output of
/// [`LaneGraph::junction_context`]. Carries the resolved ego lane and BOTH
/// consumer-ready right-of-way sets, derived from the one `priority_over` map so they
/// cannot disagree. The deployment maps each set to its path's runtime type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JunctionContext {
    /// The ego's lane, or `None` if it is off the mapped road (→ empty sets).
    pub ego_lane: Option<u64>,
    /// Objects that cede to the ego → Occy's `PlanInput.cedes_to_ego_ids`.
    pub cedes_to_ego: Vec<u64>,
    /// Objects the ego must yield to / wait for → Parko SG5's `NonYieldingScene`.
    pub must_yield_to: Vec<u64>,
}

/// A collection of connected [`Lane`]s — the Lanelet2-lite lane graph. Lanes are
/// keyed by id (deterministic iteration). The planner derives a corridor + typed
/// boundaries from a chosen span of lanes; a real route (the preferred-primitive
/// id sequence) would select that span.
#[derive(Debug, Clone, Default)]
pub struct LaneGraph {
    lanes: BTreeMap<u64, Lane>,
    /// Right-of-way: `priority lane → lanes that must yield to it`. Populated from a
    /// Lanelet2 `right_of_way` regulatory element. Drives [`cedes_to_ego`] so the cede
    /// list is *derived from the map* rather than integrator-supplied.
    ///
    /// [`cedes_to_ego`]: Self::cedes_to_ego
    priority_over: BTreeMap<u64, BTreeSet<u64>>,
    /// **Junction occlusion**: `approach lane → assured-clear sight distance (m)` toward the
    /// junction's cross-traffic conflict. Populated from the map + perception (a building /
    /// hedge / parked car limits the view at the approach). Drives the occluded-junction
    /// approach speed cap so the ego CREEPS into a blind junction (RSS Rule 4 applied laterally
    /// at the junction). A lane absent from this map has an open view (no occlusion cap).
    occlusion_sight: BTreeMap<u64, f64>,
}

impl LaneGraph {
    /// An empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self { lanes: BTreeMap::new(), priority_over: BTreeMap::new(), occlusion_sight: BTreeMap::new() }
    }

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

    /// Record that `priority_lane` has right-of-way over `yielding_lane` — traffic in
    /// the yielding lane must cede to traffic in the priority lane at their conflict.
    pub fn add_right_of_way(&mut self, priority_lane: u64, yielding_lane: u64) {
        self.priority_over.entry(priority_lane).or_default().insert(yielding_lane);
    }

    /// Builder form of [`add_right_of_way`](Self::add_right_of_way).
    #[must_use]
    pub fn with_right_of_way(mut self, priority_lane: u64, yielding_lane: u64) -> Self {
        self.add_right_of_way(priority_lane, yielding_lane);
        self
    }

    /// The lanes that must yield to `priority_lane` (empty if it has no asserted priority).
    pub fn lanes_yielding_to(&self, priority_lane: u64) -> impl Iterator<Item = u64> + '_ {
        self.priority_over.get(&priority_lane).into_iter().flatten().copied()
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
            .filter(|o| self.lane_at(o.pos).is_some_and(|l| priority_lanes.contains(&l)))
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

    /// Insert (or replace) a lane.
    pub fn add_lane(&mut self, lane: Lane) {
        self.lanes.insert(lane.id, lane);
    }

    /// Builder form of [`add_lane`](Self::add_lane).
    #[must_use]
    pub fn with_lane(mut self, lane: Lane) -> Self {
        self.add_lane(lane);
        self
    }

    /// Look up a lane by id.
    #[must_use]
    pub fn lane(&self, id: u64) -> Option<&Lane> {
        self.lanes.get(&id)
    }

    /// Which lane contains world point `p` (first match in id order), if any.
    /// `None` = off the mapped road. Used to attribute a perceived object to a
    /// lane so its travel direction can be compared against the ego's.
    #[must_use]
    pub fn lane_at(&self, p: Point) -> Option<u64> {
        self.lanes.values().find(|l| l.contains(p)).map(|l| l.id)
    }

    /// Is world point `p` in a lane whose traffic **opposes** the ego lane (a
    /// head-on conflict candidate)? `Some(true)` = oncoming, `Some(false)` =
    /// same-direction (incl. the ego's own lane), `None` = `ego_lane` unknown or
    /// `p` is off the mapped road (fail-closed: an unattributable object is not
    /// silently classified same-direction). This is the discriminator the
    /// oncoming-traffic RSS bound (the next piece) keys off.
    #[must_use]
    pub fn is_oncoming_at(&self, ego_lane: u64, p: Point) -> Option<bool> {
        let ego = self.lanes.get(&ego_lane)?;
        let other = self.lanes.get(&self.lane_at(p)?)?;
        Some(other.opposes(ego))
    }

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
            let Some(lane) = self.lane(lane_id) else { continue };
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

    /// Number of lanes in the graph.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lanes.len()
    }

    /// Whether the graph holds no lanes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lanes.is_empty()
    }

    /// Derive the drivable [`LaneCorridor`] spanning the given laterally-adjacent
    /// lanes — the **outer envelope**: the left boundary of the leftmost lane and
    /// the right boundary of the rightmost lane. This is what makes a commanded
    /// lane change checkable: the corridor covers the source *and* target lanes, so
    /// the shifted trajectory stays contained. Returns `None` if any id is unknown
    /// or the slice is empty.
    #[must_use]
    pub fn corridor_over(&self, lane_ids: &[u64], confidence: f32, age_ms: u64) -> Option<LaneCorridor> {
        let lanes = self.resolve(lane_ids)?;
        // Leftmost = greatest mean-y; rightmost = least mean-y.
        let leftmost = lanes.iter().copied().max_by(|a, b| a.mean_y().total_cmp(&b.mean_y()))?;
        let rightmost = lanes.iter().copied().min_by(|a, b| a.mean_y().total_cmp(&b.mean_y()))?;
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
    pub fn route_corridor(&self, route: &[u64], confidence: f32, age_ms: u64) -> Option<LaneCorridor> {
        let lanes = self.resolve(route)?;
        let mut left: Vec<Point> = Vec::new();
        let mut right: Vec<Point> = Vec::new();
        for lane in lanes {
            concat_dedup(&mut left, &offset_polyline(&lane.centerline, lane.half_width_m));
            concat_dedup(&mut right, &offset_polyline(&lane.centerline, -lane.half_width_m));
        }
        (left.len() >= 2 && right.len() >= 2).then_some(LaneCorridor { left, right, confidence, age_ms })
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
    pub fn route_drivable(&self, route: &[u64], confidence: f32, age_ms: u64) -> Option<LaneCorridor> {
        let lanes = self.resolve(route)?;
        let mut left: Vec<Point> = Vec::new();
        let mut right: Vec<Point> = Vec::new();
        for lane in lanes {
            let left_lane = lane.left_neighbor().and_then(|n| self.lanes.get(&n)).unwrap_or(lane);
            let right_lane = lane.right_neighbor().and_then(|n| self.lanes.get(&n)).unwrap_or(lane);
            concat_dedup(&mut left, &offset_polyline(&left_lane.centerline, left_lane.half_width_m));
            concat_dedup(&mut right, &offset_polyline(&right_lane.centerline, -right_lane.half_width_m));
        }
        (left.len() >= 2 && right.len() >= 2).then_some(LaneCorridor { left, right, confidence, age_ms })
    }

    /// Derive the typed lane boundaries across a span of lanes, expressed as
    /// lateral offsets **relative to `ego_lane`'s centerline** (the frame Occy's
    /// `lane_boundaries` input uses). Boundaries shared between adjacent lanes are
    /// deduplicated by lateral position. Returns `None` if any id is unknown.
    #[must_use]
    pub fn boundaries_relative_to(&self, ego_lane: u64, lane_ids: &[u64]) -> Option<Vec<LaneBoundary>> {
        let ego_y = self.lanes.get(&ego_lane)?.mean_y();
        let lanes = self.resolve(lane_ids)?;
        let mut out: Vec<LaneBoundary> = Vec::new();
        for lane in lanes {
            let c = lane.mean_y() - ego_y;
            for b in [
                LaneBoundary { y_m: c + lane.half_width_m, line: lane.left_line },
                LaneBoundary { y_m: c - lane.half_width_m, line: lane.right_line },
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
            Lane::straight(ego_lane_id, ego_c, x0, x1, lane_half, LineType::Unmarked, LineType::Solid)
                .with_edge(LaneEdge::LeftNeighbor { to: oncoming_lane_id }),
        );
        // Oncoming (left half): road edge on the left, unmarked centerline on the
        // right, and travel OPPOSING the ego (heading π) — the head-on direction.
        g.add_lane(
            Lane::straight(oncoming_lane_id, onc_c, x0, x1, lane_half, LineType::Solid, LineType::Unmarked)
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

/// Wrap an angle to `(-π, π]`.
fn wrap_pi(a: f64) -> f64 {
    use std::f64::consts::PI;
    let mut x = a % (2.0 * PI);
    if x > PI {
        x -= 2.0 * PI;
    } else if x <= -PI {
        x += 2.0 * PI;
    }
    x
}

/// Mean lateral position of a boundary polyline.
fn mean_y_of(pts: &[Point]) -> f64 {
    pts.iter().map(|p| p.y_m).sum::<f64>() / pts.len() as f64
}

/// Squared distance from point `p` to segment `a→b` (projection clamped to the segment), the
/// kernel of the curved-lane [`Lane::contains`] test. Squared to avoid a `sqrt` per segment.
fn point_segment_dist_sq(p: Point, a: Point, b: Point) -> f64 {
    let (abx, aby) = (b.x_m - a.x_m, b.y_m - a.y_m);
    let len_sq = abx * abx + aby * aby;
    // Clamp the projection parameter to [0, 1] so a point off either end measures to the
    // nearest endpoint (degenerate zero-length segment → distance to `a`).
    let t = if len_sq <= f64::EPSILON {
        0.0
    } else {
        (((p.x_m - a.x_m) * abx + (p.y_m - a.y_m) * aby) / len_sq).clamp(0.0, 1.0)
    };
    let (cx, cy) = (a.x_m + t * abx, a.y_m + t * aby);
    let (dx, dy) = (p.x_m - cx, p.y_m - cy);
    dx * dx + dy * dy
}

/// Longitudinal `[x_min, x_max]` spanned by two boundary polylines, or `None` if
/// degenerate (non-finite or zero length).
fn x_extent(a: &[Point], b: &[Point]) -> Option<(f64, f64)> {
    let x0 = a.iter().chain(b).map(|p| p.x_m).fold(f64::INFINITY, f64::min);
    let x1 = a.iter().chain(b).map(|p| p.x_m).fold(f64::NEG_INFINITY, f64::max);
    (x0.is_finite() && x1.is_finite() && x1 > x0).then_some((x0, x1))
}

/// Append `pts` onto `acc`, dropping `pts`'s first vertex if it coincides (within a
/// tolerance) with `acc`'s last — so concatenating consecutive route-lane boundary
/// polylines at a shared junction node leaves no zero-length segment.
fn concat_dedup(acc: &mut Vec<Point>, pts: &[Point]) {
    let start = match (acc.last(), pts.first()) {
        (Some(last), Some(first)) if (last.x_m - first.x_m).hypot(last.y_m - first.y_m) <= 1e-6 => 1,
        _ => 0,
    };
    acc.extend_from_slice(&pts[start..]);
}

/// Offset a centerline polyline laterally by `signed_offset` (>0 = +y/left side)
/// along the local segment normal. Exact for straight lanes; correct for
/// gently-curved polylines (per-vertex normal from the adjacent segments).
fn offset_polyline(centerline: &[Point], signed_offset: f64) -> Vec<Point> {
    let n = centerline.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        // Degenerate: no tangent — offset straight along +y.
        return vec![Point { x_m: centerline[0].x_m, y_m: centerline[0].y_m + signed_offset }];
    }
    (0..n)
        .map(|i| {
            // Tangent from the adjacent segment(s).
            let (ax, ay) = if i == 0 {
                (centerline[1].x_m - centerline[0].x_m, centerline[1].y_m - centerline[0].y_m)
            } else if i == n - 1 {
                (centerline[n - 1].x_m - centerline[n - 2].x_m, centerline[n - 1].y_m - centerline[n - 2].y_m)
            } else {
                (centerline[i + 1].x_m - centerline[i - 1].x_m, centerline[i + 1].y_m - centerline[i - 1].y_m)
            };
            let len = ax.hypot(ay).max(1e-9);
            // Left-normal of the tangent (rotate +90°): (-ty, tx).
            let (nx, ny) = (-ay / len, ax / len);
            Point {
                x_m: centerline[i].x_m + signed_offset * nx,
                y_m: centerline[i].y_m + signed_offset * ny,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-lane road: ego (left) lane centered at y=0, right lane at y=-3.5.
    /// The divider between them is BROKEN (crossable); the outer edges are SOLID.
    fn two_lane_road() -> LaneGraph {
        LaneGraph::new()
            .with_lane(
                Lane::straight(1, 0.0, 0.0, 100.0, 1.75, LineType::Solid, LineType::Broken)
                    .with_edge(LaneEdge::RightNeighbor { to: 2 }),
            )
            .with_lane(
                Lane::straight(2, -3.5, 0.0, 100.0, 1.75, LineType::Broken, LineType::Solid)
                    .with_edge(LaneEdge::LeftNeighbor { to: 1 }),
            )
    }

    #[test]
    fn single_lane_corridor_is_a_valid_source() {
        let lane = Lane::straight(1, 0.0, 0.0, 50.0, 1.75, LineType::Solid, LineType::Broken);
        let c = lane.corridor(0.95, 10);
        assert_eq!(c.left_boundary().len(), 2);
        assert_eq!(c.right_boundary().len(), 2);
        assert!((c.left_boundary()[0].y_m - 1.75).abs() < 1e-9, "left at +half_width");
        assert!((c.right_boundary()[0].y_m + 1.75).abs() < 1e-9, "right at -half_width");
        assert_eq!(c.confidence(), 0.95);
        assert_eq!(c.age_ms(), 10);
    }

    #[test]
    fn lane_boundaries_are_typed_at_real_offsets() {
        let lane = Lane::straight(1, 0.0, 0.0, 50.0, 1.75, LineType::Solid, LineType::Broken);
        let b = lane.lane_boundaries();
        assert_eq!(b.len(), 2);
        assert!(b.iter().any(|x| (x.y_m - 1.75).abs() < 1e-9 && x.line == LineType::Solid));
        assert!(b.iter().any(|x| (x.y_m + 1.75).abs() < 1e-9 && x.line == LineType::Broken));
    }

    #[test]
    fn corridor_over_spans_both_lanes() {
        let g = two_lane_road();
        let c = g.corridor_over(&[1, 2], 0.95, 10).expect("both lanes resolve");
        // Outer envelope: left edge of lane 1 (+1.75), right edge of lane 2 (-5.25).
        assert!((c.left_boundary()[0].y_m - 1.75).abs() < 1e-9, "left envelope +1.75");
        assert!((c.right_boundary()[0].y_m + 5.25).abs() < 1e-9, "right envelope -5.25");
    }

    #[test]
    fn boundaries_relative_to_ego_dedup_the_shared_divider() {
        let g = two_lane_road();
        let b = g.boundaries_relative_to(1, &[1, 2]).expect("resolve");
        // Three distinct boundaries: +1.75 solid, -1.75 broken (shared), -5.25 solid.
        assert_eq!(b.len(), 3, "shared divider deduped, got {b:?}");
        assert!(b.iter().any(|x| (x.y_m - 1.75).abs() < 1e-9 && x.line == LineType::Solid));
        assert!(b.iter().any(|x| (x.y_m + 1.75).abs() < 1e-9 && x.line == LineType::Broken));
        assert!(b.iter().any(|x| (x.y_m + 5.25).abs() < 1e-9 && x.line == LineType::Solid));
    }

    #[test]
    fn connectivity_round_trips() {
        let g = two_lane_road();
        assert_eq!(g.lane(1).unwrap().right_neighbor(), Some(2));
        assert_eq!(g.lane(2).unwrap().left_neighbor(), Some(1));
        assert_eq!(g.lane(1).unwrap().left_neighbor(), None);
        assert_eq!(g.len(), 2);
    }

    #[test]
    fn unknown_lane_id_fails_closed() {
        let g = two_lane_road();
        assert!(g.corridor_over(&[1, 99], 0.95, 10).is_none());
        assert!(g.boundaries_relative_to(1, &[]).is_none());
        assert!(g.boundaries_relative_to(99, &[1]).is_none());
    }

    #[test]
    fn undivided_corridor_synthesizes_keep_right_split() {
        use kirra_core::corridor::MockCorridorSource;
        // A 10 m-wide undivided road (±5). Keep-right split → ego on the right half.
        let road = MockCorridorSource::straight_5m_half_width(100.0);
        let g = LaneGraph::from_undivided_corridor(&road, 1, 2).expect("synthesize");
        let ego = g.lane(1).unwrap();
        let onc = g.lane(2).unwrap();
        // Each synthesized lane is a quarter-width half-lane (2.5 m), centered in
        // its half: ego at -2.5 (right), oncoming at +2.5 (left).
        assert!((ego.centerline[0].y_m + 2.5).abs() < 1e-9, "ego keeps right (-2.5)");
        assert!((onc.centerline[0].y_m - 2.5).abs() < 1e-9, "oncoming is the left half (+2.5)");
        assert!((ego.half_width_m - 2.5).abs() < 1e-9);
        // The shared centerline is UNMARKED (crossable to pass), road edges SOLID.
        assert_eq!(ego.left_line, LineType::Unmarked, "ego↔oncoming divider unmarked");
        assert_eq!(ego.right_line, LineType::Solid, "right road edge");
        assert_eq!(onc.right_line, LineType::Unmarked);
        assert_eq!(ego.left_neighbor(), Some(2));
    }

    #[test]
    fn ego_lane_corridor_keeps_right_of_the_road_center() {
        use kirra_core::corridor::{CorridorSource, MockCorridorSource};
        let road = MockCorridorSource::straight_5m_half_width(100.0);
        let g = LaneGraph::from_undivided_corridor(&road, 1, 2).unwrap();
        let ego = g.lane(1).unwrap().corridor(0.95, 10);
        // The ego-lane corridor spans the RIGHT half: [-5, 0], never crossing the
        // road center into the +y (oncoming) half.
        for p in ego.left_boundary().iter().chain(ego.right_boundary()) {
            assert!(p.y_m <= 1e-9, "ego lane stays right of center, got y={}", p.y_m);
        }
        assert!((ego.left_boundary()[0].y_m).abs() < 1e-9, "left edge at the road center (0)");
        assert!((ego.right_boundary()[0].y_m + 5.0).abs() < 1e-9, "right edge at the road edge (-5)");
    }

    #[test]
    fn degenerate_corridor_fails_closed() {
        // An inline source whose boundaries are flipped (left below right) — not a
        // valid +y-left / -y-right road → synthesis fails closed (None).
        struct FlippedSource {
            left: Vec<Point>,
            right: Vec<Point>,
        }
        impl CorridorSource for FlippedSource {
            fn left_boundary(&self) -> &[Point] {
                &self.left
            }
            fn right_boundary(&self) -> &[Point] {
                &self.right
            }
            fn confidence(&self) -> f32 {
                0.9
            }
            fn age_ms(&self) -> u64 {
                5
            }
        }
        let flipped = FlippedSource {
            left: vec![Point { x_m: 0.0, y_m: -5.0 }, Point { x_m: 50.0, y_m: -5.0 }],
            right: vec![Point { x_m: 0.0, y_m: 5.0 }, Point { x_m: 50.0, y_m: 5.0 }],
        };
        assert!(LaneGraph::from_undivided_corridor(&flipped, 1, 2).is_none());
    }

    #[test]
    fn undivided_road_marks_the_oncoming_lane_opposing() {
        use kirra_core::corridor::MockCorridorSource;
        let road = MockCorridorSource::straight_5m_half_width(100.0);
        let g = LaneGraph::from_undivided_corridor(&road, 1, 2).unwrap();
        let ego = g.lane(1).unwrap();
        let onc = g.lane(2).unwrap();
        assert_eq!(ego.heading_rad, 0.0, "ego travels forward (+X)");
        assert!((onc.heading_rad - std::f64::consts::PI).abs() < 1e-9, "oncoming travels -X");
        assert!(onc.opposes(ego), "oncoming lane opposes the ego");
        assert!(ego.opposes(onc), "opposition is symmetric");
        assert!(!ego.opposes(ego), "a lane never opposes itself");
    }

    #[test]
    fn lane_at_attributes_points_to_their_half() {
        use kirra_core::corridor::MockCorridorSource;
        let road = MockCorridorSource::straight_5m_half_width(100.0);
        let g = LaneGraph::from_undivided_corridor(&road, 1, 2).unwrap();
        // Right half (y<0) → ego lane 1; left half (y>0) → oncoming lane 2.
        assert_eq!(g.lane_at(Point { x_m: 20.0, y_m: -2.5 }), Some(1));
        assert_eq!(g.lane_at(Point { x_m: 20.0, y_m: 2.5 }), Some(2));
        // Off the road (beyond the edge) or past the longitudinal extent → None.
        assert_eq!(g.lane_at(Point { x_m: 20.0, y_m: 9.0 }), None);
        assert_eq!(g.lane_at(Point { x_m: 200.0, y_m: -2.5 }), None);
    }

    #[test]
    fn is_oncoming_at_discriminates_head_on_from_lead() {
        use kirra_core::corridor::MockCorridorSource;
        let road = MockCorridorSource::straight_5m_half_width(100.0);
        let g = LaneGraph::from_undivided_corridor(&road, 1, 2).unwrap();
        // An object in the oncoming half is a head-on candidate; one in the ego
        // half is same-direction (a lead). Off-road / unknown ego → None (fail-closed).
        assert_eq!(g.is_oncoming_at(1, Point { x_m: 30.0, y_m: 2.5 }), Some(true));
        assert_eq!(g.is_oncoming_at(1, Point { x_m: 30.0, y_m: -2.5 }), Some(false));
        assert_eq!(g.is_oncoming_at(1, Point { x_m: 30.0, y_m: 20.0 }), None);
        assert_eq!(g.is_oncoming_at(99, Point { x_m: 30.0, y_m: 2.5 }), None);
    }

    #[test]
    fn marked_lanes_default_to_forward_travel() {
        // The straight() constructor defaults to forward; with_heading overrides.
        let fwd = Lane::straight(1, 0.0, 0.0, 50.0, 1.75, LineType::Solid, LineType::Broken);
        assert_eq!(fwd.heading_rad, 0.0);
        let back = fwd.clone().with_heading(std::f64::consts::PI);
        assert!(fwd.opposes(&back) && !fwd.opposes(&fwd));
    }

    #[test]
    fn curved_centerline_offsets_along_the_normal() {
        // A centerline turning toward +y: the left offset is longer-arc/outside.
        let cl = vec![
            Point { x_m: 0.0, y_m: 0.0 },
            Point { x_m: 10.0, y_m: 0.0 },
            Point { x_m: 20.0, y_m: 5.0 },
        ];
        let left = offset_polyline(&cl, 1.0);
        let right = offset_polyline(&cl, -1.0);
        assert_eq!(left.len(), 3);
        // At the straight start the normal is +y → left point sits at y≈+1.
        assert!((left[0].y_m - 1.0).abs() < 1e-9 && left[0].x_m.abs() < 1e-9);
        assert!((right[0].y_m + 1.0).abs() < 1e-9);
    }

    // ----- Router (lane selection over the connectivity graph) -------------

    /// Two parallel forward lanes, each a 3-segment successor chain:
    ///   left lane  (y=0):    1 → 2 → 3
    ///   right lane (y=-3.5): 11 → 12 → 13
    /// with lateral neighbor links between the abreast segments (1↔11, 2↔12, 3↔13).
    fn routing_grid() -> LaneGraph {
        let l = LineType::Solid;
        let b = LineType::Broken;
        let seg = |id, y, x0: f64, x1: f64, succ: Option<u64>, neigh: LaneEdge| {
            let mut lane = Lane::straight(id, y, x0, x1, 1.75, l, b);
            if let Some(s) = succ {
                lane = lane.with_edge(LaneEdge::Successor { to: s });
            }
            lane.with_edge(neigh)
        };
        LaneGraph::new()
            .with_lane(seg(1, 0.0, 0.0, 30.0, Some(2), LaneEdge::RightNeighbor { to: 11 }))
            .with_lane(seg(2, 0.0, 30.0, 60.0, Some(3), LaneEdge::RightNeighbor { to: 12 }))
            .with_lane(seg(3, 0.0, 60.0, 90.0, None, LaneEdge::RightNeighbor { to: 13 }))
            .with_lane(seg(11, -3.5, 0.0, 30.0, Some(12), LaneEdge::LeftNeighbor { to: 1 }))
            .with_lane(seg(12, -3.5, 30.0, 60.0, Some(13), LaneEdge::LeftNeighbor { to: 2 }))
            .with_lane(seg(13, -3.5, 60.0, 90.0, None, LaneEdge::LeftNeighbor { to: 3 }))
    }

    #[test]
    fn route_follows_the_successor_chain() {
        assert_eq!(routing_grid().route(1, 3), Some(vec![1, 2, 3]));
    }

    #[test]
    fn route_to_self_is_a_singleton() {
        assert_eq!(routing_grid().route(2, 2), Some(vec![2]));
    }

    #[test]
    fn route_takes_a_lane_change_only_when_needed() {
        // Goal in the right lane, one segment ahead: drive forward then change lanes.
        assert_eq!(routing_grid().route(1, 12), Some(vec![1, 2, 12]));
        // Goal directly abreast: a single lane change.
        assert_eq!(routing_grid().route(1, 11), Some(vec![1, 11]));
    }

    #[test]
    fn route_prefers_staying_in_lane_over_weaving() {
        // 1→3 via the left chain (two successors, cost 2) beats any route that dips
        // into the right lane and back (≥ two lane changes, cost ≥ 6).
        assert_eq!(routing_grid().route(1, 3), Some(vec![1, 2, 3]));
    }

    #[test]
    fn route_to_a_point_resolves_the_goal_lane() {
        let g = routing_grid();
        assert_eq!(g.route_to_point(1, Point { x_m: 75.0, y_m: 0.0 }), Some(vec![1, 2, 3]));
        // Off the mapped road → None.
        assert_eq!(g.route_to_point(1, Point { x_m: 75.0, y_m: 50.0 }), None);
    }

    #[test]
    fn route_fails_closed_on_unknown_or_unreachable() {
        let g = routing_grid();
        assert_eq!(g.route(1, 9999), None, "unknown destination");
        assert_eq!(g.route(9999, 3), None, "unknown source");
        // A disconnected lane is unreachable.
        let g2 = g.with_lane(Lane::straight(77, 100.0, 0.0, 30.0, 1.75, LineType::Solid, LineType::Solid));
        assert_eq!(g2.route(1, 77), None, "no path to an isolated lane");
    }

    #[test]
    fn route_terminates_on_a_cycle() {
        // A → B → A. Routing to an unreachable node must not loop forever.
        let g = LaneGraph::new()
            .with_lane(Lane::straight(1, 0.0, 0.0, 30.0, 1.75, LineType::Solid, LineType::Solid).with_edge(LaneEdge::Successor { to: 2 }))
            .with_lane(Lane::straight(2, 0.0, 30.0, 60.0, 1.75, LineType::Solid, LineType::Solid).with_edge(LaneEdge::Successor { to: 1 }))
            .with_lane(Lane::straight(3, 0.0, 60.0, 90.0, 1.75, LineType::Solid, LineType::Solid));
        assert_eq!(g.route(1, 3), None);
        assert_eq!(g.route(1, 2), Some(vec![1, 2]));
    }

    // ----- Route corridor (longitudinal stitch through a junction) ---------

    /// A quarter-circle arc (n+1 points) from `start_angle` sweeping +π/2 about
    /// `(cx, cy)` at radius `r` — a smooth left-turn centerline.
    fn quarter_arc(cx: f64, cy: f64, r: f64, start_angle: f64, n: usize) -> Vec<Point> {
        (0..=n)
            .map(|i| {
                let t = start_angle + std::f64::consts::FRAC_PI_2 * (i as f64 / n as f64);
                Point { x_m: cx + r * t.cos(), y_m: cy + r * t.sin() }
            })
            .collect()
    }

    #[test]
    fn route_corridor_concats_a_straight_succession_deduping_the_seam() {
        // Two straight lanes end-to-end: 1 (x 0..20) → 2 (x 20..40), both at y=0.
        let g = LaneGraph::new()
            .with_lane(
                Lane::straight(1, 0.0, 0.0, 20.0, 2.0, LineType::Solid, LineType::Solid)
                    .with_edge(LaneEdge::Successor { to: 2 }),
            )
            .with_lane(Lane::straight(2, 0.0, 20.0, 40.0, 2.0, LineType::Solid, LineType::Solid));
        assert_eq!(g.route(1, 2), Some(vec![1, 2]));

        let c = g.route_corridor(&[1, 2], 0.95, 10).expect("stitch");
        // Seam at x=20 deduped → 3 vertices a side, spanning x 0..40 at ±2.
        assert_eq!(c.left_boundary().len(), 3, "shared seam vertex deduped: {:?}", c.left_boundary());
        assert_eq!(c.right_boundary().len(), 3);
        assert!(c.left_boundary().iter().all(|p| (p.y_m - 2.0).abs() < 1e-9));
        assert!(c.right_boundary().iter().all(|p| (p.y_m + 2.0).abs() < 1e-9));
        assert!((c.left_boundary().last().unwrap().x_m - 40.0).abs() < 1e-9, "spans to x=40");
    }

    #[test]
    fn route_corridor_curves_through_a_left_turn() {
        // Ego straight (0,0)→(20,0); a quarter-arc junction lane curving LEFT from
        // (20,0) up to (30,10); a vertical exit lane (30,10)→(30,30). The stitched
        // corridor must FOLLOW the turn — its boundaries swing from heading-east to
        // heading-north (the exit), not stay flat.
        let arc = quarter_arc(20.0, 10.0, 10.0, -std::f64::consts::FRAC_PI_2, 8); // (20,0)→(30,10)
        let junction = Lane {
            id: 2,
            centerline: arc,
            half_width_m: 2.0,
            left_line: LineType::Solid,
            right_line: LineType::Solid,
            heading_rad: std::f64::consts::FRAC_PI_4, // mean of the turn; not load-bearing here
            edges: vec![LaneEdge::Successor { to: 3 }],
            control: None,        };
        let exit = Lane {
            id: 3,
            centerline: vec![Point { x_m: 30.0, y_m: 10.0 }, Point { x_m: 30.0, y_m: 30.0 }],
            half_width_m: 2.0,
            left_line: LineType::Solid,
            right_line: LineType::Solid,
            heading_rad: std::f64::consts::FRAC_PI_2, // north
            edges: Vec::new(),
            control: None,
        };
        let g = LaneGraph::new()
            .with_lane(
                Lane::straight(1, 0.0, 0.0, 20.0, 2.0, LineType::Solid, LineType::Solid)
                    .with_edge(LaneEdge::Successor { to: 2 }),
            )
            .with_lane(junction)
            .with_lane(exit);

        let route = g.route(1, 3).expect("route through the junction");
        assert_eq!(route, vec![1, 2, 3]);
        let c = g.route_corridor(&route, 0.95, 10).expect("stitch the turn");

        // The corridor starts flat (east, y≈0) and ends pointed north (x≈30, y≈30) —
        // i.e. it genuinely turned, not stayed on the entry heading.
        let last_l = *c.left_boundary().last().unwrap();
        assert!(last_l.y_m > 25.0, "corridor reaches up the exit lane, got y={}", last_l.y_m);
        assert!((last_l.x_m - 30.0).abs() < 3.0, "and is laterally at the exit (x≈30±half), got x={}", last_l.x_m);
        // Sanity: the entry is still near the origin heading east.
        assert!(c.right_boundary()[0].x_m.abs() < 1e-6, "entry starts at x≈0");
    }

    #[test]
    fn route_corridor_fails_closed_on_unknown_or_empty() {
        let g = LaneGraph::new()
            .with_lane(Lane::straight(1, 0.0, 0.0, 20.0, 2.0, LineType::Solid, LineType::Solid));
        assert!(g.route_corridor(&[], 0.95, 10).is_none(), "empty route → None");
        assert!(g.route_corridor(&[1, 99], 0.95, 10).is_none(), "unknown id → None");
    }

    #[test]
    fn route_drivable_widens_to_include_lateral_neighbors() {
        // Lane 1 (y=0, half 1.75) with a LEFT neighbor lane 2 (y=3.5). A route over lane 1
        // alone yields a drivable area spanning BOTH lanes (the borrowable turn width),
        // where route_corridor stays single-lane.
        let g = LaneGraph::new()
            .with_lane(
                Lane::straight(1, 0.0, 0.0, 30.0, 1.75, LineType::Broken, LineType::Solid)
                    .with_edge(LaneEdge::LeftNeighbor { to: 2 }),
            )
            .with_lane(
                Lane::straight(2, 3.5, 0.0, 30.0, 1.75, LineType::Solid, LineType::Broken)
                    .with_edge(LaneEdge::RightNeighbor { to: 1 }),
            );

        let d = g.route_drivable(&[1], 0.95, 10).expect("widen");
        assert!((d.left_boundary()[0].y_m - 5.25).abs() < 1e-9, "left widened to the neighbor edge (3.5+1.75), got {}", d.left_boundary()[0].y_m);
        assert!((d.right_boundary()[0].y_m + 1.75).abs() < 1e-9, "right stays at lane 1's edge");

        let c = g.route_corridor(&[1], 0.95, 10).expect("narrow");
        assert!((c.left_boundary()[0].y_m - 1.75).abs() < 1e-9, "route_corridor stays single-lane (+1.75)");

        assert!(g.route_drivable(&[], 0.95, 10).is_none(), "empty → None");
        assert!(g.route_drivable(&[1, 99], 0.95, 10).is_none(), "unknown id → None");
    }

    #[test]
    fn contains_follows_a_curved_centerline_not_a_mean_y_box() {
        use std::f64::consts::FRAC_PI_2;
        // A quarter-arc lane from (30,0) curving up to (42,12), half-width 3.0. Its mean_y≈6,
        // so the old |y−mean_y|≤half box was [3,9] — it missed the arc's own ends.
        let arc: Vec<Point> = (0..=12)
            .map(|i| {
                let t = -FRAC_PI_2 + FRAC_PI_2 * (i as f64 / 12.0);
                Point { x_m: 30.0 + 12.0 * t.cos(), y_m: 12.0 + 12.0 * t.sin() }
            })
            .collect();
        let lane = Lane {
            id: 1,
            centerline: arc,
            half_width_m: 3.0,
            left_line: LineType::Solid,
            right_line: LineType::Solid,
            heading_rad: FRAC_PI_2,
            edges: Vec::new(),
            control: None,
        };

        // Points ON the arc near its low end (y≈0) and high end (y≈12) — the box [3,9] excluded
        // these, which is exactly what stranded `lane_at` at the approach→arc seam.
        assert!(lane.contains(Point { x_m: 30.1, y_m: 0.0 }), "the arc's low (junction-seam) end is inside");
        assert!(lane.contains(Point { x_m: 41.9, y_m: 12.0 }), "the arc's high (exit) end is inside");
        // A point at a mid-arc station, just inside the half-width perpendicular to the curve.
        assert!(lane.contains(Point { x_m: 35.0, y_m: 1.6 }), "a mid-arc point within half-width is inside");
        // The box [3,9] would have FALSELY included a point at the chord interior far off the arc.
        assert!(!lane.contains(Point { x_m: 33.0, y_m: 6.0 }), "a point off the actual curve is outside");
        assert!(!lane.contains(Point { x_m: 35.0, y_m: 6.0 }), "well off the curve laterally → outside");
    }

    // ----- Right-of-way: cede vs non-yield, consistent from one source -----

    #[test]
    fn cedes_and_non_yielding_are_consistent_inverses() {
        use kirra_core::trajectory::PerceivedObject;
        // Lane 1 (along y=0) has priority over lane 2 (along y=10).
        let g = LaneGraph::new()
            .with_lane(Lane::straight(1, 0.0, 0.0, 30.0, 1.75, LineType::Solid, LineType::Solid))
            .with_lane(Lane::straight(2, 10.0, 0.0, 30.0, 1.75, LineType::Solid, LineType::Solid))
            .with_right_of_way(1, 2);
        let obj = |id, x, y| PerceivedObject {
            id,
            pos: Point { x_m: x, y_m: y },
            velocity_mps: 3.0,
            heading_rad: 0.0,
            vel: Point { x_m: 3.0, y_m: 0.0 },
        };

        // The inverse priority queries agree on one fact from one map.
        assert_eq!(g.lanes_yielding_to(1).collect::<Vec<_>>(), vec![2]);
        assert_eq!(g.lanes_with_priority_over(2).collect::<Vec<_>>(), vec![1]);

        // From the PRIORITY lane (ego in 1): an agent in lane 2 cedes to me, and is
        // NOT something I yield to — the two sets are disjoint by construction.
        let in_l2 = [obj(7, 15.0, 10.0)];
        assert_eq!(g.cedes_to_ego(1, &in_l2), vec![7]);
        assert!(g.non_yielding_to_ego(1, &in_l2).is_empty());

        // From the YIELDING lane (ego in 2): an agent in lane 1 is non-yielding (I must
        // wait for it), and does NOT cede to me — the exact inverse, same source.
        let in_l1 = [obj(9, 15.0, 0.0)];
        assert_eq!(g.non_yielding_to_ego(2, &in_l1), vec![9]);
        assert!(g.cedes_to_ego(2, &in_l1).is_empty());
    }

    #[test]
    fn occluded_approach_sight_distance_round_trips_and_fails_safe() {
        let g = LaneGraph::new()
            .with_lane(Lane::straight(1, 0.0, 0.0, 30.0, 2.0, LineType::Solid, LineType::Solid))
            .with_occluded_approach(1, 6.0);
        assert_eq!(g.sight_distance(1), Some(6.0), "the sight distance round-trips");
        assert_eq!(g.sight_distance(2), None, "a lane with no datum has an open view");

        // Non-finite / negative distances are ignored (no spurious occlusion datum).
        let g2 = LaneGraph::new().with_occluded_approach(5, f64::NAN).with_occluded_approach(6, -3.0);
        assert_eq!(g2.sight_distance(5), None, "NaN sight ignored (fail-safe)");
        assert_eq!(g2.sight_distance(6), None, "negative sight ignored (fail-safe)");
    }

    #[test]
    fn junction_context_resolves_the_ego_lane_and_both_sets() {
        use kirra_core::trajectory::PerceivedObject;
        let g = LaneGraph::new()
            .with_lane(Lane::straight(1, 0.0, 0.0, 30.0, 2.5, LineType::Solid, LineType::Solid))
            .with_lane(Lane::straight(2, 10.0, 0.0, 30.0, 2.5, LineType::Solid, LineType::Solid))
            .with_right_of_way(1, 2);
        let obj = |id, x, y| PerceivedObject {
            id,
            pos: Point { x_m: x, y_m: y },
            velocity_mps: 3.0,
            heading_rad: 0.0,
            vel: Point { x_m: 3.0, y_m: 0.0 },
        };
        let objs = [obj(7, 15.0, 10.0)];

        // Ego in lane 1 (priority): one call resolves the lane + both sets.
        let ctx = g.junction_context(Point { x_m: 15.0, y_m: 0.0 }, &objs);
        assert_eq!(ctx.ego_lane, Some(1));
        assert_eq!(ctx.cedes_to_ego, vec![7]);
        assert!(ctx.must_yield_to.is_empty());

        // Off the mapped road → empty context (fail-safe).
        assert_eq!(
            g.junction_context(Point { x_m: 15.0, y_m: 99.0 }, &objs),
            JunctionContext::default()
        );
    }
}
