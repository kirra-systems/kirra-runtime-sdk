//! **Pure-Rust Lanelet2 (`.osm`) map parse → [`LaneGraph`]** (gap #4 follow-up).
//!
//! Lanelet2 maps are stored in OSM-XML: `<node>` points (local metric `local_x` /
//! `local_y`), `<way>` linestrings (lane boundaries, with a `subtype` marking), and
//! `<relation type=lanelet>` = a `left` + `right` boundary way. This parses that subset
//! into the planner's [`LaneGraph`] — centerlines, half-widths, typed lane lines,
//! connectivity, and the **`regulatory_element` relations**: `right_of_way` (priority →
//! yields-to edges) and `traffic_sign` / `traffic_light` (a lane's STOP / YIELD / TRAFFIC
//! LIGHT [`LaneControl`]) — **without the C++ `lanelet2_core` library**. Only Lanelet2's advanced
//! geometry/routing ops needed that lib; the `LaneGraph` + its router (`route`) already
//! cover routing, so all we need from the file is the primitives.
//!
//! This is *offline map loading*; KIRRA still backstops at runtime. Connectivity is
//! derived from shared OSM ids (Lanelet2 lanelets share node/linestring ids at
//! adjacencies and junctions), which is exact — no geometric tolerance guessing.

use std::collections::BTreeMap;

use kirra_core::corridor::Point;
use roxmltree::Document;

use crate::lane_lines::LineType;
use crate::lanemap::{Lane, LaneControl, LaneEdge, LaneGraph};

/// Why a Lanelet2 `.osm` document could not be turned into a [`LaneGraph`].
#[derive(Debug, PartialEq)]
pub enum Lanelet2ParseError {
    /// The XML itself is malformed.
    Xml(String),
    /// A `<node>` lacked a parseable `id` or `local_x`/`local_y`.
    BadNode(String),
    /// A `<way>` referenced a node id that no `<node>` defined.
    DanglingNodeRef { way: u64, node: u64 },
    /// A lanelet relation referenced a `left`/`right` way that no `<way>` defined.
    MissingBoundary { lanelet: u64, role: &'static str },
    /// A lanelet relation had no `left` or no `right` member.
    IncompleteLanelet(u64),
}

/// Parse a Lanelet2 `.osm` document into a [`LaneGraph`].
///
/// Mapping: a lanelet's `left`/`right` boundary ways become the lane's two typed lines;
/// the centerline is their per-vertex midpoint and `half_width_m` their mean half-gap;
/// the heading is the centerline's overall direction. Connectivity (shared ids):
/// two lanelets that **share a boundary way** are lateral neighbors; a lanelet whose
/// boundary ways **end on the node a later lanelet's boundaries start from** is its
/// predecessor (a `Successor` edge). Fail-closed: any missing primitive → `Err`.
pub fn parse_lanelet2_osm(xml: &str) -> Result<LaneGraph, Lanelet2ParseError> {
    let doc = Document::parse(xml).map_err(|e| Lanelet2ParseError::Xml(e.to_string()))?;
    let root = doc.root_element();

    let mut nodes: BTreeMap<u64, Point> = BTreeMap::new();
    let mut ways: BTreeMap<u64, Way> = BTreeMap::new();
    let mut raw_lanelets: Vec<RawLanelet> = Vec::new();
    // `right_of_way` regulatory elements, as (priority lanes, yielding lanes).
    let mut right_of_way: Vec<(Vec<u64>, Vec<u64>)> = Vec::new();
    // `traffic_sign` / `traffic_light` regulatory elements, resolved to controls after the scan.
    let mut raw_regs: Vec<RawReg> = Vec::new();

    for el in root.children().filter(roxmltree::Node::is_element) {
        match el.tag_name().name() {
            "node" => {
                let id = attr_u64(&el, "id").ok_or_else(|| Lanelet2ParseError::BadNode("id".into()))?;
                let x = tag_f64(&el, "local_x")
                    .ok_or_else(|| Lanelet2ParseError::BadNode(format!("node {id} local_x")))?;
                let y = tag_f64(&el, "local_y")
                    .ok_or_else(|| Lanelet2ParseError::BadNode(format!("node {id} local_y")))?;
                nodes.insert(id, Point { x_m: x, y_m: y });
            }
            "way" => {
                let Some(id) = attr_u64(&el, "id") else { continue };
                let node_ids: Vec<u64> = el
                    .children()
                    .filter(|c| c.has_tag_name("nd"))
                    .filter_map(|c| attr_u64(&c, "ref"))
                    .collect();
                let subtype = tag_value(&el, "subtype");
                let line = line_type_of(tag_value(&el, "type"), subtype);
                ways.insert(id, Way { node_ids, line, subtype: subtype.map(str::to_string) });
            }
            "relation" => match tag_value(&el, "type") {
                Some("lanelet") => {
                    // Driveable-subtype filter: a vehicle planner must not route over a
                    // walkway / crosswalk / bicycle lane. Skip explicitly non-vehicle
                    // lanelets; include road/highway and untagged ones (fail-open on a
                    // missing subtype, the common case in simple maps).
                    if !is_driveable_subtype(tag_value(&el, "subtype")) {
                        continue;
                    }
                    let Some(id) = attr_u64(&el, "id") else { continue };
                    let first_ref = |r: &str| {
                        el.children()
                            .filter(|c| c.has_tag_name("member"))
                            .find(|c| c.attribute("role") == Some(r))
                            .and_then(|c| attr_u64(&c, "ref"))
                    };
                    let (Some(left), Some(right)) = (first_ref("left"), first_ref("right")) else {
                        return Err(Lanelet2ParseError::IncompleteLanelet(id));
                    };
                    // Regulatory elements this lanelet is subject to (role `regulatory_element`).
                    let reg_elems: Vec<u64> = el
                        .children()
                        .filter(|c| c.has_tag_name("member"))
                        .filter(|c| c.attribute("role") == Some("regulatory_element"))
                        .filter_map(|c| attr_u64(&c, "ref"))
                        .collect();
                    raw_lanelets.push(RawLanelet { id, left, right, reg_elems });
                }
                Some("regulatory_element") => {
                    let Some(reg_id) = attr_u64(&el, "id") else { continue };
                    if tag_value(&el, "subtype") == Some("right_of_way") {
                        // Members reference lanelets by id: role `right_of_way` = the lanes
                        // with priority, role `yield` = the lanes that must cede.
                        let refs_with_role = |r: &str| -> Vec<u64> {
                            el.children()
                                .filter(|c| c.has_tag_name("member"))
                                .filter(|c| c.attribute("role") == Some(r))
                                .filter_map(|c| attr_u64(&c, "ref"))
                                .collect()
                        };
                        right_of_way.push((refs_with_role("right_of_way"), refs_with_role("yield")));
                    } else {
                        // A traffic-sign / traffic-light element: collect it raw and resolve to a
                        // control after the scan (the `refers` way may appear later in the file).
                        let refers = el
                            .children()
                            .filter(|c| c.has_tag_name("member"))
                            .find(|c| c.attribute("role") == Some("refers"))
                            .and_then(|c| attr_u64(&c, "ref"));
                        raw_regs.push(RawReg {
                            id: reg_id,
                            subtype: tag_value(&el, "subtype").map(str::to_string),
                            sign_type: tag_value(&el, "sign_type").map(str::to_string),
                            refers,
                        });
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    // Resolve each way's node ids → points (fail-closed on a dangling ref).
    let way_points = |way_id: u64| -> Result<Vec<Point>, Lanelet2ParseError> {
        let w = &ways[&way_id];
        w.node_ids
            .iter()
            .map(|n| nodes.get(n).copied().ok_or(Lanelet2ParseError::DanglingNodeRef { way: way_id, node: *n }))
            .collect()
    };

    // Resolve each traffic-sign / traffic-light regulatory element to a control. A light is a
    // light; a sign's stop-vs-yield comes from its `sign_type` tag, else its `refers` way's
    // subtype; some maps put the specific sign straight in the element subtype. An unrecognized
    // element yields nothing (the parser never fabricates a stop). `reg_id → control`.
    let mut reg_controls: BTreeMap<u64, LaneControl> = BTreeMap::new();
    for r in &raw_regs {
        let control = match r.subtype.as_deref() {
            Some("traffic_light") => Some(LaneControl::TrafficLight),
            Some("traffic_sign") => r
                .sign_type
                .as_deref()
                .and_then(sign_control)
                .or_else(|| r.refers.and_then(|w| ways.get(&w)).and_then(|w| w.subtype.as_deref()).and_then(sign_control)),
            other => other.and_then(sign_control), // direct-subtype convention (e.g. subtype=stop_sign)
        };
        if let Some(c) = control {
            reg_controls.insert(r.id, c);
        }
    }

    let mut graph = LaneGraph::new();
    for ll in &raw_lanelets {
        for (role, w) in [("left", ll.left), ("right", ll.right)] {
            if !ways.contains_key(&w) {
                return Err(Lanelet2ParseError::MissingBoundary { lanelet: ll.id, role });
            }
        }
        let left = way_points(ll.left)?;
        let right = way_points(ll.right)?;
        let (centerline, half_width_m) = centerline_and_half_width(&left, &right);
        let heading_rad = heading_of(&centerline);
        graph.add_lane(Lane {
            id: ll.id,
            centerline,
            half_width_m,
            left_line: ways[&ll.left].line,
            right_line: ways[&ll.right].line,
            heading_rad,
            edges: connectivity(ll, &raw_lanelets, &ways),
            // The regulatory control (STOP / YIELD sign or TRAFFIC LIGHT) from the first of this
            // lanelet's `regulatory_element` members that resolves to one; `None` if it has none.
            control: ll.reg_elems.iter().find_map(|e| reg_controls.get(e).copied()),
        });
    }

    // Apply right-of-way: every priority lane gains a yields-to edge to each yield
    // lane (the cross product within each regulatory element). `cedes_to_ego` then
    // derives the cede list at runtime from these + object→lane attribution.
    for (priorities, yields) in &right_of_way {
        for &p in priorities {
            for &y in yields {
                graph.add_right_of_way(p, y);
            }
        }
    }
    Ok(graph)
}

/// Whether a Lanelet2 lanelet `subtype` is **vehicle-driveable**. Explicitly
/// non-vehicle subtypes (walkway / crosswalk / bicycle lane / stairs) are excluded so
/// the router never routes a car over them; `road` / `highway` and an **absent**
/// subtype (common in simple/test maps) are driveable. A `participants:vehicle=no`
/// refinement is a follow-up.
fn is_driveable_subtype(subtype: Option<&str>) -> bool {
    !matches!(
        subtype,
        Some("walkway") | Some("crosswalk") | Some("bicycle_lane") | Some("stairs")
    )
}

struct Way {
    node_ids: Vec<u64>,
    line: LineType,
    /// Raw `subtype` tag, kept so a `traffic_sign` regulatory element that `refers` to this
    /// way (the sign linestring) can resolve the specific sign (stop vs yield).
    subtype: Option<String>,
}

struct RawLanelet {
    id: u64,
    left: u64,
    right: u64,
    /// Ids of the `regulatory_element` relations this lanelet references (role
    /// `regulatory_element`) — resolved to a [`LaneControl`] after the full parse.
    reg_elems: Vec<u64>,
}

/// A regulatory element collected during the scan, resolved to a control afterward (so it is
/// order-independent — the `refers` way may appear after the relation).
struct RawReg {
    id: u64,
    subtype: Option<String>,
    sign_type: Option<String>,
    refers: Option<u64>,
}

/// Derive a lanelet's edges from shared OSM ids: a lateral neighbor shares a boundary
/// way; a successor's boundaries start on the nodes this lanelet's boundaries end on.
fn connectivity(me: &RawLanelet, all: &[RawLanelet], ways: &BTreeMap<u64, Way>) -> Vec<LaneEdge> {
    let mut edges = Vec::new();
    let ends = |w: u64| -> Option<(u64, u64)> {
        let ns = &ways.get(&w)?.node_ids;
        Some((*ns.first()?, *ns.last()?))
    };
    let (Some((_, my_left_end)), Some((_, my_right_end))) = (ends(me.left), ends(me.right)) else {
        return edges;
    };
    for other in all {
        if other.id == me.id {
            continue;
        }
        // Lateral neighbor: a shared boundary way. `me.right == other.left` ⇒ `other`
        // is on my right; `me.left == other.right` ⇒ `other` is on my left.
        if me.right == other.left {
            edges.push(LaneEdge::RightNeighbor { to: other.id });
        }
        if me.left == other.right {
            edges.push(LaneEdge::LeftNeighbor { to: other.id });
        }
        // Successor: `other`'s boundaries START where mine END.
        if let (Some((o_left_start, _)), Some((o_right_start, _))) = (ends(other.left), ends(other.right)) {
            if o_left_start == my_left_end && o_right_start == my_right_end {
                edges.push(LaneEdge::Successor { to: other.id });
            }
        }
    }
    edges
}

/// Per-vertex midpoint centerline + mean half-gap. Vertices are paired by index up to
/// the shorter boundary (typical Lanelet2 boundaries are vertex-aligned; arc-length
/// resampling for ragged pairs is a follow-up). Empty/length-1 → a degenerate lane the
/// graph still stores (the router only needs ids; the checker reads the corridor).
fn centerline_and_half_width(left: &[Point], right: &[Point]) -> (Vec<Point>, f64) {
    let n = left.len().min(right.len());
    if n == 0 {
        return (Vec::new(), 0.0);
    }
    let mut centerline = Vec::with_capacity(n);
    let mut gap_sum = 0.0;
    for i in 0..n {
        let (l, r) = (left[i], right[i]);
        centerline.push(Point { x_m: (l.x_m + r.x_m) / 2.0, y_m: (l.y_m + r.y_m) / 2.0 });
        gap_sum += (l.x_m - r.x_m).hypot(l.y_m - r.y_m);
    }
    (centerline, gap_sum / n as f64 / 2.0)
}

fn heading_of(centerline: &[Point]) -> f64 {
    match (centerline.first(), centerline.last()) {
        (Some(a), Some(b)) if a != b => (b.y_m - a.y_m).atan2(b.x_m - a.x_m),
        _ => 0.0,
    }
}

/// Map a Lanelet2 traffic-sign identifier (a `sign_type`, a `refers` way `subtype`, or a
/// direct element subtype) to a STOP / YIELD [`LaneControl`]. Recognizes the descriptive
/// names and the common German (`deNNN`) / US (`usR1-N`) MUTCD codes, case-insensitively.
/// An unrecognized identifier yields `None` — the parser does not fabricate a control.
fn sign_control(id: &str) -> Option<LaneControl> {
    match id.trim().to_ascii_lowercase().as_str() {
        "stop_sign" | "stop" | "de206" | "usr1-1" => Some(LaneControl::Stop),
        "yield_sign" | "yield" | "give_way" | "de205" | "usr1-2" => Some(LaneControl::Yield),
        _ => None,
    }
}

/// Map a Lanelet2 linestring `type`/`subtype` to a crossing-rule [`LineType`].
/// Fail-safe: an unknown / border / curbstone marking is treated as `Solid` (no
/// crossing) — the conservative default; an over-restrictive line only suppresses a
/// lane change, and KIRRA backstops physical safety regardless. Combined markings
/// (`solid_dashed` / `dashed_solid`) are also conservative `Solid` for now, since their
/// crossable side is linestring-orientation-dependent (a tracked refinement).
fn line_type_of(ty: Option<&str>, subtype: Option<&str>) -> LineType {
    if ty == Some("virtual") {
        return LineType::Unmarked;
    }
    match subtype {
        Some("dashed") | Some("dashed_dashed") => LineType::Broken,
        Some("solid") => LineType::Solid,
        Some("solid_solid") => LineType::DoubleSolid,
        _ => LineType::Solid,
    }
}

// ----- small OSM-XML helpers ------------------------------------------------

fn attr_u64(el: &roxmltree::Node, name: &str) -> Option<u64> {
    el.attribute(name)?.parse().ok()
}

/// The `v` of a child `<tag k="key" v="...">`, if present.
fn tag_value<'a>(el: &roxmltree::Node<'a, 'a>, key: &str) -> Option<&'a str> {
    el.children()
        .filter(|c| c.has_tag_name("tag"))
        .find(|c| c.attribute("k") == Some(key))
        .and_then(|c| c.attribute("v"))
}

fn tag_f64(el: &roxmltree::Node, key: &str) -> Option<f64> {
    tag_value(el, key)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two lanelets forming a successor chain: lanelet 100 (ways 10/11) then lanelet
    // 200 (ways 20/21), whose boundaries START on the nodes 100's boundaries END on
    // (left chain 1→2→3, right chain 4→5→6). Divider markings are dashed.
    const CHAIN: &str = r#"<?xml version="1.0"?>
<osm>
  <node id="1"><tag k="local_x" v="0"/><tag k="local_y" v="1.75"/></node>
  <node id="2"><tag k="local_x" v="30"/><tag k="local_y" v="1.75"/></node>
  <node id="3"><tag k="local_x" v="60"/><tag k="local_y" v="1.75"/></node>
  <node id="4"><tag k="local_x" v="0"/><tag k="local_y" v="-1.75"/></node>
  <node id="5"><tag k="local_x" v="30"/><tag k="local_y" v="-1.75"/></node>
  <node id="6"><tag k="local_x" v="60"/><tag k="local_y" v="-1.75"/></node>
  <way id="10"><nd ref="1"/><nd ref="2"/><tag k="subtype" v="solid"/></way>
  <way id="11"><nd ref="4"/><nd ref="5"/><tag k="subtype" v="dashed"/></way>
  <way id="20"><nd ref="2"/><nd ref="3"/><tag k="subtype" v="solid"/></way>
  <way id="21"><nd ref="5"/><nd ref="6"/><tag k="subtype" v="dashed"/></way>
  <relation id="100"><tag k="type" v="lanelet"/><member type="way" role="left" ref="10"/><member type="way" role="right" ref="11"/></relation>
  <relation id="200"><tag k="type" v="lanelet"/><member type="way" role="left" ref="20"/><member type="way" role="right" ref="21"/></relation>
</osm>"#;

    #[test]
    fn parses_geometry_and_lines() {
        let g = parse_lanelet2_osm(CHAIN).unwrap();
        assert_eq!(g.len(), 2);
        let l = g.lane(100).unwrap();
        // centerline runs along y=0 (midway between ±1.75), half-width 1.75.
        assert!(l.centerline.iter().all(|p| p.y_m.abs() < 1e-9));
        assert!((l.half_width_m - 1.75).abs() < 1e-9);
        assert_eq!(l.left_line, LineType::Solid);
        assert_eq!(l.right_line, LineType::Broken);
        assert!(l.heading_rad.abs() < 1e-9, "runs along +x");
    }

    #[test]
    fn derives_successor_connectivity_and_routes() {
        let g = parse_lanelet2_osm(CHAIN).unwrap();
        // 100's boundaries end on nodes 2/5; 200's start there → 100 → 200.
        assert_eq!(g.route(100, 200), Some(vec![100, 200]));
        assert_eq!(g.route(200, 100), None, "no reverse edge");
    }

    #[test]
    fn derives_lateral_neighbors_from_a_shared_boundary() {
        // Two side-by-side lanelets sharing way 11 (left lanelet's right == right
        // lanelet's left). left=lanelet 1 (ways 10/11), right=lanelet 2 (ways 11/12).
        let xml = r#"<osm>
  <node id="1"><tag k="local_x" v="0"/><tag k="local_y" v="3.5"/></node>
  <node id="2"><tag k="local_x" v="30"/><tag k="local_y" v="3.5"/></node>
  <node id="3"><tag k="local_x" v="0"/><tag k="local_y" v="0"/></node>
  <node id="4"><tag k="local_x" v="30"/><tag k="local_y" v="0"/></node>
  <node id="5"><tag k="local_x" v="0"/><tag k="local_y" v="-3.5"/></node>
  <node id="6"><tag k="local_x" v="30"/><tag k="local_y" v="-3.5"/></node>
  <way id="10"><nd ref="1"/><nd ref="2"/><tag k="subtype" v="solid"/></way>
  <way id="11"><nd ref="3"/><nd ref="4"/><tag k="subtype" v="dashed"/></way>
  <way id="12"><nd ref="5"/><nd ref="6"/><tag k="subtype" v="solid"/></way>
  <relation id="1"><tag k="type" v="lanelet"/><member type="way" role="left" ref="10"/><member type="way" role="right" ref="11"/></relation>
  <relation id="2"><tag k="type" v="lanelet"/><member type="way" role="left" ref="11"/><member type="way" role="right" ref="12"/></relation>
</osm>"#;
        let g = parse_lanelet2_osm(xml).unwrap();
        // Lanelet 2 is on lanelet 1's right (shared way 11) → a single lane change.
        assert_eq!(g.route(1, 2), Some(vec![1, 2]));
    }

    #[test]
    fn virtual_line_is_unmarked() {
        let xml = r#"<osm>
  <node id="1"><tag k="local_x" v="0"/><tag k="local_y" v="1"/></node>
  <node id="2"><tag k="local_x" v="10"/><tag k="local_y" v="1"/></node>
  <node id="3"><tag k="local_x" v="0"/><tag k="local_y" v="-1"/></node>
  <node id="4"><tag k="local_x" v="10"/><tag k="local_y" v="-1"/></node>
  <way id="10"><nd ref="1"/><nd ref="2"/><tag k="type" v="virtual"/></way>
  <way id="11"><nd ref="3"/><nd ref="4"/><tag k="subtype" v="solid"/></way>
  <relation id="1"><tag k="type" v="lanelet"/><member type="way" role="left" ref="10"/><member type="way" role="right" ref="11"/></relation>
</osm>"#;
        assert_eq!(parse_lanelet2_osm(xml).unwrap().lane(1).unwrap().left_line, LineType::Unmarked);
    }

    #[test]
    fn fails_closed_on_a_missing_boundary_and_malformed_xml() {
        let missing = r#"<osm>
  <relation id="1"><tag k="type" v="lanelet"/><member type="way" role="left" ref="99"/></relation>
</osm>"#;
        assert!(matches!(parse_lanelet2_osm(missing), Err(Lanelet2ParseError::IncompleteLanelet(1))));
        assert!(matches!(parse_lanelet2_osm("<osm><relation"), Err(Lanelet2ParseError::Xml(_))));
    }

    #[test]
    fn right_of_way_regulatory_element_derives_the_cede_list() {
        use kirra_core::trajectory::PerceivedObject;
        // Lanelet 1 (priority, along y=0) and lanelet 2 (yielding, along y=10), plus a
        // right_of_way regulatory element granting 1 priority over 2.
        let xml = r#"<osm>
  <node id="1"><tag k="local_x" v="0"/><tag k="local_y" v="1.75"/></node>
  <node id="2"><tag k="local_x" v="30"/><tag k="local_y" v="1.75"/></node>
  <node id="3"><tag k="local_x" v="0"/><tag k="local_y" v="-1.75"/></node>
  <node id="4"><tag k="local_x" v="30"/><tag k="local_y" v="-1.75"/></node>
  <node id="5"><tag k="local_x" v="0"/><tag k="local_y" v="11.75"/></node>
  <node id="6"><tag k="local_x" v="30"/><tag k="local_y" v="11.75"/></node>
  <node id="7"><tag k="local_x" v="0"/><tag k="local_y" v="8.25"/></node>
  <node id="8"><tag k="local_x" v="30"/><tag k="local_y" v="8.25"/></node>
  <way id="10"><nd ref="1"/><nd ref="2"/><tag k="subtype" v="solid"/></way>
  <way id="11"><nd ref="3"/><nd ref="4"/><tag k="subtype" v="solid"/></way>
  <way id="20"><nd ref="5"/><nd ref="6"/><tag k="subtype" v="solid"/></way>
  <way id="21"><nd ref="7"/><nd ref="8"/><tag k="subtype" v="solid"/></way>
  <relation id="1"><tag k="type" v="lanelet"/><tag k="subtype" v="road"/><member type="way" role="left" ref="10"/><member type="way" role="right" ref="11"/></relation>
  <relation id="2"><tag k="type" v="lanelet"/><tag k="subtype" v="road"/><member type="way" role="left" ref="20"/><member type="way" role="right" ref="21"/></relation>
  <relation id="9"><tag k="type" v="regulatory_element"/><tag k="subtype" v="right_of_way"/><member type="relation" role="right_of_way" ref="1"/><member type="relation" role="yield" ref="2"/></relation>
</osm>"#;
        let g = parse_lanelet2_osm(xml).unwrap();
        assert_eq!(g.lanes_yielding_to(1).collect::<Vec<_>>(), vec![2]);

        let obj = |id, x, y| PerceivedObject {
            id,
            pos: Point { x_m: x, y_m: y },
            velocity_mps: 3.0,
            heading_rad: 0.0,
            vel: Point { x_m: 3.0, y_m: 0.0 },
        };
        // An object in the yielding lane 2 cedes to an ego in lane 1; one in the ego's
        // own lane (or off-map) does not.
        let objs = [obj(42, 15.0, 10.0), obj(7, 15.0, 0.0), obj(8, 15.0, 99.0)];
        assert_eq!(g.cedes_to_ego(1, &objs), vec![42]);
        // The ego in the yielding lane asserts no priority.
        assert!(g.cedes_to_ego(2, &objs).is_empty());
    }

    #[test]
    fn traffic_sign_and_light_regulatory_elements_set_the_lane_control() {
        // Lanelet 1 → STOP (traffic_sign whose `refers` way 90 has subtype stop_sign);
        // lanelet 2 → TRAFFIC LIGHT; lanelet 3 → YIELD (traffic_sign with a `sign_type` code);
        // lanelet 4 → uncontrolled (its sign is unrecognized — the parser never fabricates one).
        let xml = r#"<osm>
  <node id="1"><tag k="local_x" v="0"/><tag k="local_y" v="1.75"/></node>
  <node id="2"><tag k="local_x" v="30"/><tag k="local_y" v="1.75"/></node>
  <node id="3"><tag k="local_x" v="0"/><tag k="local_y" v="-1.75"/></node>
  <node id="4"><tag k="local_x" v="30"/><tag k="local_y" v="-1.75"/></node>
  <node id="5"><tag k="local_x" v="0"/><tag k="local_y" v="11.75"/></node>
  <node id="6"><tag k="local_x" v="30"/><tag k="local_y" v="11.75"/></node>
  <node id="7"><tag k="local_x" v="0"/><tag k="local_y" v="8.25"/></node>
  <node id="8"><tag k="local_x" v="30"/><tag k="local_y" v="8.25"/></node>
  <node id="9"><tag k="local_x" v="0"/><tag k="local_y" v="21.75"/></node>
  <node id="10"><tag k="local_x" v="30"/><tag k="local_y" v="21.75"/></node>
  <node id="11"><tag k="local_x" v="0"/><tag k="local_y" v="18.25"/></node>
  <node id="12"><tag k="local_x" v="30"/><tag k="local_y" v="18.25"/></node>
  <node id="13"><tag k="local_x" v="0"/><tag k="local_y" v="31.75"/></node>
  <node id="14"><tag k="local_x" v="30"/><tag k="local_y" v="31.75"/></node>
  <node id="15"><tag k="local_x" v="0"/><tag k="local_y" v="28.25"/></node>
  <node id="16"><tag k="local_x" v="30"/><tag k="local_y" v="28.25"/></node>
  <way id="10"><nd ref="1"/><nd ref="2"/><tag k="subtype" v="solid"/></way>
  <way id="11"><nd ref="3"/><nd ref="4"/><tag k="subtype" v="solid"/></way>
  <way id="20"><nd ref="5"/><nd ref="6"/><tag k="subtype" v="solid"/></way>
  <way id="21"><nd ref="7"/><nd ref="8"/><tag k="subtype" v="solid"/></way>
  <way id="30"><nd ref="9"/><nd ref="10"/><tag k="subtype" v="solid"/></way>
  <way id="31"><nd ref="11"/><nd ref="12"/><tag k="subtype" v="solid"/></way>
  <way id="40"><nd ref="13"/><nd ref="14"/><tag k="subtype" v="solid"/></way>
  <way id="41"><nd ref="15"/><nd ref="16"/><tag k="subtype" v="solid"/></way>
  <way id="90"><nd ref="2"/><tag k="type" v="traffic_sign"/><tag k="subtype" v="stop_sign"/></way>
  <relation id="1"><tag k="type" v="lanelet"/><member type="way" role="left" ref="10"/><member type="way" role="right" ref="11"/><member type="relation" role="regulatory_element" ref="50"/></relation>
  <relation id="2"><tag k="type" v="lanelet"/><member type="way" role="left" ref="20"/><member type="way" role="right" ref="21"/><member type="relation" role="regulatory_element" ref="60"/></relation>
  <relation id="3"><tag k="type" v="lanelet"/><member type="way" role="left" ref="30"/><member type="way" role="right" ref="31"/><member type="relation" role="regulatory_element" ref="70"/></relation>
  <relation id="4"><tag k="type" v="lanelet"/><member type="way" role="left" ref="40"/><member type="way" role="right" ref="41"/><member type="relation" role="regulatory_element" ref="80"/></relation>
  <relation id="50"><tag k="type" v="regulatory_element"/><tag k="subtype" v="traffic_sign"/><member type="way" role="refers" ref="90"/></relation>
  <relation id="60"><tag k="type" v="regulatory_element"/><tag k="subtype" v="traffic_light"/></relation>
  <relation id="70"><tag k="type" v="regulatory_element"/><tag k="subtype" v="traffic_sign"/><tag k="sign_type" v="de205"/></relation>
  <relation id="80"><tag k="type" v="regulatory_element"/><tag k="subtype" v="traffic_sign"/><tag k="sign_type" v="speed_limit_50"/></relation>
</osm>"#;
        let g = parse_lanelet2_osm(xml).unwrap();
        assert_eq!(g.lane(1).unwrap().control, Some(LaneControl::Stop), "STOP via refers-way subtype stop_sign");
        assert_eq!(g.lane(2).unwrap().control, Some(LaneControl::TrafficLight), "TRAFFIC LIGHT");
        assert_eq!(g.lane(3).unwrap().control, Some(LaneControl::Yield), "YIELD via sign_type de205");
        assert_eq!(g.lane(4).unwrap().control, None, "an unrecognized sign fabricates no control");
    }

    #[test]
    fn a_lanelet_with_no_regulatory_element_is_uncontrolled() {
        // The CHAIN fixture has no regulatory elements → every lane's control is None.
        let g = parse_lanelet2_osm(CHAIN).unwrap();
        assert!(g.lane(100).unwrap().control.is_none());
        assert!(g.lane(200).unwrap().control.is_none());
    }

    #[test]
    fn non_vehicle_lanelets_are_filtered_out() {
        // A road lanelet and a crosswalk lanelet; only the road becomes a lane.
        let xml = r#"<osm>
  <node id="1"><tag k="local_x" v="0"/><tag k="local_y" v="1"/></node>
  <node id="2"><tag k="local_x" v="30"/><tag k="local_y" v="1"/></node>
  <node id="3"><tag k="local_x" v="0"/><tag k="local_y" v="-1"/></node>
  <node id="4"><tag k="local_x" v="30"/><tag k="local_y" v="-1"/></node>
  <way id="10"><nd ref="1"/><nd ref="2"/><tag k="subtype" v="solid"/></way>
  <way id="11"><nd ref="3"/><nd ref="4"/><tag k="subtype" v="solid"/></way>
  <relation id="1"><tag k="type" v="lanelet"/><tag k="subtype" v="road"/><member type="way" role="left" ref="10"/><member type="way" role="right" ref="11"/></relation>
  <relation id="2"><tag k="type" v="lanelet"/><tag k="subtype" v="crosswalk"/><member type="way" role="left" ref="10"/><member type="way" role="right" ref="11"/></relation>
</osm>"#;
        let g = parse_lanelet2_osm(xml).unwrap();
        assert_eq!(g.len(), 1, "the crosswalk lanelet is excluded");
        assert!(g.lane(1).is_some() && g.lane(2).is_none());
    }
}
