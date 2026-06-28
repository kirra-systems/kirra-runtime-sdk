// crates/kirra-trajectory/src/corridor.rs
//
// The lean corridor seam. `CorridorSource` / `Point` / `MockCorridorSource` live in
// `kirra-core` (de-monolith Stage 6a); this module re-exports them so the checker's
// `crate::corridor::*` paths resolve here exactly as they did inside the adapter.
//
// The heavy `Lanelet2CorridorSource` (cxx/C++ bridge, `lanelet2` feature) stays in
// `kirra-ros2-adapter::corridor` — it is an integration concern, not part of the
// checker contract.

pub use kirra_core::corridor::{CorridorSource, MockCorridorSource, Point};
