//! #1215 — typed geometric regions derived from the canonical platform contract.
//!
//! # The property
//!
//! > Every consumer geometry value is constructed from the canonical platform
//! > profile plus an explicitly named margin or transformation; no consumer
//! > independently re-derives physical vehicle dimensions.
//!
//! # Why types and not just a manifest
//!
//! #1219 can transport `length_m` and `width_m`. It cannot answer whether
//! `0.11` means physical half-width, self-filter padding, proposal envelope, or
//! observation clearance. Those are four different claims about the world, and
//! before this module they were all spelled "some geometry number" and
//! independently re-derived in four places:
//!
//! | Location | half-length | half-width | meaning (undeclared) |
//! |---|---|---|---|
//! | `contract_profiles.rs` | *length 0.330* | *width 0.203* | the physical body — the authority |
//! | `kirra-trajectory` `config.rs::r2()` | 0.165 | 0.102 | checker collision extent |
//! | `kirra_params.yaml` `occy_doer` | 0.17 | 0.11 | doer proposal envelope |
//! | `kirra_params.yaml` `perception_governor` | — | `vehicle_width_m: 0.203` | **full** width, corridor input |
//!
//! The last row is the trap this module exists to prevent: `0.203` there is a
//! FULL width feeding `required_corridor_width = vehicle_width + 2·clearance`,
//! not a half-extent. Substituting it for a half-width would halve the corridor
//! the ego demands. A type that carries its meaning cannot be swapped that way.
//!
//! # Margins are named, never accidental
//!
//! The doer's `0.17 × 0.11` is not a mistake to normalize away — it is a
//! deliberate margin over the physical `0.165 × 0.1015`, in the conservative
//! direction (the doer proposes within a larger body than the checker demands).
//! What was missing is that nothing *said so*, and nothing bound the two: change
//! `width_m` in the contract and neither followed.
//!
//! So every region is built as
//!
//! ```text
//! physical footprint  +  explicitly named margin  =  region
//! ```
//!
//! and the tests assert that RELATIONSHIP rather than an accidental equality.
//! A region is never allowed to be smaller than the body it bounds.
//!
//! # Frames are load-bearing, and two conventions are in play
//!
//! This is the subtler half, and it is why every region carries a [`Frame`].
//!
//! `containment::footprint_corners` places the body at the **rear axle**:
//! forward extent is `wheelbase + overhang_front`, rearward is `-overhang_rear`.
//! An asymmetric body is described honestly there.
//!
//! The doer's `half_length_m` assumes a **geometric centre** origin: half the
//! length each way, symmetric by construction.
//!
//! For the R2 those agree — but only because the overhang split is assumed
//! even, and that split is an **ESTIMATE**, not a measurement (`r2.overhangs`,
//! #1219; the measurement is owed under #1213). If the real split is uneven, a
//! centre-origin symmetric half-length under-covers the longer end while
//! reporting the same number. Nothing in the tree previously stated that
//! coupling. [`PhysicalFootprint::centre_origin_is_exact`] makes it checkable.

use crate::containment::VehicleFootprint;
use crate::kinematics_contract::VehicleKinematicsContract;

/// The coordinate frame a region's extents are expressed in.
///
/// Carried on every region because the same body has two valid descriptions and
/// they are not interchangeable — see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    /// Origin at the **rear axle centre**, +x forward, +y left. The convention
    /// `containment::footprint_corners` uses, and the only one that describes an
    /// asymmetric body without loss.
    RearAxleBody,
    /// Origin at the **geometric centre** of the body, +x forward, +y left.
    /// Symmetric by construction: exact only when the overhang split is even.
    GeometricCentreBody,
    /// The sensor's own origin. Self-filtering happens HERE, before any
    /// transform, so a mount error cannot move the mask onto real returns
    /// (#1210).
    Sensor,
}

impl Frame {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Frame::RearAxleBody => "rear_axle_body",
            Frame::GeometricCentreBody => "geometric_centre_body",
            Frame::Sensor => "sensor",
        }
    }
}

/// An explicitly named, non-negative expansion of a region beyond the physical
/// body.
///
/// Named because "why is this region bigger than the robot" is a safety
/// question with a different answer per region, and an unnamed epsilon is
/// indistinguishable from a rounding accident. The doer's extra ~8 mm and the
/// checker's ~0.5 mm rounding were both of that kind before this type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Margin {
    /// Why this margin exists. Not decoration — it is the field a reviewer reads
    /// to decide whether the margin is still justified.
    pub reason: &'static str,
    pub longitudinal_m: f64,
    pub lateral_m: f64,
}

/// A margin was not usable as declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarginError {
    /// Negative margins would SHRINK a region below the body it bounds.
    /// Refused rather than clamped: a negative margin is a statement someone
    /// meant to make, and silently zeroing it hides the intent.
    Negative,
    /// Non-finite margins propagate into every downstream comparison.
    NotFinite,
    /// A margin with no stated reason is an unexplained expansion.
    Unnamed,
}

impl Margin {
    /// The zero margin — a region exactly the size of the body.
    pub const NONE: Margin = Margin {
        reason: "no expansion: the region is exactly the physical body",
        longitudinal_m: 0.0,
        lateral_m: 0.0,
    };

    /// Construct a validated margin. FAIL-CLOSED on negative, non-finite, or
    /// unnamed.
    pub fn new(
        reason: &'static str,
        longitudinal_m: f64,
        lateral_m: f64,
    ) -> Result<Self, MarginError> {
        if reason.trim().is_empty() {
            return Err(MarginError::Unnamed);
        }
        if !longitudinal_m.is_finite() || !lateral_m.is_finite() {
            return Err(MarginError::NotFinite);
        }
        if longitudinal_m < 0.0 || lateral_m < 0.0 {
            return Err(MarginError::Negative);
        }
        Ok(Margin {
            reason,
            longitudinal_m,
            lateral_m,
        })
    }
}

/// The physical vehicle body. **The one thing every region derives from.**
///
/// Constructed only from a [`VehicleKinematicsContract`], so a consumer cannot
/// introduce a body dimension the contract does not know about.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicalFootprint {
    pub length_m: f64,
    pub width_m: f64,
    pub wheelbase_m: f64,
    pub overhang_front_m: f64,
    pub overhang_rear_m: f64,
}

impl PhysicalFootprint {
    /// Derive from the canonical contract. The ONLY constructor — there is
    /// deliberately no way to hand-build one from loose numbers.
    #[must_use]
    pub fn from_contract(c: &VehicleKinematicsContract) -> Self {
        Self {
            length_m: c.length_m,
            width_m: c.width_m,
            wheelbase_m: c.wheelbase_m,
            overhang_front_m: c.overhang_front_m,
            overhang_rear_m: c.overhang_rear_m,
        }
    }

    /// Half the body width. The value a consumer means by "half_width_m".
    #[must_use]
    pub fn half_width_m(&self) -> f64 {
        self.width_m * 0.5
    }

    /// Half the body length, in the GEOMETRIC CENTRE convention.
    ///
    /// Exact only when the overhang split is even — see
    /// [`Self::centre_origin_is_exact`].
    #[must_use]
    pub fn half_length_m(&self) -> f64 {
        self.length_m * 0.5
    }

    /// Forward extent from the REAR AXLE. The honest description of an
    /// asymmetric body.
    #[must_use]
    pub fn forward_extent_from_rear_axle_m(&self) -> f64 {
        self.wheelbase_m + self.overhang_front_m
    }

    /// Rearward extent from the rear axle (a positive distance).
    #[must_use]
    pub fn rearward_extent_from_rear_axle_m(&self) -> f64 {
        self.overhang_rear_m
    }

    /// Whether a symmetric centre-origin description loses nothing.
    ///
    /// True exactly when the overhangs are equal. When false, a consumer using
    /// `half_length_m` describes the body as symmetric when it is not, and
    /// **under-covers the longer end** while reporting a plausible number.
    ///
    /// For the R2 this is true only because the overhang split is an assumed
    /// even division of `(length − wheelbase)`; the measurement is owed under
    /// #1213 and the assumption is registered as `r2.overhangs` ESTIMATE in the
    /// #1219 provenance table.
    #[must_use]
    pub fn centre_origin_is_exact(&self) -> bool {
        (self.overhang_front_m - self.overhang_rear_m).abs() <= f64::EPSILON
    }

    /// How much a symmetric centre-origin description under-covers the longer
    /// end. Zero when [`Self::centre_origin_is_exact`].
    #[must_use]
    pub fn centre_origin_shortfall_m(&self) -> f64 {
        ((self.overhang_front_m - self.overhang_rear_m) * 0.5).abs()
    }
}

impl From<&PhysicalFootprint> for VehicleFootprint {
    fn from(p: &PhysicalFootprint) -> Self {
        VehicleFootprint {
            width_m: p.width_m,
            length_m: p.length_m,
            overhang_front_m: p.overhang_front_m,
            overhang_rear_m: p.overhang_rear_m,
            wheelbase_m: p.wheelbase_m,
        }
    }
}

/// **"Would the ego contact this object?"**
///
/// The region an object must lie inside to count as a collision candidate.
/// Half-extents, because the question is symmetric about the ego's own axis.
///
/// Absorbs: `kirra-trajectory` `config.rs`'s `half_length_m` / `half_width_m`,
/// and `kirra_params.yaml` `occy_doer`'s same-named pair. Those differed
/// (0.165/0.102 vs 0.17/0.11) with no stated relationship; they are now the same
/// construction with different named margins.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CollisionRegion {
    pub frame: Frame,
    /// Forward extent from the frame origin. Kept SEPARATE from the rearward
    /// extent so an asymmetric body (robotaxi: overhangs 0.9 / 1.1) is never
    /// silently averaged into a symmetric one — the average under-covers the
    /// longer end while reporting a plausible number.
    pub front_extent_m: f64,
    /// Rearward extent from the frame origin (a positive distance).
    pub rear_extent_m: f64,
    pub half_width_m: f64,
    pub margin: Margin,
    /// The body this was derived from, retained so the relationship stays
    /// checkable after construction.
    pub physical: PhysicalFootprint,
}

impl CollisionRegion {
    /// Build from the canonical footprint plus a named margin, in the
    /// centre-origin convention. Front and rear extents are each `length/2`
    /// BY DEFINITION of the geometric centre — the asymmetry hazard for this
    /// constructor lives in where the ORIGIN is placed, not in the extents;
    /// see [`PhysicalFootprint::centre_origin_is_exact`].
    #[must_use]
    pub fn centre_origin(physical: PhysicalFootprint, margin: Margin) -> Self {
        Self {
            frame: Frame::GeometricCentreBody,
            front_extent_m: physical.half_length_m() + margin.longitudinal_m,
            rear_extent_m: physical.half_length_m() + margin.longitudinal_m,
            half_width_m: physical.half_width_m() + margin.lateral_m,
            margin,
            physical,
        }
    }

    /// Build in the rear-axle convention, preserving the body's true
    /// asymmetry: forward is `wheelbase + overhang_front`, rearward is
    /// `overhang_rear`. The honest description of an asymmetric body, and the
    /// only correct one for the robotaxi class.
    #[must_use]
    pub fn rear_axle_origin(physical: PhysicalFootprint, margin: Margin) -> Self {
        Self {
            frame: Frame::RearAxleBody,
            front_extent_m: physical.forward_extent_from_rear_axle_m() + margin.longitudinal_m,
            rear_extent_m: physical.rearward_extent_from_rear_axle_m() + margin.longitudinal_m,
            half_width_m: physical.half_width_m() + margin.lateral_m,
            margin,
            physical,
        }
    }

    /// The symmetric half-length as a DELIBERATELY CONSERVATIVE derived bound:
    /// `max(front_extent, rear_extent)`, never the average.
    ///
    /// A consumer that structurally requires one symmetric number (the doer's
    /// `half_length_m` parameter) must take it from here. The average would
    /// under-cover the longer end of an asymmetric body — for the robotaxi's
    /// 0.9 / 1.1 overhang split, by a real margin — while reporting a value
    /// that looks right. `max` over-covers the shorter end instead, which is
    /// the only acceptable direction for a bound.
    #[must_use]
    pub fn conservative_symmetric_half_length_m(&self) -> f64 {
        self.front_extent_m.max(self.rear_extent_m)
    }

    /// Whether an object at `(x, y)` in this region's frame is a collision
    /// candidate. `+x` forward, so the rearward test is against
    /// `-rear_extent_m`.
    #[must_use]
    pub fn contains(&self, x_m: f64, y_m: f64) -> bool {
        x_m.is_finite()
            && y_m.is_finite()
            && x_m <= self.front_extent_m
            && x_m >= -self.rear_extent_m
            && y_m.abs() <= self.half_width_m
    }

    /// A region must never be smaller than the body it bounds — checked in the
    /// region's OWN frame, since the extents mean different things in each.
    /// Holds by construction given a validated [`Margin`]; asserted anyway,
    /// because "holds by construction" survives only until someone adds a
    /// constructor.
    #[must_use]
    pub fn covers_physical_body(&self) -> bool {
        let (body_front, body_rear) = match self.frame {
            Frame::GeometricCentreBody => {
                (self.physical.half_length_m(), self.physical.half_length_m())
            }
            Frame::RearAxleBody | Frame::Sensor => (
                self.physical.forward_extent_from_rear_axle_m(),
                self.physical.rearward_extent_from_rear_axle_m(),
            ),
        };
        self.front_extent_m >= body_front
            && self.rear_extent_m >= body_rear
            && self.half_width_m >= self.physical.half_width_m()
    }
}

/// **"How much room must the corridor provide?"**
///
/// Deliberately NOT a half-extent. This carries the **full** vehicle width plus
/// clearance on each side, matching
/// `required_corridor_width = vehicle_width + 2·clearance` in the Taj sidecar.
///
/// Absorbs `kirra_params.yaml` `perception_governor`'s `vehicle_width_m: 0.203`
/// and `lateral_clearance_m`. The type exists chiefly so that value cannot be
/// mistaken for a half-width — doing so would halve the corridor the ego demands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CorridorObservationRegion {
    pub frame: Frame,
    /// FULL body width, not halved.
    pub vehicle_width_m: f64,
    /// Clearance demanded on EACH side.
    pub clearance_each_side_m: f64,
    pub physical: PhysicalFootprint,
}

impl CorridorObservationRegion {
    #[must_use]
    pub fn new(physical: PhysicalFootprint, clearance: Margin) -> Self {
        Self {
            frame: Frame::GeometricCentreBody,
            vehicle_width_m: physical.width_m,
            clearance_each_side_m: clearance.lateral_m,
            physical,
        }
    }

    /// The total corridor width the ego requires.
    #[must_use]
    pub fn required_width_m(&self) -> f64 {
        self.vehicle_width_m + 2.0 * self.clearance_each_side_m
    }

    /// Half the required width — the comparison a lane-gate half-extent must
    /// satisfy. Provided so a caller never has to halve by hand, which is where
    /// the full-vs-half confusion enters.
    #[must_use]
    pub fn required_half_width_m(&self) -> f64 {
        self.required_width_m() * 0.5
    }
}

/// **"Returns inside this are the robot itself."**
///
/// Sensor frame, always. #1210's argument runs backwards from every other
/// region here: this is the one operation that REMOVES evidence, so its bound
/// limits how much a mask may destroy rather than how much space the ego needs.
///
/// The vertex radius derives from the body's own diagonal — a mask may not
/// reach beyond the robot it is masking.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelfFilterRegion {
    pub frame: Frame,
    /// No mask vertex may sit further from the sensor origin than this.
    pub max_vertex_radius_m: f64,
    pub physical: PhysicalFootprint,
}

impl SelfFilterRegion {
    /// The body's half-diagonal: the furthest any point of the robot can be from
    /// its own centre. A mask vertex beyond this is masking something that is
    /// not the robot.
    #[must_use]
    pub fn from_footprint(physical: PhysicalFootprint) -> Self {
        let hl = physical.half_length_m();
        let hw = physical.half_width_m();
        Self {
            frame: Frame::Sensor,
            max_vertex_radius_m: (hl * hl + hw * hw).sqrt(),
            physical,
        }
    }
}

/// **"What does the body sweep through a turn?"**
///
/// A NAMED VIEW over the existing containment geometry, not a second
/// implementation. `containment::footprint_corners` already places the swept
/// corners at `(wheelbase + overhang_front, ±width/2)` and `(−overhang_rear,
/// ±width/2)` from the rear axle; this type gives that placement a name, a
/// frame, and a home, so a reader can find out what "the swept region" means
/// without reading the containment internals.
///
/// Deliberately carries no margin: the swept region is the body's actual
/// excursion, and the safety margin against the corridor is applied separately
/// by the containment check (`CONTAINMENT_LATERAL_MARGIN_M`). Folding a margin
/// in here would double-count it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TurnSweptRegion {
    pub frame: Frame,
    pub forward_extent_m: f64,
    pub rearward_extent_m: f64,
    pub half_width_m: f64,
    pub physical: PhysicalFootprint,
}

impl TurnSweptRegion {
    /// Rear-axle frame, matching `containment::footprint_corners` exactly.
    #[must_use]
    pub fn rear_axle_origin(physical: PhysicalFootprint) -> Self {
        Self {
            frame: Frame::RearAxleBody,
            forward_extent_m: physical.forward_extent_from_rear_axle_m(),
            rearward_extent_m: physical.rearward_extent_from_rear_axle_m(),
            half_width_m: physical.half_width_m(),
            physical,
        }
    }

    /// Total longitudinal extent. Equals `length_m` exactly when
    /// `wheelbase + overhang_front + overhang_rear == length` — the family
    /// invariant `every_profile_footprint_positive` assumes and this makes
    /// checkable.
    #[must_use]
    pub fn total_length_m(&self) -> f64 {
        self.forward_extent_m + self.rearward_extent_m
    }
}

#[cfg(test)]
mod tests;
