// crates/kirra-ros2-adapter/src/corridor/mod.rs
//
// Sub-modules:
//   - the `CorridorSource` trait + `Point` + `MockCorridorSource` now live in the
//     lean `kirra-core` crate (de-monolith Stage 6a) and are re-exported below, so
//     the shared lane map (kirra-map) and the planner can depend on them without the
//     heavy adapter. Every existing `crate::corridor::*` /
//     `kirra_ros2_adapter::corridor::*` path keeps the SAME type — zero churn.
//   - `lanelet2_bridge` (feature `lanelet2`): the cxx::bridge calling into
//     the lanelet2_core C++ boost::serialization deserializer.
//   - `lanelet2` (feature `lanelet2`): the `Lanelet2CorridorSource` impl
//     of `CorridorSource` (impls the now-`kirra-core` trait for its local type).
//
// The lanelet2 corridor bridge is gated on the `lanelet2` feature (which
// implies `ros2`), NOT on `ros2` alone — the perception-governance path
// builds with `--features ros2` and pulls no C++ / no cxx / no lanelet2.
//
// CorridorSource — the seam between the map/perception side (Lanelet2 +
// localization in production) and the slow-loop containment check.
//

// The lean trait + `Point` + `MockCorridorSource` — same types, now sourced from
// `kirra-core` (relocated verbatim in Stage 6a).
pub use kirra_core::corridor::{CorridorSource, MockCorridorSource, Point};

#[cfg(feature = "lanelet2")]
pub mod lanelet2_bridge;

#[cfg(feature = "lanelet2")]
pub mod lanelet2;

#[cfg(feature = "lanelet2")]
pub use self::lanelet2::{Lanelet2CorridorSource, Lanelet2Error};
