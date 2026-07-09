// crates/kirra-ros2-adapter/src/corridor/lanelet2.rs
//
// `Lanelet2CorridorSource` — the production `CorridorSource` impl that
// reads from a `LaneletMapBin.data` payload + a lanelet-id sequence
// (the planner-published route's preferred-primitive ids).
//
// All work is delegated to the cxx::bridge (lanelet2_bridge); this
// module owns only:
//   - the constructor (call the bridge, copy boundary points into the
//     adapter-side `Point` storage),
//   - the trait impl (slice borrows into that owned storage),
//   - the error type.

use crate::corridor::lanelet2_bridge;
use crate::corridor::{CorridorSource, Point};

/// Reasons a `Lanelet2CorridorSource` construction can fail. All errors
/// flow through cxx as `cxx::Exception` from the C++ shim; we wrap into
/// our enum so consumers don't have to import cxx.
#[derive(Debug)]
pub enum Lanelet2Error {
    /// Empty input buffer or empty lanelet-ids slice.
    InvalidInput(String),
    /// `boost::archive::binary_iarchive` failed (corrupt stream,
    /// version mismatch, ABI mismatch). Diagnostic carries the C++
    /// `what()` string.
    Deserialize(String),
    /// Lanelet id not present in the map or geometry NaN/Inf.
    Extract(String),
}

impl std::fmt::Display for Lanelet2Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(s) => write!(f, "InvalidInput: {s}"),
            Self::Deserialize(s) => write!(f, "Deserialize: {s}"),
            Self::Extract(s) => write!(f, "Extract: {s}"),
        }
    }
}

impl std::error::Error for Lanelet2Error {}

/// `CorridorSource` backed by a parsed Lanelet2 map + a route. Owns the
/// boundary polylines; the trait methods return slice borrows into the
/// owned `Vec<Point>`s.
pub struct Lanelet2CorridorSource {
    left: Vec<Point>,
    right: Vec<Point>,
    confidence: f32,
    age_ms: u64,
}

impl Lanelet2CorridorSource {
    /// Construct from a serialized `LaneletMapBin.data` buffer + the
    /// list of lanelet IDs to walk (typically the preferred-primitive
    /// IDs from `autoware_planning_msgs::LaneletRoute`).
    ///
    /// `confidence` / `age_ms` are reported by the integrator's
    /// map-server pipeline and passed through to the kernel's
    /// `Corridor::is_healthy` check.
    pub fn from_map_bin_and_route(
        bin: &[u8],
        lanelet_ids: &[i64],
        confidence: f32,
        age_ms: u64,
    ) -> Result<Self, Lanelet2Error> {
        let map = lanelet2_bridge::load_lanelet_map(bin)
            .map_err(|e| Lanelet2Error::Deserialize(e.what().to_string()))?;
        let pts = lanelet2_bridge::extract_corridor(map.as_ref().unwrap(), lanelet_ids)
            .map_err(|e| Lanelet2Error::Extract(e.what().to_string()))?;
        let left: Vec<Point> = pts
            .left
            .iter()
            .map(|p| Point { x_m: p.x, y_m: p.y })
            .collect();
        let right: Vec<Point> = pts
            .right
            .iter()
            .map(|p| Point { x_m: p.x, y_m: p.y })
            .collect();
        Ok(Self {
            left,
            right,
            confidence,
            age_ms,
        })
    }

    /// Total vertex count across both boundaries. Test + diagnostic
    /// helper.
    pub fn vertex_count(&self) -> usize {
        self.left.len() + self.right.len()
    }
}

impl CorridorSource for Lanelet2CorridorSource {
    fn left_boundary(&self) -> &[Point] {
        &self.left
    }
    fn right_boundary(&self) -> &[Point] {
        &self.right
    }
    fn confidence(&self) -> f32 {
        self.confidence
    }
    fn age_ms(&self) -> u64 {
        self.age_ms
    }
}

// ---------------------------------------------------------------------------
// Tests (feature-gated — require ROS sourced + lanelet2 installed).
//
// The fixture path is documented in the README; if absent we panic with
// a precise message telling the integrator how to regenerate it.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod lanelet2_tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_path(name: &str) -> PathBuf {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests");
        p.push("fixtures");
        p.push(name);
        p
    }

    fn load_fixture(name: &str) -> Vec<u8> {
        let path = fixture_path(name);
        std::fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "kirra-ros2-adapter::lanelet2 tests: could not read fixture {}: {e}\n\
                 \n\
                 To regenerate (one-shot, requires lanelet2 installed):\n\
                  python3 -c 'import lanelet2; \\\n\
                              p = lanelet2.io.Origin(0,0); \\\n\
                              m = lanelet2.io.loadRobust(\"tests/fixtures/straight_corridor.osm\", p); \\\n\
                              lanelet2.io.write(\"tests/fixtures/straight_corridor.osm.bin\", m, p)'\n\
                 \n\
                 The README has the full regeneration recipe.",
                path.display(),
            )
        })
    }

    /// Smoke test: load the fixture, extract a corridor over the
    /// known-good lanelet IDs (1001, 1002), check shape invariants.
    /// The fixture geometry is a 50 m straight corridor 4 m wide;
    /// expected ≥ 4 points per side, all within
    /// 0 ≤ x ≤ 50, |y| ≤ 2.5.
    #[test]
    fn test_load_and_extract_corridor() {
        let bin = load_fixture("straight_corridor.osm.bin");
        let source = Lanelet2CorridorSource::from_map_bin_and_route(&bin, &[1001, 1002], 0.95, 50)
            .expect("Lanelet2CorridorSource construction");
        let left = source.left_boundary();
        let right = source.right_boundary();
        assert!(
            left.len() >= 4,
            "left side ≥ 4 vertices, got {}",
            left.len()
        );
        assert!(
            right.len() >= 4,
            "right side ≥ 4 vertices, got {}",
            right.len()
        );
        for p in left.iter().chain(right.iter()) {
            assert!(
                p.x_m >= 0.0 && p.x_m <= 50.0,
                "vertex x={} out of [0, 50]",
                p.x_m
            );
            assert!(p.y_m.abs() <= 2.5, "vertex |y|={} out of [0, 2.5]", p.y_m);
        }
    }

    /// Unknown lanelet id → Err (the route is stale or the map is
    /// wrong). Conservative: caller must MRC.
    #[test]
    fn test_unknown_lanelet_id_returns_error() {
        let bin = load_fixture("straight_corridor.osm.bin");
        let err = Lanelet2CorridorSource::from_map_bin_and_route(&bin, &[999_999_999], 0.95, 50)
            .err()
            .expect("expected Err for unknown lanelet id");
        match err {
            Lanelet2Error::Extract(msg) => {
                assert!(
                    msg.contains("not found"),
                    "expected 'not found' in error message, got: {msg}"
                );
            }
            other => panic!("expected Extract error, got {other:?}"),
        }
    }

    /// The trait object form is what the slow loop sees. Confirm
    /// `dyn CorridorSource` round-trips.
    #[test]
    fn test_corridor_source_trait_impl() {
        let bin = load_fixture("straight_corridor.osm.bin");
        let src = Lanelet2CorridorSource::from_map_bin_and_route(&bin, &[1001, 1002], 0.95, 50)
            .expect("source");
        let dyn_src: &dyn CorridorSource = &src;
        assert_eq!(dyn_src.left_boundary().len(), src.left.len());
        assert_eq!(dyn_src.right_boundary().len(), src.right.len());
        assert_eq!(dyn_src.confidence(), 0.95);
        assert_eq!(dyn_src.age_ms(), 50);
    }
}
