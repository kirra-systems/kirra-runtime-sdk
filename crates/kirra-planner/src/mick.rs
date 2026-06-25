//! **Mick** — the LLM "brain" (System-2) intent seam for Occy.
//!
//! Mick *proposes* high-level typed **intent**; it never commands the actuator and
//! never describes the world. An intent sets only the **goal / maneuver** of a
//! plan — the WORLD it is planned against (corridor, objects, posture, kinematic
//! envelope) comes from perception / KIRRA, never from Mick. Occy *grounds* the
//! intent into a trajectory inside that world; the #131 checker then *bounds* it
//! (RSS + containment). So even a hallucinated or adversarial intent cannot make
//! the robot unsafe: at worst Occy stops short / refuses the maneuver, and KIRRA
//! rejects an unsafe path. This is the doer-checker thesis with a black-box doer —
//! the same safety case holds whatever (or whoever) authored the intent.
//!
//! Distinct from the main crate's low-level `action_filter` (an LLM `cmd_vel`
//! scalar → governor sanitization): that is a *command* gate; this is the
//! *intent → plan* bridge that routes Mick through the full Occy + KIRRA pipeline.

use serde::{Deserialize, Serialize};

use crate::behavior::{
    accept_turn_gap, ConflictApproach, SignalState, TrafficControl, DEFAULT_TURN_CRITICAL_GAP_S,
};
use crate::{
    FleetPosture, Goal, Lane, LaneControl, LaneGraph, PlanInput, PlanOutput, Planner, Pose,
    PredictedPath, MAX_ROUTE_LANES,
};
use kirra_core::corridor::{CorridorSource, Point as MapPoint};

/// Which way a [`MickIntent::TurnAt`] heads at the next junction, relative to the ego
/// lane's travel direction. Resolved to a successor lane by heading at grounding time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnDirection {
    /// A left turn (the successor whose heading rotates ≈ +90°).
    Left,
    /// A right turn (≈ −90°).
    Right,
    /// Continue straight through the junction (≈ 0°).
    Straight,
}

impl TurnDirection {
    /// The wire / capture token (`left` / `right` / `straight`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TurnDirection::Left => "left",
            TurnDirection::Right => "right",
            TurnDirection::Straight => "straight",
        }
    }

    /// Does a successor whose heading differs from the ego lane by `delta_rad`
    /// (wrapped to (−π, π]) count as this turn? A ±45° band splits straight from the
    /// left/right quadrants (+ve = left / CCW).
    fn matches(self, delta_rad: f64) -> bool {
        const STRAIGHT_BAND: f64 = std::f64::consts::FRAC_PI_4; // ±45°
        match self {
            TurnDirection::Straight => delta_rad.abs() < STRAIGHT_BAND,
            TurnDirection::Left => delta_rad >= STRAIGHT_BAND,
            TurnDirection::Right => delta_rad <= -STRAIGHT_BAND,
        }
    }
}

/// A high-level intent the LLM brain proposes — the Mick → Occy contract. It maps
/// ONLY to the goal / maneuver of a plan; it can express nothing about the world
/// or the actuator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MickIntent {
    /// Drive toward a world-frame goal point. Occy plans the route within the
    /// corridor and stops short of any hazard; it never drives *to* the point if
    /// getting there is unsafe.
    GoTo { x_m: f64, y_m: f64 },
    /// Change to the lane at `target_offset_m` from the current path centerline.
    /// Honored only if the lane-line rules permit the crossing and the corridor
    /// fits — Occy's behavioral layer adjudicates; an unlawful change is ignored.
    LaneChange { target_offset_m: f64 },
    /// Stop and hold.
    Hold,
    /// Cruise at a requested speed (m/s), keeping the current goal / lane. The request can
    /// only SLOW the chauffeur: Occy clamps it to `min(posture_ceiling, request)` and KIRRA
    /// caps it again, so "go faster" never exceeds the safe envelope. A non-finite request
    /// fails closed (the caller HOLDs).
    Cruise { target_speed_mps: f64 },
    /// Pass the slow / stopped lead ahead — a discretionary overtake using the drivable
    /// area, then return. Honored only if the world supplies a drivable area, the pass fits
    /// it, and the lane line is crossable; otherwise Occy stays in lane. KIRRA bounds the
    /// pass (head-on RSS), so an overtake into oncoming traffic is refused regardless.
    Overtake,
    /// Get to the road edge and stop — e.g. to let an emergency vehicle (ambulance,
    /// police, fire) pass, or a commanded curb stop. Occy shifts as far right as
    /// containment admits (onto a drivable shoulder if present, else the lane edge) and
    /// decelerates to a controlled stop. Honored only if the rightward move is lawful and
    /// fits; a nearer hazard still stops the ego first, and KIRRA bounds the parked pose.
    PullOver,
    /// Take the `direction` branch at the next junction. Honored only if a `lane_graph` is
    /// supplied, the ego resolves to a lane, and that lane has a successor turning the
    /// requested way; grounding routes through the branch and follows the materialized
    /// route corridor through the turn. No graph / no such branch → fail-closed HOLD. KIRRA
    /// bounds the route corridor exactly as it bounds any corridor (a too-tight turn is
    /// refused).
    TurnAt { direction: TurnDirection },
    /// Drive to a **world-frame destination across as many junctions as it takes** — the
    /// multi-junction sibling of [`TurnAt`] (one junction) and [`GoTo`] (geometry, no
    /// topology). Honored only if a `lane_graph` is supplied, the ego resolves to a lane,
    /// the destination point resolves to a lane, and a route connects them: grounding plans
    /// the lane-id route ([`LaneGraph::route_to_point`], Dijkstra — it takes the correct
    /// turn at *each* junction and changes lane only when the route requires it),
    /// materializes that whole route's corridor ([`LaneGraph::route_corridor`], which curves
    /// through every junction on the way), and follows it toward the destination. No graph /
    /// ego off-map / destination off-map / unreachable → fail-closed HOLD. The planner only
    /// PROPOSES along the route; KIRRA bounds the materialized corridor exactly as it bounds
    /// any other (a too-tight turn anywhere on the route is refused). The route is re-planned
    /// each tick, so it is robust to the ego being nudged between lanes (receding horizon).
    ///
    /// [`GoTo`]: MickIntent::GoTo
    RouteTo { x_m: f64, y_m: f64 },
}

/// LLM JSON wire schema (tagged on `"intent"`). Kept separate from [`MickIntent`]
/// so the public type is decoupled from the wire format.
#[derive(Deserialize)]
#[serde(tag = "intent")]
enum IntentJson {
    #[serde(rename = "go_to")]
    GoTo { x_m: f64, y_m: f64 },
    #[serde(rename = "lane_change")]
    LaneChange { target_offset_m: f64 },
    #[serde(rename = "hold")]
    Hold,
    #[serde(rename = "cruise")]
    Cruise { target_speed_mps: f64 },
    #[serde(rename = "overtake")]
    Overtake,
    #[serde(rename = "pull_over")]
    PullOver,
    #[serde(rename = "turn_at")]
    TurnAt { direction: String },
    #[serde(rename = "route_to")]
    RouteTo { x_m: f64, y_m: f64 },
}

impl MickIntent {
    /// Parse the LLM's JSON intent into a typed [`MickIntent`]. **Fail-closed**: any
    /// malformed / unknown-tag / non-finite payload returns `Err` so the caller
    /// HOLDs rather than acting on garbage — a hallucinated `NaN` goal must never
    /// flow into the planner.
    ///
    /// Tolerant of small-model framing: Gemma-class models routinely wrap the object
    /// in a ```json fence or prepend a sentence of prose. We extract the first
    /// balanced `{…}` object before parsing, so well-formed intent inside that
    /// framing is recovered rather than needlessly rejected. This widens *parse
    /// acceptance only* — the typed-schema, unknown-tag, and finiteness checks below
    /// are unchanged, so a genuinely malformed payload still fails closed.
    pub fn from_llm_json(raw: &str) -> Result<Self, &'static str> {
        let json = extract_first_json_object(raw).ok_or("MICK_JSON_PARSE_ERROR")?;
        let parsed: IntentJson =
            serde_json::from_str(json).map_err(|_| "MICK_JSON_PARSE_ERROR")?;
        let intent = match parsed {
            IntentJson::GoTo { x_m, y_m } => MickIntent::GoTo { x_m, y_m },
            IntentJson::LaneChange { target_offset_m } => MickIntent::LaneChange { target_offset_m },
            IntentJson::Hold => MickIntent::Hold,
            IntentJson::Cruise { target_speed_mps } => MickIntent::Cruise { target_speed_mps },
            IntentJson::Overtake => MickIntent::Overtake,
            IntentJson::PullOver => MickIntent::PullOver,
            IntentJson::TurnAt { direction } => {
                let dir = match direction.as_str() {
                    "left" => TurnDirection::Left,
                    "right" => TurnDirection::Right,
                    "straight" => TurnDirection::Straight,
                    _ => return Err("MICK_UNKNOWN_TURN_DIRECTION"),
                };
                MickIntent::TurnAt { direction: dir }
            }
            IntentJson::RouteTo { x_m, y_m } => MickIntent::RouteTo { x_m, y_m },
        };
        if !intent.is_finite() {
            return Err("MICK_NONFINITE_INTENT");
        }
        Ok(intent)
    }

    fn is_finite(&self) -> bool {
        match self {
            MickIntent::GoTo { x_m, y_m } => x_m.is_finite() && y_m.is_finite(),
            MickIntent::LaneChange { target_offset_m } => target_offset_m.is_finite(),
            MickIntent::Hold => true,
            MickIntent::Cruise { target_speed_mps } => target_speed_mps.is_finite(),
            MickIntent::Overtake => true,
            MickIntent::PullOver => true,
            MickIntent::TurnAt { .. } => true,
            MickIntent::RouteTo { x_m, y_m } => x_m.is_finite() && y_m.is_finite(),
        }
    }
}

/// Extract the first balanced top-level JSON object `{…}` from arbitrary LLM text.
/// Brace-matching is **string-aware**: braces and quotes inside a JSON string value
/// (and `\`-escaped quotes) do not count, so a stray `{` in prose or a `{` inside a
/// string field cannot mis-terminate the object. Returns the `{…}` slice, or `None`
/// if there is no balanced object — which keeps [`MickIntent::from_llm_json`]
/// fail-closed on text that merely *looks* like it might contain intent.
///
/// All structural bytes (`{` `}` `"` `\`) are ASCII, so the byte offsets used for
/// slicing always fall on `char` boundaries even when the prose is multi-byte UTF-8.
fn extract_first_json_object(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    let start = raw.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &c) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
            continue;
        }
        match c {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&raw[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Ground a Mick intent into a trajectory: the intent overrides ONLY the goal /
/// maneuver on the perception-derived `world` plan input; everything safety-bearing
/// (corridor, objects, posture, channels) is re-borrowed unchanged, and Occy plans
/// within it. The returned trajectory is still a *proposal* — the #131 checker
/// validates it downstream. Fail-closed on a non-finite intent (→ safe stop).
///
/// Generic over the planner so the same grounding holds for any doer behind the
/// seam (the geometric Occy today, a learned planner tomorrow).
pub fn plan_for_intent(
    planner: &mut impl Planner,
    intent: &MickIntent,
    world: &PlanInput,
) -> PlanOutput {
    // Junction wiring: when a lane graph is supplied and the integrator did NOT hand the
    // corresponding list, derive it from the map — `cedes_to_ego_ids` from the right-of-way
    // (`junction_context`, so the ego asserts map-granted priority over yielding-lane agents)
    // and `controls` from the approach lane's STOP / YIELD sign (`derive_controls`, so the
    // ego stops/slows at the junction). Fail-safe — no graph / ego off-map → empty (yield to
    // all, no extra stop); an integrator-supplied list is never overridden; KIRRA still
    // backstops every agent and bounds the motion. (`must_yield_to` needs no wiring: the
    // planner already yields to every non-cede agent; that set is the parko-boundary input.)
    let derived_cedes: Vec<u64> = if world.lane_graph.is_some() && world.cedes_to_ego_ids.is_empty() {
        derive_cedes_to_ego(world)
    } else {
        Vec::new()
    };
    let derived_controls: Vec<TrafficControl> = if world.lane_graph.is_some() && world.controls.is_empty() {
        derive_controls(world)
    } else {
        Vec::new()
    };
    // Map-intention predicted paths: when a lane graph is supplied and the integrator did NOT hand
    // per-object predicted paths, derive the lane-follow hypothesis for each moving object from the
    // map (`LaneGraph::lane_follow_path`). The planner's predictive yield worst-cases over modes, so
    // this both CATCHES a vehicle that will follow a curving lane into the ego's path (a kinematic
    // CV/CTRV rollout cannot know the road bends) and SUPPRESSES a spurious yield to one keeping its
    // own diverging lane. `derived_paths_pts` owns the point vectors; `derived_paths` borrows them —
    // both live to the end of the function so the enriched world can reference them.
    let derived_paths_pts: Vec<(u64, Vec<MapPoint>)> = if world.lane_graph.is_some() && world.predicted_paths.is_empty() {
        derive_predicted_paths(world)
    } else {
        Vec::new()
    };
    let derived_paths: Vec<PredictedPath> =
        derived_paths_pts.iter().map(|(id, pts)| PredictedPath { id: *id, points: pts }).collect();
    let enriched: PlanInput;
    let world: &PlanInput = if !derived_cedes.is_empty() || !derived_controls.is_empty() || !derived_paths.is_empty() {
        enriched = PlanInput {
            cedes_to_ego_ids: if derived_cedes.is_empty() { world.cedes_to_ego_ids } else { &derived_cedes },
            controls: if derived_controls.is_empty() { world.controls } else { &derived_controls },
            predicted_paths: if derived_paths.is_empty() { world.predicted_paths } else { &derived_paths },
            ..world.clone()
        };
        &enriched
    } else {
        world
    };
    match *intent {
        MickIntent::Hold => PlanOutput::safe_stop(world.ego.pose),
        MickIntent::GoTo { x_m, y_m } => {
            if !x_m.is_finite() || !y_m.is_finite() {
                return PlanOutput::safe_stop(world.ego.pose);
            }
            // Override ONLY the goal; keep the world's heading reference.
            let goal = Goal {
                target: Pose { x_m, y_m, heading_rad: world.goal.target.heading_rad },
            };
            planner.plan(&PlanInput { goal, ..world.clone() })
        }
        MickIntent::LaneChange { target_offset_m } => {
            if !target_offset_m.is_finite() {
                return PlanOutput::safe_stop(world.ego.pose);
            }
            planner.plan(&PlanInput { lane_change_to_m: Some(target_offset_m), ..world.clone() })
        }
        MickIntent::Cruise { target_speed_mps } => {
            if !target_speed_mps.is_finite() {
                return PlanOutput::safe_stop(world.ego.pose);
            }
            // A negative "cruise" is a hold, not reverse → cap at 0. The planner then
            // clamps to the posture ceiling and KIRRA caps again, so this only ever slows.
            planner.plan(&PlanInput {
                target_speed_mps: Some(target_speed_mps.max(0.0)),
                ..world.clone()
            })
        }
        MickIntent::Overtake => {
            // Request the discretionary pass; Occy honors it only if a drivable area is
            // present and the pass fits + the lane line is crossable (else it stays in lane),
            // and KIRRA bounds it (head-on RSS). Nothing unsafe flows from the request itself.
            planner.plan(&PlanInput { request_overtake: true, ..world.clone() })
        }
        MickIntent::PullOver => {
            // Request the edge-park-and-stop; Occy honors it only if the rightward move is
            // lawful and fits the corridor (else it stays in lane), a nearer hazard still
            // stops the ego first, and KIRRA bounds the parked pose. Safe by construction.
            planner.plan(&PlanInput { request_pull_over: true, ..world.clone() })
        }
        MickIntent::TurnAt { direction } => {
            // Resolve the turn against the lane graph, FAIL-CLOSED at every step (no graph
            // / ego off-map / no branch that way / unroutable / degenerate corridor → HOLD).
            // On success, follow the materialized route corridor through the junction; KIRRA
            // bounds it like any corridor (a too-tight turn is refused — proven in #526).
            let Some(graph) = world.lane_graph else {
                return PlanOutput::safe_stop(world.ego.pose);
            };
            let ego_pt = MapPoint { x_m: world.ego.pose.x_m, y_m: world.ego.pose.y_m };
            let Some(ego_lane) = graph.lane_at(ego_pt) else {
                return PlanOutput::safe_stop(world.ego.pose);
            };
            let Some(route) = turn_route(graph, ego_lane, direction) else {
                return PlanOutput::safe_stop(world.ego.pose);
            };
            // UNPROTECTED-TURN GAP-ACCEPTANCE: before committing the turn, check there is an
            // adequate gap in the traffic the ego must yield to. The conflict point is the
            // junction (the ego lane's terminus); the conflicting stream is every perceived
            // vehicle CLOSING on it that the ego does NOT have asserted priority over (i.e. not on
            // the right-of-way cede set). No adequate gap → HOLD at the junction and wait; KIRRA's
            // head-on / crossing RSS independently backstops a misjudged acceptance. A protected
            // turn (conflicting vehicles are on the cede set) is not gated.
            if let Some(term) = graph.lane(ego_lane).and_then(|l| l.centerline.last().copied()) {
                let conflicts = turn_conflict_approaches(world, term, world.cedes_to_ego_ids);
                if !accept_turn_gap(&conflicts, DEFAULT_TURN_CRITICAL_GAP_S) {
                    return PlanOutput::safe_stop(world.ego.pose);
                }
            }
            // Follow the turn's route corridor; the goal is unchanged (the turn drives toward
            // whatever the world goal already is). KIRRA bounds it like any corridor.
            plan_along_route(planner, world, graph, ego_lane, &route, world.goal)
        }
        MickIntent::RouteTo { x_m, y_m } => {
            // Multi-junction routing: resolve ego + destination lanes and plan the lane-id
            // route between them, FAIL-CLOSED at every step (no graph / ego off-map /
            // destination off-map / unreachable → HOLD). `route_to_point` (Dijkstra) takes
            // the correct turn at EACH junction; `plan_along_route` materializes that whole
            // route's corridor (curving through every junction) and follows it. KIRRA bounds
            // the corridor exactly as it bounds a single-junction turn.
            if !x_m.is_finite() || !y_m.is_finite() {
                return PlanOutput::safe_stop(world.ego.pose);
            }
            let Some(graph) = world.lane_graph else {
                return PlanOutput::safe_stop(world.ego.pose);
            };
            let ego_pt = MapPoint { x_m: world.ego.pose.x_m, y_m: world.ego.pose.y_m };
            let Some(ego_lane) = graph.lane_at(ego_pt) else {
                return PlanOutput::safe_stop(world.ego.pose);
            };
            let Some(route) = graph.route_to_point(ego_lane, MapPoint { x_m, y_m }) else {
                return PlanOutput::safe_stop(world.ego.pose);
            };
            // Drive toward the destination point (world frame), keeping the world's heading
            // reference — the same goal override `GoTo` uses, but along the routed corridor.
            let goal = Goal {
                target: Pose { x_m, y_m, heading_rad: world.goal.target.heading_rad },
            };
            plan_along_route(planner, world, graph, ego_lane, &route, goal)
        }
    }
}

/// Materialize a lane-id `route` into its drivable corridor and plan along it toward `goal`.
/// The shared grounding behind the single-junction [`MickIntent::TurnAt`] and the
/// multi-junction [`MickIntent::RouteTo`]: both reduce to "follow THIS route's corridor",
/// differing only in how the route is chosen and whether the goal is overridden.
///
/// Materializes three map-derived inputs (each only when the integrator didn't hand-supply
/// the corresponding field, so an explicit input is never overridden):
/// * the reference corridor ([`LaneGraph::route_corridor`]) the path follows — `map`;
/// * the widened drivable area ([`LaneGraph::route_drivable`]) a route-around / lane-change
///   may borrow within a turn — `drivable`;
/// * the typed lane boundaries over the ego lane + its lateral neighbors — `lane_boundaries`.
///
/// **Fail-closed:** a degenerate route corridor (`route_corridor` → `None`) HOLDs. KIRRA
/// bounds the materialized corridor regardless — the planner only PROPOSES along it.
fn plan_along_route(
    planner: &mut impl Planner,
    world: &PlanInput,
    graph: &LaneGraph,
    ego_lane: u64,
    route: &[u64],
    goal: Goal,
) -> PlanOutput {
    let Some(corridor) = graph.route_corridor(route, ROUTE_CORRIDOR_CONFIDENCE, ROUTE_CORRIDOR_AGE_MS)
    else {
        return PlanOutput::safe_stop(world.ego.pose);
    };
    // Widen the route: its full width (route + lateral neighbors) is the `drivable` area a
    // route-around / lane-change may borrow WITHIN the route, and the typed lines over the ego
    // lane + its neighbors gate that lateral move. Only when the integrator didn't supply them;
    // `None`/empty → the single-lane route-follow.
    let drivable = if world.drivable.is_none() {
        graph.route_drivable(route, ROUTE_CORRIDOR_CONFIDENCE, ROUTE_CORRIDOR_AGE_MS)
    } else {
        None
    };
    let lane = graph.lane(ego_lane);
    let neighbors: Vec<u64> = std::iter::once(ego_lane)
        .chain(lane.and_then(Lane::left_neighbor))
        .chain(lane.and_then(Lane::right_neighbor))
        .collect();
    let boundaries = if world.lane_boundaries.is_empty() {
        // Curve-correct: the lane-line offsets are measured in the ego's Frenet frame at its
        // current station (not each lane's global mean_y), so the crossing rules see a neighbor
        // boundary where it actually is through a turn — admitting/blocking a lateral move
        // correctly on the arc, not just on straights.
        graph.boundaries_relative_to_at(ego_lane, &neighbors, MapPoint { x_m: world.ego.pose.x_m, y_m: world.ego.pose.y_m })
    } else {
        None
    };
    planner.plan(&PlanInput {
        goal,
        map: &corridor,
        drivable: drivable
            .as_ref()
            .map(|d| d as &dyn CorridorSource)
            .or(world.drivable),
        lane_boundaries: boundaries.as_deref().unwrap_or(world.lane_boundaries),
        ..world.clone()
    })
}

/// Map-server health stamped on a route corridor materialized for a `TurnAt`: fresh and
/// confident, so the checker's corridor-health gate admits it (the geometry, not staleness,
/// is what a turn is judged on).
const ROUTE_CORRIDOR_CONFIDENCE: f32 = 0.95;
const ROUTE_CORRIDOR_AGE_MS: u64 = 0;

/// Derive the `cedes_to_ego` set from the lane graph's right-of-way for the ego at its
/// current pose — the agents the map says yield to the ego (so the planner asserts priority
/// over them rather than waiting). Empty if there is no graph or the ego is off the mapped
/// road (fail-safe → yield to all). Uses [`LaneGraph::junction_context`], the same source
/// Parko's non-yielding set is derived from, so the two cannot disagree.
fn derive_cedes_to_ego(world: &PlanInput<'_>) -> Vec<u64> {
    match world.lane_graph {
        Some(graph) => {
            let ego_pt = MapPoint { x_m: world.ego.pose.x_m, y_m: world.ego.pose.y_m };
            graph.junction_context(ego_pt, world.objects).cedes_to_ego
        }
        None => Vec::new(),
    }
}

/// Below this closing speed a vehicle is not meaningfully approaching the turn conflict point —
/// it is stopped, receding, or moving tangentially — so it is not a gap-acceptance conflict.
const TURN_CONFLICT_MIN_CLOSING_MPS: f64 = 0.5;

/// Build the [`ConflictApproach`] list for a turn whose conflict point is `conflict` (the junction):
/// every perceived vehicle that is CLOSING on the conflict and which the ego must yield to — i.e.
/// NOT on its right-of-way `cedes` set. Each contributes its time-to-conflict
/// (`distance / closing-speed`). A vehicle that is not closing (stopped / receding / tangential) is
/// excluded — it is not a conflict — while one already AT the conflict yields `0.0` (an immediate
/// hold). This is the conservative junction proxy: any vehicle bearing down on the junction the ego
/// has no priority over gates the turn, and KIRRA's RSS backstops regardless.
fn turn_conflict_approaches(world: &PlanInput<'_>, conflict: MapPoint, cedes: &[u64]) -> Vec<ConflictApproach> {
    world
        .objects
        .iter()
        .filter(|o| !cedes.contains(&o.id))
        .filter_map(|o| {
            let (dx, dy) = (conflict.x_m - o.pos.x_m, conflict.y_m - o.pos.y_m);
            let dist = dx.hypot(dy);
            if dist <= f64::EPSILON {
                return Some(ConflictApproach { time_to_conflict_s: 0.0 }); // already at the conflict
            }
            // Closing speed = the object's velocity component along the bearing to the conflict.
            let closing = (o.vel.x_m * dx + o.vel.y_m * dy) / dist;
            (closing > TURN_CONFLICT_MIN_CLOSING_MPS)
                .then_some(ConflictApproach { time_to_conflict_s: dist / closing })
        })
        .collect()
}

/// Horizon (s) over which a map-intention predicted path is materialized — a generous fixed
/// value (the predictive-yield walker clamps a path shorter than `speed × horizon`). Matches the
/// planner's default `prediction_horizon_s`; `plan_for_intent` is planner-generic so it cannot
/// read the planner's config.
const MICK_PREDICT_HORIZON_S: f64 = 3.0;
/// Object speed (m/s) below which no lane-follow path is derived — a near-stationary object is
/// handled by stop-short, not predictive yield (which also gates on a crossing-speed threshold).
const MICK_PREDICT_MIN_SPEED_MPS: f64 = 0.5;
/// Extra path length (m) beyond `speed × horizon`, so the materialized path comfortably covers
/// the yield walker's reach even as the object accelerates.
const MICK_PREDICT_LENGTH_MARGIN_M: f64 = 5.0;

/// Derive the map-intention predicted path for each MOVING object on the mapped road — the
/// lane-follow hypothesis (`LaneGraph::lane_follow_path`) the planner's predictive yield consumes
/// alongside its kinematic CV/CTRV rollout. Empty if there is no graph; an object off the mapped
/// road or below [`MICK_PREDICT_MIN_SPEED_MPS`] is skipped (its kinematic rollout still applies).
/// Returns owned `(id, points)`; the caller borrows them into `PredictedPath`s.
fn derive_predicted_paths(world: &PlanInput<'_>) -> Vec<(u64, Vec<MapPoint>)> {
    let Some(graph) = world.lane_graph else {
        return Vec::new();
    };
    world
        .objects
        .iter()
        .filter_map(|o| {
            let speed = o.vel.x_m.hypot(o.vel.y_m);
            if speed < MICK_PREDICT_MIN_SPEED_MPS {
                return None;
            }
            let length = speed * MICK_PREDICT_HORIZON_S + MICK_PREDICT_LENGTH_MARGIN_M;
            graph.lane_follow_path(o.pos, length).map(|pts| (o.id, pts))
        })
        .collect()
}

/// Speed (m/s) below which the ego counts as having stopped for a STOP-sign full stop.
const STOP_SATISFIED_SPEED_MPS: f64 = 0.3;
/// Distance (m) before the stop line within which a near-stopped ego is `satisfied`.
const STOP_SATISFIED_DIST_M: f64 = 2.0;

/// Derive the regulatory [`TrafficControl`]s the ego currently faces from the lane graph:
/// the STOP / YIELD sign or TRAFFIC LIGHT at the end of the ego's lane (its junction
/// approach), mapped to the behavioral-layer control. Empty if there is no graph, the ego is
/// off the mapped road, or its lane carries no control (fail-safe → nothing beyond what
/// objects/posture impose).
///
/// STOP is **stateless stop-and-go**: `satisfied` once the ego is essentially stopped just
/// before the line (`< STOP_SATISFIED_SPEED_MPS` within `STOP_SATISFIED_DIST_M`), so it
/// proceeds. This approximates the legal full-stop dwell without loop memory — in the closed
/// loop the ego decelerates to the line then creeps across; a precisely-latched full-stop
/// dwell is a tracked follow-up. KIRRA bounds the actual motion regardless. YIELD is exact
/// (a speed cap at the line). A TRAFFIC LIGHT takes its live state from `world.signal_states`
/// (keyed by the governed lane id), **fail-closed to red** when absent — the behavioral
/// layer then holds on red / amber-dilemma and clears on green.
fn derive_controls(world: &PlanInput<'_>) -> Vec<TrafficControl> {
    let Some(graph) = world.lane_graph else {
        return Vec::new();
    };
    let ego = world.ego.pose;
    let Some(lane_id) = graph.lane_at(MapPoint { x_m: ego.x_m, y_m: ego.y_m }) else {
        return Vec::new();
    };
    let lane = graph.lane(lane_id);
    let mut out: Vec<TrafficControl> = Vec::new();

    // Regulatory sign / signal at the lane terminus (its junction approach).
    if let Some((control, line_x)) = lane.and_then(|l| l.control.map(|c| (c, l.stop_line_x()))) {
        out.push(match control {
            LaneControl::Yield => TrafficControl::YieldSign { line_x_m: line_x },
            LaneControl::Stop => {
                let dist = line_x - ego.x_m;
                let satisfied = world.ego.linear_x_mps.abs() < STOP_SATISFIED_SPEED_MPS
                    && dist > 0.0
                    && dist < STOP_SATISFIED_DIST_M;
                TrafficControl::StopSign { stop_line_x_m: line_x, satisfied }
            }
            LaneControl::TrafficLight => {
                // Live signal state for the governed (ego) lane — fail-closed to RED when the
                // perception/V2X feed has no entry, so an unknown light HOLDS rather than runs it.
                let state = world
                    .signal_states
                    .iter()
                    .find(|(id, _)| *id == lane_id)
                    .map_or(SignalState::Red, |(_, s)| *s);
                TrafficControl::TrafficLight { stop_line_x_m: line_x, state }
            }
        });
    }

    // Occluded junction approach: if the ego lane has limited cross-traffic visibility, derive
    // the assured-clear-distance speed cap toward its terminus (the conflict line) so the ego
    // creeps into the blind junction (RSS Rule 4). Composes with any sign control above (a blind
    // STOP/YIELD approach gets both the stop/yield line AND the creep cap). A lane with an open
    // view contributes nothing.
    if let (Some(sight), Some(conflict_x)) = (graph.sight_distance(lane_id), lane.map(Lane::stop_line_x)) {
        out.push(TrafficControl::OccludedApproach { conflict_line_x_m: conflict_x, sight_distance_m: sight });
    }

    out
}

/// Pick the ego lane's successor that turns `direction` (successor-by-heading): the matching
/// successor whose heading change from the ego lane is smallest in magnitude, ties by id for
/// determinism. `None` if no successor turns that way.
fn turn_target(graph: &LaneGraph, ego_lane: u64, direction: TurnDirection) -> Option<u64> {
    let ego = graph.lane(ego_lane)?;
    let mut best: Option<(u64, f64)> = None; // (lane id, |delta heading|)
    for s in ego.successors() {
        let Some(succ) = graph.lane(s) else { continue };
        let delta = wrap_pi(succ.heading_rad - ego.heading_rad);
        if direction.matches(delta) {
            let score = delta.abs();
            // Smaller heading change wins; equal scores break by lower id (deterministic).
            if best.is_none_or(|(bid, bscore)| score < bscore || (score == bscore && s < bid)) {
                best = Some((s, score));
            }
        }
    }
    best.map(|(id, _)| id)
}

/// The net heading change along a lane's centerline (first segment → last segment), wrapped
/// to `(−π, π]`: ~0 for a straight lane, ±π/2 for a quarter-arc. Lets `turn_route` recognize
/// that the ego is already ON a turning lane (mid-arc) without any per-tick route state.
fn lane_net_heading_change(lane: &Lane) -> f64 {
    let c = &lane.centerline;
    if c.len() < 3 {
        return 0.0; // a 2-point (straight) lane has no measurable curvature
    }
    let seg = |a: &MapPoint, b: &MapPoint| (b.y_m - a.y_m).atan2(b.x_m - a.x_m);
    let start = seg(&c[0], &c[1]);
    let end = seg(&c[c.len() - 2], &c[c.len() - 1]);
    wrap_pi(end - start)
}

/// The route through a `direction` turn: the ego lane, the chosen turn branch, then the
/// branch's forward successors (deterministic lowest-id, cycle-guarded, bounded by
/// `MAX_ROUTE_LANES`) so the route corridor spans the approach, the turn, and the exit.
///
/// **Route-progress (the turn completes in a re-planning loop):** if no successor turns
/// `direction` from the ego lane BUT the ego lane is itself a `direction`-curving lane — i.e.
/// the ego has already entered the arc — the route *continues* along that committed arc to the
/// exit rather than HOLDing. A re-issued `TurnAt` therefore drives the whole turn, not just the
/// approach. A *straight* lane with no matching branch still returns `None` (fail-closed: it
/// cannot turn that way here), because a straight lane has ~0 net heading change.
fn turn_route(graph: &LaneGraph, ego_lane: u64, direction: TurnDirection) -> Option<Vec<u64>> {
    let mut route = match turn_target(graph, ego_lane, direction) {
        Some(branch) => vec![ego_lane, branch],
        None => {
            // Continue ONLY a genuine LEFT/RIGHT arc the ego is already on — its centerline
            // bends that way (`matches` requires ≥45°, so this is a real curve). `Straight` has
            // no committed arc to continue, and a straight lane's ~0 net heading change would
            // otherwise spuriously match it, so a `Straight` with no straight branch still HOLDs
            // (fail-closed), as does any direction on a straight (non-arc) lane.
            let lane = graph.lane(ego_lane)?;
            if direction != TurnDirection::Straight && direction.matches(lane_net_heading_change(lane)) {
                vec![ego_lane] // mid-turn continuation; extended forward below
            } else {
                return None;
            }
        }
    };
    let mut cur = *route.last().expect("route seeded with at least the ego lane");
    while route.len() < MAX_ROUTE_LANES {
        match graph.lane(cur).and_then(|l| l.successors().min()) {
            Some(next) if !route.contains(&next) => {
                route.push(next);
                cur = next;
            }
            _ => break,
        }
    }
    // The continuation case must actually advance (have a successor); a dead-end mid-arc lane
    // with no onward route is just HOLD, same as the no-branch approach case.
    (route.len() >= 2).then_some(route)
}

/// Wrap an angle to `(−π, π]` (the heading-difference frame `TurnDirection::matches` reads).
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

// ===========================================================================
// The Mick BRAIN seam — the pluggable System-2 driver.
//
// `plan_for_intent` above grounds ONE intent. The pieces below close the loop: a
// bounded, owned snapshot of the world (`WorldContext`) the brain reasons over, the
// `MickBrain` trait a model plugs into, and `mick_drive_once` — ask the brain → ground
// through Occy → (downstream) KIRRA bounds it. The brain is NEVER trusted: it authors
// INTENT only, sees a derived view (never the safety-bearing borrows), and any failure
// fails closed to a safe stop. The whole point of the chauffeur is that Mick may be as
// smart — or as wrong — as it likes, because the doer grounds and the checker bounds.
// ===========================================================================

/// Max objects surfaced to the brain — bounds the prompt size and per-tick work. Excess
/// objects are dropped *after* a nearest-first sort, so the closest (most relevant) are
/// always kept; KIRRA still sees every object regardless of what the brain was shown.
pub const MICK_MAX_OBJECTS: usize = 24;

/// Lateral probe distance (m) used to report whether a lane change to each side is
/// lawful. A context hint only — the real crossing rule is enforced when the maneuver
/// grounds (`behavior::lateral_move_permitted`), so an over/under-eager hint cannot make
/// an unlawful change happen.
const MICK_LANE_PROBE_M: f64 = 2.0;

/// Error from Mick's brain. ANY failure — parse error, timeout, refusal, a non-finite
/// or out-of-vocabulary intent — collapses to this, and the caller HOLDs (fail-closed).
/// The brain is never trusted, so a failure is simply "no new intent → keep it safe."
pub type MickError = &'static str;

/// One nearby object as the brain sees it — **ego-relative**, finite, bounded. The
/// brain never receives raw world borrows; it gets this owned view.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ObjectView {
    pub id: u64,
    /// Longitudinal distance from the ego along its heading (+ = ahead), meters.
    pub ahead_m: f64,
    /// Lateral offset from the ego (+ = to the ego's left), meters.
    pub left_m: f64,
    /// Object ground speed magnitude, m/s.
    pub speed_mps: f64,
}

/// An owned, bounded, finite snapshot of the world for Mick's brain — the factual
/// content of the prompt. The brain reasons over THIS, never the borrowed `PlanInput`;
/// grounding re-borrows the real world, so nothing the brain "sees" can be smuggled into
/// a safety-bearing field. Ego-relative where that aids the model. `Serialize` so it
/// renders straight into a prompt (the model backend owns the exact framing).
#[derive(Debug, Clone, Serialize)]
pub struct WorldContext {
    /// Ego forward speed, m/s.
    pub ego_speed_mps: f64,
    /// Fleet posture token (`NOMINAL` / `DEGRADED` / `LOCKED_OUT`). The brain should be
    /// more conservative off-Nominal; the stack enforces it regardless.
    pub posture: &'static str,
    /// The current goal in the ego frame: meters ahead / to the left of the ego.
    pub goal_ahead_m: f64,
    pub goal_left_m: f64,
    /// Whether a lane change to each side is lawful per the lane lines (a hard rule the
    /// brain need not re-derive — Occy enforces it on grounding).
    pub may_change_left: bool,
    pub may_change_right: bool,
    /// Nearby objects, ego-relative, NEAREST-FIRST, capped at [`MICK_MAX_OBJECTS`].
    pub objects: Vec<ObjectView>,
    /// Turn branches available at the next junction (`left` / `right` / `straight`), derived
    /// from the lane graph when one is supplied — so the brain only chooses `turn_at` where a
    /// branch actually exists (a `turn_at` with no such branch fails closed to HOLD anyway).
    /// Empty when there is no graph or the ego is off the mapped road.
    pub available_turns: Vec<&'static str>,
}

impl WorldContext {
    /// Derive the brain's view from the perception/map-derived `PlanInput`. Pure: copies
    /// out owned, finite, ego-relative facts; surfaces no borrow and no actuator handle.
    #[must_use]
    pub fn from_plan_input(world: &PlanInput<'_>) -> Self {
        let ego = world.ego.pose;
        let (sin_h, cos_h) = ego.heading_rad.sin_cos();
        // World point → ego frame (ahead along heading, left along the left-normal).
        let to_ego = |x: f64, y: f64| -> (f64, f64) {
            let (dx, dy) = (x - ego.x_m, y - ego.y_m);
            (dx * cos_h + dy * sin_h, -dx * sin_h + dy * cos_h)
        };

        let (goal_ahead_m, goal_left_m) = to_ego(world.goal.target.x_m, world.goal.target.y_m);

        let mut objects: Vec<ObjectView> = world
            .objects
            .iter()
            .map(|o| {
                let (ahead_m, left_m) = to_ego(o.pos.x_m, o.pos.y_m);
                ObjectView { id: o.id, ahead_m, left_m, speed_mps: o.velocity_mps }
            })
            .collect();
        // Nearest-first so the truncation keeps the most relevant objects.
        objects.sort_by(|a, b| {
            a.ahead_m.hypot(a.left_m).total_cmp(&b.ahead_m.hypot(b.left_m))
        });
        objects.truncate(MICK_MAX_OBJECTS);

        WorldContext {
            ego_speed_mps: world.ego.linear_x_mps,
            posture: posture_token(world.posture.clone()),
            goal_ahead_m,
            goal_left_m,
            may_change_left: crate::behavior::lateral_move_permitted(
                world.lane_boundaries, 0.0, MICK_LANE_PROBE_M,
            ),
            may_change_right: crate::behavior::lateral_move_permitted(
                world.lane_boundaries, 0.0, -MICK_LANE_PROBE_M,
            ),
            objects,
            available_turns: available_turns(world),
        }
    }
}

/// The turn branches available to the ego at the next junction, derived from the
/// `lane_graph` (empty if none / ego off-map). Uses the same successor-by-heading
/// resolution `TurnAt` grounds with, so what the brain is offered is exactly what will
/// ground.
fn available_turns(world: &PlanInput<'_>) -> Vec<&'static str> {
    let Some(graph) = world.lane_graph else {
        return Vec::new();
    };
    let ego_pt = MapPoint { x_m: world.ego.pose.x_m, y_m: world.ego.pose.y_m };
    let Some(ego_lane) = graph.lane_at(ego_pt) else {
        return Vec::new();
    };
    [TurnDirection::Left, TurnDirection::Right, TurnDirection::Straight]
        .into_iter()
        .filter(|d| turn_target(graph, ego_lane, *d).is_some())
        .map(TurnDirection::as_str)
        .collect()
}

/// The pluggable System-2 brain behind Mick. Given the bounded [`WorldContext`], it
/// returns a high-level [`MickIntent`] — or an `Err`, on which the caller fail-closes to
/// a safe stop. This is the ONLY seam a model plugs into: a local Gemma, a cloud model,
/// or a scripted policy. The brain authors INTENT only; Occy grounds it and KIRRA bounds
/// it, so the safety case is independent of how good — or bad, or adversarial — it is.
pub trait MickBrain {
    /// Decide the next intent for `ctx`. Returning `Err` means "no usable intent" and the
    /// caller HOLDs — the brain is expected to be fallible and is never trusted for safety.
    fn decide(&mut self, ctx: &WorldContext) -> Result<MickIntent, MickError>;
}

/// A deterministic stub brain for tests / sim: replays a fixed intent script, then HOLDs.
/// Exercises the whole Mick → Occy → KIRRA loop — including deliberately adversarial
/// intents — with zero model dependency.
pub struct ScriptedBrain {
    script: std::vec::IntoIter<MickIntent>,
}

impl ScriptedBrain {
    #[must_use]
    pub fn new(intents: Vec<MickIntent>) -> Self {
        Self { script: intents.into_iter() }
    }
}

impl MickBrain for ScriptedBrain {
    fn decide(&mut self, _ctx: &WorldContext) -> Result<MickIntent, MickError> {
        // Past the end of the script, keep driving safely rather than erroring.
        Ok(self.script.next().unwrap_or(MickIntent::Hold))
    }
}

/// One tick of the Mick chauffeur loop: derive the brain's view of the world, ask the
/// brain for an intent, and ground it through Occy. **Fail-closed**: a brain error yields
/// a safe stop, never a propagated bad command. The returned `PlanOutput` is STILL a
/// proposal — the #131 checker (KIRRA) bounds it downstream, so even a malicious intent
/// that grounds into a trajectory cannot reach the actuator unchecked.
///
/// Generic over both the brain and the planner: any model behind the seam, any doer.
pub fn mick_drive_once(
    brain: &mut impl MickBrain,
    world: &PlanInput<'_>,
    planner: &mut impl Planner,
) -> PlanOutput {
    let ctx = WorldContext::from_plan_input(world);
    match brain.decide(&ctx) {
        Ok(intent) => plan_for_intent(planner, &intent, world),
        // The brain failed / refused → HOLD. The doer never invents motion on a
        // brain fault; the safe disposition is a controlled stop.
        Err(_) => PlanOutput::safe_stop(world.ego.pose),
    }
}

/// Default System-2 cadence: re-ask the brain for an intent at ~2 Hz. A local 4B model
/// cannot be called at the fast-loop rate (10–50 Hz) on a vehicle, and the *maneuver*
/// rarely needs to change that fast. VALIDATION-PENDING (tune per model latency + ODD).
pub const DEFAULT_DECIDE_INTERVAL_MS: u64 = 500;
/// Default intent staleness bound: if the brain has produced no fresh intent within this
/// window (it is timing out / erroring), the driver fails closed to `Hold` rather than
/// grounding an arbitrarily-old maneuver. ~4× the decide interval — tolerates a few missed
/// decisions before holding. VALIDATION-PENDING.
pub const DEFAULT_INTENT_STALENESS_MS: u64 = 2_000;

/// **The dual-rate Mick driver — the deployable form of the brain seam.**
///
/// `mick_drive_once` asks the brain *every* call; that is fine for sim but wrong for a
/// vehicle, where the brain is a slow System-2 model. `MickDriver` separates the two rates:
/// the **slow path** re-asks the brain for an *intent* only every `decide_interval_ms`
/// (System-2), while the **fast path** grounds the cached intent against the FRESH world on
/// *every* tick (System-1) — so the trajectory tracks live perception even though the
/// maneuver is stable, and KIRRA still bounds every grounded trajectory.
///
/// **Fail-closed on a stale brain.** A re-decide that fails keeps the last cached intent
/// (still safe — it is re-grounded live and re-checked by KIRRA), but the intent *ages*; if
/// no fresh intent arrives within `intent_staleness_ms`, the driver grounds `Hold` instead
/// (a controlled stop), mirroring the posture-tracker staleness rule. Cold start with no
/// intent yet also grounds `Hold`.
pub struct MickDriver<B: MickBrain> {
    brain: B,
    decide_interval_ms: u64,
    intent_staleness_ms: u64,
    /// The last intent the brain produced and when (`now_ms`). `None` until the first
    /// successful decision.
    cached: Option<(MickIntent, u64)>,
}

impl<B: MickBrain> MickDriver<B> {
    /// Construct with the default System-2 cadence + staleness bound.
    #[must_use]
    pub fn new(brain: B) -> Self {
        Self::with_rates(brain, DEFAULT_DECIDE_INTERVAL_MS, DEFAULT_INTENT_STALENESS_MS)
    }

    /// Construct with explicit `decide_interval_ms` (re-decide cadence) and
    /// `intent_staleness_ms` (beyond which a non-refreshed intent → `Hold`).
    #[must_use]
    pub fn with_rates(brain: B, decide_interval_ms: u64, intent_staleness_ms: u64) -> Self {
        Self { brain, decide_interval_ms, intent_staleness_ms, cached: None }
    }

    /// The current cached intent (for observability / tests), if any.
    #[must_use]
    pub fn current_intent(&self) -> Option<MickIntent> {
        self.cached.map(|(intent, _)| intent)
    }

    /// One fast-loop tick at wall-clock `now_ms`: re-ask the brain only if the System-2
    /// interval has elapsed (or there is no intent yet), then ground the current intent
    /// against the fresh `world`. Always returns a grounded `PlanOutput` — a stale/absent
    /// intent grounds `Hold` (fail-closed). The result is still a proposal KIRRA bounds.
    pub fn drive_tick(
        &mut self,
        world: &PlanInput<'_>,
        planner: &mut impl Planner,
        now_ms: u64,
    ) -> PlanOutput {
        // Slow path: re-decide when due (interval elapsed) or no intent cached yet.
        let due = match self.cached {
            Some((_, decided_at)) => now_ms.saturating_sub(decided_at) >= self.decide_interval_ms,
            None => true,
        };
        if due {
            let ctx = WorldContext::from_plan_input(world);
            if let Ok(intent) = self.brain.decide(&ctx) {
                self.cached = Some((intent, now_ms));
            }
            // On a brain failure we KEEP the (now-ageing) cached intent — it is still
            // re-grounded live and re-checked by KIRRA — and let the staleness gate below
            // decide whether it is too old to use.
        }

        // Choose the intent to ground: the cached one iff still fresh, else fail closed.
        let intent = match self.cached {
            Some((intent, decided_at)) if now_ms.saturating_sub(decided_at) <= self.intent_staleness_ms => {
                intent
            }
            _ => MickIntent::Hold,
        };

        // Fast path: ground the chosen intent against the FRESH world, every tick.
        plan_for_intent(planner, &intent, world)
    }
}

/// Posture → stable prompt token. Kept in lock-step with `FleetPosture`.
fn posture_token(p: FleetPosture) -> &'static str {
    match p {
        FleetPosture::Nominal => "NOMINAL",
        FleetPosture::Degraded => "DEGRADED",
        FleetPosture::LockedOut => "LOCKED_OUT",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EgoState, GeometricPlanner, LaneBoundary, LineType, PerceivedObject, ProposalKind,
        TrajectoryVerdict,
    };
    use kirra_ros2_adapter::corridor::{CorridorSource, MockCorridorSource, Point};
    use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};
    use kirra_core::FleetPosture;

    /// A perception-derived world: ego at x=5, a placeholder goal (the intent
    /// overrides it), and whatever objects / lane lines the test supplies.
    fn world<'a>(
        map: &'a dyn CorridorSource,
        objects: &'a [PerceivedObject],
        lanes: &'a [LaneBoundary],
    ) -> PlanInput<'a> {
        PlanInput {
            ego: EgoState {
                pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 },
                linear_x_mps: 2.0,
                yaw_rate_rads: 0.0,
                stamp_ms: 0,
            },
            goal: Goal { target: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 } },
            map,
            objects,
            controls: &[],
            lane_boundaries: lanes,
            motion: &[],
            predicted_paths: &[],
            cedes_to_ego_ids: &[],
            lane_change_to_m: None,
            no_overtake_ids: &[],
            drivable: None,
            posture: FleetPosture::Nominal,
            target_speed_mps: None,
            request_overtake: false,
            request_pull_over: false,
            lane_graph: None,
            signal_states: &[],        }
    }

    fn stopped_car(x: f64) -> PerceivedObject {
        PerceivedObject {
            id: 1,
            pos: Point { x_m: x, y_m: 0.0 },
            velocity_mps: 0.0,
            heading_rad: 0.0,
            vel: Point { x_m: 0.0, y_m: 0.0 },
        }
    }

    fn admits(traj: &[crate::TrajectoryPoint], corr: &dyn CorridorSource, objs: &[PerceivedObject]) -> bool {
        matches!(
            validate_trajectory_slow(traj, corr, objs, &VehicleConfig::default_urban(), None, FleetPosture::Nominal),
            TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp
        )
    }

    #[test]
    fn reasonable_intent_plans_toward_the_goal_and_kirra_admits() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = world(&corr, &[], &[]);
        let mut p = GeometricPlanner::default();
        let out = plan_for_intent(&mut p, &MickIntent::GoTo { x_m: 40.0, y_m: 0.0 }, &w);
        assert_eq!(out.kind, ProposalKind::Motion);
        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(max_x > 10.0, "Mick's GoTo drives the ego toward the goal, got {max_x}");
        assert!(admits(&out.trajectory, &corr, &[]), "KIRRA admits the grounded plan");
    }

    #[test]
    fn unsafe_intent_is_grounded_by_occy_and_kirra_not_obeyed() {
        // Mick says "go to x=40", but a stopped car blocks the lane at x=25. Occy
        // grounds the intent — it STOPS SHORT of the obstacle rather than driving to
        // the point Mick named — and KIRRA admits the safe trajectory. The LLM's
        // intent does not override safety: the doer is bounded whatever it proposes.
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [stopped_car(25.0)];
        let w = world(&corr, &objs, &[]);
        let mut p = GeometricPlanner::default();
        let out = plan_for_intent(&mut p, &MickIntent::GoTo { x_m: 40.0, y_m: 0.0 }, &w);
        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(max_x < 25.0, "stops short of the obstacle Mick told it to drive past, got {max_x}");
        assert!(admits(&out.trajectory, &corr, &objs), "and the bounded plan is admissible");
    }

    #[test]
    fn unlawful_lane_change_intent_is_refused() {
        // Mick proposes a lane change across a SOLID line; Occy's behavioral layer
        // refuses it (stays in lane). Even a maneuver intent is adjudicated, not obeyed.
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let solid = [LaneBoundary { y_m: -0.5, line: LineType::Solid }];
        let w = PlanInput {
            goal: Goal { target: Pose { x_m: 40.0, y_m: 0.0, heading_rad: 0.0 } },
            ..world(&corr, &[], &solid)
        };
        let mut p = GeometricPlanner::default();
        let out = plan_for_intent(&mut p, &MickIntent::LaneChange { target_offset_m: -3.0 }, &w);
        let min_y = out.trajectory.iter().map(|t| t.pose.y_m).fold(0.0, f64::min);
        assert!(min_y > -0.5, "solid line → lane-change intent refused (no crossing), got {min_y}");
    }

    #[test]
    fn hold_intent_is_a_safe_stop() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = world(&corr, &[], &[]);
        let mut p = GeometricPlanner::default();
        let out = plan_for_intent(&mut p, &MickIntent::Hold, &w);
        assert_eq!(out.kind, ProposalKind::SafeStop);
        assert!(out.trajectory.iter().all(|t| t.velocity_mps == 0.0));
    }

    #[test]
    fn nonfinite_intent_fails_closed_to_a_safe_stop() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = world(&corr, &[], &[]);
        let mut p = GeometricPlanner::default();
        let out = plan_for_intent(&mut p, &MickIntent::GoTo { x_m: f64::NAN, y_m: 0.0 }, &w);
        assert_eq!(out.kind, ProposalKind::SafeStop, "a NaN goal must not flow into the planner");
    }

    #[test]
    fn malformed_or_adversarial_llm_json_is_fail_closed() {
        // No object; prose with only a stray brace; unknown action tag; and a
        // non-finite (overflow → Inf) number all reject — the caller HOLDs rather
        // than acting on a hallucination. The stray-brace case proves the tolerant
        // extractor widens *acceptance* without weakening the schema/finiteness gate.
        assert!(MickIntent::from_llm_json("the robot should floor it").is_err());
        assert!(MickIntent::from_llm_json("floor it {now}!").is_err());
        assert!(MickIntent::from_llm_json(r#"{"intent":"deploy_at_max_velocity"}"#).is_err());
        assert!(MickIntent::from_llm_json(r#"{"intent":"go_to","x_m":1e400,"y_m":0.0}"#).is_err());
        // A well-formed intent parses to the typed value.
        assert_eq!(
            MickIntent::from_llm_json(r#"{"intent":"go_to","x_m":40.0,"y_m":0.0}"#).unwrap(),
            MickIntent::GoTo { x_m: 40.0, y_m: 0.0 }
        );
        assert_eq!(MickIntent::from_llm_json(r#"{"intent":"hold"}"#).unwrap(), MickIntent::Hold);
    }

    #[test]
    fn gemma_style_fenced_or_preambled_output_still_parses() {
        // Small models (Gemma especially) wrap intent in a ```json fence, prepend a
        // sentence of prose, or trail an offer to help. The tolerant extractor
        // recovers the object instead of forcing a needless HOLD.
        let fenced = "```json\n{\"intent\":\"go_to\",\"x_m\":40.0,\"y_m\":0.0}\n```";
        assert_eq!(
            MickIntent::from_llm_json(fenced).unwrap(),
            MickIntent::GoTo { x_m: 40.0, y_m: 0.0 }
        );

        let preamble = "Sure — given the goal ahead, the intent is:\n{\"intent\":\"hold\"}";
        assert_eq!(MickIntent::from_llm_json(preamble).unwrap(), MickIntent::Hold);

        let trailing =
            "{\"intent\":\"lane_change\",\"target_offset_m\":-3.0}\nLet me know if you'd like to adjust.";
        assert_eq!(
            MickIntent::from_llm_json(trailing).unwrap(),
            MickIntent::LaneChange { target_offset_m: -3.0 }
        );

        // A brace inside a string value must not mis-terminate the object.
        let nested = "{\"intent\":\"go_to\",\"x_m\":1.0,\"y_m\":2.0,\"note\":\"pass the {gate}\"}";
        assert_eq!(
            MickIntent::from_llm_json(nested).unwrap(),
            MickIntent::GoTo { x_m: 1.0, y_m: 2.0 }
        );
    }

    // ----- the brain seam: WorldContext + MickBrain + mick_drive_once -----

    #[test]
    fn world_context_is_ego_relative() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        // Ego facing +y (heading π/2) at (5,0); a goal 40 m in +y is "40 ahead, 0 left".
        let w = PlanInput {
            ego: EgoState {
                pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: std::f64::consts::FRAC_PI_2 },
                linear_x_mps: 3.0,
                yaw_rate_rads: 0.0,
                stamp_ms: 0,
            },
            goal: Goal { target: Pose { x_m: 5.0, y_m: 40.0, heading_rad: 0.0 } },
            ..world(&corr, &[], &[])
        };
        let ctx = WorldContext::from_plan_input(&w);
        assert!((ctx.goal_ahead_m - 40.0).abs() < 1e-9, "goal 40 m ahead, got {}", ctx.goal_ahead_m);
        assert!(ctx.goal_left_m.abs() < 1e-9, "goal dead ahead (0 left), got {}", ctx.goal_left_m);
        assert_eq!(ctx.ego_speed_mps, 3.0);
        assert_eq!(ctx.posture, "NOMINAL");
    }

    #[test]
    fn world_context_objects_bounded_and_nearest_first() {
        let corr = MockCorridorSource::straight_5m_half_width(500.0);
        // More objects than the cap, at increasing distance ahead of the ego (x=5).
        let objs: Vec<PerceivedObject> = (0..(MICK_MAX_OBJECTS as u64 + 10))
            .map(|i| PerceivedObject {
                id: i,
                pos: Point { x_m: 10.0 + i as f64 * 5.0, y_m: 0.0 },
                velocity_mps: 1.0,
                heading_rad: 0.0,
                vel: Point { x_m: 1.0, y_m: 0.0 },
            })
            .collect();
        let w = world(&corr, &objs, &[]);
        let ctx = WorldContext::from_plan_input(&w);
        assert_eq!(ctx.objects.len(), MICK_MAX_OBJECTS, "the brain's object list is capped");
        assert_eq!(ctx.objects[0].id, 0, "nearest object (id 0 at x=10) is first");
        assert!(ctx.objects[0].ahead_m < ctx.objects[1].ahead_m, "sorted nearest-first");
    }

    #[test]
    fn scripted_brain_drives_the_loop_toward_the_goal() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = world(&corr, &[], &[]);
        let mut brain = ScriptedBrain::new(vec![MickIntent::GoTo { x_m: 40.0, y_m: 0.0 }]);
        let mut p = GeometricPlanner::default();
        let out = mick_drive_once(&mut brain, &w, &mut p);
        assert_eq!(out.kind, ProposalKind::Motion);
        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(max_x > 10.0, "the brain's GoTo drives the loop forward, got {max_x}");
        assert!(admits(&out.trajectory, &corr, &[]), "and KIRRA admits the grounded plan");
    }

    #[test]
    fn mick_loop_bounds_an_adversarial_brain() {
        // The brain insists on driving to x=40 straight through a stopped car at x=25.
        // The loop grounds it (stops short) and KIRRA admits the safe result — the brain
        // is not obeyed past the safety floor, end to end.
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let objs = [stopped_car(25.0)];
        let w = world(&corr, &objs, &[]);
        let mut brain = ScriptedBrain::new(vec![MickIntent::GoTo { x_m: 40.0, y_m: 0.0 }]);
        let mut p = GeometricPlanner::default();
        let out = mick_drive_once(&mut brain, &w, &mut p);
        let max_x = out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);
        assert!(max_x < 25.0, "loop stops short of the obstacle the brain drove at, got {max_x}");
        assert!(admits(&out.trajectory, &corr, &objs), "the bounded plan is admissible");
    }

    #[test]
    fn brain_failure_fails_closed_to_safe_stop() {
        struct ErrBrain;
        impl MickBrain for ErrBrain {
            fn decide(&mut self, _ctx: &WorldContext) -> Result<MickIntent, MickError> {
                Err("MICK_TEST_REFUSAL")
            }
        }
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = world(&corr, &[], &[]);
        let mut p = GeometricPlanner::default();
        let out = mick_drive_once(&mut ErrBrain, &w, &mut p);
        assert_eq!(out.kind, ProposalKind::SafeStop, "a brain failure must HOLD, not drive");
        assert!(out.trajectory.iter().all(|t| t.velocity_mps == 0.0));
    }

    // ----- speed control: the Cruise intent / target_speed_mps knob -----

    /// A world with a far goal so the planner actually cruises (uncapped it heads toward
    /// the default 8 m/s).
    fn cruising_world(corr: &dyn CorridorSource) -> PlanInput<'_> {
        PlanInput {
            goal: Goal { target: Pose { x_m: 40.0, y_m: 0.0, heading_rad: 0.0 } },
            ..world(corr, &[], &[])
        }
    }

    fn vmax(out: &PlanOutput) -> f64 {
        out.trajectory.iter().map(|t| t.velocity_mps).fold(0.0, f64::max)
    }

    #[test]
    fn cruise_intent_slows_the_chauffeur_below_the_default() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = cruising_world(&corr);
        let mut p = GeometricPlanner::default(); // cruise ceiling 8 m/s

        let fast = plan_for_intent(&mut p, &MickIntent::GoTo { x_m: 40.0, y_m: 0.0 }, &w);
        let slow = plan_for_intent(&mut p, &MickIntent::Cruise { target_speed_mps: 3.0 }, &w);

        assert!(vmax(&slow) <= 3.0 + 1e-6, "Cruise(3) caps speed at 3, got {}", vmax(&slow));
        assert!(vmax(&fast) > vmax(&slow), "and it is slower than the uncapped GoTo ({} vs {})", vmax(&fast), vmax(&slow));
    }

    #[test]
    fn cruise_request_above_the_ceiling_cannot_speed_up() {
        // The chauffeur asking to "go 50 m/s" can NEVER exceed the configured envelope —
        // the request is clamped to the posture ceiling (8), and KIRRA caps again.
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = cruising_world(&corr);
        let mut p = GeometricPlanner::default();
        let over = plan_for_intent(&mut p, &MickIntent::Cruise { target_speed_mps: 50.0 }, &w);
        assert!(vmax(&over) <= 8.0 + 1e-6, "a request above the ceiling clamps to the cruise config (8), got {}", vmax(&over));
    }

    #[test]
    fn nonfinite_cruise_intent_fails_closed() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = cruising_world(&corr);
        let mut p = GeometricPlanner::default();
        let out = plan_for_intent(&mut p, &MickIntent::Cruise { target_speed_mps: f64::NAN }, &w);
        assert_eq!(out.kind, ProposalKind::SafeStop, "a NaN cruise speed must HOLD, not flow into the planner");
    }

    #[test]
    fn cruise_llm_json_parses_and_rejects_nonfinite() {
        assert_eq!(
            MickIntent::from_llm_json(r#"{"intent":"cruise","target_speed_mps":5.0}"#).unwrap(),
            MickIntent::Cruise { target_speed_mps: 5.0 }
        );
        // 1e400 overflows to Inf → finiteness gate rejects it (fail-closed).
        assert!(MickIntent::from_llm_json(r#"{"intent":"cruise","target_speed_mps":1e400}"#).is_err());
    }

    // ----- the Overtake intent (discretionary pass) -----

    #[test]
    fn overtake_intent_grounds_to_request_overtake() {
        // A recording planner captures the flag the intent set on the PlanInput.
        struct Recorder { req: bool }
        impl Planner for Recorder {
            fn plan(&mut self, input: &PlanInput<'_>) -> PlanOutput {
                self.req = input.request_overtake;
                PlanOutput::safe_stop(input.ego.pose)
            }
        }
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = world(&corr, &[], &[]);

        let mut rec = Recorder { req: false };
        let _ = plan_for_intent(&mut rec, &MickIntent::Overtake, &w);
        assert!(rec.req, "Overtake grounds to request_overtake = true");

        // A non-overtake maneuver leaves it false (start true to prove it is cleared).
        let mut rec2 = Recorder { req: true };
        let _ = plan_for_intent(&mut rec2, &MickIntent::Cruise { target_speed_mps: 5.0 }, &w);
        assert!(!rec2.req, "Cruise leaves request_overtake = false");
    }

    #[test]
    fn overtake_llm_json_parses() {
        assert_eq!(MickIntent::from_llm_json(r#"{"intent":"overtake"}"#).unwrap(), MickIntent::Overtake);
    }

    // ----- the PullOver intent (edge-park and stop) -----

    #[test]
    fn pull_over_intent_grounds_to_request_pull_over() {
        // A recording planner captures the flag the intent set on the PlanInput.
        struct Recorder { req: bool }
        impl Planner for Recorder {
            fn plan(&mut self, input: &PlanInput<'_>) -> PlanOutput {
                self.req = input.request_pull_over;
                PlanOutput::safe_stop(input.ego.pose)
            }
        }
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = world(&corr, &[], &[]);

        let mut rec = Recorder { req: false };
        let _ = plan_for_intent(&mut rec, &MickIntent::PullOver, &w);
        assert!(rec.req, "PullOver grounds to request_pull_over = true");

        // A non-pull-over maneuver leaves it false (start true to prove it is cleared).
        let mut rec2 = Recorder { req: true };
        let _ = plan_for_intent(&mut rec2, &MickIntent::Cruise { target_speed_mps: 5.0 }, &w);
        assert!(!rec2.req, "Cruise leaves request_pull_over = false");
    }

    #[test]
    fn pull_over_llm_json_parses() {
        assert_eq!(MickIntent::from_llm_json(r#"{"intent":"pull_over"}"#).unwrap(), MickIntent::PullOver);
    }

    // ----- the TurnAt intent (junction turn) -----

    #[test]
    fn turn_at_llm_json_parses_each_direction_and_fails_closed_otherwise() {
        let parse = |s| MickIntent::from_llm_json(s);
        assert_eq!(parse(r#"{"intent":"turn_at","direction":"left"}"#).unwrap(), MickIntent::TurnAt { direction: TurnDirection::Left });
        assert_eq!(parse(r#"{"intent":"turn_at","direction":"right"}"#).unwrap(), MickIntent::TurnAt { direction: TurnDirection::Right });
        assert_eq!(parse(r#"{"intent":"turn_at","direction":"straight"}"#).unwrap(), MickIntent::TurnAt { direction: TurnDirection::Straight });
        assert!(parse(r#"{"intent":"turn_at","direction":"sideways"}"#).is_err(), "unknown direction fails closed");
        assert!(parse(r#"{"intent":"turn_at"}"#).is_err(), "missing direction fails closed");
    }

    #[test]
    fn world_context_lists_the_available_turns_from_the_graph() {
        use std::f64::consts::FRAC_PI_2;
        // Approach lane 1 (east) branches LEFT (succ 2, heading +π/2) and RIGHT (succ 4,
        // heading −π/2); there is no straight branch.
        let g = crate::LaneGraph::new()
            .with_lane(
                crate::Lane::straight(1, 0.0, 0.0, 20.0, 2.0, crate::LineType::Solid, crate::LineType::Solid)
                    .with_edge(crate::LaneEdge::Successor { to: 2 })
                    .with_edge(crate::LaneEdge::Successor { to: 4 }),
            )
            .with_lane(crate::Lane::straight(2, 10.0, 20.0, 40.0, 2.0, crate::LineType::Solid, crate::LineType::Solid).with_heading(FRAC_PI_2))
            .with_lane(crate::Lane::straight(4, -10.0, 20.0, 40.0, 2.0, crate::LineType::Solid, crate::LineType::Solid).with_heading(-FRAC_PI_2));
        let corr = MockCorridorSource::straight_5m_half_width(100.0);

        // Ego inside lane 1 → both turns surface; no straight branch.
        let with_graph = PlanInput { map: &corr, lane_graph: Some(&g), ..world(&corr, &[], &[]) };
        let ctx = WorldContext::from_plan_input(&with_graph);
        assert!(ctx.available_turns.contains(&"left"), "left branch surfaced: {:?}", ctx.available_turns);
        assert!(ctx.available_turns.contains(&"right"), "right branch surfaced: {:?}", ctx.available_turns);
        assert!(!ctx.available_turns.contains(&"straight"), "no straight branch: {:?}", ctx.available_turns);

        // No graph → empty (the brain is offered no turns, and a TurnAt would HOLD anyway).
        let no_graph = WorldContext::from_plan_input(&world(&corr, &[], &[]));
        assert!(no_graph.available_turns.is_empty());
    }

    /// A left-turn junction: approach lane 1 (east) → arc lane 2 (curving east→north) →
    /// straight exit lane 5 (north). Lanes are 3 m half-width so the turning footprint stays
    /// contained (cf. the #526 turn test).
    fn left_turn_junction() -> crate::LaneGraph {
        use std::f64::consts::FRAC_PI_2;
        let arc: Vec<MapPoint> = (0..=12)
            .map(|i| {
                let t = -FRAC_PI_2 + FRAC_PI_2 * (i as f64 / 12.0);
                MapPoint { x_m: 30.0 + 12.0 * t.cos(), y_m: 12.0 + 12.0 * t.sin() }
            })
            .collect();
        crate::LaneGraph::new()
            .with_lane(
                crate::Lane::straight(1, 0.0, 0.0, 30.0, 3.0, crate::LineType::Solid, crate::LineType::Solid)
                    .with_edge(crate::LaneEdge::Successor { to: 2 }),
            )
            .with_lane(crate::Lane {
                id: 2,
                centerline: arc,
                half_width_m: 3.0,
                left_line: crate::LineType::Solid,
                right_line: crate::LineType::Solid,
                heading_rad: FRAC_PI_2,
                edges: vec![crate::LaneEdge::Successor { to: 5 }],
                control: None,
            })
            .with_lane(
                crate::Lane::straight(5, 0.0, 42.0, 62.0, 3.0, crate::LineType::Solid, crate::LineType::Solid)
                    .with_heading(FRAC_PI_2),
            )
    }

    #[test]
    fn turn_route_resolves_the_approach_then_continues_the_committed_arc() {
        let g = left_turn_junction();
        // From the APPROACH lane the branch resolves and the route spans approach→arc→exit.
        assert_eq!(turn_route(&g, 1, TurnDirection::Left), Some(vec![1, 2, 5]), "approach routes through the branch");
        // ROUTE-PROGRESS: from the ARC lane (the ego mid-turn) a re-issued left turn CONTINUES
        // the committed arc to the exit instead of HOLDing — the fix that lets the turn finish.
        assert_eq!(turn_route(&g, 2, TurnDirection::Left), Some(vec![2, 5]), "mid-arc continues to the exit");
        // From the straight EXIT lane there is no left branch and no curvature → None (the turn
        // is done; a still-asserted TurnAt HOLDs, fail-closed).
        assert_eq!(turn_route(&g, 5, TurnDirection::Left), None, "the straight exit does not continue a left turn");
        // Fail-closed preserved: a straight approach with no branch that way is still None.
        assert_eq!(turn_route(&g, 1, TurnDirection::Right), None, "no right branch from the approach → None (cannot turn that way here)");
    }

    #[test]
    fn turn_at_grounds_a_continuing_turn_when_the_ego_is_already_on_the_arc() {
        // The route-progress payoff at the grounding level: with the ego MID-ARC, a re-issued
        // TurnAt Left grounds a CONTINUING turn (a Motion plan that climbs the arc toward the
        // exit) rather than the safe-stop HOLD the old single-shot resolution produced once the
        // ego had left the approach lane.
        let g = left_turn_junction();
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        // On the arc (lane 2) partway up — heading between east and north, as on the curve.
        let ego = EgoState {
            pose: Pose { x_m: 35.0, y_m: 1.6, heading_rad: 0.6 },
            linear_x_mps: 3.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        };
        let goal = Pose { x_m: 42.0, y_m: 28.0, heading_rad: std::f64::consts::FRAC_PI_2 };
        let w = PlanInput { ego, goal: Goal { target: goal }, map: &corr, lane_graph: Some(&g), ..world(&corr, &[], &[]) };

        let plan = plan_for_intent(&mut GeometricPlanner::default(), &MickIntent::TurnAt { direction: TurnDirection::Left }, &w);

        assert_eq!(plan.kind, crate::ProposalKind::Motion, "mid-arc TurnAt continues the turn, not a HOLD");
        let max_y = plan.trajectory.iter().map(|p| p.pose.y_m).fold(f64::MIN, f64::max);
        assert!(max_y > 5.0, "the continued turn climbs the arc toward the exit (y≈12), got max_y {max_y}");
    }

    // ----- the RouteTo intent (multi-junction routing) -----

    /// A planner that records the reference corridor (`map`) it was grounded with — so a test
    /// can assert that `RouteTo` materialized the WHOLE multi-junction route's corridor (it
    /// curves up into the final exit lane), not just the ego's current straight lane.
    struct MapRecorder {
        left_last: Option<MapPoint>,
        left_len: usize,
    }
    impl Planner for MapRecorder {
        fn plan(&mut self, input: &PlanInput<'_>) -> PlanOutput {
            let lb = input.map.left_boundary();
            self.left_len = lb.len();
            self.left_last = lb.last().copied();
            PlanOutput::safe_stop(input.ego.pose)
        }
    }

    /// A quarter-circle arc (n+1 points) sweeping `sweep` rad (±π/2) from `start_angle` about
    /// `(cx, cy)` at radius `r` — a smooth turn centerline (+ = CCW/left, − = CW/right).
    fn quarter_arc(cx: f64, cy: f64, r: f64, start_angle: f64, sweep: f64, n: usize) -> Vec<MapPoint> {
        (0..=n)
            .map(|i| {
                let t = start_angle + sweep * (i as f64 / n as f64);
                MapPoint { x_m: cx + r * t.cos(), y_m: cy + r * t.sin() }
            })
            .collect()
    }

    /// A **two-junction** route: straight east (1) → LEFT arc (2) → straight north (3) → RIGHT
    /// arc (4) → straight east (5). Lane 1 also carries a DECOY right branch (6, a dead-end
    /// south) so reaching the destination genuinely requires the router to pick the correct
    /// branch at the first junction. Lanes are 3 m half-width (the turning footprint stays
    /// contained, cf. the closed-loop turn test); arcs use r = 12.
    fn two_junction_route() -> crate::LaneGraph {
        use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};
        let r = 12.0;
        // J1: left arc (CCW), centre (30,12), θ −π/2→0: (30,0)→(42,12), east→north.
        let arc_left = quarter_arc(30.0, 12.0, r, -FRAC_PI_2, FRAC_PI_2, 12);
        // J2: right arc (CW), centre (54,40), θ π→π/2: (42,40)→(54,52), north→east.
        let arc_right = quarter_arc(54.0, 40.0, r, PI, -FRAC_PI_2, 12);
        let lane = |id, cl: Vec<MapPoint>, heading, succ: &[u64]| crate::Lane {
            id,
            centerline: cl,
            half_width_m: 3.0,
            left_line: crate::LineType::Solid,
            right_line: crate::LineType::Solid,
            heading_rad: heading,
            edges: succ.iter().map(|&to| crate::LaneEdge::Successor { to }).collect(),
            control: None,
        };
        crate::LaneGraph::new()
            .with_lane(lane(1, vec![MapPoint { x_m: 0.0, y_m: 0.0 }, MapPoint { x_m: 30.0, y_m: 0.0 }], 0.0, &[2, 6]))
            .with_lane(lane(2, arc_left, FRAC_PI_4, &[3]))
            .with_lane(lane(3, vec![MapPoint { x_m: 42.0, y_m: 12.0 }, MapPoint { x_m: 42.0, y_m: 40.0 }], FRAC_PI_2, &[4]))
            .with_lane(lane(4, arc_right, FRAC_PI_4, &[5]))
            .with_lane(lane(5, vec![MapPoint { x_m: 54.0, y_m: 52.0 }, MapPoint { x_m: 80.0, y_m: 52.0 }], 0.0, &[]))
            // Decoy: a right branch off lane 1 heading south, dead-ending (never reaches 5).
            .with_lane(lane(6, vec![MapPoint { x_m: 30.0, y_m: 0.0 }, MapPoint { x_m: 30.0, y_m: -20.0 }], -FRAC_PI_2, &[]))
    }

    #[test]
    fn route_to_llm_json_parses_and_fails_closed_on_nonfinite() {
        assert_eq!(
            MickIntent::from_llm_json(r#"{"intent":"route_to","x_m":72.0,"y_m":52.0}"#).unwrap(),
            MickIntent::RouteTo { x_m: 72.0, y_m: 52.0 }
        );
        // A hallucinated non-finite destination must fail closed (caller HOLDs), never flow in.
        assert!(MickIntent::from_llm_json(r#"{"intent":"route_to","x_m":1e400,"y_m":0.0}"#).is_err());
        assert!(MickIntent::from_llm_json(r#"{"intent":"route_to","y_m":0.0}"#).is_err(), "missing field fails closed");
    }

    #[test]
    fn route_to_grounds_a_multi_junction_route_following_the_stitched_corridor() {
        // The router must pick the LEFT branch at J1 (over the decoy) to reach the destination
        // in lane 5, then the route corridor stitches BOTH turns into one materialized handle.
        let g = two_junction_route();
        assert_eq!(g.route_to_point(1, Point { x_m: 72.0, y_m: 52.0 }), Some(vec![1, 2, 3, 4, 5]),
            "routing selects the correct branch at each junction across both turns");

        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let ego = EgoState {
            pose: Pose { x_m: 16.0, y_m: 0.0, heading_rad: 0.0 },
            linear_x_mps: 4.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        };
        let w = PlanInput { ego, map: &corr, lane_graph: Some(&g), ..world(&corr, &[], &[]) };

        // The corridor the planner is grounded with is the WHOLE route's, curving through both
        // junctions up into the final east-bound exit lane (its far end sits at y≈52, x≈80) —
        // not the flat world corridor (which would end at y≈0). That is the multi-junction stitch.
        let mut rec = MapRecorder { left_last: None, left_len: 0 };
        let _ = plan_for_intent(&mut rec, &MickIntent::RouteTo { x_m: 72.0, y_m: 52.0 }, &w);
        let last = rec.left_last.expect("the route corridor was materialized and grounded");
        assert!(last.y_m > 45.0, "the stitched corridor climbs through both junctions into lane 5 (y≈52), got y={}", last.y_m);
        assert!(last.x_m > 60.0, "and reaches east along the final exit lane, got x={}", last.x_m);

        // And a real planner produces a MOTION plan along it (not a fail-closed HOLD).
        let plan = plan_for_intent(&mut GeometricPlanner::default(), &MickIntent::RouteTo { x_m: 72.0, y_m: 52.0 }, &w);
        assert_eq!(plan.kind, crate::ProposalKind::Motion, "RouteTo grounds a motion plan along the route");
    }

    #[test]
    fn route_to_fails_closed_when_it_cannot_route() {
        let g = two_junction_route();
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let on_map = EgoState {
            pose: Pose { x_m: 16.0, y_m: 0.0, heading_rad: 0.0 },
            linear_x_mps: 4.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        };
        let is_hold = |p: &PlanOutput| p.kind == crate::ProposalKind::SafeStop && p.trajectory.iter().all(|t| t.velocity_mps == 0.0);

        // No lane graph → HOLD.
        let no_graph = PlanInput { ego: on_map, map: &corr, lane_graph: None, ..world(&corr, &[], &[]) };
        assert!(is_hold(&plan_for_intent(&mut GeometricPlanner::default(), &MickIntent::RouteTo { x_m: 72.0, y_m: 52.0 }, &no_graph)), "no graph → HOLD");

        // Ego off the mapped road → HOLD.
        let off = EgoState { pose: Pose { x_m: 16.0, y_m: 99.0, heading_rad: 0.0 }, ..on_map };
        let ego_off = PlanInput { ego: off, map: &corr, lane_graph: Some(&g), ..world(&corr, &[], &[]) };
        assert!(is_hold(&plan_for_intent(&mut GeometricPlanner::default(), &MickIntent::RouteTo { x_m: 72.0, y_m: 52.0 }, &ego_off)), "ego off-map → HOLD");

        // Destination off the mapped road → HOLD.
        let w = PlanInput { ego: on_map, map: &corr, lane_graph: Some(&g), ..world(&corr, &[], &[]) };
        assert!(is_hold(&plan_for_intent(&mut GeometricPlanner::default(), &MickIntent::RouteTo { x_m: 72.0, y_m: 999.0 }, &w)), "destination off-map → HOLD");

        // Reachable-only-via-the-decoy is fine, but a genuinely unreachable destination HOLDs:
        // the decoy lane 6 is a dead-end, so a point beyond it that no route reaches → HOLD.
        // (Routing to the decoy's own lane is reachable; pick a point off every lane instead —
        // covered above. Here assert a non-finite destination also HOLDs at grounding.)
        assert!(is_hold(&plan_for_intent(&mut GeometricPlanner::default(), &MickIntent::RouteTo { x_m: f64::NAN, y_m: 0.0 }, &w)), "non-finite destination → HOLD");
    }

    // ----- map-intention predicted paths (lane-follow mode) -----

    #[test]
    fn a_lane_following_object_merging_in_is_yielded_to_where_cv_misses() {
        // A merging lane: straight east at y=4 (x 12..25) then merges down into the ego lane y=0
        // (x 25..60). An object at (20,4) moving EAST — a constant-velocity predictor keeps it at
        // y=4, clear of the ego. Its lane-follow path traces the merge INTO the ego's path, so
        // with the map supplied the predictive yield fires; without it (CV-only) the ego does not
        // yield. This is the planner-side map-intention mode made live by `derive_predicted_paths`.
        let g = crate::LaneGraph::new().with_lane(crate::Lane {
            id: 1,
            centerline: vec![
                MapPoint { x_m: 12.0, y_m: 4.0 }, MapPoint { x_m: 25.0, y_m: 4.0 },
                MapPoint { x_m: 35.0, y_m: 0.0 }, MapPoint { x_m: 60.0, y_m: 0.0 },
            ],
            half_width_m: 2.5,
            left_line: crate::LineType::Solid,
            right_line: crate::LineType::Solid,
            heading_rad: 0.0,
            edges: Vec::new(),
            control: None,
        });
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let obj = PerceivedObject { id: 1, pos: Point { x_m: 20.0, y_m: 4.0 }, velocity_mps: 5.0, heading_rad: 0.0, vel: Point { x_m: 5.0, y_m: 0.0 } };
        let objs = [obj];
        let intent = MickIntent::GoTo { x_m: 80.0, y_m: 0.0 };
        let reach = |out: &PlanOutput| out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);

        // CV-only (no lane graph): the object holds y=4, never enters the ego band → no yield.
        let w_cv = PlanInput { map: &corr, objects: &objs, ..world(&corr, &objs, &[]) };
        let cv = plan_for_intent(&mut GeometricPlanner::default(), &intent, &w_cv);
        assert!(reach(&cv) > 30.0, "CV: a lane-parallel object → no yield, near-natural reach, got {}", reach(&cv));

        // With the map: the lane-follow mode predicts the merge into the ego path → yields short.
        let w_map = PlanInput { map: &corr, objects: &objs, lane_graph: Some(&g), ..world(&corr, &objs, &[]) };
        let mapped = plan_for_intent(&mut GeometricPlanner::default(), &intent, &w_map);
        assert!(reach(&mapped) < 28.0, "map-intention: the merging object is yielded to (short of the merge), got {}", reach(&mapped));
        assert!(reach(&mapped) < reach(&cv) - 5.0, "the lane-follow mode yields meaningfully shorter than CV");
    }

    #[test]
    fn a_lane_keeping_object_in_its_own_lane_is_not_spuriously_yielded_to() {
        // The suppression direction: an object in a DIVERGING lane (peels away to +y) moving with
        // a slight inward velocity component. A naive predictor might extrapolate it into the ego
        // lane, but its lane-follow path shows it LEAVES → no spurious yield (the ego drives on).
        let g = crate::LaneGraph::new().with_lane(crate::Lane {
            id: 1,
            centerline: vec![
                MapPoint { x_m: 12.0, y_m: 3.0 }, MapPoint { x_m: 25.0, y_m: 4.0 },
                MapPoint { x_m: 40.0, y_m: 10.0 }, MapPoint { x_m: 60.0, y_m: 18.0 },
            ],
            half_width_m: 2.5,
            left_line: crate::LineType::Solid,
            right_line: crate::LineType::Solid,
            heading_rad: 0.0,
            edges: Vec::new(),
            control: None,
        });
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        // Velocity slightly toward the ego lane (vy<0) — CV would creep it inward — but the lane
        // diverges, so the lane-follow mode keeps it clear.
        let obj = PerceivedObject { id: 1, pos: Point { x_m: 20.0, y_m: 3.6 }, velocity_mps: 5.0, heading_rad: 0.0, vel: Point { x_m: 5.0, y_m: -0.4 } };
        let objs = [obj];
        let intent = MickIntent::GoTo { x_m: 80.0, y_m: 0.0 };
        let reach = |out: &PlanOutput| out.trajectory.iter().map(|t| t.pose.x_m).fold(0.0, f64::max);

        let w_map = PlanInput { map: &corr, objects: &objs, lane_graph: Some(&g), ..world(&corr, &objs, &[]) };
        let mapped = plan_for_intent(&mut GeometricPlanner::default(), &intent, &w_map);
        assert!(reach(&mapped) > 30.0, "lane-follow shows the object diverging → no spurious yield (near-natural reach), got {}", reach(&mapped));
    }

    // ----- junction right-of-way wired into cedes_to_ego_ids -----

    /// A planner that records the `cedes_to_ego_ids` it was grounded with.
    struct CedeRecorder {
        cedes: Vec<u64>,
    }
    impl Planner for CedeRecorder {
        fn plan(&mut self, input: &PlanInput<'_>) -> PlanOutput {
            self.cedes = input.cedes_to_ego_ids.to_vec();
            PlanOutput::safe_stop(input.ego.pose)
        }
    }

    /// Lane 1 (ego, y=0) has priority over lane 2 (y=10); an object sits in lane 2.
    fn priority_graph() -> crate::LaneGraph {
        crate::LaneGraph::new()
            .with_lane(crate::Lane::straight(1, 0.0, 0.0, 30.0, 2.0, crate::LineType::Solid, crate::LineType::Solid))
            .with_lane(crate::Lane::straight(2, 10.0, 0.0, 30.0, 2.0, crate::LineType::Solid, crate::LineType::Solid))
            .with_right_of_way(1, 2)
    }

    fn obj_in_lane2() -> PerceivedObject {
        PerceivedObject { id: 7, pos: Point { x_m: 15.0, y_m: 10.0 }, velocity_mps: 3.0, heading_rad: 0.0, vel: Point { x_m: 3.0, y_m: 0.0 } }
    }

    #[test]
    fn junction_right_of_way_is_wired_into_cedes_to_ego_ids() {
        let g = priority_graph();
        let objs = [obj_in_lane2()];
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        // Ego in lane 1; no integrator cede list → derive from the map.
        let w = PlanInput { map: &corr, objects: &objs, lane_graph: Some(&g), ..world(&corr, &objs, &[]) };

        let mut rec = CedeRecorder { cedes: Vec::new() };
        let _ = plan_for_intent(&mut rec, &MickIntent::GoTo { x_m: 25.0, y_m: 0.0 }, &w);
        assert_eq!(rec.cedes, vec![7], "the yielding-lane agent is derived into cedes_to_ego_ids");
    }

    #[test]
    fn an_explicit_cede_list_is_never_overridden_by_the_map() {
        let g = priority_graph();
        let objs = [obj_in_lane2()];
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let explicit = [99_u64];
        let w = PlanInput { map: &corr, objects: &objs, lane_graph: Some(&g), cedes_to_ego_ids: &explicit, ..world(&corr, &objs, &[]) };

        let mut rec = CedeRecorder { cedes: Vec::new() };
        let _ = plan_for_intent(&mut rec, &MickIntent::GoTo { x_m: 25.0, y_m: 0.0 }, &w);
        assert_eq!(rec.cedes, vec![99], "an integrator-supplied cede list stands (not overridden)");
    }

    #[test]
    fn no_graph_derives_no_cedes_and_yields_to_all() {
        let objs = [obj_in_lane2()];
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = PlanInput { map: &corr, objects: &objs, lane_graph: None, ..world(&corr, &objs, &[]) };

        let mut rec = CedeRecorder { cedes: vec![1] };
        let _ = plan_for_intent(&mut rec, &MickIntent::GoTo { x_m: 25.0, y_m: 0.0 }, &w);
        assert!(rec.cedes.is_empty(), "no graph → empty cede list (fail-safe: yield to all)");
    }

    // ----- junction STOP / YIELD signs wired into the loop -----

    /// A planner that records the `controls` it was grounded with.
    struct ControlRecorder {
        controls: Vec<TrafficControl>,
    }
    impl Planner for ControlRecorder {
        fn plan(&mut self, input: &PlanInput<'_>) -> PlanOutput {
            self.controls = input.controls.to_vec();
            PlanOutput::safe_stop(input.ego.pose)
        }
    }

    /// A single approach lane (0,0)→(18,0) carrying control `c` at its terminus (x=18).
    fn lane_with_control(c: LaneControl) -> crate::LaneGraph {
        crate::LaneGraph::new().with_lane(
            crate::Lane::straight(1, 0.0, 0.0, 18.0, 2.0, crate::LineType::Solid, crate::LineType::Solid)
                .with_control(c),
        )
    }

    fn derived_controls_for<'a>(g: &'a crate::LaneGraph, ego: EgoState, corr: &'a MockCorridorSource) -> Vec<TrafficControl> {
        let w = PlanInput { ego, map: corr, lane_graph: Some(g), ..world(corr, &[], &[]) };
        let mut rec = ControlRecorder { controls: Vec::new() };
        let _ = plan_for_intent(&mut rec, &MickIntent::GoTo { x_m: 50.0, y_m: 0.0 }, &w);
        rec.controls
    }

    fn ego_at(x: f64, v: f64) -> EgoState {
        EgoState { pose: Pose { x_m: x, y_m: 0.0, heading_rad: 0.0 }, linear_x_mps: v, yaw_rate_rads: 0.0, stamp_ms: 0 }
    }

    #[test]
    fn a_stop_sign_derives_an_unsatisfied_stop_while_approaching() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        // Approaching at speed, well before the line → not satisfied (stop imposed).
        let controls = derived_controls_for(&lane_with_control(LaneControl::Stop), ego_at(5.0, 2.0), &corr);
        assert_eq!(controls, vec![TrafficControl::StopSign { stop_line_x_m: 18.0, satisfied: false }]);
    }

    #[test]
    fn a_stop_sign_is_satisfied_once_stopped_at_the_line() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        // Essentially stopped just before the line → satisfied (proceed) — the stateless go.
        let controls = derived_controls_for(&lane_with_control(LaneControl::Stop), ego_at(17.0, 0.0), &corr);
        assert_eq!(controls, vec![TrafficControl::StopSign { stop_line_x_m: 18.0, satisfied: true }]);
    }

    #[test]
    fn a_yield_sign_derives_a_yield_control() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let controls = derived_controls_for(&lane_with_control(LaneControl::Yield), ego_at(5.0, 2.0), &corr);
        assert_eq!(controls, vec![TrafficControl::YieldSign { line_x_m: 18.0 }]);
    }

    #[test]
    fn no_control_derives_nothing_and_explicit_controls_stand() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        // A lane with no control → nothing derived.
        let plain = crate::LaneGraph::new()
            .with_lane(crate::Lane::straight(1, 0.0, 0.0, 18.0, 2.0, crate::LineType::Solid, crate::LineType::Solid));
        assert!(derived_controls_for(&plain, ego_at(5.0, 2.0), &corr).is_empty());

        // An explicit integrator control list is never overridden by the map.
        let g = lane_with_control(LaneControl::Stop);
        let explicit = [TrafficControl::YieldSign { line_x_m: 9.0 }];
        let w = PlanInput { ego: ego_at(5.0, 2.0), map: &corr, lane_graph: Some(&g), controls: &explicit, ..world(&corr, &[], &[]) };
        let mut rec = ControlRecorder { controls: Vec::new() };
        let _ = plan_for_intent(&mut rec, &MickIntent::GoTo { x_m: 50.0, y_m: 0.0 }, &w);
        assert_eq!(rec.controls, vec![TrafficControl::YieldSign { line_x_m: 9.0 }], "explicit controls stand");
    }

    #[test]
    fn the_planner_stops_at_a_map_derived_stop_line() {
        // End to end: the derived StopSign makes Occy decelerate to a stop at the line (x=18)
        // and not pass it — the junction stop is real, not just recorded.
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let g = lane_with_control(LaneControl::Stop);
        let w = PlanInput { ego: ego_at(5.0, 2.0), map: &corr, lane_graph: Some(&g), ..world(&corr, &[], &[]) };
        let plan = plan_for_intent(&mut GeometricPlanner::default(), &MickIntent::GoTo { x_m: 50.0, y_m: 0.0 }, &w);

        let max_x = plan.trajectory.iter().map(|t| t.pose.x_m).fold(f64::MIN, f64::max);
        assert!(max_x <= 18.5, "the plan holds at/before the stop line x=18, got max_x {max_x}");
        assert!(
            plan.trajectory.last().unwrap().velocity_mps < 0.5,
            "and comes to a stop at the line (final v {})",
            plan.trajectory.last().unwrap().velocity_mps
        );
    }

    #[test]
    fn a_traffic_light_takes_its_live_state_and_fails_closed_to_red() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let g = lane_with_control(LaneControl::TrafficLight);

        // No signal feed → RED (fail-closed): an unknown light HOLDS.
        let red_default = derived_controls_for(&g, ego_at(5.0, 2.0), &corr);
        assert_eq!(red_default, vec![TrafficControl::TrafficLight { stop_line_x_m: 18.0, state: SignalState::Red }]);

        // Live GREEN supplied for the ego's lane → the light passes through as green.
        let green = [(1u64, SignalState::Green)];
        let w = PlanInput { ego: ego_at(5.0, 2.0), map: &corr, lane_graph: Some(&g), signal_states: &green, ..world(&corr, &[], &[]) };
        let mut rec = ControlRecorder { controls: Vec::new() };
        let _ = plan_for_intent(&mut rec, &MickIntent::GoTo { x_m: 50.0, y_m: 0.0 }, &w);
        assert_eq!(rec.controls, vec![TrafficControl::TrafficLight { stop_line_x_m: 18.0, state: SignalState::Green }]);
    }

    #[test]
    fn the_planner_stops_on_red_and_drives_through_on_green() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let g = lane_with_control(LaneControl::TrafficLight);

        // RED (absent state → red): decelerate to a stop at the line, not past it.
        let w_red = PlanInput { ego: ego_at(5.0, 2.0), map: &corr, lane_graph: Some(&g), ..world(&corr, &[], &[]) };
        let red = plan_for_intent(&mut GeometricPlanner::default(), &MickIntent::GoTo { x_m: 50.0, y_m: 0.0 }, &w_red);
        let red_max_x = red.trajectory.iter().map(|t| t.pose.x_m).fold(f64::MIN, f64::max);
        assert!(red_max_x <= 18.5, "red light → stop at the line x=18, got max_x {red_max_x}");
        assert!(red.trajectory.last().unwrap().velocity_mps < 0.5, "and comes to a stop");

        // GREEN: drive through the line toward the goal.
        let green = [(1u64, SignalState::Green)];
        let w_green = PlanInput { ego: ego_at(5.0, 2.0), map: &corr, lane_graph: Some(&g), signal_states: &green, ..world(&corr, &[], &[]) };
        let go = plan_for_intent(&mut GeometricPlanner::default(), &MickIntent::GoTo { x_m: 50.0, y_m: 0.0 }, &w_green);
        let green_max_x = go.trajectory.iter().map(|t| t.pose.x_m).fold(f64::MIN, f64::max);
        assert!(green_max_x > 18.5, "green light → drive through the line, got max_x {green_max_x}");
    }

    // ----- the dual-rate driver: System-2 intent rate vs System-1 grounding rate -----

    use std::cell::Cell;
    use std::rc::Rc;

    /// A brain that counts its `decide` calls (via a shared counter) and returns a fixed reply.
    struct CountingBrain {
        calls: Rc<Cell<u32>>,
        reply: Result<MickIntent, MickError>,
    }
    impl MickBrain for CountingBrain {
        fn decide(&mut self, _ctx: &WorldContext) -> Result<MickIntent, MickError> {
            self.calls.set(self.calls.get() + 1);
            self.reply
        }
    }

    #[test]
    fn driver_asks_the_brain_at_the_slow_rate_but_grounds_every_tick() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = cruising_world(&corr);
        let mut p = GeometricPlanner::default();
        let calls = Rc::new(Cell::new(0));
        let brain = CountingBrain { calls: Rc::clone(&calls), reply: Ok(MickIntent::Cruise { target_speed_mps: 5.0 }) };
        // Decide at 2 Hz (500 ms); run 20 fast ticks at 100 ms (10 Hz).
        let mut driver = MickDriver::with_rates(brain, 500, 2_000);

        let mut grounded = 0;
        for tick in 1..=20u64 {
            let out = driver.drive_tick(&w, &mut p, tick * 100);
            // Fast path runs EVERY tick — always a grounded proposal.
            assert!(matches!(out.kind, ProposalKind::Motion | ProposalKind::SafeStop));
            grounded += 1;
        }
        assert_eq!(grounded, 20, "the fast path grounds an output on every tick");
        // Slow path: ~1 decision per 500 ms over 2 s (+ the cold-start one), NOT 20.
        let n = calls.get();
        assert!((3..=6).contains(&n), "brain decided at the System-2 rate, not every tick: {n} calls / 20 ticks");
        assert_eq!(driver.current_intent(), Some(MickIntent::Cruise { target_speed_mps: 5.0 }));
    }

    #[test]
    fn driver_grounds_the_cached_intent_between_decisions() {
        // The brain replies once, then would change its mind — but between re-decides the
        // SAME cached intent is grounded each fast tick.
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = world(&corr, &[], &[]);
        let mut p = GeometricPlanner::default();
        let calls = Rc::new(Cell::new(0));
        let brain = CountingBrain { calls: Rc::clone(&calls), reply: Ok(MickIntent::Hold) };
        let mut driver = MickDriver::with_rates(brain, 1_000, 5_000);

        driver.drive_tick(&w, &mut p, 100); // cold decide → Hold cached
        assert_eq!(calls.get(), 1);
        // Three more fast ticks before the 1 s interval elapses → no new decision.
        for t in [200u64, 300, 400] {
            driver.drive_tick(&w, &mut p, t);
        }
        assert_eq!(calls.get(), 1, "no re-decision before the interval elapses");
        assert_eq!(driver.current_intent(), Some(MickIntent::Hold));
    }

    #[test]
    fn driver_fails_closed_to_hold_when_the_brain_goes_stale() {
        // The brain succeeds once (Cruise), then errors forever. After the staleness window
        // the driver must HOLD rather than keep grounding the arbitrarily-old intent.
        struct OnceThenErr { calls: Rc<Cell<u32>> }
        impl MickBrain for OnceThenErr {
            fn decide(&mut self, _ctx: &WorldContext) -> Result<MickIntent, MickError> {
                self.calls.set(self.calls.get() + 1);
                if self.calls.get() == 1 { Ok(MickIntent::Cruise { target_speed_mps: 5.0 }) } else { Err("down") }
            }
        }
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = cruising_world(&corr); // far goal so a fresh Cruise grounds to motion
        let mut p = GeometricPlanner::default();
        let calls = Rc::new(Cell::new(0));
        // decide every 500 ms, stale after 1500 ms.
        let mut driver = MickDriver::with_rates(OnceThenErr { calls: Rc::clone(&calls) }, 500, 1_500);

        let out0 = driver.drive_tick(&w, &mut p, 0); // succeeds → Cruise (Motion)
        assert_eq!(out0.kind, ProposalKind::Motion, "fresh Cruise grounds to motion");
        // Within the staleness window the (now-erroring) brain leaves the last intent usable.
        let out1 = driver.drive_tick(&w, &mut p, 1_000);
        assert_eq!(out1.kind, ProposalKind::Motion, "intent still fresh enough → still driving");
        // Past the staleness window → fail closed to Hold (a controlled stop).
        let out2 = driver.drive_tick(&w, &mut p, 2_000);
        assert_eq!(out2.kind, ProposalKind::SafeStop, "stale brain → HOLD (fail-closed)");
    }

    #[test]
    fn driver_cold_start_with_a_failing_brain_holds() {
        let corr = MockCorridorSource::straight_5m_half_width(100.0);
        let w = world(&corr, &[], &[]);
        let mut p = GeometricPlanner::default();
        let calls = Rc::new(Cell::new(0));
        let brain = CountingBrain { calls: Rc::clone(&calls), reply: Err("no model") };
        let mut driver = MickDriver::new(brain);
        let out = driver.drive_tick(&w, &mut p, 0);
        assert_eq!(out.kind, ProposalKind::SafeStop, "no intent ever → HOLD");
        assert_eq!(driver.current_intent(), None);
    }
}
