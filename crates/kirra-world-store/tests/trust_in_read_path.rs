//! Trust reaching a reader — "no bare values", at the projection.
//!
//! Strand C put the four axes into the evidence log. Nothing carried them out
//! of it: `world_current` had no axis columns and `ProjectedClaim` had no axis
//! fields, so every read returned a value with no trust attached and a consumer
//! had to go back to the raw log to learn anything about it.
//!
//! These tests assert the two halves of the fix, and the second matters more
//! than the first: the three **stored** axes travel with the claim, and the
//! fourth — validity — is still **computed at read time**, so rule 6 stays
//! unbreakable rather than merely documented.

use kirra_world_store::{
    Adjudication, ClaimStatus, Corroboration, EventId, NewEvent, ObservationId, Origin, TrustAxes,
    TrustGrade, Validity, WorldStore, WriterClass,
};

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-world-trustread-{}-{}.sqlite",
        name,
        std::process::id()
    ));
    clean(&p);
    p
}

fn clean(p: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
}

struct Ids {
    event: EventId,
    observation: ObservationId,
}

fn ids(n: i64) -> Ids {
    Ids {
        event: EventId::new(format!("evt-{n}")).unwrap(),
        observation: ObservationId::new(format!("obs-{n}")).unwrap(),
    }
}

const T0: i64 = 1_700_000_000_000;

fn ev<'a>(id: &'a Ids, trust: Option<&'a TrustAxes>, valid_to_ms: Option<i64>) -> NewEvent<'a> {
    NewEvent {
        event_id: &id.event,
        observation_id: &id.observation,
        txn_time_ms: T0,
        valid_from_ms: T0,
        valid_to_ms,
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

fn axes() -> TrustAxes {
    TrustAxes::new(
        Origin::Observed,
        Corroboration::Corroborated(2),
        Adjudication::Confirmed,
    )
    .expect("constructible")
}

/// The axes survive the fold and arrive with the claim.
#[test]
fn a_read_claim_carries_the_axes_it_was_written_with() {
    let path = tmp("carries");
    let mut s = WorldStore::open(&path).expect("open");
    let a = axes();
    s.append(&ev(&ids(1), Some(&a), None)).expect("append");
    s.fold().expect("fold");

    let got = s.current("cup-1", T0 + 1).expect("read");
    assert_eq!(got.len(), 1);
    let trust = got[0].trust.as_ref().expect("axes present");
    assert_eq!(trust.origin(), Origin::Observed);
    assert_eq!(trust.corroboration(), Corroboration::Corroborated(2));
    assert_eq!(trust.adjudication(), Adjudication::Confirmed);
    clean(&path);
}

/// An unlabelled claim reads as unlabelled — **not** as a default set of axes.
///
/// Manufacturing an origin and an adjudication for a claim that carries none is
/// the "mush after eighteen months" the four-axis model exists to prevent, and
/// it would be indistinguishable downstream from a claim that really was
/// observed and confirmed.
#[test]
fn an_unlabelled_claim_reads_as_unlabelled_not_as_a_default() {
    let path = tmp("unlabelled");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&ev(&ids(1), None, None)).expect("append");
    s.fold().expect("fold");

    let got = s.current("cup-1", T0 + 1).expect("read");
    assert!(got[0].trust.is_none(), "no axes must stay no axes");
    assert!(
        got[0].grade_at((T0 + 1).unsigned_abs(), None).is_none(),
        "an unlabelled claim has no trust to grade"
    );
    clean(&path);
}

/// The **provenance handle**: a read answer names its position in the
/// tamper-evident log, so it can be cited and verified rather than trusted
/// because the API returned it.
#[test]
fn a_read_claim_carries_its_chain_position() {
    let path = tmp("handle");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&ev(&ids(1), Some(&axes()), None)).expect("append");
    s.fold().expect("fold");

    let got = s.current("cup-1", T0 + 1).expect("read");
    assert!(!got[0].chain_digest.is_empty(), "a citable handle");
    assert_eq!(got[0].generation, 1);
    assert_eq!(got[0].event_id, "evt-1");
    clean(&path);
}

// ---------------------------------------------------------------------------
// Rule 6 — validity is computed, never stored
// ---------------------------------------------------------------------------

/// **There is no validity column anywhere**, which is what makes rule 6
/// unbreakable rather than documented. Asserted against the live schema of both
/// tables rather than against the DDL text, so adding one later fails here.
#[test]
fn no_table_has_a_validity_column() {
    let path = tmp("no-validity");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&ev(&ids(1), Some(&axes()), None)).expect("append");
    s.fold().expect("fold");

    for table in ["world_events", "world_current"] {
        let sql = format!(
            "SELECT COUNT(*) FROM pragma_table_info('{table}') \
             WHERE name IN ('validity', 'freshness', 'is_fresh', 'stale')"
        );
        let n: i64 = s.query_scalar_for_test(&sql).expect("pragma");
        assert_eq!(n, 0, "{table} must not store validity");
    }
    clean(&path);
}

/// The same claim, read at two different times, gives two different
/// validities — which is only possible because it is computed with a clock
/// passed in rather than read from a column.
#[test]
fn the_same_claim_is_valid_then_expired_as_the_clock_moves() {
    let path = tmp("clock");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&ev(&ids(1), Some(&axes()), Some(T0 + 1_000)))
        .expect("append");
    s.fold().expect("fold");

    // `current` filters on expiry, so read the row itself and ask it twice.
    let got = s.current("cup-1", T0 + 1).expect("read");
    let claim = &got[0];

    assert_eq!(
        claim.validity_at((T0 + 500).unsigned_abs(), None),
        Validity::Timeless,
        "in force, and with no staleness budget it does not go stale"
    );
    assert_eq!(
        claim.validity_at((T0 + 5_000).unsigned_abs(), None),
        Validity::Expired,
        "past valid_to_ms, the same row reads as expired"
    );
    clean(&path);
}

/// The staleness budget comes from the **caller**, not from storage.
///
/// How long a claim stays fresh is a policy of the asking consumer — a floor
/// plan and a person's location go stale at wildly different rates — so baking
/// one budget into the row would answer for every reader at once.
#[test]
fn the_staleness_budget_is_the_callers_not_the_rows() {
    let path = tmp("budget");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&ev(&ids(1), Some(&axes()), None)).expect("append");
    s.fold().expect("fold");

    let got = s.current("cup-1", T0 + 1).expect("read");
    let claim = &got[0];
    let now = (T0 + 10_000).unsigned_abs();

    assert_eq!(
        claim.validity_at(now, None),
        Validity::Timeless,
        "no budget: the claim does not go stale"
    );
    assert_eq!(
        claim.validity_at(now, Some(1_000)),
        Validity::Stale,
        "a one-second budget: the same row, same clock, now stale"
    );
    assert_eq!(
        claim.validity_at(now, Some(1_000_000)),
        Validity::Fresh,
        "a generous budget: still fresh"
    );
    clean(&path);
}

/// Collapsing to a grade happens **only when a caller asks**, and the grade
/// moves with validity — so the collapse cannot be cached into the row either.
#[test]
fn the_grade_is_collapsed_at_the_boundary_and_moves_with_validity() {
    let path = tmp("grade");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&ev(&ids(1), Some(&axes()), None)).expect("append");
    s.fold().expect("fold");

    let got = s.current("cup-1", T0 + 1).expect("read");
    let claim = &got[0];
    let now = (T0 + 10_000).unsigned_abs();

    let fresh = claim.grade_at(now, Some(1_000_000)).expect("graded");
    let stale = claim.grade_at(now, Some(1_000)).expect("graded");

    assert_ne!(
        fresh, stale,
        "a stale claim must not grade the same as a fresh one"
    );
    assert_eq!(
        fresh,
        TrustGrade::Strong,
        "observed + corroborated(2) + confirmed, in force"
    );
    assert_eq!(
        stale,
        TrustGrade::Adequate,
        "the same axes, stale, grade one step down rather than becoming inadmissible"
    );
    clean(&path);
}

// ---------------------------------------------------------------------------
// Migration of a derived table
// ---------------------------------------------------------------------------

/// A projection folded before the trust columns existed is **rebuilt**, not
/// patched.
///
/// Legitimate here and nowhere else in this crate: a projection is derived, so
/// dropping it loses no evidence. The evidence log gets additive migrations
/// precisely because the same move there would be a forgery.
#[test]
fn a_projection_of_the_old_shape_is_rebuilt_rather_than_left_short() {
    let path = tmp("reshape");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&ev(&ids(1), Some(&axes()), None)).expect("append");
    s.fold().expect("fold");

    // Simulate the pre-trust shape by rebuilding the table without the axes.
    s.raw_execute_for_test(
        "CREATE TABLE wc_old AS SELECT subject, predicate_key, predicate, object, kind,
             payload, frame_id, map_id, source, valid_from_ms, valid_to_ms, txn_time_ms,
             generation, event_id
         FROM world_current",
    )
    .expect("project away the trust columns");
    s.raw_execute_for_test("DROP TABLE world_current")
        .expect("drop");
    s.raw_execute_for_test("ALTER TABLE wc_old RENAME TO world_current")
        .expect("rename");

    // The next fold must notice the shape and rebuild rather than fail or
    // silently return claims with no trust.
    s.fold().expect("fold against the old shape");

    let got = s.current("cup-1", T0 + 1).expect("read");
    assert_eq!(got.len(), 1, "the claim survived the rebuild");
    assert!(
        got[0].trust.is_some(),
        "and now carries the trust it was written with"
    );
    clean(&path);
}

/// **An orphan `corroboration_n` in the projection is corruption, not an
/// unlabelled claim.**
///
/// Found in review, and the same defect class already fixed on the verify path
/// in #1376 — fixing it there did not fix it here, because the claim path is a
/// second reader with its own column order.
///
/// The reason is stronger in the projection than in the log: `world_current`'s
/// columns are **not hashed at all**, so an orphan count in this derived table
/// is invisible to `verify_chain`. It is the one place such a value could sit
/// undetected, which is exactly why the read has to refuse it.
#[test]
fn an_orphan_count_in_the_projection_is_refused_not_read_as_unlabelled() {
    let path = tmp("orphan-projection");
    let mut s = WorldStore::open(&path).expect("open");
    s.append(&ev(&ids(1), None, None)).expect("unlabelled");
    s.fold().expect("fold");

    // The projection carries no CHECK constraints — it is derived, and its
    // integrity comes from being rebuildable rather than from the schema. So
    // this write succeeds, and the READ is what has to catch it.
    s.raw_execute_for_test("UPDATE world_current SET corroboration_n = 5")
        .expect("the projection has no constraint to stop this");

    let err = s
        .current("cup-1", T0 + 1)
        .expect_err("an orphan count must be refused, not read as unlabelled");
    assert!(
        format!("{err:?}").contains("orphan"),
        "the diagnosis must name what is wrong: {err:?}"
    );
    clean(&path);
}
