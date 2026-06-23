//! **kirra-map** — the shared lane-map substrate of the Kirra stack.
//!
//! Extracted from the planner (de-monolith Stage 6b) so the lane map is a reusable,
//! lean library — depending only on `kirra-core` (the corridor seam + trajectory /
//! perception data types) and `roxmltree`, NOT on the planner, the ROS 2 adapter, or
//! the verifier service. The planner re-exports everything here, so existing paths
//! (`kirra_planner::{LaneGraph, Lane, LaneBoundary, LineType, parse_lanelet2_osm, …}`)
//! are unchanged.
//!
//! Three modules:
//! - [`lane_lines`] — the typed lane-marking crossing rules (`LineType` / `LaneBoundary`
//!   / `lateral_move_permitted`): the *lateral* legal constraint (when you may cross a
//!   line), distinct from KIRRA's physical safety.
//! - [`lanemap`] — the Lanelet2-lite lane graph (`LaneGraph` / `Lane` / `LaneCorridor` /
//!   `JunctionContext`), its router, and right-of-way derivation.
//! - [`lanelet2`] — the pure-Rust Lanelet2 `.osm` (OSM-XML) parser → [`lanemap::LaneGraph`].

pub mod lane_lines;
pub mod lanemap;
pub mod lanelet2;
