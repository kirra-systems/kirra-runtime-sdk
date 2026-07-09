// crates/kirra-core/src/corridor.rs (de-monolith Stage 6a: relocated verbatim from the
// kirra-ros2-adapter `corridor` module)
//
// CorridorSource — the seam between the map/perception side (Lanelet2 +
// localization in production) and the slow-loop containment check. The lean
// trait + `Point` + `MockCorridorSource` live here so the shared lane map
// (kirra-map) and the planner can depend on them without the heavy adapter.
// The Lanelet2 cxx bridge (C++ linkage, feature-gated) STAYS in the adapter and
// impls this trait.
//
// `Point` matches `kirra_core::containment::Point` field-for-field. The match is
// by convention here; the slow loop converts via a field-for-field copy (no
// semantic translation) so it can call `validate_trajectory_containment`.

/// 2D point in world frame. Field-compatible with
/// `kirra_core::containment::Point`. The match is by convention here; the slow
/// loop copies field-for-field into the safety-kernel type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x_m: f64,
    pub y_m: f64,
}

/// Trait-object friendly view of a drivable-space corridor. The slow loop
/// reads the four accessors per per-trajectory validation and never
/// materializes a separate corridor allocation.
///
/// Implementors own the underlying polyline storage (typically a `Vec<Point>`)
/// and return borrows into it. This keeps `Corridor<'a>` (the kernel-side
/// type that `validate_trajectory_containment` consumes) constructable
/// without copy.
pub trait CorridorSource: Send + Sync {
    /// Left boundary polyline, advancing in the same direction along the
    /// corridor as `right_boundary`.
    fn left_boundary(&self) -> &[Point];

    /// Right boundary polyline.
    fn right_boundary(&self) -> &[Point];

    /// Source confidence, `[0.0, 1.0]`. Compared against
    /// `min_confidence` on the kernel-side `Corridor` to decide health.
    fn confidence(&self) -> f32;

    /// Snapshot age in ms vs. now. Compared against `max_age_ms` on the
    /// kernel-side `Corridor` to decide freshness.
    fn age_ms(&self) -> u64;
}

/// 5 m half-width straight corridor along the +X axis. Sole purpose:
/// exercise the adapter end-to-end in Phase 1 unit / smoke tests
/// without a Lanelet2 dependency. Production builds use a Lanelet2-
/// derived source (Phase 2).
pub struct MockCorridorSource {
    left: Vec<Point>,
    right: Vec<Point>,
    confidence: f32,
    age_ms: u64,
}

impl MockCorridorSource {
    /// 5 m half-width × `length_m` long corridor centred on the +X axis.
    /// Both polylines have two vertices (the minimum the kernel accepts).
    pub fn straight_5m_half_width(length_m: f64) -> Self {
        Self {
            left: vec![
                Point { x_m: 0.0, y_m: 5.0 },
                Point {
                    x_m: length_m,
                    y_m: 5.0,
                },
            ],
            right: vec![
                Point {
                    x_m: 0.0,
                    y_m: -5.0,
                },
                Point {
                    x_m: length_m,
                    y_m: -5.0,
                },
            ],
            confidence: 0.95,
            age_ms: 10,
        }
    }

    /// Custom-tuned mock for staleness / low-confidence tests.
    pub fn with_health(mut self, confidence: f32, age_ms: u64) -> Self {
        self.confidence = confidence;
        self.age_ms = age_ms;
        self
    }
}

impl CorridorSource for MockCorridorSource {
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_5m_corridor_has_2_vertex_polylines() {
        let c = MockCorridorSource::straight_5m_half_width(100.0);
        assert_eq!(c.left_boundary().len(), 2);
        assert_eq!(c.right_boundary().len(), 2);
        // Half-width = 5 m on each side.
        assert_eq!(c.left_boundary()[0].y_m, 5.0);
        assert_eq!(c.right_boundary()[0].y_m, -5.0);
    }

    #[test]
    fn mock_with_health_overrides_confidence_and_age() {
        let c = MockCorridorSource::straight_5m_half_width(50.0).with_health(0.3, 1_000);
        assert_eq!(c.confidence(), 0.3);
        assert_eq!(c.age_ms(), 1_000);
    }

    /// The trait object form is what the adapter Node carries. Confirm
    /// `Arc<dyn CorridorSource>` constructs and the methods dispatch
    /// dynamically.
    #[test]
    fn corridor_source_is_dyn_safe() {
        use std::sync::Arc;
        let src: Arc<dyn CorridorSource> =
            Arc::new(MockCorridorSource::straight_5m_half_width(20.0));
        assert_eq!(src.left_boundary().len(), 2);
        assert!(src.confidence() > 0.0);
    }
}
