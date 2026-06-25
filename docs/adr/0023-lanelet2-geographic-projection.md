# ADR-0023: Lanelet2 geographic (lat/lon) projection

| Field | Value |
|---|---|
| Status | **Proposed (design note)** — ratified on merge. |
| Date | 2026-06-25 |
| Deciders | Project / safety-case owner |
| Safety goals | n/a directly — a map-loader feature. Fail-closed parsing preserves the SG2/SG5 inputs (containment corridor, right-of-way, controls) the downstream derivations consume. |
| Cross-refs | roadmap #1 (the map-file parse); code: `crates/kirra-map/src/lanelet2.rs` (`parse_lanelet2_osm`, `project_geographic`, `WGS84_RADIUS_M`); tests in the same module |

## Context

`parse_lanelet2_osm` already builds a `LaneGraph` from the Lanelet2 OSM-XML subset — geometry, typed
lane lines, connectivity, `right_of_way` and `traffic_sign` / `traffic_light` regulatory elements.
But it read **only** Autoware's pre-projected metric node coordinates (`<tag k="local_x">` /
`local_y`) and **failed** on a node carrying geographic `lat` / `lon` attributes — the standard
OSM-Lanelet2 form a JOSM-authored or geographically-referenced map uses. That left the whole
map-derived stack (occlusion, right-of-way, turn negotiation, KIRRA bounding) unable to load a real
geographic map.

## Decision

Read geographic nodes and project them to local metric coordinates:

- A node uses `local_x` / `local_y` when present (unchanged); otherwise it falls back to `lat` /
  `lon` attributes. A node with **neither** is a hard parse error (fail-closed — never a silent
  `(0, 0)`).
- Geographic nodes are projected after the scan via `project_geographic`: an **equirectangular /
  local-tangent-plane** projection about the lowest-id geographic node — `x = R·Δlon·cos(lat0)`,
  `y = R·Δlat`, `R` = WGS84 mean radius. East = +x, north = +y.
- The origin (lowest-id node) is deterministic, and the choice only **translates** the whole map —
  connectivity, headings, routing, and the derivations are translation-invariant, so behaviour does
  not depend on it (an origin-invariance test pins this).

## Consequences

- **Positive:** the parser now loads a geographically-referenced Lanelet2 map, not only an Autoware
  pre-projected one — the derived stack runs off real maps either way. A pre-projected map is
  byte-identical to before (the `local_x`/`local_y` path is unchanged).
- **Accuracy:** the local-tangent projection is accurate to centimetres over a lanelet-scale map
  (sub-km), the regime these maps live in; error grows with extent away from the origin.
- **Honest scope:** this is a local-Cartesian projection, **not** UTM / MGRS / a geoid model, and it
  assumes a single local origin — adequate for a bounded operational map, not a region-spanning one.
  A pluggable projector (UTM zone / MGRS / an explicit map-origin tag) is a follow-up. Mixed
  local+geographic maps resolve per node (a `local_x` node is never overwritten by a projection).
