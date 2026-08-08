//! Persisting an identity adjudication as evidence — `WM_SCOPE.md` §5.
//!
//! Two things are being demonstrated, and only the second is interesting:
//!
//! 1. Every verb round-trips through a **real `world_events` row**, chain
//!    intact. Written against the store rather than against the codec alone,
//!    because a codec that round-trips in memory and cannot survive `append`'s
//!    constraints has proved nothing.
//! 2. **A malformed row is refused, not repaired.** Decoding goes back through
//!    the domain's constructors, so the refusals are the model's own — the tests
//!    below feed the decoder rows that no honest writer produces and check that
//!    the storage boundary is exactly as strict as the point of judgement.

use kirra_world::adjudication::{
    AssertIdentity, ForgetEntity, IdentityAdjudication, Justification, MergeEntities,
    RetirementReason, SplitEntity,
};
use kirra_world::observation::{ClockDomain, DomainInstant};
use kirra_world::reference::{EntityId, EventId, ObservationId};
use kirra_world_store::adjudication_record::{
    adjudication_provenance, adjudication_subject, decode_adjudication, encode_adjudication,
    AdjudicationDecodeError, ADJUDICATION_KIND, ADJUDICATION_PAYLOAD_SCHEMA,
    ADJUDICATION_RETENTION_CLASS,
};
use kirra_world_store::{ClaimStatus, NewEvent, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;

fn eid(s: &str) -> EntityId {
    EntityId::new(s).expect("entity id")
}

fn oid(s: &str) -> ObservationId {
    ObservationId::new(s).expect("observation id")
}

fn just() -> Justification {
    Justification::new([oid("obs-1"), oid("obs-2")]).expect("justification")
}

fn at() -> DomainInstant {
    DomainInstant {
        ms: 1_700_000_000_000,
        domain: ClockDomain::System,
    }
}

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("kirra-adj-{}-{}.sqlite", name, std::process::id()));
    for suffix in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

fn every_verb() -> Vec<(&'static str, IdentityAdjudication)> {
    vec![
        (
            "assert",
            IdentityAdjudication::Assert(AssertIdentity::new(eid("e-new"), just(), at())),
        ),
        (
            "merge",
            IdentityAdjudication::Merge(
                MergeEntities::new([eid("a"), eid("b")], eid("keep"), just(), at()).expect("merge"),
            ),
        ),
        (
            "split/partition",
            IdentityAdjudication::Split(
                SplitEntity::partition(eid("p"), [eid("p1"), eid("p2")], just(), at())
                    .expect("partition"),
            ),
        ),
        (
            "split/subtraction",
            IdentityAdjudication::Split(
                SplitEntity::subtract(eid("s"), [eid("off")], just(), at()).expect("subtract"),
            ),
        ),
        (
            "forget",
            IdentityAdjudication::Forget(ForgetEntity::new(
                eid("gone"),
                RetirementReason::new("decommissioned").expect("reason"),
                just(),
                at(),
            )),
        ),
    ]
}

/// **Every verb survives a real append and comes back identical**, and the
/// chain still verifies.
///
/// Both split *shapes* are here. They differ only by a token in the payload and
/// by which lifecycle they imply, so an encoding that dropped `shape` would
/// round-trip a partition as a subtraction — silently turning a superseded
/// source back into a surviving one, undoing `KIRRA-WM-SPLIT-SURVIVAL-001` at
/// the storage layer.
#[test]
fn every_verb_round_trips_through_a_real_row() {
    let path = tmp("roundtrip");
    let mut s = WorldStore::open(&path).expect("open");

    for (i, (name, adj)) in every_verb().into_iter().enumerate() {
        let event_id = EventId::new(format!("ev-{i}")).expect("event id");
        let observation_id = oid(&format!("obs-src-{i}"));
        let payload = encode_adjudication(&adj);
        let provenance = adjudication_provenance(&adj);
        let prov_refs: Vec<&str> = provenance.iter().map(String::as_str).collect();
        let subject = adjudication_subject(&adj).as_str().to_owned();

        s.append(&NewEvent {
            event_id: &event_id,
            observation_id: &observation_id,
            txn_time_ms: T0 + i as i64,
            valid_from_ms: T0 + i as i64,
            valid_to_ms: None,
            source: "operator-console",
            source_version: "1.0.0",
            writer_class: WriterClass::Operator,
            claim_status: ClaimStatus::Confirmed,
            provenance: &prov_refs,
            frame_id: None,
            map_id: None,
            kind: ADJUDICATION_KIND,
            subject: &subject,
            subject_ref: None,
            predicate: None,
            object: None,
            payload: &payload,
            payload_schema: ADJUDICATION_PAYLOAD_SCHEMA,
            retention_class: ADJUDICATION_RETENTION_CLASS,
            trust: None,
        })
        .unwrap_or_else(|e| panic!("{name}: append refused: {e:?}"));

        let back = decode_adjudication(&payload, ADJUDICATION_PAYLOAD_SCHEMA, &provenance)
            .unwrap_or_else(|e| panic!("{name}: decode refused: {e}"));
        assert_eq!(back, adj, "{name}: did not round-trip");
    }

    s.verify_chain()
        .expect("the chain must still verify after adjudication rows");
    let _ = std::fs::remove_file(&path);
}

/// The justification rides `provenance`, and is what comes back.
///
/// It is deliberately not duplicated into the payload: two copies of the same
/// set is two things that can disagree, and the column already means "the
/// observation ids this claim rests on".
#[test]
fn the_justification_travels_in_the_provenance_column() {
    let adj = IdentityAdjudication::Assert(AssertIdentity::new(eid("e-1"), just(), at()));
    let payload = encode_adjudication(&adj);

    assert!(
        !payload.contains("obs-1"),
        "the payload must not carry a second copy of the justification: {payload}"
    );
    assert_eq!(
        adjudication_provenance(&adj),
        vec!["obs-1".to_owned(), "obs-2".to_owned()]
    );

    let back = decode_adjudication(
        &payload,
        ADJUDICATION_PAYLOAD_SCHEMA,
        &adjudication_provenance(&adj),
    )
    .expect("decode");
    assert_eq!(back.justification().observations(), just().observations());
}

/// The subject column names the entity the judgement is *about*, per verb.
#[test]
fn the_subject_is_the_entity_the_judgement_is_about() {
    let expected = ["e-new", "keep", "p", "s", "gone"];
    for ((name, adj), want) in every_verb().into_iter().zip(expected) {
        assert_eq!(
            adjudication_subject(&adj).as_str(),
            want,
            "{name}: wrong subject"
        );
    }
}

// -- Fail-closed decoding -------------------------------------------------

/// **The check `kirra_world::resolution` declined to make, made here.**
///
/// The resolver answers a one-element supersession rather than discarding a
/// correct answer, and records that rejecting the malformed row belongs at the
/// decoding boundary. This is that boundary. A stored partition naming one
/// destination is refused by `SplitEntity::partition` itself, so it never
/// becomes a `Lifecycle::Superseded` for the resolver to meet.
#[test]
fn a_stored_partition_into_one_destination_is_refused() {
    let payload = r#"{"verb":"split","shape":"partition","source":"p","into":["only"],
                      "at":{"ms":1,"domain":"system"}}"#;
    let err = decode_adjudication(payload, ADJUDICATION_PAYLOAD_SCHEMA, &["obs-1".to_owned()])
        .expect_err("a partition into one is not a partition");
    assert!(
        matches!(err, AdjudicationDecodeError::Inadmissible { .. }),
        "expected the domain's own refusal, got {err}"
    );
}

/// And the empty case, which is the one the resolver refuses outright.
#[test]
fn a_stored_partition_into_nothing_is_refused() {
    let payload = r#"{"verb":"split","shape":"partition","source":"p","into":[],
                      "at":{"ms":1,"domain":"system"}}"#;
    assert!(matches!(
        decode_adjudication(payload, ADJUDICATION_PAYLOAD_SCHEMA, &["obs-1".to_owned()]),
        Err(AdjudicationDecodeError::Inadmissible { .. })
    ));
}

/// A merge into one of its own sources is a self-redirect — the resolution loop
/// `MergeEntities::new` refuses, refused again on the way back in.
#[test]
fn a_stored_self_merge_is_refused() {
    let payload = r#"{"verb":"merge","sources":["a","b"],"into":"a",
                      "at":{"ms":1,"domain":"system"}}"#;
    assert!(matches!(
        decode_adjudication(payload, ADJUDICATION_PAYLOAD_SCHEMA, &["obs-1".to_owned()]),
        Err(AdjudicationDecodeError::Inadmissible { .. })
    ));
}

/// An adjudication with no evidence is not an adjudication. The empty
/// `provenance` case, refused by `Justification::new`.
#[test]
fn a_row_citing_no_evidence_is_refused() {
    let payload = r#"{"verb":"assert","entity":"e-1","at":{"ms":1,"domain":"system"}}"#;
    assert!(matches!(
        decode_adjudication(payload, ADJUDICATION_PAYLOAD_SCHEMA, &[]),
        Err(AdjudicationDecodeError::Inadmissible { .. })
    ));
}

/// An unknown verb is refused rather than degraded.
///
/// Contrast `ObservationKind`, which degrades an unrecognised token to
/// `Unknown` on purpose. That is right for a *classification* — a consumer can
/// carry on without knowing the kind. It is wrong for a judgement: degrading
/// `EntityMerged` to "some adjudication" would drop the redirect and leave the
/// merged-away id resolving to itself, which §6.3 forbids.
#[test]
fn an_unknown_verb_is_refused_rather_than_degraded() {
    let payload = r#"{"verb":"annihilate","entity":"e-1","at":{"ms":1,"domain":"system"}}"#;
    assert_eq!(
        decode_adjudication(payload, ADJUDICATION_PAYLOAD_SCHEMA, &["obs-1".to_owned()]),
        Err(AdjudicationDecodeError::UnknownVerb {
            token: "annihilate".to_owned()
        })
    );
}

/// An unknown split shape is refused, not defaulted to partition.
///
/// Defaulting would supersede a source that survived — inventing the more
/// destructive of the two readings from a token nobody wrote.
#[test]
fn an_unknown_split_shape_is_refused_not_defaulted() {
    let payload = r#"{"verb":"split","shape":"bisect","source":"p","into":["a","b"],
                      "at":{"ms":1,"domain":"system"}}"#;
    assert_eq!(
        decode_adjudication(payload, ADJUDICATION_PAYLOAD_SCHEMA, &["obs-1".to_owned()]),
        Err(AdjudicationDecodeError::UnknownSplitShape {
            token: "bisect".to_owned()
        })
    );
}

/// An unknown clock domain is refused rather than assumed.
///
/// ADR-0006's non-mixing rule is only enforceable if a stored instant still
/// knows its clock. Defaulting to `System` would let a boundary-domain instant
/// be compared with a system one — the comparison `DomainInstant::compare`
/// exists to refuse.
#[test]
fn an_unknown_clock_domain_is_refused_rather_than_assumed() {
    let payload = r#"{"verb":"assert","entity":"e-1","at":{"ms":1,"domain":"wallclock"}}"#;
    assert_eq!(
        decode_adjudication(payload, ADJUDICATION_PAYLOAD_SCHEMA, &["obs-1".to_owned()]),
        Err(AdjudicationDecodeError::UnknownClockDomain {
            token: "wallclock".to_owned()
        })
    );
}

/// A newer payload schema is refused, matching `assert_schema_not_future`.
#[test]
fn a_future_payload_schema_is_refused() {
    let adj = IdentityAdjudication::Assert(AssertIdentity::new(eid("e-1"), just(), at()));
    let payload = encode_adjudication(&adj);
    assert_eq!(
        decode_adjudication(
            &payload,
            ADJUDICATION_PAYLOAD_SCHEMA + 1,
            &["obs-1".to_owned()]
        ),
        Err(AdjudicationDecodeError::UnsupportedSchema {
            found: ADJUDICATION_PAYLOAD_SCHEMA + 1,
            supported: ADJUDICATION_PAYLOAD_SCHEMA,
        })
    );
}

/// Garbage, a missing key, and a wrong JSON type are each refused distinctly.
#[test]
fn structural_damage_is_refused_with_a_reason() {
    let cases: Vec<(&str, &str)> = vec![
        ("not json at all", "{{{"),
        ("not an object", "[1,2,3]"),
        (
            "missing the verb",
            r#"{"entity":"e-1","at":{"ms":1,"domain":"system"}}"#,
        ),
        (
            "verb is not a string",
            r#"{"verb":7,"at":{"ms":1,"domain":"system"}}"#,
        ),
        ("missing the instant", r#"{"verb":"assert","entity":"e-1"}"#),
        (
            "sources is not an array",
            r#"{"verb":"merge","sources":"a","into":"b","at":{"ms":1,"domain":"system"}}"#,
        ),
        (
            "a negative instant",
            r#"{"verb":"assert","entity":"e-1","at":{"ms":-5,"domain":"system"}}"#,
        ),
        (
            "an inadmissible entity id",
            r#"{"verb":"assert","entity":"","at":{"ms":1,"domain":"system"}}"#,
        ),
    ];
    for (name, payload) in cases {
        assert!(
            decode_adjudication(payload, ADJUDICATION_PAYLOAD_SCHEMA, &["obs-1".to_owned()])
                .is_err(),
            "{name}: decoded a row that should have been refused"
        );
    }
}

/// **A row written under a compactable retention class would lose the redirect.**
///
/// §6.3 requires a merged id to stay resolvable *forever*, and the only thing
/// keeping an adjudication row out of a compaction window is its retention
/// class. Asserted here rather than trusted, because the constant and the
/// protected list are declared in different modules.
#[test]
fn the_adjudication_retention_class_is_protected_from_compaction() {
    assert!(
        kirra_world_store::compaction::is_protected(ADJUDICATION_RETENTION_CLASS),
        "an adjudication row must never be compactable"
    );
}
