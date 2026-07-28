//! Self-filter — the one place in Taj that **removes** evidence (issue #1210).
//!
//! # Why this module needs the opposite discipline from the rest of Taj
//!
//! Everything else in Taj tightens: a detected object narrows the corridor, an
//! unhealthy scan drops confidence, a semantic hazard clips the clear distance.
//! ADR-0015 states the rule as *"Taj tightens the envelope, it never loosens
//! it"* — and every other operation in this crate obeys it, which is why a bug
//! in them fails toward a stop.
//!
//! Self-filtering is the exception. Discarding a return **adds** free space:
//! the corridor widens, an object disappears, the assured-clear distance grows.
//! A mask that is one polygon too generous does not produce a conservative
//! error — it produces a robot that cannot see the thing it is about to hit.
//!
//! So the safety argument here runs backwards from the rest of the crate, and
//! every constraint below exists to bound how much evidence a mask can destroy:
//!
//! 1. **A vertex may not exceed [`MAX_SELF_MASK_VERTEX_RADIUS_M`]** from the
//!    sensor origin. A mask cannot reach out to where obstacles live.
//! 2. **A mask is validated at construction, not at use.** There is no way to
//!    hold a `SelfFilterMask` that was never checked — the constructor is the
//!    only way to make one and it returns `Result`.
//! 3. **A point exactly on the boundary is RETAINED**, never masked. Ties break
//!    toward keeping evidence.
//! 4. **The mask is applied in the SENSOR frame**, before any transform, so its
//!    correctness does not depend on a mount transform that has not been
//!    physically verified (see [`MountProvenance`]).
//! 5. **The mask has a revision and a digest.** Changing the geometry changes
//!    the digest, so a stale mask is detectable rather than silent.
//!
//! # What this module does NOT do
//!
//! It does not tell you which returns are the robot. That question is answered
//! by a physical capture on the actual R2 — stationary, surroundings documented,
//! recording which angular sectors and ranges are body, mast, wheels or cabling
//! — and that capture is the blocked half of #1210. This module is the
//! machinery that carries the answer safely once it exists, plus the bounds that
//! stop a wrong answer from being catastrophic.
//!
//! Until a capture exists, the correct configuration is **no mask at all**
//! ([`TajPhaseA`](crate::TajPhaseA) with `self_filter: None`), which is exactly
//! today's behaviour: nothing is filtered, self-returns are treated as
//! obstacles, and the robot is over-conservative rather than blind. That is the
//! right way to be wrong.

use sha2::{Digest, Sha256};

/// Absolute ceiling on how far a mask vertex may sit from the sensor origin.
///
/// This is a LIBRARY BACKSTOP, not a policy — the same doctrine as
/// `MAX_CORRIDOR_STATIONS` in the parent module. Deployment bounds are tighter
/// and come from [`SelfFilterBounds::for_footprint`], derived from the actual
/// platform. This constant exists because [`SelfFilterBounds`] is public and a
/// direct caller could otherwise author a bound of any size.
///
/// One metre is roughly five times the R2's own circumscribed radius (a
/// 0.203 × 0.330 m body is ~0.194 m corner-to-centre). No robot that this
/// perception layer targets is two metres across, so a mask reaching further
/// than this is a configuration error rather than a tight fit.
pub const MAX_SELF_MASK_VERTEX_RADIUS_M: f64 = 1.0;

/// Maximum polygons in one mask.
///
/// A self-filter describes a small number of physical protrusions — a mast, a
/// pair of wheel arches, a cable run. Bounded so a pathological configuration
/// cannot make per-point filtering unbounded work on the perception path.
pub const MAX_SELF_MASK_POLYGONS: usize = 16;

/// Maximum vertices in one polygon. Bounded for the same reason.
pub const MAX_SELF_MASK_VERTICES: usize = 64;

/// Distance within which a point counts as lying ON a polygon edge, and is
/// therefore retained rather than masked.
///
/// Sub-millimetre: tight enough not to erode a real mask, wide enough that a
/// point placed exactly on an edge by construction does not land on whichever
/// side the floating-point crossing test happens to pick.
const EDGE_EPSILON_M: f64 = 1e-6;

/// How the sensor-to-base transform was established.
///
/// This is not bookkeeping. `scan_to_points` currently treats the sensor origin
/// as the base origin — an **implicit identity transform** that has never been
/// checked against the physical robot. If the R2's lidar is mounted forward of
/// base_link, every object Taj reports is closer or further than it says by that
/// offset, and no test in this repository would notice.
///
/// Making provenance a required field means a deployment cannot quietly inherit
/// an assumed transform: [`SelfFilterMask::assert_mount_verified`] refuses
/// anything but [`MountProvenance::Measured`], so a production configuration has
/// to have done the measurement or explicitly opt out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountProvenance {
    /// Physically measured on the robot and recorded, with the measurement
    /// cited in the platform baseline.
    Measured,
    /// Taken from a URDF or CAD model. Better than nothing and routinely wrong
    /// by the difference between the design and the built article.
    FromUrdf,
    /// Assumed — including the identity transform this crate has used since it
    /// was written. Not admissible for production.
    Assumed,
}

/// Rigid transform from the sensor frame to the robot base frame.
///
/// Applied as a rotation by `yaw_rad` followed by a translation — i.e. a point
/// at the sensor origin maps to `(x_m, y_m)` in the base frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SensorMount {
    pub x_m: f64,
    pub y_m: f64,
    pub yaw_rad: f64,
    pub provenance: MountProvenance,
}

impl SensorMount {
    /// The transform this crate has implicitly assumed since it was written:
    /// sensor origin coincident with base origin, no rotation.
    ///
    /// Named `ASSUMED` rather than `IDENTITY` on purpose. It is not a neutral
    /// default; it is an unverified claim about where the lidar is bolted, and
    /// the name should say so at every use site.
    pub const ASSUMED_IDENTITY: Self = Self {
        x_m: 0.0,
        y_m: 0.0,
        yaw_rad: 0.0,
        provenance: MountProvenance::Assumed,
    };

    /// Map a point from the sensor frame into the base frame.
    #[must_use]
    pub fn to_base(&self, x: f64, y: f64) -> (f64, f64) {
        let (s, c) = self.yaw_rad.sin_cos();
        (x * c - y * s + self.x_m, x * s + y * c + self.y_m)
    }

    fn is_finite(&self) -> bool {
        self.x_m.is_finite() && self.y_m.is_finite() && self.yaw_rad.is_finite()
    }
}

/// The bound a mask's geometry is validated against.
///
/// Kept separate from the mask so the bound can be derived once from the
/// platform profile and reused, and so a test can construct a deliberately
/// wrong bound to prove the validation reds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelfFilterBounds {
    /// No mask vertex may sit further than this from the sensor origin.
    pub max_vertex_radius_m: f64,
}

impl SelfFilterBounds {
    /// Derive the bound from the platform's own body and where the sensor sits
    /// on it.
    ///
    /// The mask lives in the sensor frame, so the furthest legitimate mask point
    /// is the body's circumscribed radius (corner-to-centre) plus however far
    /// the sensor is offset from the body centre, plus a small tolerance for
    /// protrusions the body rectangle does not capture — a mast, a bumper, a
    /// cable loop.
    ///
    /// The result is still clamped to [`MAX_SELF_MASK_VERTEX_RADIUS_M`], so a
    /// nonsense footprint cannot produce a nonsense bound.
    #[must_use]
    pub fn for_footprint(width_m: f64, length_m: f64, mount: &SensorMount) -> Self {
        /// Allowance for protrusions outside the body rectangle.
        const PROTRUSION_TOLERANCE_M: f64 = 0.10;

        let circumradius = (width_m / 2.0).hypot(length_m / 2.0);
        let offset = mount.x_m.hypot(mount.y_m);
        let derived = circumradius + offset + PROTRUSION_TOLERANCE_M;

        // A non-finite or negative footprint yields a bound of zero, which
        // refuses every non-degenerate mask. Fail-closed: a mask that cannot be
        // built filters nothing, and filtering nothing is the safe direction.
        let bounded = if derived.is_finite() && derived > 0.0 {
            derived.min(MAX_SELF_MASK_VERTEX_RADIUS_M)
        } else {
            0.0
        };

        Self {
            max_vertex_radius_m: bounded,
        }
    }
}

/// Why a mask was refused.
///
/// Every variant is a refusal, never a clamp. Silently shrinking an
/// over-large mask would leave the operator believing the robot filters a
/// region it does not, which is the failure this whole module exists to make
/// impossible.
#[derive(Debug, Clone, PartialEq)]
pub enum SelfFilterError {
    /// Fewer than three vertices — not a polygon.
    TooFewVertices { polygon: usize, vertices: usize },
    /// More vertices than [`MAX_SELF_MASK_VERTICES`].
    TooManyVertices { polygon: usize, vertices: usize },
    /// More polygons than [`MAX_SELF_MASK_POLYGONS`].
    TooManyPolygons { polygons: usize },
    /// A mask with no polygons. Refused rather than treated as "filter
    /// nothing": a caller that wants no filtering passes `None`, and an empty
    /// mask is more likely a configuration that failed to load.
    Empty,
    /// A vertex coordinate, or the mount transform, is NaN or infinite.
    NonFinite { polygon: usize, vertex: usize },
    /// A vertex sits further from the sensor origin than the bound allows.
    /// This is the constraint that stops a mask from swallowing a real
    /// obstacle.
    ExceedsRadius {
        polygon: usize,
        vertex: usize,
        radius_m: f64,
        limit_m: f64,
    },
    /// The bound itself exceeds the library backstop.
    BoundsExceedBackstop { limit_m: f64, backstop_m: f64 },
    /// The polygon encloses no area — collinear or coincident vertices. It
    /// would filter nothing, so it is a configuration error rather than a
    /// harmless no-op.
    DegenerateArea { polygon: usize },
    /// The mount transform's provenance is not [`MountProvenance::Measured`]
    /// and the caller asked for a production-grade check.
    MountNotMeasured { provenance: MountProvenance },
}

impl std::fmt::Display for SelfFilterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewVertices { polygon, vertices } => {
                write!(f, "polygon {polygon} has {vertices} vertices, needs >= 3")
            }
            Self::TooManyVertices { polygon, vertices } => write!(
                f,
                "polygon {polygon} has {vertices} vertices, limit {MAX_SELF_MASK_VERTICES}"
            ),
            Self::TooManyPolygons { polygons } => write!(
                f,
                "mask has {polygons} polygons, limit {MAX_SELF_MASK_POLYGONS}"
            ),
            Self::Empty => write!(f, "mask has no polygons (pass None to disable filtering)"),
            Self::NonFinite { polygon, vertex } => {
                write!(f, "polygon {polygon} vertex {vertex} is not finite")
            }
            Self::ExceedsRadius {
                polygon,
                vertex,
                radius_m,
                limit_m,
            } => write!(
                f,
                "polygon {polygon} vertex {vertex} at {radius_m:.4} m exceeds the \
                 {limit_m:.4} m self-mask radius bound"
            ),
            Self::BoundsExceedBackstop {
                limit_m,
                backstop_m,
            } => write!(
                f,
                "self-mask radius bound {limit_m:.4} m exceeds the {backstop_m:.4} m backstop"
            ),
            Self::DegenerateArea { polygon } => {
                write!(f, "polygon {polygon} encloses no area")
            }
            Self::MountNotMeasured { provenance } => write!(
                f,
                "sensor mount provenance is {provenance:?}, production requires Measured"
            ),
        }
    }
}

impl std::error::Error for SelfFilterError {}

/// A closed polygon in the **sensor** frame.
///
/// The closing edge is implicit — the last vertex joins the first. Winding
/// direction is irrelevant; containment is even-odd.
#[derive(Debug, Clone, PartialEq)]
pub struct SelfFilterPolygon {
    vertices: Vec<(f64, f64)>,
}

impl SelfFilterPolygon {
    /// Vertices in the sensor frame. Validation happens when the polygon is
    /// handed to [`SelfFilterMask::new`], which is the only thing that can use
    /// it, so an unvalidated polygon can be constructed but never applied.
    #[must_use]
    pub fn new(vertices: Vec<(f64, f64)>) -> Self {
        Self { vertices }
    }

    #[must_use]
    pub fn vertices(&self) -> &[(f64, f64)] {
        &self.vertices
    }

    /// Twice the signed area (the shoelace sum). Zero means degenerate.
    fn double_signed_area(&self) -> f64 {
        let n = self.vertices.len();
        let mut acc = 0.0;
        for i in 0..n {
            let (x0, y0) = self.vertices[i];
            let (x1, y1) = self.vertices[(i + 1) % n];
            acc += x0 * y1 - x1 * y0;
        }
        acc
    }

    /// Is `(px, py)` strictly inside this polygon?
    ///
    /// A point ON an edge returns `false` — it is retained, not masked. The
    /// tie-break direction is load-bearing: masking is the evidence-destroying
    /// operation, so a point we cannot classify confidently must survive.
    fn contains(&self, px: f64, py: f64) -> bool {
        if self.on_boundary(px, py) {
            return false;
        }

        // Even-odd ray casting along +X.
        let n = self.vertices.len();
        let mut inside = false;
        for i in 0..n {
            let (xi, yi) = self.vertices[i];
            let (xj, yj) = self.vertices[(i + 1) % n];
            let straddles = (yi > py) != (yj > py);
            if !straddles {
                continue;
            }
            let t = (py - yi) / (yj - yi);
            if px < xi + t * (xj - xi) {
                inside = !inside;
            }
        }
        inside
    }

    fn on_boundary(&self, px: f64, py: f64) -> bool {
        let n = self.vertices.len();
        for i in 0..n {
            let (x0, y0) = self.vertices[i];
            let (x1, y1) = self.vertices[(i + 1) % n];
            if point_segment_distance(px, py, x0, y0, x1, y1) <= EDGE_EPSILON_M {
                return true;
            }
        }
        false
    }
}

fn point_segment_distance(px: f64, py: f64, x0: f64, y0: f64, x1: f64, y1: f64) -> f64 {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len_sq = dx * dx + dy * dy;
    if len_sq <= 0.0 {
        return (px - x0).hypot(py - y0);
    }
    let t = (((px - x0) * dx + (py - y0) * dy) / len_sq).clamp(0.0, 1.0);
    (px - (x0 + t * dx)).hypot(py - (y0 + t * dy))
}

/// A validated, versioned self-filter.
///
/// Fields are private and there is exactly one constructor, so possessing a
/// `SelfFilterMask` is proof that its geometry passed [`SelfFilterBounds`].
#[derive(Debug, Clone, PartialEq)]
pub struct SelfFilterMask {
    revision: u32,
    polygons: Vec<SelfFilterPolygon>,
    mount: SensorMount,
    bounds: SelfFilterBounds,
}

impl SelfFilterMask {
    /// Validate and build a mask.
    ///
    /// `revision` is the operator-facing version. It must be bumped whenever
    /// the geometry changes — the digest changes automatically, but the
    /// revision is what a human reads in a log line, and a digest that moved
    /// while the revision stood still is itself a finding.
    pub fn new(
        revision: u32,
        polygons: Vec<SelfFilterPolygon>,
        mount: SensorMount,
        bounds: SelfFilterBounds,
    ) -> Result<Self, SelfFilterError> {
        if bounds.max_vertex_radius_m > MAX_SELF_MASK_VERTEX_RADIUS_M
            || !bounds.max_vertex_radius_m.is_finite()
        {
            return Err(SelfFilterError::BoundsExceedBackstop {
                limit_m: bounds.max_vertex_radius_m,
                backstop_m: MAX_SELF_MASK_VERTEX_RADIUS_M,
            });
        }
        if !mount.is_finite() {
            return Err(SelfFilterError::NonFinite {
                polygon: usize::MAX,
                vertex: usize::MAX,
            });
        }
        if polygons.is_empty() {
            return Err(SelfFilterError::Empty);
        }
        if polygons.len() > MAX_SELF_MASK_POLYGONS {
            return Err(SelfFilterError::TooManyPolygons {
                polygons: polygons.len(),
            });
        }

        for (p, poly) in polygons.iter().enumerate() {
            let n = poly.vertices.len();
            if n < 3 {
                return Err(SelfFilterError::TooFewVertices {
                    polygon: p,
                    vertices: n,
                });
            }
            if n > MAX_SELF_MASK_VERTICES {
                return Err(SelfFilterError::TooManyVertices {
                    polygon: p,
                    vertices: n,
                });
            }
            for (v, &(x, y)) in poly.vertices.iter().enumerate() {
                if !x.is_finite() || !y.is_finite() {
                    return Err(SelfFilterError::NonFinite {
                        polygon: p,
                        vertex: v,
                    });
                }
                let radius = x.hypot(y);
                if radius > bounds.max_vertex_radius_m {
                    return Err(SelfFilterError::ExceedsRadius {
                        polygon: p,
                        vertex: v,
                        radius_m: radius,
                        limit_m: bounds.max_vertex_radius_m,
                    });
                }
            }
            if poly.double_signed_area().abs() <= f64::EPSILON {
                return Err(SelfFilterError::DegenerateArea { polygon: p });
            }
        }

        Ok(Self {
            revision,
            polygons,
            mount,
            bounds,
        })
    }

    /// Refuse a mask whose mount transform was never physically measured.
    ///
    /// Separate from [`new`](Self::new) so a bench or simulation configuration
    /// can build a mask with an assumed transform, while a production startup
    /// path calls this and fails closed. The check a deployment owes is
    /// "did anyone actually measure where the lidar is", and the answer must be
    /// recorded rather than inferred.
    pub fn assert_mount_verified(&self) -> Result<(), SelfFilterError> {
        if self.mount.provenance == MountProvenance::Measured {
            Ok(())
        } else {
            Err(SelfFilterError::MountNotMeasured {
                provenance: self.mount.provenance,
            })
        }
    }

    #[must_use]
    pub fn revision(&self) -> u32 {
        self.revision
    }

    #[must_use]
    pub fn mount(&self) -> &SensorMount {
        &self.mount
    }

    #[must_use]
    pub fn bounds(&self) -> &SelfFilterBounds {
        &self.bounds
    }

    #[must_use]
    pub fn polygons(&self) -> &[SelfFilterPolygon] {
        &self.polygons
    }

    /// Should a sensor-frame return at `(x, y)` be discarded as the robot's own
    /// body?
    #[must_use]
    pub fn masks(&self, x: f64, y: f64) -> bool {
        self.polygons.iter().any(|p| p.contains(x, y))
    }

    /// SHA-256 over the mask's canonical encoding, as lowercase hex.
    ///
    /// Covers the revision, every vertex, the mount transform AND its
    /// provenance, and the radius bound. Everything that changes what gets
    /// filtered changes the digest — including a mount whose provenance was
    /// upgraded from assumed to measured without the numbers moving, because
    /// that is a change in what the configuration claims.
    ///
    /// Bytes are little-endian IEEE-754 for coordinates and little-endian
    /// integers for counts, with each polygon length-prefixed so two masks
    /// cannot collide by re-partitioning the same vertices.
    #[must_use]
    pub fn digest_hex(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"kirra-taj-self-filter:v1");
        h.update(self.revision.to_le_bytes());
        h.update((self.polygons.len() as u64).to_le_bytes());
        for poly in &self.polygons {
            h.update((poly.vertices.len() as u64).to_le_bytes());
            for &(x, y) in &poly.vertices {
                h.update(x.to_le_bytes());
                h.update(y.to_le_bytes());
            }
        }
        h.update(self.mount.x_m.to_le_bytes());
        h.update(self.mount.y_m.to_le_bytes());
        h.update(self.mount.yaw_rad.to_le_bytes());
        h.update([match self.mount.provenance {
            MountProvenance::Measured => 0u8,
            MountProvenance::FromUrdf => 1,
            MountProvenance::Assumed => 2,
        }]);
        h.update(self.bounds.max_vertex_radius_m.to_le_bytes());
        hex::encode(h.finalize())
    }
}

/// Why each discarded return was discarded, counted per scan.
///
/// Per-reason rather than a single total, because the reasons have completely
/// different meanings for an operator: a rising `self_masked` count is normal
/// and expected, a rising `nonfinite` count is a sensor going bad, and a
/// `beyond_max_range` count near the ray count is a robot in open space or one
/// whose optics are blocked. Collapsing them into "rejected: 412" throws away
/// the only signal that separates those.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RejectionTally {
    /// Range was NaN — a sensor emitting garbage.
    ///
    /// Deliberately NOT the bucket for `+inf`. A ROS `LaserScan` uses positive
    /// infinity to mean "this ray saw nothing", which is a completely ordinary
    /// reading in open space; filing it under a fault counter would make an
    /// empty room look like a failing lidar. NaN has no such convention behind
    /// it and is always a defect.
    pub not_a_number: u32,
    /// Range below the scan's own `range_min_m`, including `-inf`.
    pub below_min_range: u32,
    /// Range above the scan's own `range_max_m`, including the `+inf` a ROS
    /// LaserScan uses for "no return on this ray". In open space this is most
    /// of the scan and means nothing is wrong.
    pub beyond_max_range: u32,
    /// Inside the self-filter mask. Zero whenever no mask is configured.
    pub self_masked: u32,
}

impl RejectionTally {
    #[must_use]
    pub fn total(&self) -> u32 {
        self.not_a_number
            .saturating_add(self.below_min_range)
            .saturating_add(self.beyond_max_range)
            .saturating_add(self.self_masked)
    }
}

/// One scan's worth of admitted points plus the accounting for what was not
/// admitted.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScanIngest {
    /// Admitted points, angle-ordered, in the base frame.
    pub points: Vec<(f64, f64)>,
    pub rejected: RejectionTally,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square mask, 0.1 m half-side, centred on the sensor origin.
    fn square(half: f64) -> SelfFilterPolygon {
        SelfFilterPolygon::new(vec![
            (-half, -half),
            (half, -half),
            (half, half),
            (-half, half),
        ])
    }

    fn r2_bounds() -> SelfFilterBounds {
        // The measured R2 body: 0.203 m wide, 0.330 m long.
        SelfFilterBounds::for_footprint(0.203, 0.330, &SensorMount::ASSUMED_IDENTITY)
    }

    fn mask(polys: Vec<SelfFilterPolygon>) -> SelfFilterMask {
        SelfFilterMask::new(1, polys, SensorMount::ASSUMED_IDENTITY, r2_bounds())
            .expect("fixture mask must validate")
    }

    // --- the radius bound: a mask cannot reach out to where obstacles live ---

    #[test]
    fn r2_derived_bound_is_body_scale_not_obstacle_scale() {
        let b = r2_bounds();
        // circumradius of 0.203 x 0.330 is ~0.194 m, plus the 0.10 m
        // protrusion tolerance, with no mount offset.
        assert!(
            (b.max_vertex_radius_m - 0.294).abs() < 0.01,
            "expected ~0.294 m, got {}",
            b.max_vertex_radius_m
        );
        assert!(
            b.max_vertex_radius_m < 0.5,
            "the bound must sit well inside the distances at which the R2 must \
             still see an obstacle; got {}",
            b.max_vertex_radius_m
        );
    }

    #[test]
    fn a_mask_reaching_past_the_bound_is_refused_not_clamped() {
        let err = SelfFilterMask::new(
            1,
            vec![square(0.9)],
            SensorMount::ASSUMED_IDENTITY,
            r2_bounds(),
        )
        .expect_err("a 0.9 m mask on a 0.33 m robot must not validate");
        assert!(
            matches!(err, SelfFilterError::ExceedsRadius { .. }),
            "{err}"
        );
    }

    #[test]
    fn the_bound_itself_cannot_exceed_the_library_backstop() {
        let err = SelfFilterMask::new(
            1,
            vec![square(0.1)],
            SensorMount::ASSUMED_IDENTITY,
            SelfFilterBounds {
                max_vertex_radius_m: 50.0,
            },
        )
        .expect_err("a caller must not be able to raise its own ceiling");
        assert!(
            matches!(err, SelfFilterError::BoundsExceedBackstop { .. }),
            "{err}"
        );
    }

    #[test]
    fn for_footprint_clamps_a_nonsense_footprint_to_the_backstop() {
        let b = SelfFilterBounds::for_footprint(1000.0, 1000.0, &SensorMount::ASSUMED_IDENTITY);
        assert_eq!(b.max_vertex_radius_m, MAX_SELF_MASK_VERTEX_RADIUS_M);
    }

    #[test]
    fn for_footprint_refuses_everything_on_a_nonfinite_footprint() {
        let b = SelfFilterBounds::for_footprint(f64::NAN, 0.33, &SensorMount::ASSUMED_IDENTITY);
        assert_eq!(b.max_vertex_radius_m, 0.0);
        let err = SelfFilterMask::new(1, vec![square(0.01)], SensorMount::ASSUMED_IDENTITY, b)
            .expect_err("a zero bound must admit no mask");
        assert!(
            matches!(err, SelfFilterError::ExceedsRadius { .. }),
            "{err}"
        );
    }

    // --- boundary adversarial: one millimetre either side of the mask edge ---

    #[test]
    fn a_return_just_outside_the_mask_survives() {
        let m = mask(vec![square(0.10)]);
        assert!(
            !m.masks(0.101, 0.0),
            "a return 1 mm outside the mask edge must be kept"
        );
    }

    #[test]
    fn a_return_just_inside_the_mask_is_filtered() {
        let m = mask(vec![square(0.10)]);
        assert!(
            m.masks(0.099, 0.0),
            "a return 1 mm inside the mask edge must be filtered"
        );
    }

    #[test]
    fn a_return_exactly_on_the_edge_is_retained() {
        let m = mask(vec![square(0.10)]);
        assert!(
            !m.masks(0.10, 0.0),
            "on the boundary the tie must break toward keeping evidence"
        );
        assert!(!m.masks(0.10, 0.10), "the corner is boundary too");
    }

    #[test]
    fn containment_holds_along_every_edge_of_the_square() {
        let m = mask(vec![square(0.10)]);
        for &(x, y) in &[(0.0, 0.101), (0.0, -0.101), (-0.101, 0.0), (0.101, 0.0)] {
            assert!(!m.masks(x, y), "({x}, {y}) is outside and must survive");
        }
        for &(x, y) in &[(0.0, 0.099), (0.0, -0.099), (-0.099, 0.0), (0.099, 0.0)] {
            assert!(m.masks(x, y), "({x}, {y}) is inside and must be filtered");
        }
    }

    #[test]
    fn a_concave_mask_does_not_filter_its_notch() {
        // An L, with the notch in the +x/+y quadrant. A return in the notch is
        // outside the robot and must survive even though it is inside the
        // bounding box.
        let l = SelfFilterPolygon::new(vec![
            (-0.10, -0.10),
            (0.10, -0.10),
            (0.10, 0.0),
            (0.0, 0.0),
            (0.0, 0.10),
            (-0.10, 0.10),
        ]);
        let m = mask(vec![l]);
        assert!(m.masks(-0.05, 0.05), "inside the L's upper arm");
        assert!(m.masks(0.05, -0.05), "inside the L's lower arm");
        assert!(
            !m.masks(0.05, 0.05),
            "the notch is not the robot — a return there must survive"
        );
    }

    #[test]
    fn multiple_polygons_all_apply() {
        let left = SelfFilterPolygon::new(vec![
            (-0.12, 0.05),
            (-0.08, 0.05),
            (-0.08, 0.09),
            (-0.12, 0.09),
        ]);
        let right = SelfFilterPolygon::new(vec![
            (-0.12, -0.09),
            (-0.08, -0.09),
            (-0.08, -0.05),
            (-0.12, -0.05),
        ]);
        let m = mask(vec![left, right]);
        assert!(m.masks(-0.10, 0.07));
        assert!(m.masks(-0.10, -0.07));
        assert!(!m.masks(-0.10, 0.0), "the gap between them is not masked");
    }

    // --- structural validation ---

    #[test]
    fn a_two_vertex_polygon_is_refused() {
        let err = SelfFilterMask::new(
            1,
            vec![SelfFilterPolygon::new(vec![(0.0, 0.0), (0.1, 0.0)])],
            SensorMount::ASSUMED_IDENTITY,
            r2_bounds(),
        )
        .expect_err("two vertices is a line");
        assert!(
            matches!(err, SelfFilterError::TooFewVertices { .. }),
            "{err}"
        );
    }

    #[test]
    fn a_collinear_polygon_is_refused() {
        let err = SelfFilterMask::new(
            1,
            vec![SelfFilterPolygon::new(vec![
                (0.0, 0.0),
                (0.05, 0.0),
                (0.10, 0.0),
            ])],
            SensorMount::ASSUMED_IDENTITY,
            r2_bounds(),
        )
        .expect_err("three collinear points enclose nothing");
        assert!(
            matches!(err, SelfFilterError::DegenerateArea { .. }),
            "{err}"
        );
    }

    #[test]
    fn a_nonfinite_vertex_is_refused() {
        let err = SelfFilterMask::new(
            1,
            vec![SelfFilterPolygon::new(vec![
                (0.0, 0.0),
                (f64::NAN, 0.0),
                (0.05, 0.05),
            ])],
            SensorMount::ASSUMED_IDENTITY,
            r2_bounds(),
        )
        .expect_err("NaN geometry must not build");
        assert!(matches!(err, SelfFilterError::NonFinite { .. }), "{err}");
    }

    #[test]
    fn an_empty_mask_is_refused_rather_than_treated_as_disabled() {
        let err = SelfFilterMask::new(1, vec![], SensorMount::ASSUMED_IDENTITY, r2_bounds())
            .expect_err("an empty mask is a failed load, not a disabled filter");
        assert_eq!(err, SelfFilterError::Empty);
    }

    #[test]
    fn too_many_polygons_is_refused() {
        let polys = (0..=MAX_SELF_MASK_POLYGONS).map(|_| square(0.05)).collect();
        let err = SelfFilterMask::new(1, polys, SensorMount::ASSUMED_IDENTITY, r2_bounds())
            .expect_err("the polygon count is bounded");
        assert!(
            matches!(err, SelfFilterError::TooManyPolygons { .. }),
            "{err}"
        );
    }

    #[test]
    fn too_many_vertices_is_refused() {
        let verts = (0..=MAX_SELF_MASK_VERTICES)
            .map(|i| {
                let t = i as f64 * 0.1;
                (0.05 * t.cos(), 0.05 * t.sin())
            })
            .collect();
        let err = SelfFilterMask::new(
            1,
            vec![SelfFilterPolygon::new(verts)],
            SensorMount::ASSUMED_IDENTITY,
            r2_bounds(),
        )
        .expect_err("the vertex count is bounded");
        assert!(
            matches!(err, SelfFilterError::TooManyVertices { .. }),
            "{err}"
        );
    }

    // --- mount provenance ---

    #[test]
    fn an_assumed_mount_is_refused_for_production() {
        let m = mask(vec![square(0.10)]);
        let err = m
            .assert_mount_verified()
            .expect_err("the identity transform has never been measured");
        assert!(
            matches!(err, SelfFilterError::MountNotMeasured { .. }),
            "{err}"
        );
    }

    #[test]
    fn a_urdf_mount_is_also_refused_for_production() {
        let m = SelfFilterMask::new(
            1,
            vec![square(0.10)],
            SensorMount {
                x_m: 0.12,
                y_m: 0.0,
                yaw_rad: 0.0,
                provenance: MountProvenance::FromUrdf,
            },
            SelfFilterBounds::for_footprint(
                0.203,
                0.330,
                &SensorMount {
                    x_m: 0.12,
                    y_m: 0.0,
                    yaw_rad: 0.0,
                    provenance: MountProvenance::FromUrdf,
                },
            ),
        )
        .expect("a URDF mount still builds");
        assert!(
            m.assert_mount_verified().is_err(),
            "a model is not a measurement"
        );
    }

    #[test]
    fn a_measured_mount_passes() {
        let mount = SensorMount {
            x_m: 0.12,
            y_m: 0.0,
            yaw_rad: 0.0,
            provenance: MountProvenance::Measured,
        };
        let m = SelfFilterMask::new(
            1,
            vec![square(0.10)],
            mount,
            SelfFilterBounds::for_footprint(0.203, 0.330, &mount),
        )
        .expect("a measured mount builds");
        assert!(m.assert_mount_verified().is_ok());
    }

    #[test]
    fn a_mount_offset_widens_the_derived_bound_by_exactly_the_offset() {
        let at_origin =
            SelfFilterBounds::for_footprint(0.203, 0.330, &SensorMount::ASSUMED_IDENTITY);
        let offset_mount = SensorMount {
            x_m: 0.12,
            y_m: 0.0,
            yaw_rad: 0.0,
            provenance: MountProvenance::Measured,
        };
        let offset = SelfFilterBounds::for_footprint(0.203, 0.330, &offset_mount);
        assert!(
            (offset.max_vertex_radius_m - at_origin.max_vertex_radius_m - 0.12).abs() < 1e-9,
            "a sensor mounted 0.12 m forward can legitimately see 0.12 m more of \
             its own body in one direction"
        );
    }

    #[test]
    fn the_mount_transform_maps_sensor_points_into_the_base_frame() {
        let mount = SensorMount {
            x_m: 0.12,
            y_m: 0.0,
            yaw_rad: 0.0,
            provenance: MountProvenance::Measured,
        };
        let (x, y) = mount.to_base(1.0, 0.0);
        assert!((x - 1.12).abs() < 1e-12 && y.abs() < 1e-12);

        let rotated = SensorMount {
            x_m: 0.0,
            y_m: 0.0,
            yaw_rad: std::f64::consts::FRAC_PI_2,
            provenance: MountProvenance::Measured,
        };
        let (x, y) = rotated.to_base(1.0, 0.0);
        assert!(x.abs() < 1e-12 && (y - 1.0).abs() < 1e-12);
    }

    #[test]
    fn the_assumed_identity_is_a_no_op() {
        let (x, y) = SensorMount::ASSUMED_IDENTITY.to_base(1.23, -4.56);
        assert_eq!((x, y), (1.23, -4.56));
    }

    // --- digest: a geometry change is a digest change ---

    #[test]
    fn the_digest_changes_when_the_geometry_changes() {
        let a = mask(vec![square(0.10)]);
        let b = mask(vec![square(0.1001)]);
        assert_ne!(
            a.digest_hex(),
            b.digest_hex(),
            "a 0.1 mm mask change must not be invisible"
        );
    }

    #[test]
    fn the_digest_changes_when_only_the_revision_changes() {
        let a = mask(vec![square(0.10)]);
        let b = SelfFilterMask::new(
            2,
            vec![square(0.10)],
            SensorMount::ASSUMED_IDENTITY,
            r2_bounds(),
        )
        .expect("valid");
        assert_ne!(a.digest_hex(), b.digest_hex());
    }

    #[test]
    fn the_digest_changes_when_only_the_mount_provenance_changes() {
        let assumed = mask(vec![square(0.10)]);
        let measured = SelfFilterMask::new(
            1,
            vec![square(0.10)],
            SensorMount {
                x_m: 0.0,
                y_m: 0.0,
                yaw_rad: 0.0,
                provenance: MountProvenance::Measured,
            },
            r2_bounds(),
        )
        .expect("valid");
        assert_ne!(
            assumed.digest_hex(),
            measured.digest_hex(),
            "upgrading a claim from assumed to measured is a configuration change"
        );
    }

    #[test]
    fn the_digest_is_stable_across_identical_masks() {
        assert_eq!(
            mask(vec![square(0.10)]).digest_hex(),
            mask(vec![square(0.10)]).digest_hex()
        );
    }

    #[test]
    fn repartitioned_vertices_do_not_collide() {
        // Two polygons of 4 vertices vs. one of 8 covering the same points
        // must not hash the same — the length prefix is what prevents it.
        let a = mask(vec![square(0.05), square(0.08)]);
        let mut merged = square(0.05).vertices().to_vec();
        merged.extend_from_slice(square(0.08).vertices());
        let b = mask(vec![SelfFilterPolygon::new(merged)]);
        assert_ne!(a.digest_hex(), b.digest_hex());
    }

    #[test]
    fn the_digest_is_a_full_sha256() {
        let d = mask(vec![square(0.10)]).digest_hex();
        assert_eq!(d.len(), 64);
        assert!(d
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    // --- tally ---

    #[test]
    fn the_tally_totals_every_reason() {
        let t = RejectionTally {
            not_a_number: 1,
            below_min_range: 2,
            beyond_max_range: 3,
            self_masked: 4,
        };
        assert_eq!(t.total(), 10);
    }

    #[test]
    fn the_tally_total_saturates_rather_than_overflowing() {
        let t = RejectionTally {
            not_a_number: u32::MAX,
            below_min_range: u32::MAX,
            beyond_max_range: 0,
            self_masked: 0,
        };
        assert_eq!(t.total(), u32::MAX);
    }
}
