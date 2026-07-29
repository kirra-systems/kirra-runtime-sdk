//! #1215 region tests.
//!
//! The acceptance property under test:
//!
//! > Every consumer geometry value is constructed from the canonical platform
//! > profile plus an explicitly named margin or transformation; no consumer
//! > independently re-derives physical vehicle dimensions.
//!
//! Relationships are asserted, never accidental equalities — a test that passes
//! because two numbers happen to match today proves nothing about tomorrow.

use super::*;

/// A stand-in for the R2 contract. Uses the same shape as the real `r2` profile
/// so the derivations below exercise the real arithmetic; the VALUES are
/// deliberately arbitrary in the regression test further down, to prove the
/// regions follow the contract rather than these particular numbers.
fn r2_like() -> VehicleKinematicsContract {
    VehicleKinematicsContract {
        max_speed_mps: 1.5,
        max_accel_mps2: 0.5,
        max_brake_mps2: 1.5,
        max_steering_deg: 39.0,
        max_steering_rate_deg_s: 30.0,
        min_follow_distance_m: 0.5,
        max_lateral_accel_mps2: 1.0,
        wheelbase_m: 0.229,
        width_m: 0.203,
        length_m: 0.330,
        overhang_front_m: 0.0505,
        overhang_rear_m: 0.0505,
        odd_speed_cap_mps: Some(1.0),
    }
}

// ---------------------------------------------------------------------------
// Margins are named, validated, and never shrink a region
// ---------------------------------------------------------------------------

#[test]
fn a_negative_margin_is_refused_rather_than_clamped() {
    assert_eq!(
        Margin::new("proposal envelope", -0.01, 0.0),
        Err(MarginError::Negative),
        "a negative margin would shrink the region below the body; clamping it \
         to zero would hide an intent someone meant to express"
    );
}

#[test]
fn a_non_finite_margin_is_refused() {
    assert_eq!(
        Margin::new("bad", f64::NAN, 0.0),
        Err(MarginError::NotFinite)
    );
    assert_eq!(
        Margin::new("bad", 0.0, f64::INFINITY),
        Err(MarginError::NotFinite)
    );
}

#[test]
fn an_unnamed_margin_is_refused() {
    assert_eq!(Margin::new("   ", 0.01, 0.01), Err(MarginError::Unnamed));
}

#[test]
fn a_valid_margin_records_its_reason() {
    let m = Margin::new(
        "doer proposal envelope over the physical body",
        0.005,
        0.0085,
    )
    .expect("valid");
    assert!(!m.reason.is_empty());
    assert!(m.longitudinal_m >= 0.0 && m.lateral_m >= 0.0);
}

// ---------------------------------------------------------------------------
// The relationship, not an equality
// ---------------------------------------------------------------------------

/// The doer's `0.17 × 0.11` is not an error to normalize away — it is a
/// deliberate margin over the physical `0.165 × 0.1015`, in the conservative
/// direction. What was missing is that nothing SAID so.
#[test]
fn a_region_is_never_smaller_than_the_body_it_bounds() {
    let physical = PhysicalFootprint::from_contract(&r2_like());

    for (reason, lon, lat) in [
        ("no expansion", 0.0, 0.0),
        ("checker rounding headroom", 0.0, 0.0005),
        ("doer proposal envelope", 0.005, 0.0085),
    ] {
        let margin = Margin::new(reason, lon, lat).expect("valid margin");
        let region = CollisionRegion::centre_origin(physical, margin);

        assert!(
            region.covers_physical_body(),
            "{reason}: region {}x{} does not cover the body {}x{}",
            region.half_length_m,
            region.half_width_m,
            physical.half_length_m(),
            physical.half_width_m()
        );
        assert!(region.half_length_m >= physical.half_length_m());
        assert!(region.half_width_m >= physical.half_width_m());
    }
}

/// The doer and checker regions differ, and the difference is now a stated
/// margin rather than two independent derivations that happen not to match.
#[test]
fn the_doer_envelope_is_the_checker_envelope_plus_a_named_margin() {
    let physical = PhysicalFootprint::from_contract(&r2_like());

    let checker = CollisionRegion::centre_origin(physical, Margin::NONE);
    let doer = CollisionRegion::centre_origin(
        physical,
        Margin::new(
            "doer proposal envelope; propose within a body larger than the checker demands",
            0.005,
            0.0085,
        )
        .expect("valid"),
    );

    assert!(
        doer.half_length_m > checker.half_length_m,
        "the doer envelope must be strictly more conservative longitudinally"
    );
    assert!(
        doer.half_width_m > checker.half_width_m,
        "the doer envelope must be strictly more conservative laterally"
    );
    // Both still describe the SAME body.
    assert_eq!(doer.physical, checker.physical);
}

// ---------------------------------------------------------------------------
// THE regression for the defect shape that motivated #1215
// ---------------------------------------------------------------------------

/// Changing the contract's width or length changes every derived region, while
/// preserving each region's declared margin policy.
///
/// This is the property whose ABSENCE was the finding: four consumers
/// independently re-derived the R2 footprint, so a contract change would have
/// updated none of them.
#[test]
fn changing_the_contract_changes_every_derived_region_and_preserves_each_margin() {
    let before = PhysicalFootprint::from_contract(&r2_like());

    let mut grown = r2_like();
    grown.width_m *= 2.0;
    grown.length_m *= 2.0;
    // Keep the body self-consistent: wheelbase + overhangs == length.
    grown.wheelbase_m = grown.length_m - grown.overhang_front_m - grown.overhang_rear_m;
    let after = PhysicalFootprint::from_contract(&grown);

    let margin = Margin::new("doer proposal envelope", 0.005, 0.0085).expect("valid");

    let collision_before = CollisionRegion::centre_origin(before, margin);
    let collision_after = CollisionRegion::centre_origin(after, margin);
    assert!(
        collision_after.half_width_m > collision_before.half_width_m,
        "the collision region did not follow the contract width"
    );
    assert!(
        collision_after.half_length_m > collision_before.half_length_m,
        "the collision region did not follow the contract length"
    );
    // The MARGIN POLICY is preserved — the region grew by the body's growth, not
    // by an arbitrary amount, and the declared expansion is unchanged.
    assert_eq!(collision_after.margin, collision_before.margin);
    assert_eq!(
        collision_after.half_width_m - after.half_width_m(),
        collision_before.half_width_m - before.half_width_m(),
        "the declared lateral margin changed when only the body should have"
    );
    assert_eq!(
        collision_after.half_length_m - after.half_length_m(),
        collision_before.half_length_m - before.half_length_m(),
        "the declared longitudinal margin changed when only the body should have"
    );

    let clearance = Margin::new("required clearance each side", 0.0, 0.15).expect("valid");
    let corridor_before = CorridorObservationRegion::new(before, clearance);
    let corridor_after = CorridorObservationRegion::new(after, clearance);
    assert!(
        corridor_after.required_width_m() > corridor_before.required_width_m(),
        "the corridor requirement did not follow the contract width"
    );
    assert_eq!(
        corridor_after.clearance_each_side_m, corridor_before.clearance_each_side_m,
        "the clearance policy changed when only the body should have"
    );

    let self_filter_before = SelfFilterRegion::from_footprint(before);
    let self_filter_after = SelfFilterRegion::from_footprint(after);
    assert!(
        self_filter_after.max_vertex_radius_m > self_filter_before.max_vertex_radius_m,
        "the self-filter bound did not follow the body"
    );

    let swept_before = TurnSweptRegion::rear_axle_origin(before);
    let swept_after = TurnSweptRegion::rear_axle_origin(after);
    assert!(
        swept_after.total_length_m() > swept_before.total_length_m(),
        "the swept region did not follow the contract length"
    );
    assert!(swept_after.half_width_m > swept_before.half_width_m);
}

// ---------------------------------------------------------------------------
// Full width is not a half-extent
// ---------------------------------------------------------------------------

/// The trap this type exists to prevent. `perception_governor`'s
/// `vehicle_width_m: 0.203` is a FULL width feeding
/// `required_corridor_width = vehicle_width + 2·clearance`. Substituting a
/// half-width there would halve the corridor the ego demands.
#[test]
fn the_corridor_region_carries_a_full_width_not_a_half_extent() {
    let physical = PhysicalFootprint::from_contract(&r2_like());
    let corridor = CorridorObservationRegion::new(
        physical,
        Margin::new("required clearance each side", 0.0, 0.15).expect("valid"),
    );

    assert_eq!(
        corridor.vehicle_width_m, physical.width_m,
        "the corridor region must carry the FULL width"
    );
    assert!(
        corridor.vehicle_width_m > physical.half_width_m(),
        "a full width must be distinguishable from a half-extent"
    );
    assert_eq!(
        corridor.required_width_m(),
        physical.width_m + 2.0 * 0.15,
        "must match the Taj sidecar's required_corridor_width formula exactly"
    );
    assert_eq!(
        corridor.required_half_width_m() * 2.0,
        corridor.required_width_m(),
        "the halving helper must round-trip, so callers never halve by hand"
    );
}

// ---------------------------------------------------------------------------
// Membership is monotonic in the widening dimension
// ---------------------------------------------------------------------------

#[test]
fn collision_membership_is_monotonic_in_the_margin() {
    let physical = PhysicalFootprint::from_contract(&r2_like());
    let tight = CollisionRegion::centre_origin(physical, Margin::NONE);
    let loose =
        CollisionRegion::centre_origin(physical, Margin::new("wider", 0.10, 0.10).expect("valid"));

    // Every point inside the tighter region is inside the looser one. Sampled on
    // a grid spanning well beyond both.
    for i in 0..40 {
        for j in 0..40 {
            let x = -0.4 + f64::from(i) * 0.02;
            let y = -0.4 + f64::from(j) * 0.02;
            if tight.contains(x, y) {
                assert!(
                    loose.contains(x, y),
                    "({x}, {y}) is inside the tight region but not the loose one"
                );
            }
        }
    }
}

#[test]
fn collision_membership_refuses_non_finite_coordinates() {
    let region =
        CollisionRegion::centre_origin(PhysicalFootprint::from_contract(&r2_like()), Margin::NONE);
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(!region.contains(bad, 0.0));
        assert!(!region.contains(0.0, bad));
    }
}

// ---------------------------------------------------------------------------
// Frames: the two conventions, and where they stop agreeing
// ---------------------------------------------------------------------------

#[test]
fn regions_declare_the_frame_they_are_expressed_in() {
    let physical = PhysicalFootprint::from_contract(&r2_like());
    assert_eq!(
        CollisionRegion::centre_origin(physical, Margin::NONE).frame,
        Frame::GeometricCentreBody
    );
    assert_eq!(
        TurnSweptRegion::rear_axle_origin(physical).frame,
        Frame::RearAxleBody
    );
    assert_eq!(
        SelfFilterRegion::from_footprint(physical).frame,
        Frame::Sensor,
        "self-filtering happens in the SENSOR frame, before any transform (#1210)"
    );
}

/// The R2's symmetric centre-origin description is exact — but only because the
/// overhang split is an assumed even division. That assumption is an ESTIMATE
/// (`r2.overhangs`, #1219), and its measurement is owed under #1213.
#[test]
fn the_r2_centre_origin_description_is_exact_only_because_the_split_is_assumed_even() {
    let physical = PhysicalFootprint::from_contract(&r2_like());
    assert!(physical.centre_origin_is_exact());
    assert_eq!(physical.centre_origin_shortfall_m(), 0.0);
    assert_eq!(
        physical.overhang_front_m, physical.overhang_rear_m,
        "the exactness rests entirely on this equality, which is assumed, not measured"
    );
}

/// The coupling nobody had stated: if the real split is uneven, a symmetric
/// half-length UNDER-COVERS the longer end while reporting a plausible number.
#[test]
fn an_uneven_overhang_split_makes_the_symmetric_description_under_cover() {
    let mut uneven = r2_like();
    // Same total length, shifted split: more overhang at the front.
    uneven.overhang_front_m = 0.08;
    uneven.overhang_rear_m = 0.02;
    let physical = PhysicalFootprint::from_contract(&uneven);

    assert!(
        !physical.centre_origin_is_exact(),
        "an uneven split must not report as exact"
    );
    assert!(
        physical.centre_origin_shortfall_m() > 0.0,
        "the shortfall must be quantified, not merely flagged"
    );

    // The concrete consequence: the symmetric forward reach is short of the
    // body's true forward reach measured from the rear axle.
    let symmetric_forward_from_axle = physical.wheelbase_m * 0.5 + physical.half_length_m();
    assert!(
        symmetric_forward_from_axle < physical.forward_extent_from_rear_axle_m(),
        "the symmetric description should fall short of the true forward extent"
    );
}

// ---------------------------------------------------------------------------
// Agreement with the geometry that already ships
// ---------------------------------------------------------------------------

/// `TurnSweptRegion` is a NAMED VIEW over `containment::footprint_corners`, not
/// a second implementation. This pins the extents to the same arithmetic that
/// function performs, so the two cannot drift into disagreeing about where the
/// body is.
#[test]
fn the_swept_region_matches_the_containment_corner_placement() {
    let physical = PhysicalFootprint::from_contract(&r2_like());
    let swept = TurnSweptRegion::rear_axle_origin(physical);

    // containment::footprint_corners places body-frame corners at
    // (wheelbase + overhang_front, ±width/2) and (−overhang_rear, ±width/2).
    assert_eq!(
        swept.forward_extent_m,
        physical.wheelbase_m + physical.overhang_front_m
    );
    assert_eq!(swept.rearward_extent_m, physical.overhang_rear_m);
    assert_eq!(swept.half_width_m, physical.width_m * 0.5);
}

/// The family invariant the swept extents assume: the body's parts add up to its
/// length. Checked here rather than assumed, because the swept region is where a
/// violation would silently mis-place a corner.
#[test]
fn the_swept_extents_sum_to_the_contract_length() {
    let physical = PhysicalFootprint::from_contract(&r2_like());
    let swept = TurnSweptRegion::rear_axle_origin(physical);
    assert!(
        (swept.total_length_m() - physical.length_m).abs() < 1e-9,
        "wheelbase {} + overhangs {}/{} = {} != length {}",
        physical.wheelbase_m,
        physical.overhang_front_m,
        physical.overhang_rear_m,
        swept.total_length_m(),
        physical.length_m
    );
}

/// A `PhysicalFootprint` converts to the `VehicleFootprint` the containment
/// check already consumes, so a region and the shipped containment path cannot
/// describe different bodies.
#[test]
fn the_physical_footprint_converts_to_the_containment_footprint_losslessly() {
    let contract = r2_like();
    let physical = PhysicalFootprint::from_contract(&contract);
    let vf: VehicleFootprint = (&physical).into();

    assert_eq!(vf.width_m, contract.width_m);
    assert_eq!(vf.length_m, contract.length_m);
    assert_eq!(vf.wheelbase_m, contract.wheelbase_m);
    assert_eq!(vf.overhang_front_m, contract.overhang_front_m);
    assert_eq!(vf.overhang_rear_m, contract.overhang_rear_m);
}

/// The self-filter bound may not reach beyond the robot it is masking.
#[test]
fn the_self_filter_radius_is_the_body_half_diagonal() {
    let physical = PhysicalFootprint::from_contract(&r2_like());
    let region = SelfFilterRegion::from_footprint(physical);

    let hl = physical.half_length_m();
    let hw = physical.half_width_m();
    assert!((region.max_vertex_radius_m - (hl * hl + hw * hw).sqrt()).abs() < 1e-12);

    // Strictly beyond either half-extent alone (it is a diagonal), and finite.
    assert!(region.max_vertex_radius_m > hl);
    assert!(region.max_vertex_radius_m > hw);
    assert!(region.max_vertex_radius_m.is_finite());
}
