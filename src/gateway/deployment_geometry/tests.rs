//! #1215 consumer conformance — the deployed values equal contract + policy.
//!
//! Each test names the consumer, the value, and the policy. A failure reads as
//! "this consumer no longer derives from the contract", not as a bare number
//! mismatch.

use std::path::PathBuf;

use kirra_core::regions::{
    CollisionRegion, CorridorObservationRegion, PhysicalFootprint, SelfFilterRegion,
};

use super::*;
use crate::gateway::contract_profiles::{contract_for, VehicleClass};

fn r2_physical() -> PhysicalFootprint {
    PhysicalFootprint::from_contract(&contract_for(VehicleClass::R2))
}

fn params_yaml() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(R2_PARAMS_YAML);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("deployed params file {} unreadable: {e}", path.display()))
}

const EPS: f64 = 1e-9;

// ---------------------------------------------------------------------------
// The rounding-drift regression (the 0.0505 correction, pinned by formula)
// ---------------------------------------------------------------------------

/// The body closes: `wheelbase + overhang_front + overhang_rear == length`,
/// for EVERY class and BOTH profiles — asserted against the real contracts,
/// not fixtures.
///
/// The R2 shipped violating this: its overhangs were the estimate's formula
/// `(length − wheelbase)/2 = 0.0505` rounded to `0.05`, so
/// `containment::footprint_corners` placed the swept body 1 mm SHORTER than
/// the measured robot — the unsafe direction. The fix (0f513407) is a
/// **reviewed safety-value correction**, and this test is what stops the next
/// edit from silently reintroducing rounding drift.
#[test]
fn every_class_body_closes_exactly() {
    use crate::gateway::contract_profiles::mrc_fallback_for;
    for class in [
        VehicleClass::Courier,
        VehicleClass::DeliveryAv,
        VehicleClass::Robotaxi,
        VehicleClass::R2,
    ] {
        for (label, c) in [
            ("nominal", contract_for(class)),
            ("mrc", mrc_fallback_for(class)),
        ] {
            let parts = c.wheelbase_m + c.overhang_front_m + c.overhang_rear_m;
            assert!(
                (parts - c.length_m).abs() < EPS,
                "{class:?} {label}: wheelbase {} + overhangs {}/{} = {parts} != length {} — \
                 the swept body and the measured body disagree about where the robot ends",
                c.wheelbase_m,
                c.overhang_front_m,
                c.overhang_rear_m,
                c.length_m
            );
        }
    }
}

/// The R2 overhang IS its stated formula, not a rounding of it.
#[test]
fn the_r2_overhang_is_the_formula_from_the_canonical_dimensions() {
    let c = contract_for(VehicleClass::R2);
    let formula = (c.length_m - c.wheelbase_m) / 2.0;
    assert!(
        (c.overhang_front_m - formula).abs() < EPS && (c.overhang_rear_m - formula).abs() < EPS,
        "r2 overhangs {}/{} != (length − wheelbase)/2 = {formula}; the ESTIMATE's own \
         derivation has drifted",
        c.overhang_front_m,
        c.overhang_rear_m
    );
}

// ---------------------------------------------------------------------------
// kirra-trajectory (the slow-loop checker) — VALIDATE across the crate boundary
// ---------------------------------------------------------------------------

/// `kirra_trajectory::VehicleConfig::r2()` cannot import `contract_for` (the
/// profile family lives above it), so its geometry is a cited copy. This test
/// is the check the citation never had: every geometric field equals the
/// canonical contract plus [`CHECKER_R2_COLLISION_MARGIN`].
#[test]
fn the_checker_r2_config_is_the_contract_plus_its_declared_margin() {
    let config = kirra_trajectory::config::VehicleConfig::r2();
    let physical = r2_physical();
    let region = CollisionRegion::centre_origin(physical, CHECKER_R2_COLLISION_MARGIN);

    assert!(
        (config.wheelbase_m - physical.wheelbase_m).abs() < EPS,
        "checker wheelbase {} != contract {}",
        config.wheelbase_m,
        physical.wheelbase_m
    );
    assert!(
        (config.half_length_m - region.front_extent_m).abs() < EPS,
        "checker half_length {} != contract half-length {} + declared margin {}",
        config.half_length_m,
        physical.half_length_m(),
        CHECKER_R2_COLLISION_MARGIN.longitudinal_m
    );
    assert!(
        (config.half_width_m - region.half_width_m).abs() < EPS,
        "checker half_width {} != contract half-width {} + declared margin {}",
        config.half_width_m,
        physical.half_width_m(),
        CHECKER_R2_COLLISION_MARGIN.lateral_m
    );
    // The dynamic limits are the same cited copies; geometry is this slice's
    // scope, but the wheelbase-adjacent kinematic terms ride along cheaply.
    let contract = contract_for(VehicleClass::R2);
    assert!((config.max_speed_mps - contract.max_speed_mps).abs() < EPS);
    assert!(
        (config.max_steering_rad - contract.max_steering_deg.to_radians()).abs() < EPS,
        "checker max_steering {} rad != contract {}°",
        config.max_steering_rad,
        contract.max_steering_deg
    );
    assert_eq!(config.odd_speed_cap_mps, contract.odd_speed_cap_mps);
}

// ---------------------------------------------------------------------------
// kirra_params.yaml occy_doer — VALIDATE across the language boundary
// ---------------------------------------------------------------------------

/// The doer's YAML said "MEASURED … + margin" without saying how much. The
/// margin is now [`DOER_R2_PROPOSAL_MARGIN`], and the YAML is a checked mirror
/// of `contract + that margin`.
#[test]
fn the_doer_yaml_is_the_contract_plus_its_declared_margin() {
    let yaml = params_yaml();
    let physical = r2_physical();
    let region = CollisionRegion::centre_origin(physical, DOER_R2_PROPOSAL_MARGIN);

    let wheelbase = yaml_scalar(&yaml, "occy_doer", "wheelbase_m").expect("occy_doer.wheelbase_m");
    let half_length =
        yaml_scalar(&yaml, "occy_doer", "half_length_m").expect("occy_doer.half_length_m");
    let half_width =
        yaml_scalar(&yaml, "occy_doer", "half_width_m").expect("occy_doer.half_width_m");

    assert!(
        (wheelbase - physical.wheelbase_m).abs() < EPS,
        "doer wheelbase {wheelbase} != contract {} — this is also the #1219 latch value",
        physical.wheelbase_m
    );
    assert!(
        (half_length - region.front_extent_m).abs() < EPS,
        "doer half_length {half_length} != contract {} + declared margin {} = {}",
        physical.half_length_m(),
        DOER_R2_PROPOSAL_MARGIN.longitudinal_m,
        region.front_extent_m
    );
    assert!(
        (half_width - region.half_width_m).abs() < EPS,
        "doer half_width {half_width} != contract {} + declared margin {} = {}",
        physical.half_width_m(),
        DOER_R2_PROPOSAL_MARGIN.lateral_m,
        region.half_width_m
    );
}

/// The interceptor's wheelbase — the value the #1219 `wheelbase_consistent`
/// latch compares against the verifier's active contract — is the contract
/// value exactly.
#[test]
fn the_interceptor_wheelbase_is_the_contract_wheelbase() {
    let yaml = params_yaml();
    let wheelbase = yaml_scalar(&yaml, "cmd_vel_interceptor", "wheelbase_m")
        .expect("cmd_vel_interceptor.wheelbase_m");
    assert!(
        (wheelbase - r2_physical().wheelbase_m).abs() < EPS,
        "interceptor wheelbase {wheelbase} != contract — the #1219 latch would trip at runtime"
    );
}

// ---------------------------------------------------------------------------
// kirra_params.yaml perception_governor — full width is not a half-extent
// ---------------------------------------------------------------------------

/// `vehicle_width_m` is semantically the FULL body width feeding the Taj
/// sidecar's `required_corridor_width = vehicle_width + 2·clearance`. It must
/// equal the contract width exactly — not the half-width, which would halve
/// the corridor the ego demands.
#[test]
fn the_perception_vehicle_width_is_the_full_contract_width() {
    let yaml = params_yaml();
    let physical = r2_physical();
    let width = yaml_scalar(&yaml, "perception_governor", "vehicle_width_m")
        .expect("perception_governor.vehicle_width_m");

    assert!(
        (width - physical.width_m).abs() < EPS,
        "perception vehicle_width {width} != contract FULL width {}",
        physical.width_m
    );
    // The guard the type system provides in Rust, asserted for the YAML:
    // the value is unambiguously a full width, not a half-extent.
    assert!(
        (width - physical.half_width_m()).abs() > 0.05,
        "perception vehicle_width {width} is suspiciously close to the HALF width {} — \
         a half-extent here would halve the corridor the ego demands",
        physical.half_width_m()
    );
}

/// The lane gate admits the corridor the region demands: the deployed
/// `lane_half_m` and `lateral_clearance_m` satisfy the same inequality the Taj
/// sidecar enforces at request time (`LaneNarrowerThanRequiredCorridor`), so a
/// config edit cannot ship a lane the sidecar will refuse.
#[test]
fn the_deployed_lane_gate_admits_the_required_corridor() {
    let yaml = params_yaml();
    let physical = r2_physical();

    let lane_half = yaml_scalar(&yaml, "perception_governor", "lane_half_m").expect("lane_half_m");
    let clearance = yaml_scalar(&yaml, "perception_governor", "lateral_clearance_m")
        .expect("lateral_clearance_m");

    assert!(
        (clearance - PERCEPTION_R2_CLEARANCE.lateral_m).abs() < EPS,
        "deployed clearance {clearance} != declared policy {}",
        PERCEPTION_R2_CLEARANCE.lateral_m
    );

    let region = CorridorObservationRegion::new(physical, PERCEPTION_R2_CLEARANCE);
    assert!(
        lane_half >= region.required_half_width_m(),
        "lane_half {lane_half} < required half-width {} (= (width {} + 2×clearance {}) / 2) — \
         the Taj sidecar would refuse every request from this config",
        region.required_half_width_m(),
        region.vehicle_width_m,
        region.clearance_each_side_m
    );
}

// ---------------------------------------------------------------------------
// kirra-taj self-filter — the mask bound follows the body
// ---------------------------------------------------------------------------

/// `SelfFilterBounds::for_footprint(width, length, …)` takes the body
/// dimensions as parameters — the caller supplies them from the contract. This
/// pins the two crates' arithmetic together: the taj bound for the contract
/// body stays within the region type's half-diagonal-derived cap, and both
/// stay under the crate's hard backstop.
#[test]
fn the_taj_self_filter_bound_and_the_region_agree_on_the_contract_body() {
    let physical = r2_physical();
    let region = SelfFilterRegion::from_footprint(physical);

    let taj_bounds = kirra_taj::self_filter::SelfFilterBounds::for_footprint(
        physical.width_m,
        physical.length_m,
        &kirra_taj::self_filter::SensorMount::ASSUMED_IDENTITY,
    );

    // The region's radius is the half-diagonal; taj's bound must admit exactly
    // the vertices the half-diagonal admits (its own construction adds mount
    // slack on top, so >=), and never exceed the crate's absolute backstop.
    assert!(
        taj_bounds.max_vertex_radius_m >= region.max_vertex_radius_m,
        "taj self-filter radius {} rejects vertices on the body's own half-diagonal {}",
        taj_bounds.max_vertex_radius_m,
        region.max_vertex_radius_m
    );
    assert!(
        taj_bounds.max_vertex_radius_m <= kirra_taj::self_filter::MAX_SELF_MASK_VERTEX_RADIUS_M,
        "taj self-filter radius exceeds the crate backstop"
    );
    assert!(region.max_vertex_radius_m <= kirra_taj::self_filter::MAX_SELF_MASK_VERTEX_RADIUS_M);
}

// ---------------------------------------------------------------------------
// Non-vacuity: the YAML parser actually reads the deployed file
// ---------------------------------------------------------------------------

/// A parser that silently returned None for everything would make every
/// YAML-validating test above fail loudly (expect panics) — but a parser that
/// read the WRONG section would not. Pin the section discrimination.
#[test]
fn the_yaml_scalar_reader_discriminates_sections() {
    let sample = "\
alpha:\n  ros__parameters:\n    key_a: 1.5\nbeta:\n  ros__parameters:\n    key_a: 2.5\n";
    assert_eq!(yaml_scalar(sample, "alpha", "key_a"), Some(1.5));
    assert_eq!(yaml_scalar(sample, "beta", "key_a"), Some(2.5));
    assert_eq!(yaml_scalar(sample, "gamma", "key_a"), None);
    assert_eq!(yaml_scalar(sample, "alpha", "key_b"), None);
}

/// Comments after values are stripped; a renamed key is an error, not a
/// default.
#[test]
fn the_yaml_scalar_reader_strips_trailing_comments() {
    let sample = "s:\n  ros__parameters:\n    k: 0.229   # MEASURED wheelbase\n";
    assert_eq!(yaml_scalar(sample, "s", "k"), Some(0.229));
}
