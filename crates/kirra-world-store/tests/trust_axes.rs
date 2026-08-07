//! The four orthogonal trust axes in storage — schema v2.
//!
//! `KIRRA-WM2-SCHEMA-001` ratified v1 with a recorded digest, so the growth was
//! a ruling: **additive, `writer_class` kept permanently** (2026-08-07). These
//! tests assert what that ruling bought, and what it deliberately did not
//! change.
//!
//! The load-bearing property is the one that is easiest to lose: **an
//! unlabelled row must hash exactly as it did under v1**. Every store written
//! before the axes existed has to keep verifying, so the axes are appended to
//! the canonical form only when present. `canonical_form.rs`'s byte pins are the
//! other half of that claim — they were written against the pre-v2 code and
//! still pass unchanged.

use kirra_world_store::{
    canonical_event_json, Adjudication, ClaimStatus, Corroboration, EventId, NewEvent,
    ObservationId, Origin, StoreError, TrustAxes, WorldStore, WriterClass, SCHEMA_VERSION,
};

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-world-axes-{}-{}.sqlite",
        name,
        std::process::id()
    ));
    for s in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(s);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

struct Ids {
    event: EventId,
    observation: ObservationId,
}

fn ids(e: &str, o: &str) -> Ids {
    Ids {
        event: EventId::new(e).unwrap(),
        observation: ObservationId::new(o).unwrap(),
    }
}

fn ev<'a>(id: &'a Ids, trust: Option<&'a TrustAxes>) -> NewEvent<'a> {
    NewEvent {
        event_id: &id.event,
        observation_id: &id.observation,
        txn_time_ms: 1_700_000_000_000,
        valid_from_ms: 1_700_000_000_000,
        valid_to_ms: None,
        source: "sensor-a",
        source_version: "0.1.0",
        writer_class: WriterClass::Sensor,
        claim_status: ClaimStatus::Confirmed,
        provenance: &[],
        frame_id: None,
        map_id: None,
        kind: "observation",
        subject: "cup-1",
        predicate: Some("colour"),
        object: Some("red"),
        payload: r#"{"n":1}"#,
        payload_schema: 1,
        retention_class: "raw",
        trust,
    }
}

/// Confirmed axes, matching `claim_status: Confirmed` as the derivation CHECK
/// requires.
fn confirmed_axes() -> TrustAxes {
    TrustAxes::new(
        Origin::Observed,
        Corroboration::Corroborated(3),
        Adjudication::Confirmed,
    )
    .expect("constructible")
}

// ---------------------------------------------------------------------------
// The compatibility property
// ---------------------------------------------------------------------------

/// **The one that matters most.** An unlabelled row's canonical form is byte
/// for byte the v1 form — no axis keys, not even as nulls.
///
/// Spelling the axes as `null` for unlabelled rows, the way v1's own five
/// optionals are spelled, would have been more uniform and would have broken
/// every store ever written: the bytes change, so the recomputed digest
/// diverges and `verify_chain` reports tampering on rows nobody touched.
#[test]
fn an_unlabelled_row_hashes_exactly_as_it_did_under_v1() {
    let id = ids("e1", "o1");
    let got = canonical_event_json(&ev(&id, None));

    assert!(!got.contains("origin"), "no origin key: {got}");
    assert!(
        !got.contains("corroboration"),
        "no corroboration key: {got}"
    );
    assert!(!got.contains("adjudication"), "no adjudication key: {got}");
    assert!(
        got.ends_with(r#""retention_class":"raw"}"#),
        "the form must still end at retention_class: {got}"
    );
}

/// …and a labelled row's form differs, which is what puts the axes inside the
/// hash rather than beside it.
#[test]
fn a_labelled_row_carries_its_axes_into_the_hashed_bytes() {
    let id = ids("e1", "o1");
    let axes = confirmed_axes();
    let got = canonical_event_json(&ev(&id, Some(&axes)));

    assert!(got.contains(r#""origin":"observed""#), "{got}");
    assert!(got.contains(r#""corroboration":"corroborated""#), "{got}");
    assert!(got.contains(r#""corroboration_n":3"#), "{got}");
    assert!(got.contains(r#""adjudication":"confirmed""#), "{got}");
    assert!(got.ends_with('}'), "still one JSON object: {got}");

    assert_ne!(
        got,
        canonical_event_json(&ev(&id, None)),
        "labelling a row must change its hashed bytes, or the label is unprotected"
    );
}

/// `Uncorroborated` stores no count, and the absent count is spelled `null`
/// rather than `0`. Zero would read as "zero sources agree", which is a claim;
/// the axis's meaning is that there is nothing to cross-check.
#[test]
fn uncorroborated_carries_a_null_count_not_a_zero() {
    let id = ids("e1", "o1");
    let axes = TrustAxes::new(
        Origin::Asserted,
        Corroboration::Uncorroborated,
        Adjudication::Confirmed,
    )
    .unwrap();
    let got = canonical_event_json(&ev(&id, Some(&axes)));
    assert!(got.contains(r#""corroboration_n":null"#), "{got}");
}

// ---------------------------------------------------------------------------
// Round trip and chain coverage
// ---------------------------------------------------------------------------

#[test]
fn a_labelled_row_round_trips_and_verifies() {
    let path = tmp("round-trip");
    let mut s = WorldStore::open(&path).expect("open");
    let id = ids("e1", "o1");
    let axes = confirmed_axes();
    s.append(&ev(&id, Some(&axes))).expect("append");
    s.verify_chain().expect("verifies");
    let _ = std::fs::remove_file(&path);
}

/// Labelled and unlabelled rows coexist in one chain — which is exactly what a
/// migrated store looks like the moment it starts writing v2 rows.
#[test]
fn labelled_and_unlabelled_rows_verify_in_the_same_chain() {
    let path = tmp("mixed");
    let mut s = WorldStore::open(&path).expect("open");
    let axes = confirmed_axes();
    s.append(&ev(&ids("e1", "o1"), None)).expect("v1-shaped");
    s.append(&ev(&ids("e2", "o2"), Some(&axes)))
        .expect("v2-shaped");
    s.append(&ev(&ids("e3", "o3"), None)).expect("v1-shaped");
    s.verify_chain().expect("a mixed chain verifies");
    let _ = std::fs::remove_file(&path);
}

/// **Stripping the axes from a stored row does not turn it back into a valid
/// unlabelled row.** The stored digest was computed over the labelled bytes, so
/// recomputation diverges.
///
/// Without this, the axes would be editable in place — which is the exact
/// weakness SD-2 rejected for `writer_class` and `claim_status`, and the reason
/// the axes are inside the hash rather than beside it.
#[test]
fn stripping_the_axes_breaks_the_chain() {
    let path = tmp("strip");
    let mut s = WorldStore::open(&path).expect("open");
    let axes = confirmed_axes();
    s.append(&ev(&ids("e1", "o1"), Some(&axes)))
        .expect("append");

    // All three at once: the schema CHECK refuses a partial strip, so a
    // tamperer has to clear the whole set — and that is what this proves is
    // still detected.
    s.raw_execute_for_test(
        "UPDATE world_events SET origin = NULL, corroboration = NULL, \
         corroboration_n = NULL, adjudication = NULL WHERE generation = 1",
    )
    .expect("the strip itself is allowed by the schema");

    match s.verify_chain() {
        Err(StoreError::ChainBroken { generation }) => assert_eq!(generation, 1),
        other => panic!("stripping the axes must break the chain, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// The schema CHECKs — enforced against raw SQL, not only against the writer
// ---------------------------------------------------------------------------

/// Blueprint §20: `Predicted` never appears in the evidence store.
/// `TrustAxes::new` refuses it; so does the schema, so the rule holds against
/// SQL that never went through the constructor.
#[test]
fn predicted_origin_is_refused_by_the_schema_not_only_the_constructor() {
    let path = tmp("no-predicted");
    let mut s = WorldStore::open(&path).expect("open");
    let axes = confirmed_axes();
    s.append(&ev(&ids("e1", "o1"), Some(&axes)))
        .expect("append");

    let bad =
        s.raw_execute_for_test("UPDATE world_events SET origin = 'predicted' WHERE generation = 1");
    assert!(
        bad.is_err(),
        "the schema must refuse a predicted origin even via raw SQL"
    );
    let _ = std::fs::remove_file(&path);
}

/// The three stored axes travel together. A row carrying one and not the others
/// is a partial trust record a reader cannot tell from an unlabelled one.
#[test]
fn a_partial_axis_set_is_unwritable_even_by_raw_sql() {
    let path = tmp("partial");
    let mut s = WorldStore::open(&path).expect("open");
    let axes = confirmed_axes();
    s.append(&ev(&ids("e1", "o1"), Some(&axes)))
        .expect("append");

    let bad =
        s.raw_execute_for_test("UPDATE world_events SET adjudication = NULL WHERE generation = 1");
    assert!(
        bad.is_err(),
        "clearing one axis and leaving the others must be refused"
    );
    let _ = std::fs::remove_file(&path);
}

/// **D-2 restated against the axis**, so it survives `claim_status` being
/// dropped later. An LLM may never write a confirmed fact.
#[test]
fn an_llm_cannot_write_a_confirmed_adjudication() {
    let path = tmp("d2-axis");
    let mut s = WorldStore::open(&path).expect("open");

    // Through the writer: refused before the statement is built.
    let axes = confirmed_axes();
    let id = ids("e1", "o1");
    let err = s
        .append(&NewEvent {
            writer_class: WriterClass::LlmCandidate,
            ..ev(&id, Some(&axes))
        })
        .expect_err("must refuse");
    assert!(matches!(err, StoreError::LlmCannotConfirm));

    // And through raw SQL, which is the claim that matters: a candidate row
    // relabelled to an llm_candidate writer must not be able to keep a
    // confirmed adjudication.
    let pending = TrustAxes::new(
        Origin::Derived,
        Corroboration::Uncorroborated,
        Adjudication::Pending,
    )
    .unwrap();
    s.append(&NewEvent {
        writer_class: WriterClass::LlmCandidate,
        claim_status: ClaimStatus::Candidate,
        ..ev(&ids("e2", "o2"), Some(&pending))
    })
    .expect("a candidate is fine");

    let bad = s.raw_execute_for_test(
        "UPDATE world_events SET adjudication = 'confirmed' WHERE generation = 1",
    );
    assert!(
        bad.is_err(),
        "an llm_candidate row must not be updatable to a confirmed adjudication"
    );
    let _ = std::fs::remove_file(&path);
}

/// `claim_status` is now **derived** from `adjudication`, and the derivation is
/// a `CHECK` rather than a convention — so the retained proxy cannot drift from
/// the axis it stands for.
#[test]
fn claim_status_cannot_disagree_with_adjudication() {
    let path = tmp("derivation");
    let mut s = WorldStore::open(&path).expect("open");
    let axes = confirmed_axes();
    s.append(&ev(&ids("e1", "o1"), Some(&axes)))
        .expect("append");

    let bad = s.raw_execute_for_test(
        "UPDATE world_events SET claim_status = 'candidate' WHERE generation = 1",
    );
    assert!(
        bad.is_err(),
        "claim_status must not be movable away from its adjudication"
    );
    let _ = std::fs::remove_file(&path);
}

/// The two states the v1 proxy could not express at all. Rule 3 requires
/// `Ambiguous` to be a stable, reportable state — *"I have conflicting
/// information about that"* — and `Rejected` is terminal under rule 7. Both are
/// storable now, and both map to the `candidate` side of the retained proxy.
#[test]
fn rejected_and_ambiguous_are_storable_states_the_proxy_could_not_express() {
    let path = tmp("four-states");
    let mut s = WorldStore::open(&path).expect("open");

    for (n, adj) in [(1, Adjudication::Rejected), (2, Adjudication::Ambiguous)] {
        let axes = TrustAxes::new(Origin::Observed, Corroboration::Contradicted(2), adj)
            .expect("constructible");
        s.append(&NewEvent {
            claim_status: ClaimStatus::Candidate,
            ..ev(&ids(&format!("e{n}"), &format!("o{n}")), Some(&axes))
        })
        .expect("both are storable");
    }
    s.verify_chain().expect("verifies");
    let _ = std::fs::remove_file(&path);
}

/// Rule 3 is a **read-time** derivation, so storage keeps the adjudication as
/// ruled, not as read.
///
/// `Contradicted + Pending` reads as `Ambiguous` through `TrustAxes::adjudication()`.
/// Storing that derived value would bake in a conclusion that has to be
/// recomputed whenever the corroboration changes — the same reason validity has
/// no column at all.
#[test]
fn rule_three_is_not_baked_into_storage() {
    let id = ids("e1", "o1");
    let axes = TrustAxes::new(
        Origin::Observed,
        Corroboration::Contradicted(2),
        Adjudication::Pending,
    )
    .unwrap();

    assert_eq!(
        axes.adjudication(),
        Adjudication::Ambiguous,
        "rule 3 applies on read"
    );
    assert_eq!(
        axes.adjudication_stored(),
        Adjudication::Pending,
        "…and not to what is stored"
    );

    let got = canonical_event_json(&NewEvent {
        claim_status: ClaimStatus::Candidate,
        ..ev(&id, Some(&axes))
    });
    assert!(
        got.contains(r#""adjudication":"pending""#),
        "storage must keep the ruling, not the derivation: {got}"
    );
}

// ---------------------------------------------------------------------------
// Migration
// ---------------------------------------------------------------------------

/// A fresh store is born at the current version.
#[test]
fn a_new_store_is_stamped_current() {
    let path = tmp("fresh");
    let s = WorldStore::open(&path).expect("open");
    assert_eq!(s.schema_version().expect("version"), SCHEMA_VERSION);
    let _ = std::fs::remove_file(&path);
}

/// **Fail closed on a database from the future.**
///
/// The store had no version check at all before v2: an older binary would open
/// a newer database, read the columns it knew and write rows missing the ones
/// it did not — producing partial trust records in a log whose entire purpose is
/// being trustworthy later.
#[test]
fn a_future_schema_is_refused_rather_than_opened() {
    let path = tmp("future");
    {
        let s = WorldStore::open(&path).expect("open");
        s.raw_execute_for_test(
            "UPDATE world_store_meta SET value = '99' WHERE key = 'schema_version'",
        )
        .expect("stamp a future version");
    }

    match WorldStore::open(&path) {
        Err(StoreError::SchemaFromTheFuture { found, supported }) => {
            assert_eq!(found, 99);
            assert_eq!(supported, SCHEMA_VERSION);
        }
        Err(other) => panic!("wrong refusal: {other:?}"),
        Ok(_) => panic!("a future schema must be refused, not opened"),
    }
    let _ = std::fs::remove_file(&path);
}

/// Reopening is idempotent — the migration must not re-run its DDL and fail on
/// an already-added column.
#[test]
fn reopening_a_current_store_is_idempotent() {
    let path = tmp("idempotent");
    {
        let mut s = WorldStore::open(&path).expect("open");
        s.append(&ev(&ids("e1", "o1"), None)).expect("append");
    }
    let s = WorldStore::open(&path).expect("reopen");
    assert_eq!(s.schema_version().expect("version"), SCHEMA_VERSION);
    s.verify_chain().expect("still verifies");
    let _ = std::fs::remove_file(&path);
}
