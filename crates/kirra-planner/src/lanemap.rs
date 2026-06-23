//! The Lanelet2-lite lane graph (`LaneGraph` / `Lane` / `LaneCorridor` /
//! `JunctionContext`), its router, and right-of-way derivation moved VERBATIM to the
//! shared `kirra-map` crate (de-monolith Stage 6b) so the lane map is reusable and
//! lean (depends only on `kirra-core`). Re-exported here so every existing
//! `crate::lanemap::*` (and `kirra_planner::{LaneGraph, Lane, …}`) path is unchanged.
pub use kirra_map::lanemap::*;
