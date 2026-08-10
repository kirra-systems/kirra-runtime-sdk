//! **`KIRRA-WM-CLAIM-SHAPES-001` — an object-bearing claim requires a predicate.**
//!
//! | `predicate` | `object` | shape |
//! |---|---|---|
//! | `None` | `None` | payload-only claim |
//! | `Some` | `None` | predicate + payload claim |
//! | `Some` | `Some` | subject-predicate-object + payload claim |
//! | `None` | `Some` | **INVALID** |
//!
//! WHY THE FOURTH IS REFUSED, and it is not a matter of taste. `world_current`
//! keys on `(subject, predicate_key)` where `predicate_key` is the predicate or
//! `''`, so an object-without-predicate claim occupies the SAME slot as a
//! payload-only claim about that subject. The later silently replaces the
//! earlier. `the_two_predicateless_shapes_no_longer_alias` below is the measured
//! form of that: against the pre-ruling code it leaves ONE row with the
//! payload-only claim gone.
//!
//! The rule is enforced at two layers because one is not enough: the Rust
//! admission check names the rule in its error, and the v5 trigger makes SQLite
//! itself refuse — so a raw `INSERT` cannot route around a polite decoder.

use kirra_world_store::{
    ClaimStatus, EventId, NewEvent, ObservationId, StoreError, WorldStore, WriterClass,
};

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-claim-shapes-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);
    p
}

#[allow(clippy::too_many_arguments)]
fn event<'a>(
    e: &'a EventId,
    o: &'a ObservationId,
    t: i64,
    subject: &'a str,
    predicate: Option<&'a str>,
    object: Option<&'a str>,
    payload: &'a str,
) -> NewEvent<'a> {
    NewEvent {
        event_id: e,
        observation_id: o,
        txn_time_ms: t,
        valid_from_ms: t,
        valid_to_ms: None,
        source: "shapes-test",
        source_version: "1.0.0",
        writer_class: WriterClass::Sensor,
        claim_status: ClaimStatus::Confirmed,
        provenance: &[],
        frame_id: None,
        map_id: None,
        kind: "mission",
        subject,
        subject_ref: None,
        predicate,
        object,
        payload,
        payload_schema: 1,
        retention_class: "raw",
        trust: None,
    }
}

// ---------------------------------------------------------------------------
// The three valid shapes are admitted
// ---------------------------------------------------------------------------

#[test]
fn the_three_valid_shapes_are_admitted() {
    let path = tmp("valid");
    let mut s = WorldStore::open(&path).expect("open");

    for (n, predicate, object) in [
        (1, None, None),
        (2, Some("holds"), None),
        (3, Some("last_seen_at"), Some("dock_b")),
    ] {
        let e = EventId::new(format!("ev-{n}")).expect("id");
        let o = ObservationId::new(format!("obs-{n}")).expect("id");
        s.append(&event(
            &e,
            &o,
            T0 + n,
            &format!("subject-{n}"),
            predicate,
            object,
            "{}",
        ))
        .unwrap_or_else(|err| panic!("shape {n} must be admitted, got {err}"));
    }
    drop(s);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// The invalid shape is refused — at BOTH layers
// ---------------------------------------------------------------------------

/// Layer 1: admission names the rule rather than surfacing a constraint index.
#[test]
fn admission_refuses_an_object_without_a_predicate() {
    let path = tmp("admission");
    let mut s = WorldStore::open(&path).expect("open");
    let e = EventId::new("ev-1").expect("id");
    let o = ObservationId::new("obs-1").expect("id");

    let err = s
        .append(&event(&e, &o, T0, "thing", None, Some("dock_b"), "{}"))
        .expect_err("the invalid shape must be refused");

    match &err {
        StoreError::ObjectWithoutPredicate { subject, object } => {
            assert_eq!(subject, "thing");
            assert_eq!(object, "dock_b");
        }
        other => panic!("expected ObjectWithoutPredicate, got {other:?}"),
    }
    // The message names the ruling, so a reader learns the rule from the error.
    assert!(
        err.to_string().contains("KIRRA-WM-CLAIM-SHAPES-001"),
        "{err}"
    );

    drop(s);
    let _ = std::fs::remove_file(&path);
}

/// Layer 2, and the one that matters: **SQLite itself refuses**, with the Rust
/// admission check bypassed entirely.
///
/// A decoder-only guarantee is one `INSERT` away from being no guarantee. This
/// reaches past `append` to the connection and writes the row directly.
#[test]
fn raw_sql_cannot_route_around_the_rule() {
    let path = tmp("rawsql");
    let mut s = WorldStore::open(&path).expect("open");
    // A valid row first, so the failure below cannot be blamed on an empty or
    // unusable table.
    let e = EventId::new("ev-ok").expect("id");
    let o = ObservationId::new("obs-ok").expect("id");
    s.append(&event(
        &e,
        &o,
        T0,
        "thing",
        Some("holds"),
        Some("cup"),
        "{}",
    ))
    .expect("valid append");

    // `raw_execute_for_test` is the store's existing raw-SQL escape hatch — the
    // same one used to plant a corrupt chain digest. Reaching for it here is the
    // point: it bypasses `append` entirely, so what refuses the row is SQLite.
    let err = s
        .raw_execute_for_test(
            "INSERT INTO world_events
               (event_id, observation_id, txn_time_ms, valid_from_ms, valid_to_ms,
                source, source_version, writer_class, claim_status, provenance,
                frame_id, map_id, kind, subject, predicate, object,
                payload, payload_schema, payload_digest, retention_class, chain_digest)
             VALUES
               ('ev-raw','obs-raw',1,1,NULL,
                'raw','1.0.0','sensor','confirmed','[]',
                NULL,NULL,'mission','thing',NULL,'dock_b',
                '{}',1,'d','raw','c')",
        )
        .expect_err("SQLite must refuse the invalid shape");

    assert!(
        err.to_string().contains("KIRRA-WM-CLAIM-SHAPES-001"),
        "the trigger's own message should surface: {err}"
    );

    drop(s);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// The regression the ruling exists for
// ---------------------------------------------------------------------------

/// **This test fails against the pre-ruling code**, which is what makes it worth
/// having. Before the fix, appending a payload-only claim and then an
/// object-without-predicate claim for ONE subject left exactly one row in
/// `world_current` — the object-bearing one — and the payload-only claim was
/// silently gone. Now the second append is refused, so the first survives.
#[test]
fn the_two_predicateless_shapes_no_longer_alias() {
    let path = tmp("alias");
    let mut s = WorldStore::open(&path).expect("open");

    let e1 = EventId::new("ev-payload-only").expect("id");
    let o1 = ObservationId::new("obs-1").expect("id");
    s.append(&event(
        &e1,
        &o1,
        T0,
        "thing",
        None,
        None,
        r#"{"note":"payload only"}"#,
    ))
    .expect("payload-only claim is valid");

    let e2 = EventId::new("ev-object-no-pred").expect("id");
    let o2 = ObservationId::new("obs-2").expect("id");
    let err = s.append(&event(
        &e2,
        &o2,
        T0 + 1000,
        "thing",
        None,
        Some("dock_b"),
        "{}",
    ));
    assert!(
        matches!(err, Err(StoreError::ObjectWithoutPredicate { .. })),
        "the aliasing claim must be refused, got {err:?}"
    );

    s.fold().expect("fold");
    let claims = s.current("thing", T0 + 5000).expect("current");
    assert_eq!(claims.len(), 1, "{claims:?}");
    assert_eq!(
        claims[0].payload, r#"{"note":"payload only"}"#,
        "the payload-only claim must SURVIVE — before the ruling it was silently \
         replaced by a claim occupying the same (subject, '') slot"
    );

    drop(s);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// Historical rows: reported, never repaired
// ---------------------------------------------------------------------------

/// A store created at v5 has no invalid rows, and the detector says so rather
/// than being unable to tell.
#[test]
fn a_fresh_store_reports_no_invalid_shape_rows() {
    let path = tmp("detector-clean");
    let mut s = WorldStore::open(&path).expect("open");
    let e = EventId::new("ev-1").expect("id");
    let o = ObservationId::new("obs-1").expect("id");
    s.append(&event(
        &e,
        &o,
        T0,
        "thing",
        Some("holds"),
        Some("cup"),
        "{}",
    ))
    .expect("valid append");

    assert!(s.invalid_shape_rows().expect("detector").is_empty());

    drop(s);
    let _ = std::fs::remove_file(&path);
}
