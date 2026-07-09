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
    let i = match cum.binary_search_by(|c| c.partial_cmp(&target).unwrap()) {
        Ok(idx) => idx,
        Err(idx) => idx.saturating_sub(1),
    }
    .min(poly.len() - 2);
    // A zero-length segment (coincident vertices) is handled by the `seg > 0.0`
    // guard below — `t = 0.0` returns `poly[i]`, which equals `poly[i + 1]` for
    // such a segment, so no separate skip loop is needed (and none can divide by
    // zero).
    let seg = cum[i + 1] - cum[i];
    let t = if seg > 0.0 { ((target - cum[i]) / seg).clamp(0.0, 1.0) } else { 0.0 };
    Point {
        x_m: poly[i].x_m + t * (poly[i + 1].x_m - poly[i].x_m),
        y_m: poly[i].y_m + t * (poly[i + 1].y_m - poly[i].y_m),
    }
}

/// Cumulative arc length of a polyline; `None` when any coordinate is
/// non-finite or the total length is not strictly positive.
///
/// The single trailing `s.is_finite()` guard is sufficient for BOTH failure
/// modes: a non-finite input coordinate poisons the running sum (NaN/±∞
/// propagate through the subtraction, `powi`, `sqrt` and `+`), and a finite
/// input that overflows to `±∞` is caught the same way — so there is no need
/// for a redundant per-window finiteness pre-check (it could reject nothing
/// the trailing guard does not). `s > 0.0` additionally rejects a
/// zero-length (all-coincident) polyline.
fn cumulative_arc(poly: &[Point]) -> Option<Vec<f64>> {
    let mut cum = Vec::with_capacity(poly.len());
    let mut s = 0.0;
    cum.push(0.0);
    for w in poly.windows(2) {
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
        // `cumulative_arc` is the fail-closed backstop for a too-short boundary:
        // a polyline with fewer than two vertices produces no segment, so its
        // total length is 0 and it returns `None` here — no separate `len < 2`
        // pre-check is needed (it could reject nothing the arc-length guard does
        // not), and `sample_at_fraction` below is only reached once both
        // boundaries are known to have ≥ 2 vertices.
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
        // Again `cumulative_arc` is the backstop: a centerline that collapsed to
        // a single point (a fully pinched corridor) has zero total length and
        // returns `None` — so no separate `pts.len() < 2` pre-check is needed.
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
            let v = (w[1].x_m - w[0].x_m, w[1].y_m - w[0].y_m);
            // A zero-length segment carries no direction — skip it WITHOUT
            // resetting `prev`, so the turn across it is still measured between
            // the two real neighbours. (Construction collapses duplicates, so
            // this only guards a hand-built frame.)
            if v == (0.0, 0.0) {
                continue;
            }
            if let Some(p) = prev {
                // Turn angle between consecutive segment vectors. `atan2(cross,
                // dot)` is invariant to each vector's magnitude (both scale by
                // |p|·|v|, which cancels), so the RAW segment vectors give the
                // exact angle — no per-segment normalization is needed (it was
                // dead computation the angle never depended on).
                let cross = p.0 * v.1 - p.1 * v.0;
                let dot = p.0 * v.0 + p.1 * v.1;
                total += cross.atan2(dot).abs();
            }
            prev = Some(v);
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
            if best.is_none_or(|(b, _)| dist2 < b) {
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
        let i = match self.cum_s.binary_search_by(|c| c.partial_cmp(&s).unwrap()) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        }
        .min(self.pts.len() - 2);
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
    fn every_non_finite_operand_position_yields_none() {
        // `cumulative_arc` guards all four coordinates of each window; a NaN
        // or Inf in ANY position must degrade to the tangent frame, so each
        // operand's failing side is pinned individually (not just one).
        let good = vec![Point { x_m: 0.0, y_m: -5.0 }, Point { x_m: 10.0, y_m: -5.0 }];
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for (bx, by, gx, gy) in [
                (bad, 5.0, 10.0, 5.0), // w[0].x
                (0.0, bad, 10.0, 5.0), // w[0].y
                (0.0, 5.0, bad, 5.0),  // w[1].x
                (0.0, 5.0, 10.0, bad), // w[1].y
            ] {
                let poly = vec![Point { x_m: bx, y_m: by }, Point { x_m: gx, y_m: gy }];
                assert!(
                    CenterlineFrenet::from_boundaries(&poly, &good).is_none(),
                    "non-finite left boundary ({bx},{by})→({gx},{gy}) must yield None"
                );
                assert!(
                    CenterlineFrenet::from_boundaries(&good, &poly).is_none(),
                    "non-finite right boundary ({bx},{by})→({gx},{gy}) must yield None"
                );
            }
        }
        // The short-boundary guard, each side individually.
        let one = vec![Point { x_m: 0.0, y_m: 0.0 }];
        assert!(CenterlineFrenet::from_boundaries(&one, &good).is_none());
        assert!(CenterlineFrenet::from_boundaries(&good, &one).is_none());
    }

    #[test]
    fn duplicate_vertices_are_skipped_not_divided_by() {
        // A polyline may legally carry consecutive duplicate vertices (its
        // total length stays positive, so `cumulative_arc` admits it); the
        // resampler must step over the zero-length segments, never divide by
        // them. Interior run of duplicates: the binary search lands inside it
        // and the skip loop walks out.
        let poly = vec![
            Point { x_m: 0.0, y_m: 0.0 },
            Point { x_m: 4.0, y_m: 0.0 },
            Point { x_m: 4.0, y_m: 0.0 },
            Point { x_m: 4.0, y_m: 0.0 },
            Point { x_m: 16.0, y_m: 0.0 },
        ];
        let cum = cumulative_arc(&poly).unwrap();
        assert_eq!(cum, vec![0.0, 4.0, 4.0, 4.0, 16.0]);
        // frac 0.25 → target arc 4.0, exactly on the duplicate run.
        let p = sample_at_fraction(&poly, &cum, 0.25);
        assert!(p.x_m.is_finite() && p.y_m.is_finite());
        assert!((p.x_m - 4.0).abs() < 1e-9 && p.y_m.abs() < 1e-9);
        // Trailing duplicate: the skip loop cannot advance past the end, so
        // the zero-length tail resolves to its start vertex (t = 0), finite.
        let tail = vec![
            Point { x_m: 0.0, y_m: 0.0 },
            Point { x_m: 4.0, y_m: 0.0 },
            Point { x_m: 4.0, y_m: 0.0 },
        ];
        let cum_t = cumulative_arc(&tail).unwrap();
        let p = sample_at_fraction(&tail, &cum_t, 1.0);
        assert!((p.x_m - 4.0).abs() < 1e-9 && p.y_m.abs() < 1e-9);
        // End-to-end: boundaries carrying duplicates still build a frame whose
        // projection matches the clean-geometry answer.
        let dup_left: Vec<Point> = poly.iter().map(|p| Point { x_m: p.x_m, y_m: 3.0 }).collect();
        let clean_right =
            vec![Point { x_m: 0.0, y_m: -3.0 }, Point { x_m: 16.0, y_m: -3.0 }];
        let f = CenterlineFrenet::from_boundaries(&dup_left, &clean_right).unwrap();
        let c = f.project(Point { x_m: 8.0, y_m: 1.0 }).unwrap();
        assert!((c.s - 8.0).abs() < 1e-9 && (c.d - 1.0).abs() < 1e-9);
    }

    #[test]
    fn pinched_corridor_whose_midpoints_all_coincide_yields_none() {
        // Left traced +x, right traced the OPPOSITE way: every matched-fraction
        // midpoint lands on the same point, the duplicate collapse reduces the
        // centerline to one vertex, and the constructor refuses (tangent-frame
        // fallback) rather than emit a degenerate reference line.
        let left = vec![Point { x_m: 0.0, y_m: 1.0 }, Point { x_m: 10.0, y_m: 1.0 }];
        let right = vec![Point { x_m: 10.0, y_m: -1.0 }, Point { x_m: 0.0, y_m: -1.0 }];
        assert!(CenterlineFrenet::from_boundaries(&left, &right).is_none());
    }

    #[test]
    fn zero_length_interior_segment_is_stepped_over_defensively() {
        // `from_boundaries` collapses duplicates, so a zero-length centerline
        // segment cannot arise through the constructor — but the traversals
        // (`total_heading_change_rad`, `project`) still guard it. Pin the
        // defensive arms directly.
        let f = CenterlineFrenet {
            pts: vec![
                Point { x_m: 0.0, y_m: 0.0 },
                Point { x_m: 5.0, y_m: 0.0 },
                Point { x_m: 5.0, y_m: 0.0 },
                Point { x_m: 10.0, y_m: 0.0 },
            ],
            cum_s: vec![0.0, 5.0, 5.0, 10.0],
        };
        // The zero segment contributes no heading change: still straight.
        assert!(f.total_heading_change_rad().abs() < 1e-12);
        assert!(f.is_effectively_straight());
        // Projection ignores the zero segment and answers from the real ones.
        let c = f.project(Point { x_m: 7.0, y_m: 1.5 }).unwrap();
        assert!((c.s - 7.0).abs() < 1e-9 && (c.d - 1.5).abs() < 1e-9);
    }

    #[test]
    fn projection_keeps_the_nearest_segment_not_the_last() {
        // A point beside the FIRST segment of a long bent line: later segments
        // are all farther, so the best-candidate branch must decline them (the
        // "not better" side), and the answer stays on segment 0.
        let (l, r) = arc_boundaries(30.0, 2.0, std::f64::consts::FRAC_PI_2, 40);
        let f = CenterlineFrenet::from_boundaries(&l, &r).unwrap();
        let c = f.project(Point { x_m: 0.5, y_m: 0.2 }).unwrap();
        assert!(c.s < 2.0, "nearest the arc start, s = {}", c.s);
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

    #[test]
    fn cumulative_arc_pins_the_running_length_and_rejects_degenerate() {
        // Exact per-vertex arc length — a 3-4-5 triangle leg pair pins the
        // hypotenuse arithmetic (the `-`, `powi`, `+`, `sqrt` chain).
        let poly = vec![
            Point { x_m: 0.0, y_m: 0.0 },
            Point { x_m: 3.0, y_m: 4.0 }, // +5
            Point { x_m: 3.0, y_m: 4.0 + 12.0 }, // +12
        ];
        assert_eq!(cumulative_arc(&poly).unwrap(), vec![0.0, 5.0, 17.0]);
        // The single trailing guard rejects BOTH failure modes:
        //   - a zero-length (all-coincident) polyline → s == 0, not > 0.
        let p = Point { x_m: 2.0, y_m: 7.0 };
        assert!(cumulative_arc(&[p, p]).is_none(), "s>0 must reject zero length");
        assert!(cumulative_arc(&[p, p, p]).is_none());
        //   - a non-finite coordinate → s non-finite (poisoned through the sum).
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(cumulative_arc(&[Point { x_m: bad, y_m: 0.0 }, p]).is_none());
            assert!(cumulative_arc(&[p, Point { x_m: 0.0, y_m: bad }]).is_none());
        }
    }

    #[test]
    fn sample_at_fraction_pins_the_interpolation() {
        // L-corner polyline: seg0 (0,0)→(10,0) len 10, seg1 (10,0)→(10,10) len 10.
        let poly = vec![
            Point { x_m: 0.0, y_m: 0.0 },
            Point { x_m: 10.0, y_m: 0.0 },
            Point { x_m: 10.0, y_m: 10.0 },
        ];
        let cum = vec![0.0, 10.0, 20.0];
        // frac 0.25 → target 5 → seg0, t = 0.5 → (5, 0). Pins the `target-cum[i]`
        // numerator, the `/seg` division and the two lerp expressions.
        let a = sample_at_fraction(&poly, &cum, 0.25);
        assert!((a.x_m - 5.0).abs() < 1e-12 && a.y_m.abs() < 1e-12, "{a:?}");
        // frac 0.75 → target 15 → seg1, t = 0.5 → (10, 5).
        let b = sample_at_fraction(&poly, &cum, 0.75);
        assert!((b.x_m - 10.0).abs() < 1e-12 && (b.y_m - 5.0).abs() < 1e-12, "{b:?}");
        // Endpoints resolve exactly (frac 0 → start, frac 1 → end).
        let s0 = sample_at_fraction(&poly, &cum, 0.0);
        assert!(s0.x_m.abs() < 1e-12 && s0.y_m.abs() < 1e-12);
        let s1 = sample_at_fraction(&poly, &cum, 1.0);
        assert!((s1.x_m - 10.0).abs() < 1e-12 && (s1.y_m - 10.0).abs() < 1e-12);
    }

    // A hand-built frame with EXACT geometry (bypassing resampling error) so the
    // projection / tangent / heading arithmetic can be pinned to 1e-12. The two
    // legs have DIFFERENT lengths (√10 and √26) on purpose: a same-length corner
    // lets a `dx*dx + dy*dy → dx*dx * dy*dy` length mutant keep the cross/dot
    // RATIO (hence the angle) unchanged — unequal legs break that proportionality
    // so the heading mutant is caught.
    fn corner_frame() -> CenterlineFrenet {
        // seg0 (0,0)→(1,3): dir (1,3)/√10; seg1 (1,3)→(6,4): dir (5,1)/√26.
        CenterlineFrenet {
            pts: vec![
                Point { x_m: 0.0, y_m: 0.0 },
                Point { x_m: 1.0, y_m: 3.0 },
                Point { x_m: 6.0, y_m: 4.0 },
            ],
            cum_s: vec![0.0, 10.0_f64.sqrt(), 10.0_f64.sqrt() + 26.0_f64.sqrt()],
        }
    }

    #[test]
    fn tangent_at_pins_the_segment_direction() {
        let f = corner_frame();
        let (s10, s26) = (10.0_f64.sqrt(), 26.0_f64.sqrt());
        // Inside seg0 → unit (1,3)/√10; inside seg1 → unit (5,1)/√26.
        let (t0x, t0y) = f.tangent_at(s10 / 2.0);
        assert!((t0x - 1.0 / s10).abs() < 1e-12 && (t0y - 3.0 / s10).abs() < 1e-12, "{t0x},{t0y}");
        let (t1x, t1y) = f.tangent_at(s10 + s26 / 2.0);
        assert!((t1x - 5.0 / s26).abs() < 1e-12 && (t1y - 1.0 / s26).abs() < 1e-12, "{t1x},{t1y}");
        // Neither is (1,0): kills the whole-function default-return mutant.
        assert!((t0x - 1.0).abs() > 0.1 || t0y.abs() > 0.1);
    }

    #[test]
    fn total_heading_change_pins_the_corner_angle() {
        let f = corner_frame();
        // Angle between (1,3)/√10 and (5,1)/√26: cross = (1·1 − 3·5)/√260 =
        // −14/√260, dot = (1·5 + 3·1)/√260 = 8/√260 → |atan2(−14, 8)|.
        let expected = 14.0_f64.atan2(8.0);
        assert!(
            (f.total_heading_change_rad() - expected).abs() < 1e-12,
            "{} vs {expected}",
            f.total_heading_change_rad()
        );
        // A single straight segment turns through nothing.
        let straight = CenterlineFrenet {
            pts: vec![Point { x_m: 0.0, y_m: 0.0 }, Point { x_m: 3.0, y_m: 4.0 }],
            cum_s: vec![0.0, 5.0],
        };
        assert!(straight.total_heading_change_rad().abs() < 1e-12);
    }

    #[test]
    fn project_pins_signed_offset_on_both_sides() {
        // L-corner: seg0 (0,0)→(10,0), seg1 (10,0)→(10,10).
        let f = CenterlineFrenet {
            pts: vec![
                Point { x_m: 0.0, y_m: 0.0 },
                Point { x_m: 10.0, y_m: 0.0 },
                Point { x_m: 10.0, y_m: 10.0 },
            ],
            cum_s: vec![0.0, 10.0, 20.0],
        };
        // Beside seg0, LEFT of +X travel (y>0) → d = +2, s = 5.
        let a = f.project(Point { x_m: 5.0, y_m: 2.0 }).unwrap();
        assert!((a.s - 5.0).abs() < 1e-12 && (a.d - 2.0).abs() < 1e-12, "{a:?}");
        // Beside seg1 (travel +Y), a point at x=12 is to the RIGHT → d = -2,
        // s = 10 + 5. The ex=0 branch makes the `ey*dx` sign load-bearing.
        let b = f.project(Point { x_m: 12.0, y_m: 5.0 }).unwrap();
        assert!((b.s - 15.0).abs() < 1e-12 && (b.d + 2.0).abs() < 1e-12, "{b:?}");
        // Nearest-segment retention: (12,5) is 2 m from seg1 but ~5.4 m from
        // seg0's end — the near-vs-far comparison must keep seg1.
        assert!(b.s > 10.0, "must have chosen seg1, s = {}", b.s);
    }

    #[test]
    fn segment_lookup_clamps_the_final_arc_length_into_the_last_segment() {
        // A 5-point frame so `.min(len - 2)` is BOTH reachable and distinguishable
        // from a mangled bound: at s = total the binary search returns the last
        // index, which must clamp into the last SEGMENT (index len-2) — a `- → +`
        // bound would index past the end (panic) and a `- → /` bound
        // (len/2 = 2 ≠ len-2 = 3) would land on the wrong segment.
        let f = CenterlineFrenet {
            pts: vec![
                Point { x_m: 0.0, y_m: 0.0 },
                Point { x_m: 5.0, y_m: 0.0 },
                Point { x_m: 10.0, y_m: 0.0 },
                Point { x_m: 15.0, y_m: 0.0 },
                Point { x_m: 20.0, y_m: 5.0 }, // last segment turns, so its tangent is distinct
            ],
            cum_s: vec![0.0, 5.0, 10.0, 15.0, 15.0 + 29.0_f64.sqrt()],
        };
        // tangent_at(total) → last segment (5,5)/√50.
        let (tx, ty) = f.tangent_at(f.total_length_m());
        assert!((tx - 5.0 / 50.0_f64.sqrt()).abs() < 1e-12 && (ty - 5.0 / 50.0_f64.sqrt()).abs() < 1e-12, "{tx},{ty}");
        // sample_at_fraction(frac = 1.0) → the final vertex exactly.
        let poly: Vec<Point> = f.pts.clone();
        let end = sample_at_fraction(&poly, &f.cum_s, 1.0);
        assert!((end.x_m - 20.0).abs() < 1e-12 && (end.y_m - 5.0).abs() < 1e-12, "{end:?}");
    }

}
