// src/gateway/contract_profiles.rs
//
// Per-class kinematic contract profiles (#312) + the VRU-dense sidewalk-courier
// profile (#313). The contract is a FAMILY parameterized by vehicle class —
// courier (pedestrian-space, low-speed) / delivery-AV (road-going pod) / robotaxi
// (full-speed) — sharing ONE governor, ONE signed chain, ONE console. The
// per-class delta is confined to the envelope + ODD caps (+ the SG6 ImpactCfg
// thresholds, which live in the parko workspace — see docs/CONTRACT_PROFILES.md).
//
// THE HELD LINE — siblings, never edits. The frozen instance
// `src/gateway/kinematics_contract.rs` (talisman blob
// 997fb7ae15ce3e11adec9218044c7c84b049ad3b) is NOT touched. Profiles are SIBLINGS:
// new constructors here that return the existing public `VehicleKinematicsContract`
// struct, exactly the idiom `nominal_reference_profile` / `mrc_fallback_profile`
// already establish. The robotaxi class member IS the frozen instance (delegated
// verbatim — zero new numbers); a profile that changed the talisman's layout would
// be a finding, not a feature.
//
// NUMBER DISCIPLINE — every non-inherited number below is **VALIDATION-PENDING**
// (track-test / SOTIF-derived, NOT a certified constant — the same honesty the
// frozen instance and `ImpactCfg::default` already carry) and states its basis +
// the normative anchor `docs/CONTRACT_PROFILES.md` (the per-parameter table). No
// number appears here without a stated basis.
//
// SELECTION is FAIL-CLOSED (the `KIRRA_BACKEND` pattern): an unknown class string
// is an `Err`, never a silent fallback to another class's envelope — a typo must
// never select the wrong (e.g. faster) profile.
//
// CROSS-WORKSPACE DUPLICATION (stated, not hidden). The SG6 impact-decel side of
// this family lives in the parko workspace: `parko-core`'s
// `impact.rs::impact_cfg_for_class` (+ its own `VehicleClass`). parko-core and this
// SDK gateway are **separate workspaces with no dependency edge**, so the class enum
// and the per-class numbers **cannot be shared by import** — the copy is deliberate
// and CITED. The single source of truth for every per-class number is the normative
// table `docs/CONTRACT_PROFILES.md`; both sides cite it by parameter id. Change a
// number here ⇒ change `docs/CONTRACT_PROFILES.md` and the parko side too.
//
// SAFETY: contract family | REQ: KIRRA-CLASS-PROFILES-001 | TEST: courier_str_roundtrips_case_insensitive,unknown_class_is_fail_closed_err,robotaxi_equals_frozen_nominal_field_for_field,robotaxi_mrc_equals_frozen_mrc,cap_ordering_courier_lt_deliveryav_lt_robotaxi,every_profile_brake_geq_accel,every_profile_footprint_positive,every_profile_cap_leq_max_speed,mrc_leq_nominal_per_field_every_class
//
// Cross-refs: docs/CONTRACT_PROFILES.md (normative home), docs/ARCHITECTURE_STACK.md
// §2 (the three-domain model + the frozen-talisman rule), docs/MARKET_AUTONOMOUS_SERVICES.md
// §3c (why the market needs the family), ADR-0001 (the ODD speed-cap framing).

use std::str::FromStr;
use std::sync::OnceLock;

use crate::gateway::kinematics_contract::{
    VehicleKinematicsContract, URBAN_ODD_SPEED_CAP_MPS,
};

/// Env var selecting the deployment's vehicle class (#312). Parsed FAIL-CLOSED via
/// [`VehicleClass::from_str`]: `courier` | `delivery-av` | `robotaxi`. There is NO
/// default — an unset/empty/unknown value aborts startup (`init_vehicle_class_from_env`).
pub const VEHICLE_CLASS_ENV: &str = "KIRRA_VEHICLE_CLASS";

// ---------------------------------------------------------------------------
// Per-class ODD operational speed caps
// ---------------------------------------------------------------------------
//
// Defined beside `URBAN_ODD_SPEED_CAP_MPS`'s pattern (the ODD ceiling is a
// safety-case operational cap, distinct from the vehicle physical maximum — see
// ADR-0001 and `VehicleKinematicsContract::effective_max_speed_mps`). These do NOT
// modify the existing const; they are the per-class siblings of it.

/// Sidewalk-courier ODD operational speed cap (m/s). **VALIDATION-PENDING.**
///
/// Basis: commercial sidewalk-delivery fleets operate at roughly **1.5–3 m/s**
/// (walking-pace multiples); 2.5 m/s (~1.8× a 1.4 m/s walking pace) sits inside
/// that band as a conservative pedestrian-space ceiling. NOT a certified value —
/// see `docs/CONTRACT_PROFILES.md` (param `courier.odd_cap`).
pub const COURIER_ODD_SPEED_CAP_MPS: f64 = 2.5;

/// Road-going delivery-AV (pod) ODD operational speed cap (m/s).
/// **VALIDATION-PENDING.**
///
/// Basis: the Nuro-shape road pod runs a low-speed-road ODD (~25 mph ≈ 11.2 m/s);
/// 11.0 m/s is a conservative round-down. Between the courier and robotaxi caps.
/// NOT a certified value — see `docs/CONTRACT_PROFILES.md` (param `delivery-av.odd_cap`).
pub const DELIVERY_AV_ODD_SPEED_CAP_MPS: f64 = 11.0;

/// Robotaxi-class ODD operational speed cap (m/s). **INHERITED** — this is exactly
/// the existing urban Occy ODD cap (`URBAN_ODD_SPEED_CAP_MPS` = 22.35, ADR-0001 /
/// SPEED_ENVELOPE.md / KIRRA-OCCY-SPEED-VAL-001). No new number: the robotaxi class
/// IS the frozen reference deployment. Documented here for the family table; note
/// `contract_for(Robotaxi)` returns the frozen instance verbatim (`odd_speed_cap_mps:
/// None` — the deployment applies the cap, per the frozen instance's own doc).
pub const ROBOTAXI_ODD_SPEED_CAP_MPS: f64 = URBAN_ODD_SPEED_CAP_MPS;

// The ODD-cap ordering is a family invariant, pinned at COMPILE time: a sidewalk
// robot's ceiling is below a road pod's is below a robotaxi's. (Compile-time
// assertions are the clippy-approved way to assert on constants.)
const _: () = assert!(COURIER_ODD_SPEED_CAP_MPS < DELIVERY_AV_ODD_SPEED_CAP_MPS);
const _: () = assert!(DELIVERY_AV_ODD_SPEED_CAP_MPS < ROBOTAXI_ODD_SPEED_CAP_MPS);

// ---------------------------------------------------------------------------
// Vehicle class
// ---------------------------------------------------------------------------

/// The vehicle class a contract profile is selected for. Selection is FAIL-CLOSED:
/// see [`VehicleClass::from_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VehicleClass {
    /// Sidewalk courier — pedestrian-space, low-speed, small footprint (#313).
    Courier,
    /// Road-going delivery AV — low-speed-road pod (Nuro-shape).
    DeliveryAv,
    /// Robotaxi — full-speed mixed-traffic. The frozen reference instance.
    Robotaxi,
}

impl FromStr for VehicleClass {
    type Err = String;

    /// Case-insensitive parse of `"courier"` / `"delivery-av"` / `"robotaxi"`.
    ///
    /// FAIL-CLOSED (the `KIRRA_BACKEND` selection pattern): any other string —
    /// including a near-miss typo — is an `Err`, never a silent fallback to a
    /// default class. A mis-typed class must NEVER select another class's
    /// (e.g. faster) envelope.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "courier" => Ok(VehicleClass::Courier),
            "delivery-av" => Ok(VehicleClass::DeliveryAv),
            "robotaxi" => Ok(VehicleClass::Robotaxi),
            other => Err(format!(
                "unknown vehicle class {other:?}; expected one of \
                 courier | delivery-av | robotaxi (fail-closed — no default)"
            )),
        }
    }
}

impl VehicleClass {
    /// The class's canonical lowercase id (the inverse of [`from_str`](Self::from_str)).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            VehicleClass::Courier => "courier",
            VehicleClass::DeliveryAv => "delivery-av",
            VehicleClass::Robotaxi => "robotaxi",
        }
    }
}

// ---------------------------------------------------------------------------
// The Nominal profile family
// ---------------------------------------------------------------------------

/// The **Nominal** kinematic contract for `class`. The robotaxi member delegates to
/// the frozen `nominal_reference_profile()` verbatim (zero new numbers); the courier
/// and delivery-AV members are siblings whose numbers are VALIDATION-PENDING (see the
/// per-field basis in the constructors and `docs/CONTRACT_PROFILES.md`).
#[must_use]
pub fn contract_for(class: VehicleClass) -> VehicleKinematicsContract {
    match class {
        VehicleClass::Robotaxi => VehicleKinematicsContract::nominal_reference_profile(),
        VehicleClass::DeliveryAv => delivery_av_nominal(),
        VehicleClass::Courier => courier_nominal(),
    }
}

/// The **MRC fallback** kinematic contract for `class` (degraded posture). The
/// robotaxi member delegates to the frozen `mrc_fallback_profile()` verbatim; the
/// courier / delivery-AV members are stricter siblings (every field ≤ the class's
/// Nominal limit, except `min_follow_distance_m` which is ≥, and the footprint which
/// is identical — the vehicle does not shrink in degraded posture).
#[must_use]
pub fn mrc_fallback_for(class: VehicleClass) -> VehicleKinematicsContract {
    match class {
        VehicleClass::Robotaxi => VehicleKinematicsContract::mrc_fallback_profile(),
        VehicleClass::DeliveryAv => delivery_av_mrc(),
        VehicleClass::Courier => courier_mrc(),
    }
}

// ---------------------------------------------------------------------------
// Process-wide selected vehicle class (#312 binary wiring)
// ---------------------------------------------------------------------------

/// The deployment's selected vehicle class, set once at startup from
/// [`VEHICLE_CLASS_ENV`]. Mirrors the adapter-bounds `OnceLock` pattern
/// (`global_sdo_bounds`) so the per-class contract selection needs no change to
/// `ServiceState` / handler construction.
static GLOBAL_VEHICLE_CLASS: OnceLock<VehicleClass> = OnceLock::new();

/// Resolve `KIRRA_VEHICLE_CLASS` once at startup into the process-wide class.
///
/// FAIL-CLOSED (the user-confirmed disposition + `docs/CONTRACT_PROFILES.md`
/// "there is no default class"): an unset / empty / unknown value is a FATAL
/// configuration error — log and `exit(1)` rather than silently selecting a
/// (possibly faster) envelope. A typo'd class must never pick another class's
/// limits. Mirrors the parko node's `KIRRA_NODE_ID` "no safe default" handling.
pub fn init_vehicle_class_from_env() {
    let raw = std::env::var(VEHICLE_CLASS_ENV).unwrap_or_default();
    match VehicleClass::from_str(&raw) {
        Ok(class) => {
            let _ = GLOBAL_VEHICLE_CLASS.set(class);
            tracing::info!(
                vehicle_class = class.as_str(),
                "vehicle class selected — per-class kinematic contract + ODD cap in effect (#312)"
            );
        }
        Err(e) => {
            tracing::error!(
                value = %raw, error = %e,
                "FATAL: {VEHICLE_CLASS_ENV} unset or unknown — there is NO default vehicle class \
                 (a wrong class would select another class's envelope). Set it to one of \
                 courier | delivery-av | robotaxi. Refusing to start."
            );
            std::process::exit(1);
        }
    }
}

/// The process-wide selected vehicle class.
///
/// Production calls [`init_vehicle_class_from_env`] at startup (which aborts on an
/// unset/unknown value), so the live request path always observes the configured
/// class. When uninitialized — in-process tests / library embedding that never
/// called init — this returns the **frozen reference instance** (`Robotaxi`), the
/// documented baseline the existing contract tests encode, so those paths stay
/// byte-identical. The fail-closed guarantee lives at the startup boundary.
#[must_use]
pub fn global_vehicle_class() -> VehicleClass {
    GLOBAL_VEHICLE_CLASS.get().copied().unwrap_or(VehicleClass::Robotaxi)
}

// --- Courier (sidewalk, VRU-dense) — the #313 profile ----------------------
//
// VRU-dense rationale (see docs/CONTRACT_PROFILES.md "VRU-dense rationale"): every
// bound is shaped by pedestrian proximity — low absolute speed, short stopping
// distances, gentle lateral comfort, a small footprint.

fn courier_nominal() -> VehicleKinematicsContract {
    VehicleKinematicsContract {
        // VALIDATION-PENDING: mechanical top of the ~1.5–3 m/s sidewalk operating
        // band; the ODD cap (2.5) sits just below it so the effective ceiling is
        // the cap. (CONTRACT_PROFILES.md courier.max_speed)
        max_speed_mps: 3.0,
        // VALIDATION-PENDING: gentle acceleration near pedestrians. (courier.accel)
        max_accel_mps2: 1.0,
        // VALIDATION-PENDING: firm service brake → SHORT absolute stopping distance
        // (at 2.5 m/s, v²/2b ≈ 1.04 m), the VRU-dense priority. brake ≥ accel.
        // (courier.brake)
        max_brake_mps2: 3.0,
        // VALIDATION-PENDING: tight maneuvering in pedestrian space; effective
        // steering is further bounded by the low lateral-accel limit below via the
        // bicycle-model clamp. (courier.steering)
        max_steering_deg: 30.0,
        // VALIDATION-PENDING: moderate steering slew for a small platform. (courier.steering_rate)
        max_steering_rate_deg_s: 30.0,
        // VALIDATION-PENDING: conservative follow distance RELATIVE to the low speed
        // (~0.8 s headway at 2.5 m/s, plus the robot's short reaction). (courier.follow)
        min_follow_distance_m: 2.0,
        // VALIDATION-PENDING: gentle lateral comfort near VRUs (matches the frozen
        // MRC lateral limit). (courier.lat_accel)
        max_lateral_accel_mps2: 1.5,
        // VALIDATION-PENDING: small sidewalk-robot wheelbase. (courier.wheelbase)
        wheelbase_m: 0.5,
        // VALIDATION-PENDING: narrow sidewalk footprint (small delivery robot).
        // width/length/overhangs all positive. (courier.footprint)
        width_m: 0.6,
        length_m: 0.9,
        overhang_front_m: 0.2,
        overhang_rear_m: 0.2,
        // The pedestrian-space ODD ceiling (sibling of URBAN_ODD_SPEED_CAP_MPS).
        odd_speed_cap_mps: Some(COURIER_ODD_SPEED_CAP_MPS),
    }
}

fn courier_mrc() -> VehicleKinematicsContract {
    VehicleKinematicsContract {
        // VALIDATION-PENDING: degraded crawl. (courier.mrc.max_speed)
        max_speed_mps: 1.0,
        max_accel_mps2: 0.5,    // VALIDATION-PENDING (≥ ? brake below) (courier.mrc.accel)
        max_brake_mps2: 2.0,    // VALIDATION-PENDING: brake ≥ accel. (courier.mrc.brake)
        max_steering_deg: 15.0, // VALIDATION-PENDING (courier.mrc.steering)
        max_steering_rate_deg_s: 15.0, // VALIDATION-PENDING (courier.mrc.steering_rate)
        min_follow_distance_m: 3.0,    // VALIDATION-PENDING: ≥ nominal follow (courier.mrc.follow)
        max_lateral_accel_mps2: 0.75,  // VALIDATION-PENDING (courier.mrc.lat_accel)
        // Footprint is platform geometry — IDENTICAL to courier nominal (the vehicle
        // does not shrink in degraded posture).
        wheelbase_m: 0.5,
        width_m: 0.6,
        length_m: 0.9,
        overhang_front_m: 0.2,
        overhang_rear_m: 0.2,
        // MRC crawl (1.0) is already below the courier ODD cap; leave None so min()
        // selects 1.0 (the frozen-MRC idiom).
        odd_speed_cap_mps: None,
    }
}

// --- Delivery AV (road-going pod, Nuro-shape) ------------------------------
//
// Between courier and robotaxi on every limit; every number VALIDATION-PENDING.

fn delivery_av_nominal() -> VehicleKinematicsContract {
    VehicleKinematicsContract {
        // VALIDATION-PENDING: mechanical max above the ~25 mph (11 m/s) road-pod ODD
        // cap. (delivery-av.max_speed)
        max_speed_mps: 12.0,
        max_accel_mps2: 1.8,    // VALIDATION-PENDING: between courier 1.0 and robotaxi 2.5 (delivery-av.accel)
        max_brake_mps2: 4.0,    // VALIDATION-PENDING: firm service brake; ≥ accel (delivery-av.brake)
        max_steering_deg: 33.0, // VALIDATION-PENDING (delivery-av.steering)
        max_steering_rate_deg_s: 40.0, // VALIDATION-PENDING (delivery-av.steering_rate)
        min_follow_distance_m: 3.5,    // VALIDATION-PENDING: ~0.3 s @ 11 m/s + reaction (delivery-av.follow)
        max_lateral_accel_mps2: 2.5,   // VALIDATION-PENDING: between courier 1.5 and robotaxi 3.5 (delivery-av.lat_accel)
        wheelbase_m: 1.9,       // VALIDATION-PENDING: small road pod (delivery-av.wheelbase)
        width_m: 1.1,           // VALIDATION-PENDING: narrow pod footprint (delivery-av.footprint)
        length_m: 2.9,
        overhang_front_m: 0.5,
        overhang_rear_m: 0.5,
        odd_speed_cap_mps: Some(DELIVERY_AV_ODD_SPEED_CAP_MPS),
    }
}

fn delivery_av_mrc() -> VehicleKinematicsContract {
    VehicleKinematicsContract {
        max_speed_mps: 4.0,     // VALIDATION-PENDING (delivery-av.mrc.max_speed)
        max_accel_mps2: 1.0,    // VALIDATION-PENDING (delivery-av.mrc.accel)
        max_brake_mps2: 3.0,    // VALIDATION-PENDING: brake ≥ accel (delivery-av.mrc.brake)
        max_steering_deg: 15.0, // VALIDATION-PENDING (delivery-av.mrc.steering)
        max_steering_rate_deg_s: 20.0, // VALIDATION-PENDING (delivery-av.mrc.steering_rate)
        min_follow_distance_m: 5.0,    // VALIDATION-PENDING: ≥ nominal follow (delivery-av.mrc.follow)
        max_lateral_accel_mps2: 1.5,   // VALIDATION-PENDING (delivery-av.mrc.lat_accel)
        // Footprint IDENTICAL to delivery-av nominal.
        wheelbase_m: 1.9,
        width_m: 1.1,
        length_m: 2.9,
        overhang_front_m: 0.5,
        overhang_rear_m: 0.5,
        odd_speed_cap_mps: None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Every family member, Nominal + MRC, as (name, nominal, mrc).
    fn family() -> Vec<(VehicleClass, VehicleKinematicsContract, VehicleKinematicsContract)> {
        [VehicleClass::Courier, VehicleClass::DeliveryAv, VehicleClass::Robotaxi]
            .into_iter()
            .map(|c| (c, contract_for(c), mrc_fallback_for(c)))
            .collect()
    }

    #[test]
    fn courier_str_roundtrips_case_insensitive() {
        for s in ["courier", "Courier", "COURIER", "  courier  "] {
            assert_eq!(VehicleClass::from_str(s).unwrap(), VehicleClass::Courier, "{s:?}");
        }
        assert_eq!(VehicleClass::from_str("delivery-av").unwrap(), VehicleClass::DeliveryAv);
        assert_eq!(VehicleClass::from_str("ROBOTAXI").unwrap(), VehicleClass::Robotaxi);
        // round-trip through as_str
        for c in [VehicleClass::Courier, VehicleClass::DeliveryAv, VehicleClass::Robotaxi] {
            assert_eq!(VehicleClass::from_str(c.as_str()).unwrap(), c);
        }
    }

    #[test]
    fn unknown_class_is_fail_closed_err() {
        // A typo / garbage / empty must be Err — NEVER a silent fallback class.
        for s in ["", "robotaxii", "delivery_av", "car", "truck", "couri er"] {
            assert!(VehicleClass::from_str(s).is_err(), "expected fail-closed Err for {s:?}");
        }
    }

    #[test]
    fn robotaxi_equals_frozen_nominal_field_for_field() {
        // The zero-drift inheritance proof: contract_for(Robotaxi) IS the frozen
        // instance, verbatim. PartialEq compares every field.
        assert_eq!(
            contract_for(VehicleClass::Robotaxi),
            VehicleKinematicsContract::nominal_reference_profile(),
            "robotaxi nominal must equal the frozen instance field-for-field (zero new numbers)"
        );
    }

    #[test]
    fn robotaxi_mrc_equals_frozen_mrc() {
        assert_eq!(
            mrc_fallback_for(VehicleClass::Robotaxi),
            VehicleKinematicsContract::mrc_fallback_profile(),
        );
    }

    #[test]
    fn uninitialized_global_class_defaults_to_frozen_instance() {
        // #312: production aborts at startup on an unset class (init_..._from_env),
        // but the uninitialized getter (tests / library use that never called init)
        // must resolve to the frozen reference instance so the live gate's default
        // path is byte-identical. (This test does not set the env — INV-13 — and
        // relies on the OnceLock being unset in the test binary.)
        let class = global_vehicle_class();
        assert_eq!(class, VehicleClass::Robotaxi);
        assert_eq!(
            contract_for(class),
            VehicleKinematicsContract::nominal_reference_profile(),
            "the uninitialized default must select the frozen instance verbatim"
        );
    }

    #[test]
    fn cap_ordering_courier_lt_deliveryav_lt_robotaxi() {
        // Ordering sanity on the runtime-effective ceiling: a sidewalk robot is
        // slower than a road pod is slower than a robotaxi.
        let courier = contract_for(VehicleClass::Courier).effective_max_speed_mps();
        let delivery = contract_for(VehicleClass::DeliveryAv).effective_max_speed_mps();
        let robotaxi = contract_for(VehicleClass::Robotaxi).effective_max_speed_mps();
        assert!(courier < delivery, "courier {courier} !< delivery-av {delivery}");
        assert!(delivery < robotaxi, "delivery-av {delivery} !< robotaxi {robotaxi}");
        // (The documented ODD-cap consts agree with this ordering — pinned at
        // compile time via the `const _: () = assert!(...)` checks beside the consts.)
    }

    #[test]
    fn every_profile_brake_geq_accel() {
        for (c, nom, mrc) in family() {
            assert!(nom.max_brake_mps2 >= nom.max_accel_mps2,
                "{c:?} nominal brake {} < accel {}", nom.max_brake_mps2, nom.max_accel_mps2);
            assert!(mrc.max_brake_mps2 >= mrc.max_accel_mps2,
                "{c:?} mrc brake {} < accel {}", mrc.max_brake_mps2, mrc.max_accel_mps2);
        }
    }

    #[test]
    fn every_profile_footprint_positive() {
        for (c, nom, mrc) in family() {
            for (label, k) in [("nominal", nom), ("mrc", mrc)] {
                assert!(k.wheelbase_m > 0.0, "{c:?} {label} wheelbase");
                assert!(k.width_m > 0.0, "{c:?} {label} width");
                assert!(k.length_m > 0.0, "{c:?} {label} length");
                assert!(k.overhang_front_m > 0.0, "{c:?} {label} front overhang");
                assert!(k.overhang_rear_m > 0.0, "{c:?} {label} rear overhang");
            }
        }
    }

    #[test]
    fn every_profile_cap_leq_max_speed() {
        for (c, nom, mrc) in family() {
            for (label, k) in [("nominal", nom), ("mrc", mrc)] {
                if let Some(cap) = k.odd_speed_cap_mps {
                    assert!(cap <= k.max_speed_mps,
                        "{c:?} {label} odd cap {cap} > max_speed {}", k.max_speed_mps);
                }
                // effective ceiling never exceeds the mechanical max.
                assert!(k.effective_max_speed_mps() <= k.max_speed_mps + 1e-9, "{c:?} {label}");
            }
        }
    }

    #[test]
    fn mrc_leq_nominal_per_field_every_class() {
        for (c, nom, mrc) in family() {
            // The limit fields: MRC is no more permissive than Nominal.
            assert!(mrc.max_speed_mps <= nom.max_speed_mps, "{c:?} speed");
            assert!(mrc.max_accel_mps2 <= nom.max_accel_mps2, "{c:?} accel");
            assert!(mrc.max_brake_mps2 <= nom.max_brake_mps2, "{c:?} brake");
            assert!(mrc.max_steering_deg <= nom.max_steering_deg, "{c:?} steering");
            assert!(mrc.max_steering_rate_deg_s <= nom.max_steering_rate_deg_s, "{c:?} steering_rate");
            assert!(mrc.max_lateral_accel_mps2 <= nom.max_lateral_accel_mps2, "{c:?} lat_accel");
            // Following distance is the conservative direction: MRC ≥ Nominal.
            assert!(mrc.min_follow_distance_m >= nom.min_follow_distance_m, "{c:?} follow");
            // Footprint is platform geometry — identical (the vehicle doesn't shrink).
            assert_eq!(mrc.wheelbase_m, nom.wheelbase_m, "{c:?} wheelbase");
            assert_eq!(mrc.width_m, nom.width_m, "{c:?} width");
            assert_eq!(mrc.length_m, nom.length_m, "{c:?} length");
        }
    }
}
