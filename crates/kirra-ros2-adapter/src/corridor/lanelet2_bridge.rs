// crates/kirra-ros2-adapter/src/corridor/lanelet2_bridge.rs
//
// cxx::bridge to `lanelet2_core` / `lanelet2_io`. Two C++ entry points;
// every other type stays inside C++ so the bridge surface is minimal.
//
// API (documented surface):
//   load_lanelet_map(data: &[u8])
//     → Result<UniquePtr<LaneletMap>>
//     Deserializes the cereal/boost-serialized payload that
//     `LaneletMapBin.data` carries. The canonical Autoware path is
//     `autoware_lanelet2_extension::utils::conversion::fromBinMsg`
//     which uses `boost::archive::binary_iarchive`; we replicate that
//     in the shim so the adapter works with either the Autoware
//     extension or any compatible map server that publishes the same
//     wire format. Boost-version portability is the integrator's
//     responsibility (same machine produced → consumed in normal AV
//     deployments).
//
//   extract_corridor(map: &LaneletMap, lanelet_ids: &[i64])
//     → Result<CorridorPoints>
//     For each `lanelet_id`: looks up the `Lanelet` in the map; reads
//     `Lanelet::leftBound2d()` / `rightBound2d()`; flattens the
//     boundary `ConstPoint2d`s into `CorridorPoint { x, y }`. The
//     concatenated `left`/`right` vectors in the route order are
//     the polylines `validate_trajectory_containment` consumes.
//
// Bridge data types (cross the FFI boundary):
//   CorridorPoint  — { x: f64, y: f64 }
//   CorridorPoints — { left: Vec<CorridorPoint>, right: Vec<CorridorPoint> }
//
// Opaque C++ types (cxx hides the C++ representation; Rust only sees
// the handle):
//   LaneletMap — `lanelet::LaneletMap` from <lanelet2_core/LaneletMap.h>.

#[cxx::bridge(namespace = "kirra::lanelet2_bridge")]
pub mod ffi {
    /// One vertex of a corridor boundary, in the map's local x/y frame.
    /// Phase 2A `Point { x_m, y_m }` mirrors this byte-for-byte; the
    /// adapter copies into the safety-kernel-shaped `Point` on the
    /// Rust side (one `.map()` after extraction).
    struct CorridorPoint {
        x: f64,
        y: f64,
    }

    /// The flattened left + right corridor polylines, in the order the
    /// caller's `lanelet_ids` slice was given. Adjacent lanelets'
    /// boundaries concatenate; deduplication of shared endpoints is
    /// performed inside the C++ shim.
    struct CorridorPoints {
        left: Vec<CorridorPoint>,
        right: Vec<CorridorPoint>,
    }

    unsafe extern "C++" {
        // Header lives in src/corridor/; build.rs adds that path to the
        // C++ include search list (`.include("src/corridor")`).
        include!("lanelet2_bridge.h");

        /// `lanelet::LaneletMap`. Opaque on the Rust side; the C++ shim
        /// is the only code that touches its members.
        type LaneletMap;

        /// Deserialize a `LaneletMapBin.data` payload into a
        /// `lanelet::LaneletMap`. Returns `Err` on stream-corrupted or
        /// boost-version-mismatched input.
        fn load_lanelet_map(data: &[u8]) -> Result<UniquePtr<LaneletMap>>;

        /// Walk the given lanelet IDs in order, concatenating their
        /// left + right boundary polylines.
        ///
        /// Errors:
        ///   - any unknown lanelet ID (the caller's route is stale or
        ///     the map is wrong) → `Err`
        ///   - any geometry NaN/Inf → `Err`
        fn extract_corridor(map: &LaneletMap, lanelet_ids: &[i64]) -> Result<CorridorPoints>;
    }
}

// Re-export the bridge types under more ergonomic Rust paths.
pub use ffi::{extract_corridor, load_lanelet_map};
pub use ffi::{CorridorPoint, CorridorPoints, LaneletMap};
