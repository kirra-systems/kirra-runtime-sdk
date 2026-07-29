//! #1219 part 2 — provenance coverage over the canonical contract family.
//!
//! # The one property
//!
//! > Every authority-relevant field and externally applied constraint has
//! > exactly one provenance mapping; inherited mappings identify **and equal**
//! > their upstream source; serialized representations remain derived from the
//! > Rust contract and existing reviewed integrity pins.
//!
//! # Why this is a view and not an inventory
//!
//! The load-bearing constraint on this module: **it contains no numbers.**
//!
//! A hand-authored table of "field → value → provenance" would be a second
//! numeric authority, and would drift from `contract_profiles.rs` exactly as the
//! geometry literals drifted from the platform config (#1219 part 1). So every
//! record here names a [`Field`] and never a value. Values are read live from
//! [`contract_for`] / [`mrc_fallback_for`] at the moment a check or a
//! serialization runs. Change a contract number and this module reports the new
//! number without being edited; change it *wrongly* and the relational checks
//! below fail.
//!
//! The single exception is [`ExternalConstraint::value_mps`], and it is a
//! reference rather than a literal — `ROBOTAXI_ODD_SPEED_CAP_MPS`, itself an
//! alias of the talisman's `URBAN_ODD_SPEED_CAP_MPS`. Writing `22.35` there
//! would have created the duplicate this module exists to prevent.
//!
//! # Why coverage is field-to-key, not key-set equality
//!
//! Tag granularity is deliberately non-uniform. `r2` splits `r2.overhangs` out
//! from `r2.footprint` *because their provenance differs* — the footprint is
//! MEASURED off the bench, the overhang split is an ESTIMATE. `courier` lumps
//! all four geometry fields under `courier.footprint` because they share one
//! provenance.
//!
//! Computing an expected key set as classes × fields would force a uniform
//! granularity and destroy the r2 split, which is the one place the family
//! currently distinguishes a measurement from an assumption. So the assertion is
//! total coverage at the **field** level, with keys as coarse or fine as
//! provenance requires: every (class, profile, field) triple is governed by
//! exactly one record, and no triple by two.
//!
//! # Why `INHERITED` is executable
//!
//! `INHERITED` is not a politer word for unattributed. A record carrying it
//! names its upstream key, and [`check_inheritance`] resolves that key and
//! compares the two contracts' values **bit-for-bit**. The MRC footprint fields
//! previously asserted their equality in prose ("Footprint IDENTICAL to r2
//! nominal"); they now assert it in a test that fails when it stops being true.
//!
//! That is a stronger property than provenance coverage alone, and it closes a
//! real gap: `mrc_leq_nominal_per_field_every_class` compares wheelbase, width
//! and length but not the two overhangs, so 6 of the 15 inheritance claims were
//! asserted nowhere — and the overhangs are the fields that place the
//! swept-footprint corners in `containment.rs`.
//!
//! # Why the frozen class needs a sidecar
//!
//! The robotaxi contract lives in the byte-frozen kinematics talisman, whose git
//! blob hash is its integrity pin. Provenance comments cannot be added beside
//! those numbers without breaking the pin, so robotaxi's records live in
//! [`ROBOTAXI_SIDECAR`] — bound to that same pin, not to a new one.
//! [`validate_robotaxi_sidecar`] consumes the EXISTING authority (`git
//! hash-object` against the hash recorded in `docs/CAPTURE_PIPELINE_SPEC.md`,
//! the pin `ci/build_safety_case.py` and the `ci.yml` rustfmt lane already
//! check). A second hash pin would give the anti-drift mechanism its own drift
//! problem.
//!
//! The sidecar describes the frozen values. It never supplies or overrides them:
//! its records name fields, like every other record here, and the values still
//! come from the frozen instance.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use kirra_core::kinematics_contract::VehicleKinematicsContract;

use super::contract_profiles::{
    contract_for, mrc_fallback_for, VehicleClass, COURIER_ODD_SPEED_CAP_MPS,
    DELIVERY_AV_ODD_SPEED_CAP_MPS, R2_ODD_SPEED_CAP_MPS, ROBOTAXI_ODD_SPEED_CAP_MPS,
};

/// The frozen talisman, and the reviewed document that pins its blob hash. Both
/// mirror `ci/build_safety_case.py`'s constants of the same name — deliberately,
/// so the sidecar binds to the pin the safety case already gates on.
pub const TALISMAN_PATH: &str = "crates/kirra-core/src/kinematics_contract.rs";
/// The document holding the authoritative blob pin. See [`TALISMAN_PATH`].
pub const TALISMAN_PIN_DOC: &str = "docs/CAPTURE_PIPELINE_SPEC.md";

// ---------------------------------------------------------------------------
// Fields
// ---------------------------------------------------------------------------

/// Every authority-relevant field of [`VehicleKinematicsContract`].
///
/// [`Field::ALL`] is the completeness denominator: a field added to the contract
/// struct and not added here would silently escape coverage, so the test
/// `every_contract_struct_field_appears_in_the_field_enum` pins this list
/// against the struct's own serialized shape rather than against a hand-kept
/// count. (Named, not linked: it is `#[cfg(test)]`, so an intra-doc link to it
/// cannot resolve when rustdoc runs.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    MaxSpeed,
    MaxAccel,
    MaxBrake,
    MaxSteering,
    MaxSteeringRate,
    MinFollow,
    MaxLateralAccel,
    Wheelbase,
    Width,
    Length,
    OverhangFront,
    OverhangRear,
    OddSpeedCap,
}

/// A field's live value, read from a real contract. `OptionalCap` keeps the
/// absent-cap case distinguishable from a zero cap — `None` means "no ODD cap on
/// this profile", which is a claim about authority, not a magnitude.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldValue {
    Scalar(f64),
    OptionalCap(Option<f64>),
}

impl Field {
    /// Every field, in declaration order. The completeness denominator.
    pub const ALL: &'static [Field] = &[
        Field::MaxSpeed,
        Field::MaxAccel,
        Field::MaxBrake,
        Field::MaxSteering,
        Field::MaxSteeringRate,
        Field::MinFollow,
        Field::MaxLateralAccel,
        Field::Wheelbase,
        Field::Width,
        Field::Length,
        Field::OverhangFront,
        Field::OverhangRear,
        Field::OddSpeedCap,
    ];

    /// The struct field name, matching `VehicleKinematicsContract`'s serde shape.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Field::MaxSpeed => "max_speed_mps",
            Field::MaxAccel => "max_accel_mps2",
            Field::MaxBrake => "max_brake_mps2",
            Field::MaxSteering => "max_steering_deg",
            Field::MaxSteeringRate => "max_steering_rate_deg_s",
            Field::MinFollow => "min_follow_distance_m",
            Field::MaxLateralAccel => "max_lateral_accel_mps2",
            Field::Wheelbase => "wheelbase_m",
            Field::Width => "width_m",
            Field::Length => "length_m",
            Field::OverhangFront => "overhang_front_m",
            Field::OverhangRear => "overhang_rear_m",
            Field::OddSpeedCap => "odd_speed_cap_mps",
        }
    }

    /// Read this field LIVE from `c`. The only path by which a number enters
    /// this module.
    #[must_use]
    pub fn read(self, c: &VehicleKinematicsContract) -> FieldValue {
        match self {
            Field::MaxSpeed => FieldValue::Scalar(c.max_speed_mps),
            Field::MaxAccel => FieldValue::Scalar(c.max_accel_mps2),
            Field::MaxBrake => FieldValue::Scalar(c.max_brake_mps2),
            Field::MaxSteering => FieldValue::Scalar(c.max_steering_deg),
            Field::MaxSteeringRate => FieldValue::Scalar(c.max_steering_rate_deg_s),
            Field::MinFollow => FieldValue::Scalar(c.min_follow_distance_m),
            Field::MaxLateralAccel => FieldValue::Scalar(c.max_lateral_accel_mps2),
            Field::Wheelbase => FieldValue::Scalar(c.wheelbase_m),
            Field::Width => FieldValue::Scalar(c.width_m),
            Field::Length => FieldValue::Scalar(c.length_m),
            Field::OverhangFront => FieldValue::Scalar(c.overhang_front_m),
            Field::OverhangRear => FieldValue::Scalar(c.overhang_rear_m),
            Field::OddSpeedCap => FieldValue::OptionalCap(c.odd_speed_cap_mps),
        }
    }
}

/// Which posture profile a record governs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Nominal,
    Mrc,
}

impl Profile {
    pub const ALL: &'static [Profile] = &[Profile::Nominal, Profile::Mrc];

    /// The live contract for `(class, self)`.
    #[must_use]
    pub fn contract(self, class: VehicleClass) -> VehicleKinematicsContract {
        match self {
            Profile::Nominal => contract_for(class),
            Profile::Mrc => mrc_fallback_for(class),
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Nominal => "nominal",
            Profile::Mrc => "mrc",
        }
    }
}

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

/// A machine-checkable precondition that justifies a value.
///
/// Distinct from a free-text rationale because it is *executed*. The three MRC
/// `odd_speed_cap_mps: None` values are neither inherited nor unattributed:
/// their justification is an inequality, and an inequality is better provenance
/// than the prose it replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreconditionRule {
    /// Leaving the MRC cap `None` is safe because the MRC crawl already sits at
    /// or below the class's ODD cap, so `min()` selects the crawl either way.
    ///
    /// If that stopped holding, `None` would silently raise the MRC ceiling
    /// ABOVE the class's own ODD cap — more permissive in degraded posture than
    /// in nominal, in the only sense that reaches the wheels.
    MrcCrawlAtOrBelowClassOddCap,
    /// The contract carries no cap because the cap is applied OUTSIDE it.
    ///
    /// The robotaxi nominal case, and the reason this variant exists: its
    /// `max_speed_mps` (35.0) is far ABOVE the class ODD cap (22.35), so the
    /// crawl-below-cap rule emphatically does NOT justify its `None` — the
    /// deployment applying the cap elsewhere does. Requires a registered
    /// [`ExternalConstraint`] that participates in the effective ceiling and
    /// actually binds.
    ///
    /// Discovered by this checker rejecting the wrong classification on the
    /// first run, which is the argument for executing a precondition rather
    /// than writing it down.
    CapAppliedExternally,
}

/// How a value came to be what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "SCREAMING-KEBAB-CASE")]
pub enum Provenance {
    /// Observed directly. `source` cites the measurement.
    Measured { source: &'static str },
    /// Calculated or approximated. `basis` states from what.
    Estimate { basis: &'static str },
    /// Asserted, awaiting a named validation. `owed` states which.
    ValidationPending { owed: &'static str },
    /// Sourced from another authoritative key. `upstream` MUST resolve, and the
    /// values MUST compare equal — see [`check_inheritance`].
    Inherited { upstream: &'static str },
    /// Justified by a machine-checked precondition — see [`check_preconditions`].
    DerivedPrecondition { rule: PreconditionRule },
    /// No provenance. Present so an unattributed field is REPRESENTABLE and
    /// therefore visible, rather than silently absent from the register.
    Unattributed,
}

impl Provenance {
    /// The stable classification token, for the serialized manifest and for
    /// operators reading a rendered register.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Provenance::Measured { .. } => "MEASURED",
            Provenance::Estimate { .. } => "ESTIMATE",
            Provenance::ValidationPending { .. } => "VALIDATION-PENDING",
            Provenance::Inherited { .. } => "INHERITED",
            Provenance::DerivedPrecondition { .. } => "DERIVED-PRECONDITION",
            Provenance::Unattributed => "UNATTRIBUTED",
        }
    }

    /// Whether the value has been validated against reality. Deliberately
    /// conservative: only a measurement counts, and an inherited value defers to
    /// its upstream rather than claiming validation of its own.
    #[must_use]
    pub fn is_validated(self) -> bool {
        matches!(self, Provenance::Measured { .. })
    }
}

/// One provenance record. Governs one or more FIELDS of one (class, profile).
///
/// Records name fields, never values. See the module docs.
#[derive(Debug, Clone, Copy)]
pub struct FieldProvenance {
    /// Stable key, e.g. `"r2.footprint"` or `"courier.mrc.brake"`. Unique.
    pub key: &'static str,
    pub class: VehicleClass,
    pub profile: Profile,
    /// The fields this record governs. Coarse where provenance is shared, fine
    /// where it differs.
    pub fields: &'static [Field],
    pub provenance: Provenance,
}

/// A limit applied somewhere OTHER than the contract struct.
///
/// The robotaxi case is why this type exists. `contract_for(Robotaxi)` returns
/// the frozen instance with `odd_speed_cap_mps: None`, so the serialized
/// contract derives an effective ceiling of 35.0 m/s while the deployment
/// applies 22.35 elsewhere. A register that serialized only the contract would
/// report the wrong ceiling — and "this class's cap is applied somewhere other
/// than the contract" is precisely the split authority a manifest exists to
/// expose.
#[derive(Debug, Clone, Copy)]
pub struct ExternalConstraint {
    pub id: &'static str,
    pub class: VehicleClass,
    pub profile: Profile,
    /// The contract field this constraint bounds.
    pub constrains: Field,
    /// Where the constraint is applied.
    pub applied_at: &'static str,
    /// The configuration or artifact that supplies it.
    pub supplied_by: &'static str,
    /// Whether it participates in the derived effective ceiling.
    pub participates_in_effective_ceiling: bool,
    pub provenance: Provenance,
    /// A REFERENCE to the canonical constant, never a re-typed literal.
    pub value_mps: f64,
}

// ---------------------------------------------------------------------------
// The authored table — courier, delivery-av, r2
// ---------------------------------------------------------------------------

macro_rules! rec {
    ($key:literal, $class:expr, $profile:expr, [$($f:expr),+ $(,)?], $prov:expr) => {
        FieldProvenance {
            key: $key,
            class: $class,
            profile: $profile,
            fields: &[$($f),+],
            provenance: $prov,
        }
    };
}

/// Shorthand for the most common classification. A macro rather than a `const
/// fn` pointer because statics may not call function pointers.
macro_rules! vp {
    ($owed:literal) => {
        Provenance::ValidationPending { owed: $owed }
    };
}

/// Provenance for the three authored classes. Robotaxi lives in
/// [`ROBOTAXI_SIDECAR`] because its numbers are byte-frozen.
pub static AUTHORED_PROVENANCE: &[FieldProvenance] = &[
    // --- courier nominal ---
    rec!("courier.max_speed", VehicleClass::Courier, Profile::Nominal, [Field::MaxSpeed],
         Provenance::ValidationPending { owed: "sidewalk operating-band survey; no bench measurement" }),
    rec!("courier.accel", VehicleClass::Courier, Profile::Nominal, [Field::MaxAccel],
         Provenance::ValidationPending { owed: "pedestrian-proximity comfort study" }),
    rec!("courier.brake", VehicleClass::Courier, Profile::Nominal, [Field::MaxBrake],
         Provenance::ValidationPending { owed: "service-brake deceleration measurement" }),
    rec!("courier.steering", VehicleClass::Courier, Profile::Nominal, [Field::MaxSteering],
         Provenance::ValidationPending { owed: "rack-limit measurement" }),
    rec!("courier.steering_rate", VehicleClass::Courier, Profile::Nominal, [Field::MaxSteeringRate],
         Provenance::ValidationPending { owed: "steering slew-rate measurement" }),
    rec!("courier.follow", VehicleClass::Courier, Profile::Nominal, [Field::MinFollow],
         Provenance::ValidationPending { owed: "headway derivation review" }),
    rec!("courier.lat_accel", VehicleClass::Courier, Profile::Nominal, [Field::MaxLateralAccel],
         Provenance::ValidationPending { owed: "lateral comfort / traction measurement" }),
    rec!("courier.wheelbase", VehicleClass::Courier, Profile::Nominal, [Field::Wheelbase],
         Provenance::ValidationPending { owed: "platform geometry measurement" }),
    rec!("courier.footprint", VehicleClass::Courier, Profile::Nominal,
         [Field::Width, Field::Length, Field::OverhangFront, Field::OverhangRear],
         Provenance::ValidationPending { owed: "platform geometry measurement" }),
    rec!("courier.odd_cap", VehicleClass::Courier, Profile::Nominal, [Field::OddSpeedCap],
         Provenance::ValidationPending { owed: "ODD ceiling sign-off (docs/CONTRACT_PROFILES.md courier.odd_cap)" }),
    // --- courier mrc ---
    rec!("courier.mrc.max_speed", VehicleClass::Courier, Profile::Mrc, [Field::MaxSpeed],
         Provenance::ValidationPending { owed: "degraded-crawl sign-off" }),
    rec!("courier.mrc.accel", VehicleClass::Courier, Profile::Mrc, [Field::MaxAccel], vp!("degraded-envelope sign-off")),
    rec!("courier.mrc.brake", VehicleClass::Courier, Profile::Mrc, [Field::MaxBrake], vp!("degraded-envelope sign-off")),
    rec!("courier.mrc.steering", VehicleClass::Courier, Profile::Mrc, [Field::MaxSteering], vp!("degraded-envelope sign-off")),
    rec!("courier.mrc.steering_rate", VehicleClass::Courier, Profile::Mrc, [Field::MaxSteeringRate], vp!("degraded-envelope sign-off")),
    rec!("courier.mrc.follow", VehicleClass::Courier, Profile::Mrc, [Field::MinFollow], vp!("degraded-envelope sign-off")),
    rec!("courier.mrc.lat_accel", VehicleClass::Courier, Profile::Mrc, [Field::MaxLateralAccel], vp!("degraded-envelope sign-off")),
    rec!("courier.mrc.wheelbase", VehicleClass::Courier, Profile::Mrc, [Field::Wheelbase],
         Provenance::Inherited { upstream: "courier.wheelbase" }),
    rec!("courier.mrc.footprint", VehicleClass::Courier, Profile::Mrc,
         [Field::Width, Field::Length, Field::OverhangFront, Field::OverhangRear],
         Provenance::Inherited { upstream: "courier.footprint" }),
    rec!("courier.mrc.odd_cap", VehicleClass::Courier, Profile::Mrc, [Field::OddSpeedCap],
         Provenance::DerivedPrecondition { rule: PreconditionRule::MrcCrawlAtOrBelowClassOddCap }),

    // --- delivery-av nominal ---
    rec!("delivery-av.max_speed", VehicleClass::DeliveryAv, Profile::Nominal, [Field::MaxSpeed],
         vp!("road-pod mechanical maximum measurement")),
    rec!("delivery-av.accel", VehicleClass::DeliveryAv, Profile::Nominal, [Field::MaxAccel],
         vp!("acceleration measurement; currently interpolated between courier and robotaxi")),
    rec!("delivery-av.brake", VehicleClass::DeliveryAv, Profile::Nominal, [Field::MaxBrake],
         vp!("service-brake deceleration measurement")),
    rec!("delivery-av.steering", VehicleClass::DeliveryAv, Profile::Nominal, [Field::MaxSteering],
         vp!("rack-limit measurement")),
    rec!("delivery-av.steering_rate", VehicleClass::DeliveryAv, Profile::Nominal, [Field::MaxSteeringRate],
         vp!("steering slew-rate measurement")),
    rec!("delivery-av.follow", VehicleClass::DeliveryAv, Profile::Nominal, [Field::MinFollow],
         vp!("headway derivation review")),
    rec!("delivery-av.lat_accel", VehicleClass::DeliveryAv, Profile::Nominal, [Field::MaxLateralAccel],
         vp!("lateral comfort / traction measurement")),
    rec!("delivery-av.wheelbase", VehicleClass::DeliveryAv, Profile::Nominal, [Field::Wheelbase],
         vp!("platform geometry measurement")),
    // The record whose ABSENCE was the #1219 finding: the source comment tagged
    // `width_m` as a trailing comment, leaving length + both overhangs with no
    // provenance at all. Coverage is field-level precisely so this cannot recur.
    rec!("delivery-av.footprint", VehicleClass::DeliveryAv, Profile::Nominal,
         [Field::Width, Field::Length, Field::OverhangFront, Field::OverhangRear],
         vp!("platform geometry measurement")),
    rec!("delivery-av.odd_cap", VehicleClass::DeliveryAv, Profile::Nominal, [Field::OddSpeedCap],
         vp!("ODD ceiling sign-off (docs/CONTRACT_PROFILES.md delivery-av.odd_cap)")),
    // --- delivery-av mrc ---
    rec!("delivery-av.mrc.max_speed", VehicleClass::DeliveryAv, Profile::Mrc, [Field::MaxSpeed], vp!("degraded-envelope sign-off")),
    rec!("delivery-av.mrc.accel", VehicleClass::DeliveryAv, Profile::Mrc, [Field::MaxAccel], vp!("degraded-envelope sign-off")),
    rec!("delivery-av.mrc.brake", VehicleClass::DeliveryAv, Profile::Mrc, [Field::MaxBrake], vp!("degraded-envelope sign-off")),
    rec!("delivery-av.mrc.steering", VehicleClass::DeliveryAv, Profile::Mrc, [Field::MaxSteering], vp!("degraded-envelope sign-off")),
    rec!("delivery-av.mrc.steering_rate", VehicleClass::DeliveryAv, Profile::Mrc, [Field::MaxSteeringRate], vp!("degraded-envelope sign-off")),
    rec!("delivery-av.mrc.follow", VehicleClass::DeliveryAv, Profile::Mrc, [Field::MinFollow], vp!("degraded-envelope sign-off")),
    rec!("delivery-av.mrc.lat_accel", VehicleClass::DeliveryAv, Profile::Mrc, [Field::MaxLateralAccel], vp!("degraded-envelope sign-off")),
    rec!("delivery-av.mrc.wheelbase", VehicleClass::DeliveryAv, Profile::Mrc, [Field::Wheelbase],
         Provenance::Inherited { upstream: "delivery-av.wheelbase" }),
    rec!("delivery-av.mrc.footprint", VehicleClass::DeliveryAv, Profile::Mrc,
         [Field::Width, Field::Length, Field::OverhangFront, Field::OverhangRear],
         Provenance::Inherited { upstream: "delivery-av.footprint" }),
    rec!("delivery-av.mrc.odd_cap", VehicleClass::DeliveryAv, Profile::Mrc, [Field::OddSpeedCap],
         Provenance::DerivedPrecondition { rule: PreconditionRule::MrcCrawlAtOrBelowClassOddCap }),

    // --- r2 nominal ---
    rec!("r2.max_speed", VehicleClass::R2, Profile::Nominal, [Field::MaxSpeed],
         vp!("bench max-speed run (one of the 4 remaining PLATFORM_R2_PENDING items)")),
    rec!("r2.accel", VehicleClass::R2, Profile::Nominal, [Field::MaxAccel], vp!("bench acceleration run")),
    rec!("r2.brake", VehicleClass::R2, Profile::Nominal, [Field::MaxBrake], vp!("bench braking run")),
    rec!("r2.steering", VehicleClass::R2, Profile::Nominal, [Field::MaxSteering],
         Provenance::Measured {
             source: "protractor sweep at command ±45; robot/r2_drive_calibration_results.txt Phase C 2026-07-17",
         }),
    rec!("r2.steering_rate", VehicleClass::R2, Profile::Nominal, [Field::MaxSteeringRate],
         vp!("servo slew rate — time a full −45→+45 sweep (PLATFORM_R2_PENDING; also gates #1213)")),
    rec!("r2.follow", VehicleClass::R2, Profile::Nominal, [Field::MinFollow], vp!("headway derivation review")),
    rec!("r2.lat_accel", VehicleClass::R2, Profile::Nominal, [Field::MaxLateralAccel], vp!("lateral comfort measurement")),
    rec!("r2.wheelbase", VehicleClass::R2, Profile::Nominal, [Field::Wheelbase],
         Provenance::Measured { source: "bench tape, front-to-rear ~9 in (CONFIRM still owed); KIRRA_R2_WHEELBASE_M" }),
    // Split from the overhangs BECAUSE the provenance differs — the body was
    // measured, the overhang split was not. A uniform key granularity would
    // erase exactly this distinction.
    rec!("r2.footprint", VehicleClass::R2, Profile::Nominal, [Field::Width, Field::Length],
         Provenance::Measured { source: "bench tape: width 8 in = 0.203 m, length 13 in = 0.330 m" }),
    rec!("r2.overhangs", VehicleClass::R2, Profile::Nominal, [Field::OverhangFront, Field::OverhangRear],
         Provenance::Estimate {
             basis: "(length − wheelbase)/2 assumed split evenly; UNMEASURED. Feeds the swept-footprint \
                     corner placement in containment.rs — an under-estimated rear overhang under-covers the body. \
                     Rounding this estimate to 0.05 left the parts 1 mm short of the measured length, placing the \
                     swept body inside the real one (#1215)",
         }),
    rec!("r2.odd_cap", VehicleClass::R2, Profile::Nominal, [Field::OddSpeedCap],
         vp!("ODD ceiling sign-off (docs/CONTRACT_PROFILES.md r2.odd_cap)")),
    // --- r2 mrc ---
    rec!("r2.mrc.max_speed", VehicleClass::R2, Profile::Mrc, [Field::MaxSpeed], vp!("degraded-envelope sign-off")),
    rec!("r2.mrc.accel", VehicleClass::R2, Profile::Mrc, [Field::MaxAccel], vp!("degraded-envelope sign-off")),
    rec!("r2.mrc.brake", VehicleClass::R2, Profile::Mrc, [Field::MaxBrake], vp!("degraded-envelope sign-off")),
    rec!("r2.mrc.steering", VehicleClass::R2, Profile::Mrc, [Field::MaxSteering], vp!("degraded-envelope sign-off")),
    rec!("r2.mrc.steering_rate", VehicleClass::R2, Profile::Mrc, [Field::MaxSteeringRate], vp!("degraded-envelope sign-off")),
    rec!("r2.mrc.follow", VehicleClass::R2, Profile::Mrc, [Field::MinFollow], vp!("degraded-envelope sign-off")),
    rec!("r2.mrc.lat_accel", VehicleClass::R2, Profile::Mrc, [Field::MaxLateralAccel], vp!("degraded-envelope sign-off")),
    rec!("r2.mrc.wheelbase", VehicleClass::R2, Profile::Mrc, [Field::Wheelbase],
         Provenance::Inherited { upstream: "r2.wheelbase" }),
    rec!("r2.mrc.footprint", VehicleClass::R2, Profile::Mrc, [Field::Width, Field::Length],
         Provenance::Inherited { upstream: "r2.footprint" }),
    rec!("r2.mrc.overhangs", VehicleClass::R2, Profile::Mrc, [Field::OverhangFront, Field::OverhangRear],
         Provenance::Inherited { upstream: "r2.overhangs" }),
    rec!("r2.mrc.odd_cap", VehicleClass::R2, Profile::Mrc, [Field::OddSpeedCap],
         Provenance::DerivedPrecondition { rule: PreconditionRule::MrcCrawlAtOrBelowClassOddCap }),
];

/// Robotaxi provenance. Valid ONLY against the reviewed talisman blob pin — see
/// [`validate_robotaxi_sidecar`] and the module docs.
pub static ROBOTAXI_SIDECAR: &[FieldProvenance] = &[
    rec!(
        "robotaxi.max_speed",
        VehicleClass::Robotaxi,
        Profile::Nominal,
        [Field::MaxSpeed],
        vp!("frozen reference-deployment value; validation predates this register")
    ),
    rec!(
        "robotaxi.accel",
        VehicleClass::Robotaxi,
        Profile::Nominal,
        [Field::MaxAccel],
        vp!("frozen reference-deployment value")
    ),
    rec!(
        "robotaxi.brake",
        VehicleClass::Robotaxi,
        Profile::Nominal,
        [Field::MaxBrake],
        vp!("frozen reference-deployment value")
    ),
    rec!(
        "robotaxi.steering",
        VehicleClass::Robotaxi,
        Profile::Nominal,
        [Field::MaxSteering],
        vp!("frozen reference-deployment value")
    ),
    rec!(
        "robotaxi.steering_rate",
        VehicleClass::Robotaxi,
        Profile::Nominal,
        [Field::MaxSteeringRate],
        vp!("frozen reference-deployment value")
    ),
    rec!(
        "robotaxi.follow",
        VehicleClass::Robotaxi,
        Profile::Nominal,
        [Field::MinFollow],
        vp!("frozen reference-deployment value")
    ),
    rec!(
        "robotaxi.lat_accel",
        VehicleClass::Robotaxi,
        Profile::Nominal,
        [Field::MaxLateralAccel],
        vp!("frozen reference-deployment value")
    ),
    rec!(
        "robotaxi.wheelbase",
        VehicleClass::Robotaxi,
        Profile::Nominal,
        [Field::Wheelbase],
        vp!("frozen reference-deployment value")
    ),
    rec!(
        "robotaxi.footprint",
        VehicleClass::Robotaxi,
        Profile::Nominal,
        [
            Field::Width,
            Field::Length,
            Field::OverhangFront,
            Field::OverhangRear
        ],
        vp!("frozen reference-deployment value")
    ),
    // The frozen instance carries `None` here ON PURPOSE — the deployment
    // applies the ODD cap externally. That is why ROBOTAXI_ODD_CAP exists as an
    // ExternalConstraint rather than as contract provenance.
    rec!(
        "robotaxi.odd_cap",
        VehicleClass::Robotaxi,
        Profile::Nominal,
        [Field::OddSpeedCap],
        Provenance::DerivedPrecondition {
            rule: PreconditionRule::CapAppliedExternally
        }
    ),
    rec!(
        "robotaxi.mrc.max_speed",
        VehicleClass::Robotaxi,
        Profile::Mrc,
        [Field::MaxSpeed],
        vp!("frozen MRC value")
    ),
    rec!(
        "robotaxi.mrc.accel",
        VehicleClass::Robotaxi,
        Profile::Mrc,
        [Field::MaxAccel],
        vp!("frozen MRC value")
    ),
    rec!(
        "robotaxi.mrc.brake",
        VehicleClass::Robotaxi,
        Profile::Mrc,
        [Field::MaxBrake],
        vp!("frozen MRC value")
    ),
    rec!(
        "robotaxi.mrc.steering",
        VehicleClass::Robotaxi,
        Profile::Mrc,
        [Field::MaxSteering],
        vp!("frozen MRC value")
    ),
    rec!(
        "robotaxi.mrc.steering_rate",
        VehicleClass::Robotaxi,
        Profile::Mrc,
        [Field::MaxSteeringRate],
        vp!("frozen MRC value")
    ),
    rec!(
        "robotaxi.mrc.follow",
        VehicleClass::Robotaxi,
        Profile::Mrc,
        [Field::MinFollow],
        vp!("frozen MRC value")
    ),
    rec!(
        "robotaxi.mrc.lat_accel",
        VehicleClass::Robotaxi,
        Profile::Mrc,
        [Field::MaxLateralAccel],
        vp!("frozen MRC value")
    ),
    rec!(
        "robotaxi.mrc.wheelbase",
        VehicleClass::Robotaxi,
        Profile::Mrc,
        [Field::Wheelbase],
        Provenance::Inherited {
            upstream: "robotaxi.wheelbase"
        }
    ),
    rec!(
        "robotaxi.mrc.footprint",
        VehicleClass::Robotaxi,
        Profile::Mrc,
        [
            Field::Width,
            Field::Length,
            Field::OverhangFront,
            Field::OverhangRear
        ],
        Provenance::Inherited {
            upstream: "robotaxi.footprint"
        }
    ),
    rec!(
        "robotaxi.mrc.odd_cap",
        VehicleClass::Robotaxi,
        Profile::Mrc,
        [Field::OddSpeedCap],
        Provenance::DerivedPrecondition {
            rule: PreconditionRule::MrcCrawlAtOrBelowClassOddCap
        }
    ),
];

/// Constraints applied OUTSIDE the contract struct.
///
/// Only the robotaxi ODD cap qualifies today: courier / delivery-av / r2 carry
/// their caps inside `odd_speed_cap_mps`, so their effective ceiling is derivable
/// from the serialized contract alone. Robotaxi's is not.
pub static EXTERNAL_CONSTRAINTS: &[ExternalConstraint] = &[ExternalConstraint {
    id: "robotaxi.external.odd_cap",
    class: VehicleClass::Robotaxi,
    profile: Profile::Nominal,
    constrains: Field::MaxSpeed,
    applied_at: "the deployment, not the contract — the frozen instance's odd_speed_cap_mps is None by design",
    supplied_by: "ROBOTAXI_ODD_SPEED_CAP_MPS (= URBAN_ODD_SPEED_CAP_MPS, the frozen talisman constant); ADR-0001 / SPEED_ENVELOPE.md / KIRRA-OCCY-SPEED-VAL-001",
    participates_in_effective_ceiling: true,
    provenance: Provenance::ValidationPending {
        owed: "carried from ADR-0001's RSS stopping-distance chain; re-validated unchanged in OCCY_SPEED_CAP_VALIDATION.md",
    },
    value_mps: ROBOTAXI_ODD_SPEED_CAP_MPS,
}];

/// Every provenance record, authored plus sidecar.
#[must_use]
pub fn all_records() -> Vec<FieldProvenance> {
    AUTHORED_PROVENANCE
        .iter()
        .chain(ROBOTAXI_SIDECAR.iter())
        .copied()
        .collect()
}

// ---------------------------------------------------------------------------
// Checks
// ---------------------------------------------------------------------------

/// A coverage defect. Every variant names the exact triple at fault so the
/// failure is actionable rather than a bare count mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverageError {
    /// No record governs this field. THE defect #1219 found in the wild.
    Uncovered {
        class: &'static str,
        profile: &'static str,
        field: &'static str,
    },
    /// Two or more records govern it — provenance would be ambiguous.
    DoubleCovered {
        class: &'static str,
        profile: &'static str,
        field: &'static str,
        keys: Vec<&'static str>,
    },
    /// Two records share a key.
    DuplicateKey { key: &'static str },
}

/// Assert total field-level coverage over `classes`.
///
/// The completeness property, stated as code: every (class, profile, field)
/// triple is governed by **exactly one** record. Not "every key resolves" —
/// that direction only proves the table invented no keys, and says nothing
/// about fields no record mentions, which is where the real defect lives.
pub fn check_coverage(
    records: &[FieldProvenance],
    classes: &[VehicleClass],
) -> Result<(), Vec<CoverageError>> {
    let mut errors = Vec::new();

    let mut seen_keys: Vec<&'static str> = Vec::new();
    for r in records {
        if seen_keys.contains(&r.key) {
            errors.push(CoverageError::DuplicateKey { key: r.key });
        }
        seen_keys.push(r.key);
    }

    for &class in classes {
        for &profile in Profile::ALL {
            for &field in Field::ALL {
                let owners: Vec<&'static str> = records
                    .iter()
                    .filter(|r| {
                        r.class == class && r.profile == profile && r.fields.contains(&field)
                    })
                    .map(|r| r.key)
                    .collect();
                match owners.len() {
                    1 => {}
                    0 => errors.push(CoverageError::Uncovered {
                        class: class.as_str(),
                        profile: profile.as_str(),
                        field: field.name(),
                    }),
                    _ => errors.push(CoverageError::DoubleCovered {
                        class: class.as_str(),
                        profile: profile.as_str(),
                        field: field.name(),
                        keys: owners,
                    }),
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// An inheritance defect.
#[derive(Debug, Clone, PartialEq)]
pub enum InheritanceError {
    /// The named upstream key does not exist — an unresolvable claim.
    UpstreamMissing {
        key: &'static str,
        upstream: &'static str,
    },
    /// Upstream belongs to a different class. Inheritance is within a class;
    /// crossing one would import another platform's geometry.
    UpstreamWrongClass {
        key: &'static str,
        upstream: &'static str,
    },
    /// Upstream does not govern a field the downstream record claims to inherit.
    UpstreamMissingField {
        key: &'static str,
        upstream: &'static str,
        field: &'static str,
    },
    /// The values differ. The claim "IDENTICAL to nominal" is false.
    ValueDiverged {
        key: &'static str,
        upstream: &'static str,
        field: &'static str,
        downstream: FieldValue,
        upstream_value: FieldValue,
    },
}

/// Assert every `INHERITED` record resolves to its upstream AND equals it.
///
/// This is the check that makes `INHERITED` more than a classification. Values
/// are compared bit-for-bit (`f64::to_bits`) rather than with a tolerance: the
/// claim is that the MRC profile carries the SAME value, and "same" admits no
/// epsilon.
pub fn check_inheritance(records: &[FieldProvenance]) -> Result<usize, Vec<InheritanceError>> {
    let mut errors = Vec::new();
    let mut verified = 0usize;

    for r in records {
        let Provenance::Inherited { upstream } = r.provenance else {
            continue;
        };
        let Some(up) = records.iter().find(|c| c.key == upstream) else {
            errors.push(InheritanceError::UpstreamMissing {
                key: r.key,
                upstream,
            });
            continue;
        };
        if up.class != r.class {
            errors.push(InheritanceError::UpstreamWrongClass {
                key: r.key,
                upstream,
            });
            continue;
        }

        let down_contract = r.profile.contract(r.class);
        let up_contract = up.profile.contract(up.class);

        for &field in r.fields {
            if !up.fields.contains(&field) {
                errors.push(InheritanceError::UpstreamMissingField {
                    key: r.key,
                    upstream,
                    field: field.name(),
                });
                continue;
            }
            let d = field.read(&down_contract);
            let u = field.read(&up_contract);
            if !field_values_identical(d, u) {
                errors.push(InheritanceError::ValueDiverged {
                    key: r.key,
                    upstream,
                    field: field.name(),
                    downstream: d,
                    upstream_value: u,
                });
            } else {
                verified += 1;
            }
        }
    }

    if errors.is_empty() {
        Ok(verified)
    } else {
        Err(errors)
    }
}

/// Bit-identical comparison. `to_bits` rather than `==` so the check is exact
/// and total (no NaN surprise, no epsilon creep).
fn field_values_identical(a: FieldValue, b: FieldValue) -> bool {
    match (a, b) {
        (FieldValue::Scalar(x), FieldValue::Scalar(y)) => x.to_bits() == y.to_bits(),
        (FieldValue::OptionalCap(None), FieldValue::OptionalCap(None)) => true,
        (FieldValue::OptionalCap(Some(x)), FieldValue::OptionalCap(Some(y))) => {
            x.to_bits() == y.to_bits()
        }
        _ => false,
    }
}

/// A precondition defect: a justifying inequality that no longer holds.
#[derive(Debug, Clone, PartialEq)]
pub struct PreconditionError {
    pub key: &'static str,
    pub rule: PreconditionRule,
    pub detail: String,
}

/// The class ODD cap constant, by class. A REFERENCE table, not new numbers.
#[must_use]
pub fn class_odd_cap_mps(class: VehicleClass) -> f64 {
    match class {
        VehicleClass::Courier => COURIER_ODD_SPEED_CAP_MPS,
        VehicleClass::DeliveryAv => DELIVERY_AV_ODD_SPEED_CAP_MPS,
        VehicleClass::Robotaxi => ROBOTAXI_ODD_SPEED_CAP_MPS,
        VehicleClass::R2 => R2_ODD_SPEED_CAP_MPS,
    }
}

/// Execute every `DERIVED-PRECONDITION` record's rule.
pub fn check_preconditions(records: &[FieldProvenance]) -> Result<usize, Vec<PreconditionError>> {
    let mut errors = Vec::new();
    let mut checked = 0usize;

    for r in records {
        let Provenance::DerivedPrecondition { rule } = r.provenance else {
            continue;
        };
        match rule {
            PreconditionRule::MrcCrawlAtOrBelowClassOddCap => {
                let contract = r.profile.contract(r.class);
                // The rule only justifies a cap that is actually absent. A
                // record claiming it on a profile that HAS a cap is a
                // mis-classification, not a satisfied precondition.
                if contract.odd_speed_cap_mps.is_some() {
                    errors.push(PreconditionError {
                        key: r.key,
                        rule,
                        detail: format!(
                            "{} carries an explicit ODD cap; the None-is-safe rule does not apply",
                            r.key
                        ),
                    });
                    continue;
                }
                let cap = class_odd_cap_mps(r.class);
                if contract.max_speed_mps > cap {
                    errors.push(PreconditionError {
                        key: r.key,
                        rule,
                        detail: format!(
                            "max_speed {} EXCEEDS the class ODD cap {cap}; leaving the cap None \
                             raises this profile's enforced ceiling above the class ceiling",
                            contract.max_speed_mps
                        ),
                    });
                } else {
                    checked += 1;
                }
            }
            PreconditionRule::CapAppliedExternally => {
                let contract = r.profile.contract(r.class);
                if contract.odd_speed_cap_mps.is_some() {
                    errors.push(PreconditionError {
                        key: r.key,
                        rule,
                        detail: format!(
                            "{} carries an explicit ODD cap; the applied-externally rule does not apply",
                            r.key
                        ),
                    });
                    continue;
                }
                // The claim is only honest if an external constraint actually
                // exists AND actually binds. Otherwise "applied externally" is
                // an assertion that nothing enforces the cap at all.
                let external = EXTERNAL_CONSTRAINTS.iter().find(|c| {
                    c.class == r.class
                        && c.profile == r.profile
                        && c.constrains == Field::MaxSpeed
                        && c.participates_in_effective_ceiling
                });
                match external {
                    None => errors.push(PreconditionError {
                        key: r.key,
                        rule,
                        detail: format!(
                            "{} claims its cap is applied externally, but no participating                              ExternalConstraint is registered — nothing caps this profile",
                            r.key
                        ),
                    }),
                    Some(c) if c.value_mps >= contract.max_speed_mps => {
                        errors.push(PreconditionError {
                            key: r.key,
                            rule,
                            detail: format!(
                                "the external constraint {} ({}) does not bind below max_speed {}",
                                c.id, c.value_mps, contract.max_speed_mps
                            ),
                        });
                    }
                    Some(_) => checked += 1,
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(checked)
    } else {
        Err(errors)
    }
}

/// The enforced speed ceiling for `(class, profile)`, composing the contract's
/// own `effective_max_speed_mps()` with any participating [`ExternalConstraint`].
///
/// This is the function that makes robotaxi honest. `contract_for(Robotaxi)`
/// alone derives 35.0 m/s, because the frozen instance's cap is `None` and the
/// deployment applies 22.35 elsewhere. Serializing the contract without this
/// composition would publish a ceiling 12.65 m/s above the one actually enforced.
#[must_use]
pub fn effective_ceiling_mps(class: VehicleClass, profile: Profile) -> f64 {
    let mut ceiling = profile.contract(class).effective_max_speed_mps();
    for c in EXTERNAL_CONSTRAINTS {
        if c.class == class
            && c.profile == profile
            && c.participates_in_effective_ceiling
            && c.value_mps < ceiling
        {
            ceiling = c.value_mps;
        }
    }
    ceiling
}

// ---------------------------------------------------------------------------
// The frozen sidecar's hash binding
// ---------------------------------------------------------------------------

/// Why a sidecar is not usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarError {
    /// The pin document holds no hash for the talisman.
    PinNotFound { doc: &'static str },
    /// `git hash-object` could not be run. FAIL-CLOSED: an integrity gate that
    /// skips when its tool is missing is worse than no gate, because it reports
    /// success.
    HashUnavailable { detail: String },
    /// The talisman changed without a reviewed re-pin. The sidecar describes
    /// numbers that may no longer be there.
    PinMismatch { pinned: String, actual: String },
}

/// Read the reviewed blob pin out of [`TALISMAN_PIN_DOC`].
///
/// Parses the same `<path> = <40 hex>` shape `ci/build_safety_case.py` greps.
/// No regex dependency; the shape is fixed and simple.
fn read_pin(repo_root: &Path) -> Result<String, SidecarError> {
    let doc = std::fs::read_to_string(repo_root.join(TALISMAN_PIN_DOC)).map_err(|_| {
        SidecarError::PinNotFound {
            doc: TALISMAN_PIN_DOC,
        }
    })?;
    for line in doc.lines() {
        let Some(after_path) = line.split_once(TALISMAN_PATH) else {
            continue;
        };
        let rest = after_path.1;
        let hex: String = rest
            .chars()
            .skip_while(|c| !c.is_ascii_hexdigit())
            .take_while(char::is_ascii_hexdigit)
            .collect();
        if hex.len() == 40 {
            return Ok(hex);
        }
    }
    Err(SidecarError::PinNotFound {
        doc: TALISMAN_PIN_DOC,
    })
}

/// Validate the frozen sidecar against the EXISTING talisman pin.
///
/// Two conditions, both required:
///
/// 1. `git hash-object <talisman>` equals the hash recorded in the reviewed pin
///    doc — the same check `ci/build_safety_case.py::talisman_gate` and the
///    `ci.yml` rustfmt lane perform. Reused, not reimplemented.
/// 2. The sidecar's records cover every field of every profile for its class —
///    enforced by the caller via [`check_coverage`], so a sidecar cannot be
///    partially populated and look complete.
///
/// If the blob changes, this fails and the provenance association is refused
/// until the sidecar is deliberately revisited. That is the point: the sidecar
/// DESCRIBES the frozen values, and a description of numbers that changed is
/// worse than no description.
pub fn validate_robotaxi_sidecar(repo_root: &Path) -> Result<String, SidecarError> {
    let pinned = read_pin(repo_root)?;

    let out = Command::new("git")
        .args(["hash-object", TALISMAN_PATH])
        .current_dir(repo_root)
        .output()
        .map_err(|e| SidecarError::HashUnavailable {
            detail: format!("could not run `git hash-object`: {e}"),
        })?;
    if !out.status.success() {
        return Err(SidecarError::HashUnavailable {
            detail: format!(
                "`git hash-object` failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        });
    }
    let actual = String::from_utf8_lossy(&out.stdout).trim().to_string();

    if actual != pinned {
        return Err(SidecarError::PinMismatch { pinned, actual });
    }
    Ok(actual)
}

// ---------------------------------------------------------------------------
// The serialized manifest
// ---------------------------------------------------------------------------

/// One serialized field row: the live value plus its provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestField {
    pub field: Field,
    pub name: String,
    pub value: FieldValue,
    pub provenance_key: String,
    pub provenance_class: String,
    pub provenance_detail: String,
    pub upstream: Option<String>,
    pub validated: bool,
}

/// One serialized (class, profile) profile row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestProfile {
    pub class: String,
    pub profile: String,
    /// The composed ceiling, INCLUDING external constraints. See
    /// [`effective_ceiling_mps`].
    pub effective_ceiling_mps: f64,
    /// The ceiling derivable from the contract alone. Differs from the above
    /// exactly when an external constraint participates — which is the split
    /// authority the manifest exists to expose.
    pub contract_only_ceiling_mps: f64,
    pub fields: Vec<ManifestField>,
}

/// One serialized external-constraint row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestExternalConstraint {
    pub id: String,
    pub class: String,
    pub profile: String,
    pub constrains: String,
    pub applied_at: String,
    pub supplied_by: String,
    pub participates_in_effective_ceiling: bool,
    pub provenance_class: String,
    pub value_mps: f64,
}

/// The serialized provenance manifest — a DERIVED view. Every number in it was
/// read from a live contract or a canonical constant at render time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceManifest {
    pub manifest_version: u32,
    /// The talisman blob hash this render was validated against, when the
    /// robotaxi sidecar was included.
    pub talisman_blob: Option<String>,
    pub profiles: Vec<ManifestProfile>,
    pub external_constraints: Vec<ManifestExternalConstraint>,
}

/// The manifest schema version. Bump on a breaking shape change.
pub const MANIFEST_VERSION: u32 = 1;

fn provenance_detail(p: Provenance) -> String {
    match p {
        Provenance::Measured { source } => source.to_string(),
        Provenance::Estimate { basis } => basis.to_string(),
        Provenance::ValidationPending { owed } => owed.to_string(),
        Provenance::Inherited { upstream } => format!("inherited from {upstream}"),
        Provenance::DerivedPrecondition { rule } => format!("{rule:?}"),
        Provenance::Unattributed => String::new(),
    }
}

/// Render the manifest for `classes` from `records`.
///
/// Fails if coverage is incomplete: a manifest with a missing row would present
/// as authoritative while being silently partial, which is the exact failure the
/// completeness property exists to prevent.
pub fn render_manifest(
    records: &[FieldProvenance],
    classes: &[VehicleClass],
    talisman_blob: Option<String>,
) -> Result<ProvenanceManifest, Vec<CoverageError>> {
    check_coverage(records, classes)?;

    let mut profiles = Vec::new();
    for &class in classes {
        for &profile in Profile::ALL {
            let contract = profile.contract(class);
            let mut fields = Vec::new();
            for &field in Field::ALL {
                let owner = records
                    .iter()
                    .find(|r| r.class == class && r.profile == profile && r.fields.contains(&field))
                    .expect("coverage checked above");
                fields.push(ManifestField {
                    field,
                    name: field.name().to_string(),
                    value: field.read(&contract),
                    provenance_key: owner.key.to_string(),
                    provenance_class: owner.provenance.token().to_string(),
                    provenance_detail: provenance_detail(owner.provenance),
                    upstream: match owner.provenance {
                        Provenance::Inherited { upstream } => Some(upstream.to_string()),
                        _ => None,
                    },
                    validated: owner.provenance.is_validated(),
                });
            }
            profiles.push(ManifestProfile {
                class: class.as_str().to_string(),
                profile: profile.as_str().to_string(),
                effective_ceiling_mps: effective_ceiling_mps(class, profile),
                contract_only_ceiling_mps: contract.effective_max_speed_mps(),
                fields,
            });
        }
    }

    let external_constraints = EXTERNAL_CONSTRAINTS
        .iter()
        .filter(|c| classes.contains(&c.class))
        .map(|c| ManifestExternalConstraint {
            id: c.id.to_string(),
            class: c.class.as_str().to_string(),
            profile: c.profile.as_str().to_string(),
            constrains: c.constrains.name().to_string(),
            applied_at: c.applied_at.to_string(),
            supplied_by: c.supplied_by.to_string(),
            participates_in_effective_ceiling: c.participates_in_effective_ceiling,
            provenance_class: c.provenance.token().to_string(),
            value_mps: c.value_mps,
        })
        .collect();

    Ok(ProvenanceManifest {
        manifest_version: MANIFEST_VERSION,
        talisman_blob,
        profiles,
        external_constraints,
    })
}

/// Every class this register covers.
pub const ALL_CLASSES: &[VehicleClass] = &[
    VehicleClass::Courier,
    VehicleClass::DeliveryAv,
    VehicleClass::Robotaxi,
    VehicleClass::R2,
];

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// Agreement with the source comments
// ---------------------------------------------------------------------------

/// A parsed provenance tag from `contract_profiles.rs`'s comments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceTag {
    pub key: String,
    /// The classification token, read from the ONE permitted syntactic position.
    pub token: String,
}

/// The classification vocabulary, longest-first so `VALIDATION-PENDING` is
/// matched before any prefix of it could be.
const CLASSIFICATION_TOKENS: &[&str] = &["VALIDATION-PENDING", "MEASURED", "ESTIMATE", "INHERITED"];

/// Extract provenance tags from a `contract_profiles.rs`-shaped source.
///
/// The grammar is **tag-led**, and that is the whole safety property:
///
/// 1. locate a stable `(class.field)` tag,
/// 2. read the classification ONLY from the permitted syntactic position — the
///    first token of the comment block carrying the tag,
/// 3. reject anything else.
///
/// Searching comments for the classification word instead would fail three ways,
/// each producing a clean-looking but wrong result:
///
/// - **Substring**: `ESTIMATE (overhang split UNMEASURED)` contains `MEASURED`,
///   so a naive `contains` promotes the one estimate in the file to a
///   measurement — in the optimistic direction, on the field #1213 depends on.
///   Matching only at the leading position makes that impossible.
/// - **Single dialect**: the ODD caps live in `///` doc comments with the token
///   bolded mid-sentence and the tag written ``(param `r2.odd_cap`)``. A parser
///   written for the field dialect silently drops all four — and for the R2 the
///   dropped row is the one that actually binds.
/// - **Narrative**: comments like "the FIRST profile with MEASURED geometry"
///   carry a token and no tag. Keying on the tag excludes them.
#[must_use]
pub fn extract_source_tags(source: &str) -> Vec<SourceTag> {
    let mut out = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let trimmed = lines[i].trim();

        // A contiguous comment block (either dialect).
        if trimmed.starts_with("//") {
            let start = i;
            let mut body = String::new();
            while i < lines.len() && lines[i].trim().starts_with("//") {
                body.push(' ');
                body.push_str(lines[i].trim().trim_start_matches('/').trim());
                i += 1;
            }
            let is_doc = lines[start].trim().starts_with("///");
            push_tag(&body, is_doc, &mut out);
            continue;
        }

        // A trailing comment on a code line.
        if let Some((code, comment)) = lines[i].split_once("//") {
            if !code.trim().is_empty() {
                push_tag(comment.trim(), false, &mut out);
            }
        }
        i += 1;
    }
    out
}

/// The classification's permitted position differs by dialect, and only by
/// dialect — never "anywhere in the text".
fn classification_in(body: &str, is_doc: bool) -> Option<String> {
    let cleaned = body.trim().trim_start_matches('*').trim();
    if is_doc {
        // Const dialect: the token is bolded. `**MEASURED.**` — permitted only
        // inside the bold markers, not as loose prose.
        for tok in CLASSIFICATION_TOKENS {
            if cleaned.contains(&format!("**{tok}")) {
                return Some((*tok).to_string());
            }
        }
        return None;
    }
    // Field dialect: the token LEADS the block. Anchored, so `UNMEASURED`
    // appearing later in the sentence can never match.
    for tok in CLASSIFICATION_TOKENS {
        if cleaned.starts_with(tok) {
            return Some((*tok).to_string());
        }
    }
    None
}

/// Pull the trailing stable tag: the last parenthesised `class.field` path.
fn stable_tag_in(body: &str) -> Option<String> {
    let mut found = None;
    let bytes: Vec<char> = body.chars().collect();
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == '(' {
            let mut j = idx + 1;
            let mut inner = String::new();
            while j < bytes.len() && bytes[j] != ')' {
                inner.push(bytes[j]);
                j += 1;
            }
            if j < bytes.len() {
                // Tolerates both `(r2.steering)` and
                // ``(param `r2.odd_cap`)`` / `(CONTRACT_PROFILES.md courier.max_speed)`.
                let candidate = inner
                    .split_whitespace()
                    .next_back()
                    .unwrap_or("")
                    .trim_matches('`');
                if is_stable_tag(candidate) {
                    found = Some(candidate.to_string());
                }
                idx = j;
            }
        }
        idx += 1;
    }
    found
}

fn is_stable_tag(s: &str) -> bool {
    let Some((class, rest)) = s.split_once('.') else {
        return false;
    };
    if !matches!(class, "courier" | "delivery-av" | "r2" | "robotaxi") || rest.is_empty() {
        return false;
    }
    rest.split('.')
        .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
}

fn push_tag(body: &str, is_doc: bool, out: &mut Vec<SourceTag>) {
    if let (Some(token), Some(key)) = (classification_in(body, is_doc), stable_tag_in(body)) {
        out.push(SourceTag { key, token });
    }
}

/// Disagreement between a source comment and the provenance table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceAgreementError {
    /// The comment classifies a key one way; the table another.
    Divergent {
        key: String,
        comment_token: String,
        table_token: &'static str,
    },
    /// The comment carries a tag no record claims.
    UnknownKey { key: String },
}

/// Assert the table agrees with the comments it derives from.
///
/// Without this, the table would be a SECOND authored inventory of
/// classifications, free to drift from the prose beside the numbers — the exact
/// failure this register exists to prevent, reintroduced one level up.
///
/// Only keys that appear in BOTH are compared. `INHERITED` and
/// `DERIVED-PRECONDITION` records have no comment tag (the source states those
/// relationships as prose — "Footprint IDENTICAL to nominal"), so the table is
/// where they become checkable; nothing is asserted about them here.
pub fn check_source_agreement(
    source: &str,
    records: &[FieldProvenance],
) -> Result<usize, Vec<SourceAgreementError>> {
    let mut errors = Vec::new();
    let mut agreed = 0usize;

    for tag in extract_source_tags(source) {
        let Some(record) = records.iter().find(|r| r.key == tag.key) else {
            errors.push(SourceAgreementError::UnknownKey { key: tag.key });
            continue;
        };
        let table_token = record.provenance.token();
        if table_token == tag.token {
            agreed += 1;
        } else {
            errors.push(SourceAgreementError::Divergent {
                key: tag.key,
                comment_token: tag.token,
                table_token,
            });
        }
    }

    if errors.is_empty() {
        Ok(agreed)
    } else {
        Err(errors)
    }
}
