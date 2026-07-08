//! EP-08 (ex-WP-11, G-12 geometry half) — the corridor-centerline **Frenet
//! frame** for curved-lane RSS.
//!
//! ## The unsound simplification this removes
//!
//! The snapshot RSS (validation.rs §C) measures each object in the TANGENT
//! frame of the nearest trajectory pose: `dx_ego` (longitudinal) / `dy_ego`
//! (lateral) are chord projections along the pose heading. On a straight lane
//! that is exact. On a CURVED lane it is wrong in the dangerous direction: an
//! in-lane object ahead *around the bend* acquires a large tangent-frame
//! lateral offset, escapes the lateral-alignment filter, and is never
//! longitudinally evaluated — fail-OPEN. (Symmetrically, an object dead ahead
//! in the tangent direction but OUTSIDE the curving lane was spuriously
//! rejected — the over-rejection half.)
//!
//! ## The Frenet correction
//!
//! [`CenterlineFrenet`] derives the corridor **centerline** (matched-fraction
//! resampling of the left/right boundary polylines, midpointed) and projects
//! world points onto it: `s` = arc length along the lane, `d` = signed lateral
//! offset (left of travel positive). "Longitudinal" and "lateral" then follow
//! the ROAD, not the chord:
//!
//! - object–ego longitudinal gap = `s_obj − s_ego` (arc distance),
//! - lateral offset = `d_obj − d_ego`,
//! - object velocity resolves against the centerline tangent AT THE OBJECT.
//!
//! ## Fail-safe composition (the additive rule)
//!
//! The WCET-critical per-pose path and the straight-lane RSS are UNCHANGED:
//! - a corridor whose centerline is *effectively straight*
//!   ([`CenterlineFrenet::is_effectively_straight`]) never engages the curved
//!   path — the existing tangent-frame code runs bit-for-bit;
//! - a degenerate corridor (too few vertices, non-finite, zero length) or a
//!   point that fails to project falls back to the tangent frame **for that
//!   pair** — i.e. exactly today's behaviour, never a skip.
//!
//! On a straight corridor the two frames agree (`s = x`, `d = y` for a +X
//! lane) — pinned by the equivalence property tests in `validation.rs`.

use crate::corridor::Point;

/// Heading change (radians) across the whole centerline below which the
/// corridor is treated as straight and the curved path never engages —
/// keeping the straight-lane RSS bit-for-bit. ~0.6° total: an order of
/// magnitude below any real curve, comfortably above polyline resampling
/// noise on a genuinely straight lane.
pub const STRAIGHT_EPS_RAD: f64 = 0.01;

/// Number of matched-fraction samples used to derive the centerline when the
/// boundaries carry fewer vertices. Curvature between samples is invisible,
/// so this floor bounds the discretization error on smooth lane geometry.
const MIN_CENTERLINE_SAMPLES: usize = 16;

/// A point in the lane's Frenet frame: `s` metres of arc length along the
/// centerline from its start, `d` metres of signed lateral offset (positive =
/// LEFT of the direction of travel).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrenetCoord {
    pub s: f64,
    pub d: f64,
}

/// The corridor centerline as a Frenet reference line: resampled vertices +
/// cumulative arc length. Built once per slow-loop validation call.
#[derive(Debug, Clone)]
pub struct CenterlineFrenet {
    /// Centerline vertices (≥ 2, all finite, consecutive points distinct).
    pts: Vec<Point>,
    /// Cumulative arc length at each vertex; `cum_s[0] == 0`.
    cum_s: Vec<f64>,
}

/// Arc-length positions (fractions of total) for `n` resample points.
fn arc_fractions(n: usize) -> impl Iterator<Item = f64> {
    (0..n).map(move |i| i as f64 / (n - 1) as f64)
}

/// Sample a polyline at `frac` (∈ [0,1]) of its own total arc length.
fn sample_at_fraction(poly: &[Point], cum: &[f64], frac: f64) -> Point {
    let target = frac * cum[cum.len() - 1];
    // Find the segment containing `target` (cum is non-decreasing).
    let mut i = match cum.binary_search_by(|c| c.partial_cmp(&target).unwrap()) {
        Ok(idx) => idx.min(poly.len() - 2),
        Err(idx) => idx.saturating_sub(1).min(poly.len() - 2),
    };
    // Skip zero-length segments (guarded against div-by-zero below).
    while i + 2 < poly.len() && (cum[i + 1] - cum[i]) <= 0.0 {
        i += 1;
    }
    let seg = cum[i + 1] - cum[i];
    let t = if seg > 0.0 { ((target - cum[i]) / seg).clamp(0.0, 1.0) } else { 0.0 };
    Point {
        x_m: poly[i].x_m + t * (poly[i + 1].x_m - poly[i].x_m),
        y_m: poly[i].y_m + t * (poly[i + 1].y_m - poly[i].y_m),
    }
}

/// Cumulative arc length of a polyline; `None` when any coordinate is
/// non-finite or the total length is not strictly positive.
fn cumulative_arc(poly: &[Point]) -> Option<Vec<f64>> {
    let mut cum = Vec::with_capacity(poly.len());
    let mut s = 0.0;
    cum.push(0.0);
    for w in poly.windows(2) {
        if !(w[0].x_m.is_finite() && w[0].y_m.is_finite() && w[1].x_m.is_finite() && w[1].y_m.is_finite()) {
            return None;
        }
        s += ((w[1].x_m - w[0].x_m).powi(2) + (w[1].y_m - w[0].y_m).powi(2)).sqrt();
        cum.push(s);
    }
    if !(s.is_finite() && s > 0.0) {
        return None;
    }
    Some(cum)
}

impl CenterlineFrenet {
    /// Derive the centerline from the corridor boundaries: both polylines are
    /// resampled at the same arc-length FRACTIONS (so a vertex-count mismatch
    /// or uneven spacing cannot skew the midpoints), then midpointed.
    ///
    /// Returns `None` — the caller keeps the tangent-frame path — when either
    /// boundary is degenerate (< 2 vertices, non-finite, zero total length) or
    /// the derived centerline itself degenerates. Fail-safe: `None` is never a
    /// relaxation; it is exactly today's behaviour.
    #[must_use]
    pub fn from_boundaries(left: &[Point], right: &[Point]) -> Option<Self> {
        if left.len() < 2 || right.len() < 2 {
            return None;
        }
        let cum_l = cumulative_arc(left)?;
        let cum_r = cumulative_arc(right)?;

        let n = left.len().max(right.len()).max(MIN_CENTERLINE_SAMPLES);
        let mut pts: Vec<Point> = Vec::with_capacity(n);
        for frac in arc_fractions(n) {
            let l = sample_at_fraction(left, &cum_l, frac);
            let r = sample_at_fraction(right, &cum_r, frac);
            let mid = Point { x_m: (l.x_m + r.x_m) / 2.0, y_m: (l.y_m + r.y_m) / 2.0 };
            // Collapse consecutive duplicates (a pinched corridor) so segment
            // tangents below are always well-defined.
            if let Some(prev) = pts.last() {
                if (mid.x_m - prev.x_m).abs() < 1e-9 && (mid.y_m - prev.y_m).abs() < 1e-9 {
                    continue;
                }
            }
            pts.push(mid);
        }
        if pts.len() < 2 {
            return None;
        }
        let cum_s = cumulative_arc(&pts)?;
        Some(Self { pts, cum_s })
    }

    /// Total centerline arc length (m).
    #[must_use]
    pub fn total_length_m(&self) -> f64 {
        self.cum_s[self.cum_s.len() - 1]
    }

    /// The largest absolute heading change between consecutive centerline
    /// segments, summed over the line — 0 for a straight lane.
    #[must_use]
    pub fn total_heading_change_rad(&self) -> f64 {
        let mut total = 0.0;
        let mut prev: Option<(f64, f64)> = None;
        for w in self.pts.windows(2) {
            let (dx, dy) = (w[1].x_m - w[0].x_m, w[1].y_m - w[0].y_m);
            let len = (dx * dx + dy * dy).sqrt();
            if len <= 0.0 {
                continue;
            }
            let t = (dx / len, dy / len);
            if let Some(p) = prev {
                // Angle between consecutive unit tangents.
                let cross = p.0 * t.1 - p.1 * t.0;
                let dot = (p.0 * t.0 + p.1 * t.1).clamp(-1.0, 1.0);
                total += cross.atan2(dot).abs();
            }
            prev = Some(t);
        }
        total
    }

    /// Whether the corridor is straight for RSS purposes — the curved path
    /// must NOT engage (`validation.rs` keeps the tangent frame bit-for-bit).
    #[must_use]
    pub fn is_effectively_straight(&self) -> bool {
        self.total_heading_change_rad() <= STRAIGHT_EPS_RAD
    }

    /// Project a world point onto the centerline: the nearest point across all
    /// segments (interior projections clamped to segment ends), returned as
    /// `(s, d)`. `None` when the input is non-finite — the caller falls back
    /// to the tangent frame (fail-safe), never skips the object.
    #[must_use]
    pub fn project(&self, p: Point) -> Option<FrenetCoord> {
        if !(p.x_m.is_finite() && p.y_m.is_finite()) {
            return None;
        }
        let mut best: Option<(f64, FrenetCoord)> = None;
        for (i, w) in self.pts.windows(2).enumerate() {
            let (ax, ay) = (w[0].x_m, w[0].y_m);
            let (bx, by) = (w[1].x_m, w[1].y_m);
            let (ex, ey) = (bx - ax, by - ay);
            let len2 = ex * ex + ey * ey;
            if len2 <= 0.0 {
                continue;
            }
            let t = (((p.x_m - ax) * ex + (p.y_m - ay) * ey) / len2).clamp(0.0, 1.0);
            let (cx, cy) = (ax + t * ex, ay + t * ey);
            let (dx, dy) = (p.x_m - cx, p.y_m - cy);
            let dist2 = dx * dx + dy * dy;
            if best.map_or(true, |(b, _)| dist2 < b) {
                let len = len2.sqrt();
                // Signed offset: positive on the LEFT of the travel direction
                // (cross(tangent, delta) z-component).
                let d = (ex * dy - ey * dx) / len;
                let s = self.cum_s[i] + t * len;
                best = Some((dist2, FrenetCoord { s, d }));
            }
        }
        best.map(|(_, c)| c)
    }

    /// Unit tangent of the centerline segment containing arc length `s`
    /// (clamped to the line's span). Well-defined: construction collapses
    /// zero-length segments.
    #[must_use]
    pub fn tangent_at(&self, s: f64) -> (f64, f64) {
        let s = s.clamp(0.0, self.total_length_m());
        let i = match self
            .cum_s
            .binary_search_by(|c| c.partial_cmp(&s).unwrap())
        {
            Ok(idx) => idx.min(self.pts.len() - 2),
            Err(idx) => idx.saturating_sub(1).min(self.pts.len() - 2),
        };
        let (dx, dy) = (
            self.pts[i + 1].x_m - self.pts[i].x_m,
            self.pts[i + 1].y_m - self.pts[i].y_m,
        );
        let len = (dx * dx + dy * dy).sqrt();
        (dx / len, dy / len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn straight_boundaries(len: f64, half_w: f64) -> (Vec<Point>, Vec<Point>) {
        (
            vec![Point { x_m: 0.0, y_m: half_w }, Point { x_m: len, y_m: half_w }],
            vec![Point { x_m: 0.0, y_m: -half_w }, Point { x_m: len, y_m: -half_w }],
        )
    }

    /// Arc boundaries: a circular lane of centerline radius `r`, sweeping
    /// `sweep` radians counter-clockwise from the +X axis start (0, 0) with
    /// the circle center at (0, r). Left boundary radius r - hw, right r + hw.
    fn arc_boundaries(r: f64, half_w: f64, sweep: f64, n: usize) -> (Vec<Point>, Vec<Point>) {
        let ring = |radius: f64| -> Vec<Point> {
            (0..n)
                .map(|i| {
                    let a = sweep * (i as f64) / ((n - 1) as f64);
                    // Circle center (0, r): start angle -pi/2.
                    Point {
                        x_m: radius * a.sin(),
                        y_m: r - radius * a.cos(),
                    }
                })
                .collect()
        };
        (ring(r - half_w), ring(r + half_w))
    }

    #[test]
    fn straight_lane_frenet_is_the_identity_frame() {
        let (l, r) = straight_boundaries(100.0, 5.0);
        let f = CenterlineFrenet::from_boundaries(&l, &r).unwrap();
        assert!(f.is_effectively_straight());
        // s = x, d = y (lane along +X, centered on y = 0).
        for (x, y) in [(0.0, 0.0), (10.0, 1.5), (50.0, -2.0), (99.0, 4.9)] {
            let c = f.project(Point { x_m: x, y_m: y }).unwrap();
            assert!((c.s - x).abs() < 1e-9, "s({x},{y}) = {}", c.s);
            assert!((c.d - y).abs() < 1e-9, "d({x},{y}) = {}", c.d);
        }
        let (tx, ty) = f.tangent_at(50.0);
        assert!((tx - 1.0).abs() < 1e-9 && ty.abs() < 1e-9);
    }

    #[test]
    fn curved_lane_is_detected_and_projects_by_arc_length() {
        // Quarter circle, centerline radius 30 m → arc length ~47.1 m.
        let (l, r) = arc_boundaries(30.0, 2.0, std::f64::consts::FRAC_PI_2, 40);
        let f = CenterlineFrenet::from_boundaries(&l, &r).unwrap();
        assert!(!f.is_effectively_straight());
        assert!(
            (f.total_length_m() - 30.0 * std::f64::consts::FRAC_PI_2).abs() < 0.2,
            "arc length {} vs expected {}",
            f.total_length_m(),
            30.0 * std::f64::consts::FRAC_PI_2
        );
        // A point ON the centerline halfway around: s ≈ half the arc, d ≈ 0.
        let a = std::f64::consts::FRAC_PI_4;
        let p = Point { x_m: 30.0 * a.sin(), y_m: 30.0 - 30.0 * a.cos() };
        let c = f.project(p).unwrap();
        assert!((c.s - 30.0 * a).abs() < 0.1, "s = {}", c.s);
        assert!(c.d.abs() < 0.05, "d = {}", c.d);
    }

    #[test]
    fn signed_offset_is_left_positive() {
        let (l, r) = straight_boundaries(50.0, 5.0);
        let f = CenterlineFrenet::from_boundaries(&l, &r).unwrap();
        assert!(f.project(Point { x_m: 10.0, y_m: 2.0 }).unwrap().d > 0.0, "left of travel");
        assert!(f.project(Point { x_m: 10.0, y_m: -2.0 }).unwrap().d < 0.0, "right of travel");
    }

    #[test]
    fn degenerate_boundaries_yield_none() {
        // Too few vertices.
        assert!(CenterlineFrenet::from_boundaries(&[Point { x_m: 0.0, y_m: 0.0 }], &[]).is_none());
        // Zero-length polyline.
        let p = Point { x_m: 1.0, y_m: 1.0 };
        assert!(CenterlineFrenet::from_boundaries(&[p, p], &[p, p]).is_none());
        // Non-finite vertex.
        let bad = vec![Point { x_m: 0.0, y_m: f64::NAN }, Point { x_m: 10.0, y_m: 0.0 }];
        let good = vec![Point { x_m: 0.0, y_m: -5.0 }, Point { x_m: 10.0, y_m: -5.0 }];
        assert!(CenterlineFrenet::from_boundaries(&bad, &good).is_none());
    }

    #[test]
    fn non_finite_point_projection_is_none() {
        let (l, r) = straight_boundaries(50.0, 5.0);
        let f = CenterlineFrenet::from_boundaries(&l, &r).unwrap();
        assert!(f.project(Point { x_m: f64::NAN, y_m: 0.0 }).is_none());
        assert!(f.project(Point { x_m: 1.0, y_m: f64::INFINITY }).is_none());
    }

    #[test]
    fn mismatched_vertex_counts_still_center_the_lane() {
        // Left has 2 vertices, right has 7 (same geometry) — matched-fraction
        // resampling must not skew the centerline.
        let l = vec![Point { x_m: 0.0, y_m: 3.0 }, Point { x_m: 60.0, y_m: 3.0 }];
        let r: Vec<Point> =
            (0..7).map(|i| Point { x_m: 10.0 * i as f64, y_m: -3.0 }).collect();
        let f = CenterlineFrenet::from_boundaries(&l, &r).unwrap();
        let c = f.project(Point { x_m: 30.0, y_m: 0.0 }).unwrap();
        assert!(c.d.abs() < 1e-9, "centerline must sit on y=0, d = {}", c.d);
    }
}
