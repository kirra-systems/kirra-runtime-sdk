// parko/crates/parko-ros2/src/commit_zone_producer.rs
//
// #1124 (SG5 go-live, producer half) — the MAP-ANCHORED COMMIT-ZONE PRODUCER.
//
// The SG5 commit-zone gate (`scene_vetoes::apply_commit_zone_gate` over
// `parko_core::commit_zone`) shipped fully tested but with NO producer for its
// scene slot: arming it meant a permanent fail-closed immobilization, which the
// #795 F6 startup guard refuses without an explicit acknowledgment. This module
// is the first real producer — map-anchored, NO new perception:
//
//   * The ZONES are a site-authored, versioned JSON artifact (rail crossings,
//     box junctions, narrow bridges — polygons in the map frame), loaded and
//     validated FAIL-CLOSED at startup (`parse_commit_zone_spec`; any defect
//     aborts the node rather than producing a partial map).
//   * The ANCHOR is the ego pose (`PARKO_POSE_TOPIC`, `nav_msgs/Odometry`):
//     the producer computes the ego→zone distance each tick. A missing, stale,
//     or non-finite pose yields `CommitZoneScene::Unknown` — which the gate
//     vetoes ("Reject fires from MAP ALONE", the SG5 robustness property; an
//     unanchored map prior is exactly the `Unknown` case).
//   * The ENTRY CONFIRMATIONS are DERIVED, never asserted: `exit_verified` via
//     `exit_clearance_verified` over supplied receiving-space evidence, and
//     `clearance_confirmed` via `non_yielding_clearance` over the supplied
//     non-yielding-agent scene. The node currently supplies NO evidence
//     (`NonYieldingScene::Absent`, no exit measurement) — so a zone within the
//     look-ahead VETOES (stop short), which is SG5's intended fail-closed
//     behaviour until the #107/#108 evidence ingestion lands. The evidence
//     parameters exist so that landing is a producer change, not a rework, and
//     the tests prove entry becomes earnable the moment evidence arrives.
//
// CONSERVATISM OF THE GEOMETRY: the ego→zone distance is the EUCLIDEAN distance
// to the zone polygon (0 inside). Parko has no route/path model, so this
// LOWER-BOUNDS any along-path distance — the veto can only fire EARLIER than a
// path-aware producer would, never later. A zone near (but not on) the ego's
// actual path can over-veto; that is the accepted conservative direction and a
// site chooses its zone polygons accordingly.
//
// LOCALIZATION COUPLING: pose FRESHNESS is enforced here (stale → `Unknown`).
// Pose ACCURACY is AOU-LOCALIZATION-001 (the ≤0.10 m 95th-pct lateral AoU that
// every map-anchored trust in this stack rides on); when a frame-integrity feed
// lands in this node, `parko_core::localization::gate_commit_zone_scene`
// composes on top of this producer unchanged (untrusted pose → `Unknown`).

use parko_core::commit_zone::{
    exit_clearance_verified, non_yielding_clearance, CommitZoneCfg, CommitZoneMap, CommitZoneScene,
    ExitClearanceEvidence, NonYieldingScene,
};

use crate::scene_vetoes::StampedScene;

/// The one spec schema version this build understands. A different version is
/// a REFUSAL, never a best-effort parse (a future schema could carry fields
/// whose absence changes safety semantics).
pub const COMMIT_ZONE_SPEC_VERSION: u64 = 1;

/// Confidence carried on the produced `CommitZoneMap` prior. The zone map is a
/// STATIC, load-time-validated artifact — its "confidence" is not a live
/// estimate, so a validated spec is fully confident; what actually varies at
/// runtime is the POSE anchoring, carried in the map's `age_ms` (the pose age)
/// against `max_age_ms` (the pose staleness budget). The floor is kept at the
/// containment corridor's conventional 0.5 so the health check stays
/// two-sided (a future live map source can lower `confidence` meaningfully).
pub const STATIC_MAP_CONFIDENCE: f32 = 1.0;
/// See [`STATIC_MAP_CONFIDENCE`].
pub const STATIC_MAP_MIN_CONFIDENCE: f32 = 0.5;

/// One mapped commit zone: a closed polygon in the map frame.
#[derive(Debug, Clone, PartialEq)]
pub struct CommitZone {
    /// Site-unique zone id (nonempty; e.g. `rail-xing-7`).
    pub id: String,
    /// Informational kind tag (e.g. `rail_crossing` / `box_junction` /
    /// `narrow_bridge`). Not interpreted by the producer — the veto semantics
    /// are identical for every kind (SG5 does not grade commit zones).
    pub kind: String,
    /// Closed polygon vertices `(x_m, y_m)`, map frame, ≥3, all finite.
    /// The closing edge (last → first) is implicit.
    pub polygon: Vec<(f64, f64)>,
    /// Along-path traverse length used for clearance timing, DERIVED at load as
    /// the polygon DIAMETER (max pairwise vertex distance). The diameter upper-
    /// bounds any straight traverse chord, so the derived clearance time is
    /// CONSERVATIVE (a longer assumed zone → a longer required gap).
    pub length_m: f64,
}

/// The loaded, validated commit-zone map (≥1 zone, unique ids).
#[derive(Debug, Clone, PartialEq)]
pub struct CommitZoneSpec {
    pub zones: Vec<CommitZone>,
}

/// Why a spec was refused. Every variant is a startup ABORT in the binary —
/// a defective zone map must never anchor a safety veto.
#[derive(Debug, Clone, PartialEq)]
pub enum SpecError {
    /// Not valid JSON, or the top level is not an object.
    Malformed(String),
    /// `version` missing or not the supported [`COMMIT_ZONE_SPEC_VERSION`].
    UnsupportedVersion(Option<u64>),
    /// `zones` missing, not an array, or empty. An EMPTY zone map is refused
    /// deliberately: a site with no commit zones should not configure the
    /// producer at all (and not arm the gate) — an empty spec is far more
    /// likely an authoring error than a statement of fact.
    NoZones,
    /// A zone entry failed validation; the message names the zone and defect.
    BadZone(String),
    /// Two zones share an id.
    DuplicateId(String),
}

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpecError::Malformed(m) => write!(f, "commit-zone spec is not valid JSON: {m}"),
            SpecError::UnsupportedVersion(v) => write!(
                f,
                "commit-zone spec version {v:?} is not the supported version \
                 {COMMIT_ZONE_SPEC_VERSION}"
            ),
            SpecError::NoZones => write!(
                f,
                "commit-zone spec has no zones — a site with no commit zones should not \
                 configure the producer (an empty spec is refused as a likely authoring error)"
            ),
            SpecError::BadZone(m) => write!(f, "commit-zone spec zone invalid: {m}"),
            SpecError::DuplicateId(id) => {
                write!(f, "commit-zone spec has duplicate zone id {id:?}")
            }
        }
    }
}

impl std::error::Error for SpecError {}

/// Parse + validate a commit-zone spec JSON document (fail-closed: any defect
/// refuses the WHOLE spec — never a partial zone map).
///
/// Schema (version 1):
/// ```json
/// {
///   "version": 1,
///   "zones": [
///     { "id": "rail-xing-7", "kind": "rail_crossing",
///       "polygon": [[12.0, 3.0], [18.0, 3.0], [18.0, 9.0], [12.0, 9.0]] }
///   ]
/// }
/// ```
// SAFETY: SG5 | REQ: commit-zone-spec-fail-closed-load | TEST: spec_happy_path_parses,spec_wrong_version_refused,spec_missing_or_empty_zones_refused,spec_duplicate_id_refused,spec_bad_polygon_refused,spec_nonfinite_vertex_refused,spec_degenerate_polygon_refused,spec_malformed_json_refused
pub fn parse_commit_zone_spec(json: &str) -> Result<CommitZoneSpec, SpecError> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| SpecError::Malformed(e.to_string()))?;
    let obj = root
        .as_object()
        .ok_or_else(|| SpecError::Malformed("top level is not an object".to_string()))?;

    let version = obj.get("version").and_then(|v| v.as_u64());
    if version != Some(COMMIT_ZONE_SPEC_VERSION) {
        return Err(SpecError::UnsupportedVersion(version));
    }

    let zones_raw = obj
        .get("zones")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or(SpecError::NoZones)?;

    let mut zones = Vec::with_capacity(zones_raw.len());
    let mut seen_ids = std::collections::BTreeSet::new();
    for (i, z) in zones_raw.iter().enumerate() {
        let zobj = z
            .as_object()
            .ok_or_else(|| SpecError::BadZone(format!("zones[{i}] is not an object")))?;
        let id = zobj
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| SpecError::BadZone(format!("zones[{i}] has no nonempty string id")))?
            .to_string();
        if !seen_ids.insert(id.clone()) {
            return Err(SpecError::DuplicateId(id));
        }
        let kind = zobj
            .get("kind")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| SpecError::BadZone(format!("zone {id:?} has no nonempty string kind")))?
            .to_string();
        let poly_raw = zobj
            .get("polygon")
            .and_then(|v| v.as_array())
            .ok_or_else(|| SpecError::BadZone(format!("zone {id:?} has no polygon array")))?;
        if poly_raw.len() < 3 {
            return Err(SpecError::BadZone(format!(
                "zone {id:?} polygon has {} vertices (need >= 3)",
                poly_raw.len()
            )));
        }
        let mut polygon = Vec::with_capacity(poly_raw.len());
        for (j, v) in poly_raw.iter().enumerate() {
            let pair = v.as_array().filter(|a| a.len() == 2).ok_or_else(|| {
                SpecError::BadZone(format!("zone {id:?} polygon[{j}] is not an [x, y] pair"))
            })?;
            let x = pair[0].as_f64();
            let y = pair[1].as_f64();
            match (x, y) {
                (Some(x), Some(y)) if x.is_finite() && y.is_finite() => polygon.push((x, y)),
                _ => {
                    return Err(SpecError::BadZone(format!(
                        "zone {id:?} polygon[{j}] has a non-finite or non-numeric coordinate"
                    )))
                }
            }
        }
        let length_m = polygon_diameter(&polygon);
        if !(length_m.is_finite() && length_m > 0.0) {
            return Err(SpecError::BadZone(format!(
                "zone {id:?} polygon is degenerate (diameter {length_m}) — all vertices coincide"
            )));
        }
        zones.push(CommitZone {
            id,
            kind,
            polygon,
            length_m,
        });
    }
    Ok(CommitZoneSpec { zones })
}

/// Max pairwise vertex distance — the conservative traverse-length bound.
fn polygon_diameter(polygon: &[(f64, f64)]) -> f64 {
    let mut d: f64 = 0.0;
    for i in 0..polygon.len() {
        for j in (i + 1)..polygon.len() {
            let (ax, ay) = polygon[i];
            let (bx, by) = polygon[j];
            d = d.max((ax - bx).hypot(ay - by));
        }
    }
    d
}

/// The ego pose sample the producer anchors on (map frame, metres). Heading is
/// deliberately absent: the Euclidean lower-bound distance needs none, and a
/// heading-dependent "is the zone ahead of me" filter would RELAX the bound
/// (a zone behind a reversing ego must still bind).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EgoPose {
    pub x_m: f64,
    pub y_m: f64,
}

/// Distance from a point to a closed polygon: 0.0 inside (or on the boundary),
/// else the minimum distance to any edge. Assumes finite, pre-validated
/// vertices (the spec loader enforces this); the POINT is the caller's to
/// validate (the producer refuses a non-finite pose before calling).
// SAFETY: SG5 | REQ: commit-zone-distance-conservative | TEST: distance_inside_is_zero,distance_to_edge_hand_checked,distance_to_vertex_hand_checked,distance_on_boundary_is_zero
pub fn distance_to_zone(point: (f64, f64), polygon: &[(f64, f64)]) -> f64 {
    if point_in_polygon(point, polygon) {
        return 0.0;
    }
    let mut best = f64::INFINITY;
    for i in 0..polygon.len() {
        let a = polygon[i];
        let b = polygon[(i + 1) % polygon.len()];
        best = best.min(point_segment_distance(point, a, b));
    }
    best
}

fn point_segment_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (px, py) = p;
    let (ax, ay) = a;
    let (bx, by) = b;
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    // Degenerate edge (repeated vertex) → distance to the point itself.
    if len2 <= 0.0 {
        return (px - ax).hypot(py - ay);
    }
    let t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
    let (cx, cy) = (ax + t * dx, ay + t * dy);
    (px - cx).hypot(py - cy)
}

/// Even-odd ray cast; boundary points count as INSIDE via a small edge-distance
/// check (conservative: on-the-line reads distance 0, never a sliver positive).
fn point_in_polygon(p: (f64, f64), polygon: &[(f64, f64)]) -> bool {
    let (px, py) = p;
    let mut inside = false;
    let n = polygon.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = polygon[i];
        let (xj, yj) = polygon[j];
        if ((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// The entry-confirmation evidence the node supplies alongside the map prior.
/// TODAY the node passes the no-evidence values (`NonYieldingScene::Absent`,
/// `exit: None`) — both derivations then FAIL CLOSED and a zone within the
/// look-ahead vetoes (stop short). When the #107/#108 evidence ingestion lands,
/// the node fills these from perception and entry becomes earnable with NO
/// change to this producer.
pub struct CommitZoneEvidence<'a> {
    /// The non-yielding-crosser scene (train / fast agent). `Absent` → the
    /// clearance derivation fails closed (never "no train").
    pub non_yielding: &'a NonYieldingScene,
    /// Measured downstream receiving space beyond the zone's far edge. `None` →
    /// the exit derivation fails closed (an unmeasured exit is NO exit).
    pub exit: Option<&'a ExitClearanceEvidence>,
    /// The ego speed (m/s) used for the traverse-time half of the clearance
    /// derivation (the tick's proposed |linear| speed).
    pub ego_speed_mps: f64,
}

impl CommitZoneEvidence<'_> {
    /// The node's current no-evidence instance (#107/#108 ingestion deferred).
    #[must_use]
    pub fn absent(ego_speed_mps: f64) -> CommitZoneEvidence<'static> {
        CommitZoneEvidence {
            non_yielding: &NonYieldingScene::Absent,
            exit: None,
            ego_speed_mps,
        }
    }
}

/// Produce this tick's [`CommitZoneScene`] from the static zone spec anchored
/// on the latest ego pose.
///
/// FAIL-CLOSED lattice:
///   * pose missing / stale / future-skewed (per [`StampedScene::is_fresh`]) /
///     non-finite → [`CommitZoneScene::Unknown`] (an unanchored map prior; the
///     gate vetoes — "Reject fires from MAP ALONE").
///   * otherwise → [`CommitZoneScene::ZoneAhead`] for the NEAREST zone, with
///     `distance_to_zone_m` the Euclidean lower bound (0 inside),
///     `zone_length_m` the zone's conservative diameter, and BOTH entry
///     confirmations DERIVED from `evidence` via the parko-core primitives
///     (no-evidence → both false → the gate vetoes within the look-ahead; a
///     zone beyond the look-ahead is not yet actionable and does not veto).
///
/// This producer never emits `NoZone`: a validated spec always carries ≥1 zone
/// and "far away" is expressed as a large `distance_to_zone_m`, which the
/// gate's look-ahead horizon already treats as not-yet-actionable — identical
/// routing outcome, without a second horizon constant living here.
// SAFETY: SG5 | REQ: commit-zone-producer-fail-closed | TEST: producer_no_pose_is_unknown,producer_stale_pose_is_unknown,producer_future_skewed_pose_is_unknown,producer_nonfinite_pose_is_unknown,producer_reports_nearest_zone,producer_no_evidence_vetoes_within_lookahead,producer_far_zone_does_not_veto,producer_entry_earnable_with_evidence,producer_inside_zone_distance_zero,producer_map_health_carries_pose_age
pub fn produce_commit_zone_scene(
    spec: &CommitZoneSpec,
    pose: Option<&StampedScene<EgoPose>>,
    pose_max_age_ms: u64,
    now_ms: u64,
    cfg: &CommitZoneCfg,
    evidence: &CommitZoneEvidence<'_>,
) -> CommitZoneScene {
    let stamped = match pose {
        Some(s) if s.is_fresh(now_ms, pose_max_age_ms) => s,
        _ => return CommitZoneScene::Unknown,
    };
    let ego = stamped.scene;
    if !(ego.x_m.is_finite() && ego.y_m.is_finite()) {
        return CommitZoneScene::Unknown;
    }

    // Nearest zone by the conservative Euclidean lower bound. The spec loader
    // guarantees ≥1 zone, so the fold always yields one; the defensive Unknown
    // arm keeps an impossible empty spec fail-closed rather than panicking.
    let Some((zone, distance_m)) = spec
        .zones
        .iter()
        .map(|z| (z, distance_to_zone((ego.x_m, ego.y_m), &z.polygon)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
    else {
        return CommitZoneScene::Unknown;
    };

    let map = CommitZoneMap {
        zone_ahead: true,
        distance_to_zone_m: distance_m,
        confidence: STATIC_MAP_CONFIDENCE,
        // The static prior's freshness IS the pose anchoring's freshness, so the
        // gate's own `is_healthy` re-verifies what the producer just checked
        // (belt-and-braces; the two staleness gates share one budget).
        age_ms: stamped.age_ms(now_ms),
        min_confidence: STATIC_MAP_MIN_CONFIDENCE,
        max_age_ms: pose_max_age_ms,
    };
    // DERIVED confirmations — never asserted (the #107/#108 discipline).
    let exit_verified = evidence
        .exit
        .map(|e| exit_clearance_verified(e, cfg))
        .unwrap_or(false);
    let clearance_confirmed = non_yielding_clearance(
        evidence.non_yielding,
        &map,
        zone.length_m,
        evidence.ego_speed_mps,
        cfg,
    );
    CommitZoneScene::ZoneAhead {
        map,
        clearance_confirmed,
        exit_verified,
        zone_length_m: zone.length_m,
        proposed_stop_distance_m: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parko_core::commit_zone::commit_zone_blocked;

    /// A valid two-zone spec: a 6×6 square at x∈[12,18], y∈[3,9] and a far
    /// 4×4 square at x∈[200,204], y∈[0,4].
    fn spec_json() -> &'static str {
        r#"{
            "version": 1,
            "zones": [
                { "id": "rail-xing-7", "kind": "rail_crossing",
                  "polygon": [[12.0, 3.0], [18.0, 3.0], [18.0, 9.0], [12.0, 9.0]] },
                { "id": "box-junction-2", "kind": "box_junction",
                  "polygon": [[200.0, 0.0], [204.0, 0.0], [204.0, 4.0], [200.0, 4.0]] }
            ]
        }"#
    }

    fn spec() -> CommitZoneSpec {
        parse_commit_zone_spec(spec_json()).expect("fixture spec parses")
    }

    fn fresh_pose(x: f64, y: f64) -> StampedScene<EgoPose> {
        StampedScene {
            scene: EgoPose { x_m: x, y_m: y },
            stamp_ms: 1_000,
        }
    }

    fn cfg() -> CommitZoneCfg {
        CommitZoneCfg::default() // look_ahead 94 m, vehicle 4.5 m, margin 1.0 m
    }

    // ---- spec load (fail-closed) -------------------------------------------

    #[test]
    fn spec_happy_path_parses() {
        let s = spec();
        assert_eq!(s.zones.len(), 2);
        assert_eq!(s.zones[0].id, "rail-xing-7");
        assert_eq!(s.zones[0].kind, "rail_crossing");
        // Diameter of a 6×6 square = 6·√2.
        let expect = 6.0 * std::f64::consts::SQRT_2;
        assert!(
            (s.zones[0].length_m - expect).abs() < 1e-9,
            "derived length must be the polygon diameter; got {}",
            s.zones[0].length_m
        );
    }

    #[test]
    fn spec_wrong_version_refused() {
        let bad = spec_json().replace("\"version\": 1", "\"version\": 2");
        assert_eq!(
            parse_commit_zone_spec(&bad),
            Err(SpecError::UnsupportedVersion(Some(2)))
        );
        let missing = r#"{ "zones": [] }"#;
        assert_eq!(
            parse_commit_zone_spec(missing),
            Err(SpecError::UnsupportedVersion(None))
        );
    }

    #[test]
    fn spec_missing_or_empty_zones_refused() {
        for bad in [
            r#"{ "version": 1 }"#,
            r#"{ "version": 1, "zones": [] }"#,
            r#"{ "version": 1, "zones": 42 }"#,
        ] {
            assert_eq!(
                parse_commit_zone_spec(bad),
                Err(SpecError::NoZones),
                "{bad}"
            );
        }
    }

    #[test]
    fn spec_duplicate_id_refused() {
        let bad = spec_json().replace("box-junction-2", "rail-xing-7");
        assert_eq!(
            parse_commit_zone_spec(&bad),
            Err(SpecError::DuplicateId("rail-xing-7".to_string()))
        );
    }

    #[test]
    fn spec_bad_polygon_refused() {
        // Two vertices only.
        let two = r#"{ "version": 1, "zones": [
            { "id": "z", "kind": "k", "polygon": [[0.0, 0.0], [1.0, 1.0]] } ] }"#;
        assert!(matches!(
            parse_commit_zone_spec(two),
            Err(SpecError::BadZone(_))
        ));
        // A vertex that is not an [x, y] pair.
        let pair = r#"{ "version": 1, "zones": [
            { "id": "z", "kind": "k", "polygon": [[0.0, 0.0], [1.0], [1.0, 1.0]] } ] }"#;
        assert!(matches!(
            parse_commit_zone_spec(pair),
            Err(SpecError::BadZone(_))
        ));
        // Missing id / kind.
        let noid = r#"{ "version": 1, "zones": [
            { "kind": "k", "polygon": [[0.0,0.0],[1.0,0.0],[1.0,1.0]] } ] }"#;
        assert!(matches!(
            parse_commit_zone_spec(noid),
            Err(SpecError::BadZone(_))
        ));
        let nokind = r#"{ "version": 1, "zones": [
            { "id": "z", "polygon": [[0.0,0.0],[1.0,0.0],[1.0,1.0]] } ] }"#;
        assert!(matches!(
            parse_commit_zone_spec(nokind),
            Err(SpecError::BadZone(_))
        ));
    }

    #[test]
    fn spec_nonfinite_vertex_refused() {
        // JSON has no NaN/Infinity literal — a string smuggle must also refuse
        // (as_f64 on a string is None → non-numeric).
        let bad = r#"{ "version": 1, "zones": [
            { "id": "z", "kind": "k", "polygon": [[0.0, 0.0], ["NaN", 0.0], [1.0, 1.0]] } ] }"#;
        assert!(matches!(
            parse_commit_zone_spec(bad),
            Err(SpecError::BadZone(_))
        ));
    }

    #[test]
    fn spec_degenerate_polygon_refused() {
        let bad = r#"{ "version": 1, "zones": [
            { "id": "z", "kind": "k", "polygon": [[5.0, 5.0], [5.0, 5.0], [5.0, 5.0]] } ] }"#;
        assert!(matches!(
            parse_commit_zone_spec(bad),
            Err(SpecError::BadZone(_))
        ));
    }

    #[test]
    fn spec_malformed_json_refused() {
        assert!(matches!(
            parse_commit_zone_spec("not json"),
            Err(SpecError::Malformed(_))
        ));
        assert!(matches!(
            parse_commit_zone_spec("[1, 2, 3]"),
            Err(SpecError::Malformed(_))
        ));
    }

    // ---- geometry (hand-checked) -------------------------------------------

    const UNIT_SQUARE: [(f64, f64); 4] = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];

    #[test]
    fn distance_inside_is_zero() {
        assert_eq!(distance_to_zone((0.5, 0.5), &UNIT_SQUARE), 0.0);
    }

    #[test]
    fn distance_to_edge_hand_checked() {
        // (2, 0.5) is 1.0 right of the unit square's right edge.
        let d = distance_to_zone((2.0, 0.5), &UNIT_SQUARE);
        assert!((d - 1.0).abs() < 1e-12, "got {d}");
    }

    #[test]
    fn distance_to_vertex_hand_checked() {
        // (2, 2) is nearest the (1, 1) corner → √2.
        let d = distance_to_zone((2.0, 2.0), &UNIT_SQUARE);
        assert!((d - std::f64::consts::SQRT_2).abs() < 1e-12, "got {d}");
    }

    #[test]
    fn distance_on_boundary_is_zero() {
        // A point ON an edge must read 0 (edge distance is 0 even when the
        // even-odd inside test is ambiguous on the boundary).
        let d = distance_to_zone((1.0, 0.5), &UNIT_SQUARE);
        assert!(d.abs() < 1e-12, "boundary point must read 0, got {d}");
    }

    // ---- producer (fail-closed lattice) --------------------------------------

    #[test]
    fn producer_no_pose_is_unknown() {
        let s = produce_commit_zone_scene(
            &spec(),
            None,
            500,
            1_000,
            &cfg(),
            &CommitZoneEvidence::absent(1.0),
        );
        assert!(matches!(s, CommitZoneScene::Unknown));
        assert!(
            commit_zone_blocked(&s, &cfg()),
            "an unanchored map prior must veto (Reject from map alone)"
        );
    }

    #[test]
    fn producer_stale_pose_is_unknown() {
        let pose = fresh_pose(0.0, 6.0); // stamp 1_000
        let s = produce_commit_zone_scene(
            &spec(),
            Some(&pose),
            500,
            10_000, // 9 s later — stale
            &cfg(),
            &CommitZoneEvidence::absent(1.0),
        );
        assert!(matches!(s, CommitZoneScene::Unknown));
    }

    #[test]
    fn producer_future_skewed_pose_is_unknown() {
        // #770 F4 parity: a pose stamped implausibly in the future is a clock
        // fault, not an age-0 anchor.
        let pose = StampedScene {
            scene: EgoPose { x_m: 0.0, y_m: 6.0 },
            stamp_ms: 100_000,
        };
        let s = produce_commit_zone_scene(
            &spec(),
            Some(&pose),
            500,
            1_000,
            &cfg(),
            &CommitZoneEvidence::absent(1.0),
        );
        assert!(matches!(s, CommitZoneScene::Unknown));
    }

    #[test]
    fn producer_nonfinite_pose_is_unknown() {
        for (x, y) in [(f64::NAN, 6.0), (0.0, f64::INFINITY)] {
            let pose = fresh_pose(x, y);
            let s = produce_commit_zone_scene(
                &spec(),
                Some(&pose),
                500,
                1_000,
                &cfg(),
                &CommitZoneEvidence::absent(1.0),
            );
            assert!(
                matches!(s, CommitZoneScene::Unknown),
                "non-finite pose ({x},{y}) must be Unknown"
            );
        }
    }

    #[test]
    fn producer_reports_nearest_zone() {
        // Ego at (0, 6): 12 m left of the rail crossing's near edge, ~190 m from
        // the box junction → the rail crossing is the reported zone.
        let pose = fresh_pose(0.0, 6.0);
        let s = produce_commit_zone_scene(
            &spec(),
            Some(&pose),
            500,
            1_000,
            &cfg(),
            &CommitZoneEvidence::absent(1.0),
        );
        match s {
            CommitZoneScene::ZoneAhead {
                map, zone_length_m, ..
            } => {
                assert!(
                    (map.distance_to_zone_m - 12.0).abs() < 1e-9,
                    "nearest-zone distance must be the rail crossing's 12 m, got {}",
                    map.distance_to_zone_m
                );
                let expect_len = 6.0 * std::f64::consts::SQRT_2;
                assert!((zone_length_m - expect_len).abs() < 1e-9);
            }
            other => panic!("expected ZoneAhead, got {other:?}"),
        }
    }

    #[test]
    fn producer_no_evidence_vetoes_within_lookahead() {
        // The go-live semantics: a zone within the look-ahead with NO evidence
        // (Absent non-yielding scene, unmeasured exit) VETOES — stop short.
        let pose = fresh_pose(0.0, 6.0); // 12 m out, well within 94 m
        let s = produce_commit_zone_scene(
            &spec(),
            Some(&pose),
            500,
            1_000,
            &cfg(),
            &CommitZoneEvidence::absent(1.0),
        );
        match &s {
            CommitZoneScene::ZoneAhead {
                clearance_confirmed,
                exit_verified,
                ..
            } => {
                assert!(!clearance_confirmed, "Absent scene must derive NOT clear");
                assert!(!exit_verified, "unmeasured exit must derive NOT verified");
            }
            other => panic!("expected ZoneAhead, got {other:?}"),
        }
        assert!(
            commit_zone_blocked(&s, &cfg()),
            "no-evidence zone within look-ahead must veto (stop short)"
        );
    }

    #[test]
    fn producer_far_zone_does_not_veto() {
        // Ego far from every zone: nearest distance > look-ahead → the gate's
        // horizon rule leaves the command unbound (no over-veto in open space).
        let pose = fresh_pose(-500.0, 6.0);
        let s = produce_commit_zone_scene(
            &spec(),
            Some(&pose),
            500,
            1_000,
            &cfg(),
            &CommitZoneEvidence::absent(1.0),
        );
        assert!(
            !commit_zone_blocked(&s, &cfg()),
            "a zone beyond the look-ahead must not veto"
        );
    }

    #[test]
    fn producer_entry_earnable_with_evidence() {
        // The derivation seam is NOT welded shut: positive non-yielding
        // clearance (KnownNone) + ample measured receiving space earn entry.
        let pose = fresh_pose(0.0, 6.0);
        let exit = ExitClearanceEvidence {
            downstream_clear_m: 20.0, // >> 4.5 + 1.0
        };
        let evidence = CommitZoneEvidence {
            non_yielding: &NonYieldingScene::KnownNone,
            exit: Some(&exit),
            ego_speed_mps: 1.0,
        };
        let s = produce_commit_zone_scene(&spec(), Some(&pose), 500, 1_000, &cfg(), &evidence);
        match &s {
            CommitZoneScene::ZoneAhead {
                clearance_confirmed,
                exit_verified,
                ..
            } => {
                assert!(clearance_confirmed);
                assert!(exit_verified);
            }
            other => panic!("expected ZoneAhead, got {other:?}"),
        }
        assert!(
            !commit_zone_blocked(&s, &cfg()),
            "confirmed + verified entry on a fresh anchor must not veto"
        );
    }

    #[test]
    fn producer_inside_zone_distance_zero() {
        // Ego INSIDE the rail crossing → distance 0 (within look-ahead, no
        // evidence → veto: the gate holds the stop while inside an unconfirmed
        // zone rather than fabricating clearance).
        let pose = fresh_pose(15.0, 6.0);
        let s = produce_commit_zone_scene(
            &spec(),
            Some(&pose),
            500,
            1_000,
            &cfg(),
            &CommitZoneEvidence::absent(1.0),
        );
        match &s {
            CommitZoneScene::ZoneAhead { map, .. } => {
                assert_eq!(map.distance_to_zone_m, 0.0);
            }
            other => panic!("expected ZoneAhead, got {other:?}"),
        }
        assert!(commit_zone_blocked(&s, &cfg()));
    }

    #[test]
    fn producer_map_health_carries_pose_age() {
        // The produced map prior's age is the POSE age against the pose budget,
        // so the gate's own is_healthy re-verifies the anchoring freshness.
        let pose = fresh_pose(0.0, 6.0); // stamp 1_000
        let s = produce_commit_zone_scene(
            &spec(),
            Some(&pose),
            500,
            1_300, // age 300 of budget 500 — fresh
            &cfg(),
            &CommitZoneEvidence::absent(1.0),
        );
        match s {
            CommitZoneScene::ZoneAhead { map, .. } => {
                assert_eq!(map.age_ms, 300);
                assert_eq!(map.max_age_ms, 500);
                assert!(map.is_healthy());
            }
            other => panic!("expected ZoneAhead, got {other:?}"),
        }
    }
}
