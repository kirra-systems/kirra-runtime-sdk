//! #1219 part 2 test surface.
//!
//! Every test here exists to make one clause of the property executable:
//!
//! > Every authority-relevant field and externally applied constraint has
//! > exactly one provenance mapping; inherited mappings identify and equal their
//! > upstream source; serialized representations remain derived from the Rust
//! > contract and existing reviewed integrity pins.
//!
//! Several assertions pass on the current tree, so each is paired with a
//! NEGATIVE that proves the assertion can fail. An always-green check is not
//! evidence — it is decoration.

use std::path::PathBuf;

use super::*;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// ---------------------------------------------------------------------------
// The denominator itself
// ---------------------------------------------------------------------------

/// `Field::ALL` is what every coverage claim is measured against, so a field
/// added to the contract struct and not added here would let the whole register
/// report complete while silently ignoring it.
///
/// Anchored to the struct's own serde shape rather than to a hand-kept count.
#[test]
fn every_contract_struct_field_appears_in_the_field_enum() {
    let json = serde_json::to_value(contract_for(VehicleClass::R2)).expect("contract serializes");
    let obj = json.as_object().expect("contract is a JSON object");

    let mut struct_fields: Vec<&str> = obj.keys().map(String::as_str).collect();
    struct_fields.sort_unstable();

    let mut enum_fields: Vec<&str> = Field::ALL.iter().map(|f| f.name()).collect();
    enum_fields.sort_unstable();

    assert_eq!(
        struct_fields, enum_fields,
        "VehicleKinematicsContract's fields and Field::ALL have diverged — a field \
         missing from Field::ALL escapes every coverage check in this module"
    );
}

#[test]
fn field_names_are_unique() {
    let mut names: Vec<&str> = Field::ALL.iter().map(|f| f.name()).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len(), "duplicate Field::name()");
}

// ---------------------------------------------------------------------------
// 1 + 2 — coverage: exactly one record per (class, profile, field)
// ---------------------------------------------------------------------------

#[test]
fn every_field_of_every_profile_has_exactly_one_provenance_record() {
    let records = all_records();
    if let Err(errors) = check_coverage(&records, ALL_CLASSES) {
        panic!("provenance coverage is incomplete or ambiguous: {errors:#?}");
    }
}

/// The denominator, stated explicitly so a silent change to the class or field
/// list shows up as a number nobody meant to change.
#[test]
fn coverage_spans_the_expected_number_of_field_assignments() {
    let expected = ALL_CLASSES.len() * Profile::ALL.len() * Field::ALL.len();
    assert_eq!(expected, 4 * 2 * 13);

    let records = all_records();
    let mut covered = 0usize;
    for &class in ALL_CLASSES {
        for &profile in Profile::ALL {
            for &field in Field::ALL {
                covered += records
                    .iter()
                    .filter(|r| {
                        r.class == class && r.profile == profile && r.fields.contains(&field)
                    })
                    .count();
            }
        }
    }
    assert_eq!(
        covered, expected,
        "field assignments covered ({covered}) != field assignments that exist ({expected})"
    );
}

// ---------------------------------------------------------------------------
// 3 — non-vacuity: the check catches the omission that actually shipped
// ---------------------------------------------------------------------------

/// The `delivery_av_nominal` defect, reconstructed.
///
/// Before this work, the source comment tagged `(delivery-av.footprint)` as a
/// TRAILING comment on `width_m`, so it governed that one field and left
/// `length_m`, `overhang_front_m` and `overhang_rear_m` with no provenance at
/// all. Crucially, a "every key resolves to a known field" check PASSED on that
/// tree — all 49 tags were valid and unique. Only field-level coverage sees it.
///
/// This reconstructs the narrow record and asserts the checker names all three
/// missing fields.
#[test]
fn the_coverage_check_catches_the_delivery_av_omission_that_shipped() {
    let narrow = FieldProvenance {
        key: "delivery-av.footprint",
        class: VehicleClass::DeliveryAv,
        profile: Profile::Nominal,
        // The bug: width only, exactly as a trailing comment would have bound it.
        fields: &[Field::Width],
        provenance: Provenance::ValidationPending {
            owed: "platform geometry measurement",
        },
    };

    let records: Vec<FieldProvenance> = all_records()
        .into_iter()
        .map(|r| if r.key == narrow.key { narrow } else { r })
        .collect();

    let errors = check_coverage(&records, ALL_CLASSES)
        .expect_err("a table missing three fields must NOT report complete");

    for missing in ["length_m", "overhang_front_m", "overhang_rear_m"] {
        assert!(
            errors.contains(&CoverageError::Uncovered {
                class: "delivery-av",
                profile: "nominal",
                field: missing,
            }),
            "coverage check did not report {missing} as uncovered; got {errors:#?}"
        );
    }
    assert_eq!(
        errors.len(),
        3,
        "exactly the three known-missing fields should fail; got {errors:#?}"
    );
}

/// The other direction: two records governing one field is also a defect,
/// because provenance would be ambiguous — a reader could not say which
/// classification applies.
#[test]
fn the_coverage_check_catches_a_double_covered_field() {
    let mut records = all_records();
    records.push(FieldProvenance {
        key: "r2.wheelbase.duplicate",
        class: VehicleClass::R2,
        profile: Profile::Nominal,
        fields: &[Field::Wheelbase],
        provenance: Provenance::Estimate {
            basis: "a second, conflicting claim",
        },
    });

    let errors = check_coverage(&records, ALL_CLASSES).expect_err("double coverage must fail");
    assert!(
        errors.iter().any(|e| matches!(
            e,
            CoverageError::DoubleCovered {
                field: "wheelbase_m",
                class: "r2",
                ..
            }
        )),
        "expected a DoubleCovered report for r2 wheelbase_m; got {errors:#?}"
    );
}

#[test]
fn the_coverage_check_catches_a_duplicate_key() {
    let mut records = all_records();
    let first = records[0];
    records.push(FieldProvenance {
        // Same key, different (unused) field set — the key is the identity.
        fields: &[],
        ..first
    });

    let errors = check_coverage(&records, ALL_CLASSES).expect_err("duplicate keys must fail");
    assert!(
        errors.contains(&CoverageError::DuplicateKey { key: first.key }),
        "expected DuplicateKey for {}; got {errors:#?}",
        first.key
    );
}

// ---------------------------------------------------------------------------
// 4 — inheritance resolves AND compares equal
// ---------------------------------------------------------------------------

#[test]
fn every_inherited_record_resolves_and_equals_its_upstream() {
    let records = all_records();
    match check_inheritance(&records) {
        Ok(verified) => assert!(verified > 0, "no inheritance relationships were verified"),
        Err(errors) => panic!("inheritance claims are false or unresolvable: {errors:#?}"),
    }
}

/// The authored classes carry exactly 15 inheritance relationships: three
/// classes × (wheelbase + the four footprint fields).
///
/// Six of them — the two overhangs on each class — were asserted NOWHERE before
/// this module. `mrc_leq_nominal_per_field_every_class` compares wheelbase,
/// width and length under a comment claiming the whole footprint is identical.
/// The overhangs are the fields that place the swept-footprint corners in
/// `containment.rs`.
#[test]
fn the_authored_classes_carry_fifteen_verified_inheritance_relationships() {
    let verified = check_inheritance(AUTHORED_PROVENANCE).expect("authored inheritance holds");
    assert_eq!(
        verified, 15,
        "expected 15 authored inheritance field-comparisons"
    );
}

#[test]
fn the_overhangs_are_among_the_inheritance_relationships_now_verified() {
    let inherits_overhangs = |class: VehicleClass| {
        AUTHORED_PROVENANCE.iter().any(|r| {
            r.class == class
                && r.profile == Profile::Mrc
                && matches!(r.provenance, Provenance::Inherited { .. })
                && r.fields.contains(&Field::OverhangFront)
                && r.fields.contains(&Field::OverhangRear)
        })
    };
    for class in [
        VehicleClass::Courier,
        VehicleClass::DeliveryAv,
        VehicleClass::R2,
    ] {
        assert!(
            inherits_overhangs(class),
            "{} MRC overhangs carry no verified inheritance claim",
            class.as_str()
        );
    }
}

/// NON-VACUITY for the inheritance check.
///
/// All 15 relationships hold today, so the positive test above would pass even
/// if the comparison were a no-op. This constructs a record whose claimed
/// upstream genuinely differs — courier MRC `max_speed` is 1.0, courier nominal
/// is 3.0 — and asserts the check reports the divergence.
#[test]
fn the_inheritance_comparison_actually_compares_values() {
    let mut records = all_records();
    // Replace the courier MRC max-speed record with a false inheritance claim.
    for r in &mut records {
        if r.key == "courier.mrc.max_speed" {
            r.provenance = Provenance::Inherited {
                upstream: "courier.max_speed",
            };
        }
    }

    let errors = check_inheritance(&records).expect_err("a false inheritance claim must fail");
    assert!(
        errors.iter().any(|e| matches!(
            e,
            InheritanceError::ValueDiverged {
                key: "courier.mrc.max_speed",
                field: "max_speed_mps",
                ..
            }
        )),
        "expected a ValueDiverged report; got {errors:#?}"
    );
}

#[test]
fn an_unresolvable_upstream_is_reported_not_ignored() {
    let mut records = all_records();
    for r in &mut records {
        if r.key == "r2.mrc.wheelbase" {
            r.provenance = Provenance::Inherited {
                upstream: "r2.no-such-key",
            };
        }
    }
    let errors = check_inheritance(&records).expect_err("a dangling upstream must fail");
    assert!(
        errors.contains(&InheritanceError::UpstreamMissing {
            key: "r2.mrc.wheelbase",
            upstream: "r2.no-such-key",
        }),
        "expected UpstreamMissing; got {errors:#?}"
    );
}

/// Inheritance is within a class. Crossing one would import another platform's
/// geometry under the label "inherited", which is how a courier wheelbase ends
/// up on an R2.
#[test]
fn inheritance_across_classes_is_refused() {
    let mut records = all_records();
    for r in &mut records {
        if r.key == "r2.mrc.wheelbase" {
            r.provenance = Provenance::Inherited {
                upstream: "courier.wheelbase",
            };
        }
    }
    let errors = check_inheritance(&records).expect_err("cross-class inheritance must fail");
    assert!(
        errors.contains(&InheritanceError::UpstreamWrongClass {
            key: "r2.mrc.wheelbase",
            upstream: "courier.wheelbase",
        }),
        "expected UpstreamWrongClass; got {errors:#?}"
    );
}

// ---------------------------------------------------------------------------
// 5 — the `None` MRC caps carry a precondition, and it holds
// ---------------------------------------------------------------------------

#[test]
fn every_none_cap_precondition_holds() {
    let records = all_records();
    let checked = check_preconditions(&records).expect("precondition rules hold");
    assert!(checked > 0, "no preconditions were actually executed");
}

/// The three MRC `None` caps are neither inherited nor unattributed: leaving the
/// cap absent is safe only because the crawl already sits at or below the class
/// ODD cap, so `min()` selects the crawl either way.
///
/// The inequality holds for every class today and was asserted nowhere.
#[test]
fn the_mrc_crawl_sits_at_or_below_the_class_odd_cap_for_every_class() {
    for &class in ALL_CLASSES {
        let mrc = mrc_fallback_for(class);
        let cap = class_odd_cap_mps(class);
        assert!(
            mrc.max_speed_mps <= cap,
            "{}: MRC crawl {} exceeds the class ODD cap {cap} — leaving odd_speed_cap_mps None \
             would raise the degraded ceiling above the nominal one",
            class.as_str(),
            mrc.max_speed_mps
        );
    }
}

/// NON-VACUITY for the precondition check: claiming the rule on a profile that
/// actually carries a cap is a mis-classification, and must be reported rather
/// than silently counted as satisfied.
#[test]
fn claiming_the_none_rule_on_a_profile_that_has_a_cap_is_refused() {
    let mut records = all_records();
    for r in &mut records {
        // r2 NOMINAL carries Some(1.0) — the rule does not apply there.
        if r.key == "r2.odd_cap" {
            r.provenance = Provenance::DerivedPrecondition {
                rule: PreconditionRule::MrcCrawlAtOrBelowClassOddCap,
            };
        }
    }
    let errors = check_preconditions(&records).expect_err("a misapplied rule must fail");
    assert!(
        errors.iter().any(|e| e.key == "r2.odd_cap"),
        "expected r2.odd_cap to be reported; got {errors:#?}"
    );
}

/// Regression pin for a mis-classification this checker caught on its first run.
///
/// `robotaxi.odd_cap` was initially written as
/// `MrcCrawlAtOrBelowClassOddCap` — the same rule the three MRC caps use. It is
/// wrong: robotaxi NOMINAL `max_speed_mps` is 35.0, far ABOVE the 22.35 class
/// cap, so "the crawl already sits below the cap" justifies nothing there. Its
/// `None` is justified by the deployment applying the cap externally, which is a
/// different claim with a different obligation attached.
///
/// A prose rationale would have carried the wrong reason indefinitely. Executing
/// the rule rejected it immediately.
#[test]
fn the_crawl_below_cap_rule_does_not_justify_the_robotaxi_nominal_none() {
    let mut records = all_records();
    for r in &mut records {
        if r.key == "robotaxi.odd_cap" {
            r.provenance = Provenance::DerivedPrecondition {
                rule: PreconditionRule::MrcCrawlAtOrBelowClassOddCap,
            };
        }
    }
    let errors =
        check_preconditions(&records).expect_err("the wrong rule must not justify this value");
    assert!(
        errors
            .iter()
            .any(|e| e.key == "robotaxi.odd_cap" && e.detail.contains("EXCEEDS")),
        "expected the exceeds-the-cap detail; got {errors:#?}"
    );
}

/// The externally-applied claim carries an obligation: a participating
/// constraint must exist AND bind. Otherwise "applied externally" is an
/// assertion that nothing caps the profile at all — the most dangerous shape a
/// provenance record could take, because it reads as deliberate.
#[test]
fn claiming_external_application_on_a_profile_that_has_its_own_cap_is_refused() {
    let mut records = all_records();
    for r in &mut records {
        // r2 nominal carries Some(1.0) in the contract — nothing is external.
        if r.key == "r2.odd_cap" {
            r.provenance = Provenance::DerivedPrecondition {
                rule: PreconditionRule::CapAppliedExternally,
            };
        }
    }
    let errors = check_preconditions(&records).expect_err("a misapplied rule must fail");
    assert!(
        errors.iter().any(|e| e.key == "r2.odd_cap"),
        "expected r2.odd_cap to be reported; got {errors:#?}"
    );
}

// ---------------------------------------------------------------------------
// 6 — all four ODD caps present
// ---------------------------------------------------------------------------

#[test]
fn all_four_odd_caps_have_provenance() {
    let records = all_records();
    for &class in ALL_CLASSES {
        let owner = records
            .iter()
            .find(|r| {
                r.class == class
                    && r.profile == Profile::Nominal
                    && r.fields.contains(&Field::OddSpeedCap)
            })
            .unwrap_or_else(|| {
                panic!(
                    "{} nominal ODD cap has no provenance record",
                    class.as_str()
                )
            });
        assert_ne!(
            owner.provenance.token(),
            "UNATTRIBUTED",
            "{} ODD cap is unattributed",
            class.as_str()
        );
    }
}

/// The caps live in a different comment dialect (`///` doc comments on the
/// constants) from the per-field records, which is exactly why a generator
/// written against one dialect drops them. For the R2 the dropped row would have
/// been the one that actually binds: the cap is BELOW `max_speed_mps`, so it is
/// the effective ceiling.
#[test]
fn the_r2_odd_cap_is_the_binding_ceiling_not_its_max_speed() {
    let r2 = contract_for(VehicleClass::R2);
    assert!(
        r2.odd_speed_cap_mps.expect("r2 carries an ODD cap") < r2.max_speed_mps,
        "the R2 ODD cap no longer binds; this test's premise has changed"
    );
    assert_eq!(
        effective_ceiling_mps(VehicleClass::R2, Profile::Nominal),
        r2.odd_speed_cap_mps.unwrap()
    );
}

// ---------------------------------------------------------------------------
// 7 — robotaxi's external cap participates in the derived ceiling
// ---------------------------------------------------------------------------

/// The asymmetry the manifest must not hide.
///
/// `contract_for(Robotaxi)` returns the frozen instance with
/// `odd_speed_cap_mps: None`, because the deployment applies the ODD cap
/// elsewhere. A register that serialized only the contract would publish a
/// ceiling far above the enforced one.
#[test]
fn the_robotaxi_ceiling_is_not_derivable_from_the_contract_alone() {
    let contract = contract_for(VehicleClass::Robotaxi);
    assert!(
        contract.odd_speed_cap_mps.is_none(),
        "the frozen robotaxi instance now carries a cap; this test's premise has changed"
    );

    let contract_only = contract.effective_max_speed_mps();
    let composed = effective_ceiling_mps(VehicleClass::Robotaxi, Profile::Nominal);

    assert!(
        composed < contract_only,
        "the external cap must TIGHTEN the ceiling: composed {composed} !< contract-only {contract_only}"
    );
    assert_eq!(
        composed, ROBOTAXI_ODD_SPEED_CAP_MPS,
        "the composed ceiling must be the externally applied cap"
    );
}

#[test]
fn the_robotaxi_external_constraint_is_represented_with_its_source() {
    let c = EXTERNAL_CONSTRAINTS
        .iter()
        .find(|c| c.class == VehicleClass::Robotaxi)
        .expect("the robotaxi external cap is registered");

    assert!(c.participates_in_effective_ceiling);
    assert_eq!(c.constrains, Field::MaxSpeed);
    assert_eq!(c.value_mps, ROBOTAXI_ODD_SPEED_CAP_MPS);
    assert!(
        !c.applied_at.is_empty(),
        "where it is applied must be recorded"
    );
    assert!(
        !c.supplied_by.is_empty(),
        "what supplies it must be recorded"
    );
}

/// The classes whose caps DO live in the contract must not acquire a phantom
/// external constraint — their ceiling is derivable from the serialized contract
/// alone, and saying otherwise would misrepresent where authority sits.
#[test]
fn only_robotaxi_needs_an_external_constraint() {
    for class in [
        VehicleClass::Courier,
        VehicleClass::DeliveryAv,
        VehicleClass::R2,
    ] {
        assert!(
            !EXTERNAL_CONSTRAINTS.iter().any(|c| c.class == class),
            "{} should carry its cap in the contract, not externally",
            class.as_str()
        );
        assert_eq!(
            effective_ceiling_mps(class, Profile::Nominal),
            contract_for(class).effective_max_speed_mps(),
            "{}'s ceiling must be derivable from its contract alone",
            class.as_str()
        );
    }
}

// ---------------------------------------------------------------------------
// 8 — MRC-effective <= Nominal-effective (the gap the register closes)
// ---------------------------------------------------------------------------

/// `mrc_leq_nominal_per_field_every_class` compares `mrc.max_speed_mps` against
/// `nom.max_speed_mps` — the MECHANICAL maxima. But Nominal's enforced ceiling
/// is `min(max_speed, odd_cap)`, and MRC has no cap.
///
/// A courier MRC crawl of 2.8 would pass that assertion (2.8 <= 3.0) while
/// exceeding the class's own nominal enforced ceiling of 2.5 — more permissive
/// in degraded posture than in nominal, in the only sense that reaches the
/// wheels. Nothing asserted the effective comparison until now.
#[test]
fn mrc_effective_ceiling_never_exceeds_nominal_effective_ceiling() {
    for &class in ALL_CLASSES {
        let nominal = effective_ceiling_mps(class, Profile::Nominal);
        let mrc = effective_ceiling_mps(class, Profile::Mrc);
        assert!(
            mrc <= nominal,
            "{}: MRC effective ceiling {mrc} exceeds nominal effective ceiling {nominal} — \
             degraded posture would be more permissive than nominal at the wheels",
            class.as_str()
        );
    }
}

/// The gap is real, not hypothetical: for at least one class the nominal
/// MECHANICAL maximum is strictly above the nominal ENFORCED ceiling, which is
/// the room in which an MRC crawl could hide.
#[test]
fn the_mechanical_maximum_exceeds_the_enforced_ceiling_somewhere() {
    let gap_exists = ALL_CLASSES.iter().any(|&class| {
        let c = contract_for(class);
        c.effective_max_speed_mps() < c.max_speed_mps
    });
    assert!(
        gap_exists,
        "no class has a binding ODD cap, so the effective-vs-mechanical distinction \
         this test guards would be vacuous"
    );
}

// ---------------------------------------------------------------------------
// 9 — the sidecar is valid only against the EXISTING talisman pin
// ---------------------------------------------------------------------------

#[test]
fn the_robotaxi_sidecar_validates_against_the_reviewed_talisman_pin() {
    match validate_robotaxi_sidecar(&repo_root()) {
        Ok(blob) => assert_eq!(blob.len(), 40, "a git blob hash is 40 hex chars"),
        Err(e) => panic!(
            "the robotaxi provenance sidecar is not valid against the reviewed pin in {}: {e:?}",
            TALISMAN_PIN_DOC
        ),
    }
}

/// The sidecar binds to the pin `ci/build_safety_case.py` and the `ci.yml`
/// rustfmt lane already gate on — the same path, the same document. A second
/// pin would give the anti-drift mechanism its own drift problem.
#[test]
fn the_sidecar_binds_to_the_same_artifacts_the_safety_case_gates_on() {
    let ci = std::fs::read_to_string(repo_root().join("ci/build_safety_case.py"))
        .expect("the safety-case builder is readable");
    assert!(
        ci.contains(TALISMAN_PATH),
        "build_safety_case.py no longer references {TALISMAN_PATH}; the sidecar and the \
         safety case have diverged on which file is frozen"
    );
    assert!(
        ci.contains(TALISMAN_PIN_DOC),
        "build_safety_case.py no longer references {TALISMAN_PIN_DOC}; the sidecar and the \
         safety case have diverged on where the pin lives"
    );
}

/// The sidecar must describe the frozen class COMPLETELY. A partially-populated
/// sidecar that still reported valid would be the "looks complete while empty"
/// failure the field-level inversion exists to prevent.
#[test]
fn the_sidecar_covers_every_field_of_the_frozen_class() {
    check_coverage(ROBOTAXI_SIDECAR, &[VehicleClass::Robotaxi])
        .expect("the robotaxi sidecar must cover every field of both profiles");
}

#[test]
fn a_sidecar_missing_a_field_is_refused() {
    let short: Vec<FieldProvenance> = ROBOTAXI_SIDECAR
        .iter()
        .copied()
        .filter(|r| r.key != "robotaxi.mrc.follow")
        .collect();
    let errors = check_coverage(&short, &[VehicleClass::Robotaxi])
        .expect_err("a sidecar with a hole must not validate");
    assert!(
        errors.contains(&CoverageError::Uncovered {
            class: "robotaxi",
            profile: "mrc",
            field: "min_follow_distance_m",
        }),
        "expected the missing field to be named; got {errors:#?}"
    );
}

// ---------------------------------------------------------------------------
// 10 — unattributed values are visible, not absent
// ---------------------------------------------------------------------------

/// Nothing in the shipped table is unattributed. The point of the test is that
/// an unattributed value would be REPRESENTABLE and therefore VISIBLE — the
/// failure mode being guarded against is a field disappearing from the register
/// rather than appearing in it with an honest empty classification.
#[test]
fn nothing_in_the_shipped_table_is_unattributed() {
    let offenders: Vec<&str> = all_records()
        .iter()
        .filter(|r| matches!(r.provenance, Provenance::Unattributed))
        .map(|r| r.key)
        .collect();
    assert!(
        offenders.is_empty(),
        "unattributed provenance records: {offenders:?}"
    );
}

#[test]
fn an_unattributed_field_renders_visibly_rather_than_vanishing() {
    let records: Vec<FieldProvenance> = all_records()
        .into_iter()
        .map(|mut r| {
            if r.key == "r2.follow" {
                r.provenance = Provenance::Unattributed;
            }
            r
        })
        .collect();

    let manifest =
        render_manifest(&records, &[VehicleClass::R2], None).expect("coverage is still total");
    let row = manifest
        .profiles
        .iter()
        .find(|p| p.profile == "nominal")
        .expect("r2 nominal")
        .fields
        .iter()
        .find(|f| f.field == Field::MinFollow)
        .expect("the field is still present");

    assert_eq!(row.provenance_class, "UNATTRIBUTED");
    assert!(
        !row.validated,
        "an unattributed value must not read as validated"
    );
    assert!(
        matches!(row.value, FieldValue::Scalar(_)),
        "the VALUE is still reported — it is the provenance that is absent, not the number"
    );
}

/// A manifest must never render over an incomplete table: a missing row would
/// present as authoritative while being silently partial.
#[test]
fn rendering_refuses_an_incomplete_table() {
    let short: Vec<FieldProvenance> = all_records()
        .into_iter()
        .filter(|r| r.key != "r2.overhangs")
        .collect();
    render_manifest(&short, ALL_CLASSES, None)
        .expect_err("a manifest must not render over a hole in the table");
}

// ---------------------------------------------------------------------------
// 11 — serialization: derived, and lossless over the provenance metadata
// ---------------------------------------------------------------------------

#[test]
fn provenance_class_source_status_and_upstream_survive_a_round_trip() {
    let blob = validate_robotaxi_sidecar(&repo_root()).ok();
    let manifest =
        render_manifest(&all_records(), ALL_CLASSES, blob).expect("the shipped table is complete");

    let json = serde_json::to_string(&manifest).expect("manifest serializes");
    let back: ProvenanceManifest = serde_json::from_str(&json).expect("manifest deserializes");

    assert_eq!(manifest, back, "the manifest is not round-trip stable");

    // Spot-check the four things the property names, on rows that exercise each.
    let r2_nominal = back
        .profiles
        .iter()
        .find(|p| p.class == "r2" && p.profile == "nominal")
        .expect("r2 nominal survives");

    let steering = r2_nominal
        .fields
        .iter()
        .find(|f| f.field == Field::MaxSteering)
        .expect("steering survives");
    assert_eq!(steering.provenance_class, "MEASURED");
    assert!(steering.validated, "validation status survives");
    assert!(
        steering.provenance_detail.contains("Phase C"),
        "the source citation survives: {}",
        steering.provenance_detail
    );

    let overhangs = r2_nominal
        .fields
        .iter()
        .find(|f| f.field == Field::OverhangFront)
        .expect("overhang survives");
    assert_eq!(overhangs.provenance_class, "ESTIMATE");
    assert!(!overhangs.validated, "an estimate is not validated");

    let r2_mrc = back
        .profiles
        .iter()
        .find(|p| p.class == "r2" && p.profile == "mrc")
        .expect("r2 mrc survives");
    let inherited = r2_mrc
        .fields
        .iter()
        .find(|f| f.field == Field::Wheelbase)
        .expect("wheelbase survives");
    assert_eq!(inherited.provenance_class, "INHERITED");
    assert_eq!(
        inherited.upstream.as_deref(),
        Some("r2.wheelbase"),
        "the upstream relationship survives"
    );
}

/// The manifest is DERIVED. Every number in it must equal what the live contract
/// says right now — not a value transcribed into this module.
#[test]
fn every_manifest_number_comes_from_the_live_contract() {
    let manifest = render_manifest(&all_records(), ALL_CLASSES, None).expect("complete");

    for p in &manifest.profiles {
        let class = p
            .class
            .parse::<VehicleClass>()
            .expect("the manifest's class id round-trips through from_str");
        let profile = if p.profile == "nominal" {
            Profile::Nominal
        } else {
            Profile::Mrc
        };
        let contract = profile.contract(class);

        for f in &p.fields {
            assert!(
                field_values_identical(f.value, f.field.read(&contract)),
                "{}/{} {} diverged from the live contract",
                p.class,
                p.profile,
                f.name
            );
        }
        assert_eq!(
            p.contract_only_ceiling_mps,
            contract.effective_max_speed_mps()
        );
        assert_eq!(
            p.effective_ceiling_mps,
            effective_ceiling_mps(class, profile)
        );
    }
}

/// The one number this module names is a REFERENCE to the canonical constant.
/// Writing `22.35` here instead would recreate the duplicate-authority problem
/// the whole register exists to prevent.
#[test]
fn the_external_constraint_value_is_the_canonical_constant_not_a_literal() {
    let c = EXTERNAL_CONSTRAINTS
        .iter()
        .find(|c| c.class == VehicleClass::Robotaxi)
        .expect("registered");
    assert_eq!(c.value_mps.to_bits(), ROBOTAXI_ODD_SPEED_CAP_MPS.to_bits());
}

/// The manifest records the talisman blob it was validated against, so a
/// rendered register is traceable to the frozen artifact it describes.
#[test]
fn the_manifest_records_the_talisman_blob_it_was_validated_against() {
    let blob = validate_robotaxi_sidecar(&repo_root()).expect("pin matches");
    let manifest =
        render_manifest(&all_records(), ALL_CLASSES, Some(blob.clone())).expect("complete");
    assert_eq!(manifest.talisman_blob.as_deref(), Some(blob.as_str()));
}

// ---------------------------------------------------------------------------
// Closing the loop — the table must AGREE with the comments it derives from
// ---------------------------------------------------------------------------

fn contract_profiles_source() -> String {
    std::fs::read_to_string(repo_root().join("src/gateway/contract_profiles.rs"))
        .expect("contract_profiles.rs is readable")
}

/// Without this test the table is a SECOND authored inventory of
/// classifications, free to drift from the prose sitting beside the numbers.
/// That is the failure this whole register exists to prevent, reintroduced one
/// level up.
#[test]
fn the_table_agrees_with_the_source_comments_it_derives_from() {
    let source = contract_profiles_source();
    match check_source_agreement(&source, &all_records()) {
        // `check_source_agreement` errors on any tag it cannot match, so Ok(n)
        // already means every extracted tag was compared. The floor guards the
        // other direction: a parser that silently stopped matching would return
        // Ok(0) and this check would pass while asserting nothing. 52 tags
        // extract today — 49 field-dialect plus the 3 const-dialect ODD caps.
        Ok(agreed) => assert!(
            agreed >= 50,
            "only {agreed} tags were compared — the parser has probably stopped \
             matching the source, which would make this check silently vacuous"
        ),
        Err(errors) => panic!("table and source comments disagree: {errors:#?}"),
    }
}

/// THE substring hazard, pinned.
///
/// `r2.overhangs` is tagged `ESTIMATE (overhang split UNMEASURED)`. `UNMEASURED`
/// contains `MEASURED`, so a `contains`-based parser classifies the one estimate
/// in the file as a measurement — in the OPTIMISTIC direction, on the field that
/// places the swept-footprint corners.
#[test]
fn unmeasured_prose_never_classifies_a_row_as_measured() {
    let tags = extract_source_tags(&contract_profiles_source());
    let overhangs = tags
        .iter()
        .find(|t| t.key == "r2.overhangs")
        .expect("the r2 overhang tag is present in the source");
    assert_eq!(
        overhangs.token, "ESTIMATE",
        "the anchored parser must read the LEADING token, not any occurrence"
    );

    // And the naive parser really would get it wrong — so the anchoring is
    // load-bearing rather than defensive.
    let naive_line = "ESTIMATE (overhang split UNMEASURED): (length - wheelbase)/2";
    assert!(
        naive_line.contains("MEASURED"),
        "the hazard this test guards has disappeared from the source; re-check the premise"
    );
}

/// Both dialects must be recognised. The ODD caps live in `///` doc comments
/// with the token bolded mid-sentence; a parser written only for the field
/// dialect drops all four, and the R2's is the effective ceiling.
#[test]
fn both_comment_dialects_are_recognised_including_all_four_odd_caps() {
    let tags = extract_source_tags(&contract_profiles_source());

    for key in ["courier.odd_cap", "delivery-av.odd_cap", "r2.odd_cap"] {
        assert!(
            tags.iter().any(|t| t.key == key),
            "{key} was not extracted — the const dialect is being dropped"
        );
    }
    // Field dialect, for contrast.
    for key in ["r2.steering", "r2.wheelbase", "courier.brake"] {
        assert!(
            tags.iter().any(|t| t.key == key),
            "{key} was not extracted — the field dialect is being dropped"
        );
    }
}

/// Narrative mentions carry a classification word and no stable tag. Keying on
/// the tag excludes them; keying on the classification would invent rows.
#[test]
fn narrative_mentions_produce_no_rows() {
    let narrative = "\
// The FIRST profile with MEASURED geometry. Footprint + full-lock steering are
// bench numbers; the DYNAMIC limits and the overhang split are still
// VALIDATION-PENDING estimates.
";
    assert!(
        extract_source_tags(narrative).is_empty(),
        "prose with no stable tag must not produce a provenance row"
    );
}

/// NON-VACUITY for the agreement check: a divergence must be reported, not
/// quietly tolerated.
#[test]
fn a_comment_that_disagrees_with_the_table_is_reported() {
    // The source says MEASURED for r2.steering; claim ESTIMATE in the table.
    let records: Vec<FieldProvenance> = all_records()
        .into_iter()
        .map(|mut r| {
            if r.key == "r2.steering" {
                r.provenance = Provenance::Estimate {
                    basis: "a claim the source contradicts",
                };
            }
            r
        })
        .collect();

    let errors = check_source_agreement(&contract_profiles_source(), &records)
        .expect_err("a divergence must fail");
    assert!(
        errors.iter().any(|e| matches!(
            e,
            SourceAgreementError::Divergent { key, table_token: "ESTIMATE", .. } if key == "r2.steering"
        )),
        "expected a Divergent report for r2.steering; got {errors:#?}"
    );
}

/// A tag in the source that no record claims means the table has a hole the
/// coverage check cannot see — coverage is measured over FIELDS, and a stray tag
/// names a key.
#[test]
fn a_source_tag_with_no_matching_record_is_reported() {
    let source = "        // MEASURED: something new. (r2.brand_new_thing)\n";
    let errors =
        check_source_agreement(source, &all_records()).expect_err("an unknown key must fail");
    assert_eq!(
        errors,
        vec![SourceAgreementError::UnknownKey {
            key: "r2.brand_new_thing".to_string()
        }]
    );
}
