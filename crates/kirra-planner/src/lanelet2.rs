//! The pure-Rust Lanelet2 `.osm` (OSM-XML) parser → `LaneGraph` moved VERBATIM to the
//! shared `kirra-map` crate (de-monolith Stage 6b). Re-exported here so every existing
//! `crate::lanelet2::*` (and `kirra_planner::{parse_lanelet2_osm, Lanelet2ParseError}`)
//! path is unchanged.
pub use kirra_map::lanelet2::*;
