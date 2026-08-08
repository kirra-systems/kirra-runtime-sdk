//! **An inadmissible claim cannot reach a reader.** Emergent, and now pinned.
//!
//! # Why this file exists
//!
//! Found by wiring the first real consumer of Kirra World (`WM_SCOPE.md` §9's
//! sequencing call). That consumer defined an `Unknown` reason for *"claims
//! exist but none are usable"* — and could not reach it. Chasing why turned up a
//! guarantee that holds today, matters for safety, and **is written down
//! nowhere**:
//!
//! > Nothing a reader can see through [`WorldStore::current`] is ever graded
//! > [`TrustGrade::Inadmissible`].
//!
//! # Why it holds, and why that is fragile
//!
//! Not by design. It **composes** out of three mechanisms added for unrelated
//! reasons:
//!
//! | # | Mechanism | Added for |
//! |---|---|---|
//! | 1 | the projection folds only `claim_status = 'confirmed'` | candidates are not current state |
//! | 2 | `CHECK ((claim_status = 'confirmed') = (adjudication = 'confirmed'))` | stopping the retained proxy drifting from the axis |
//! | 3 | `current()` filters on `holds_at` | a claim outside its valid interval is not current |
//!
//! `trust_grade` returns `Inadmissible` for exactly three inputs: adjudication
//! `Rejected`, adjudication `Ambiguous`, and `Validity::Expired`. Filters 1+2
//! together exclude the first two from `world_current`; filter 3 excludes the
//! third from any `current()` result.
//!
//! **Each of those three could be changed for a perfectly good local reason by
//! someone who has no idea this guarantee rests on them.** That is the whole
//! argument for pinning it: an emergent property nobody wrote down is one
//! refactor away from being gone, and its absence would be silent — a doer would
//! simply start acting on rejected or expired facts.
//!
//! # What this does not claim
//!
//! It pins the property, not a decision that the property is *correct*. There is
//! a live question underneath it — an expired claim and a never-heard-of-it
//! subject are currently indistinguishable to a reader, so an operator whose
//! coverage merely lapsed is told "nothing known" and sent to the wrong
//! question. Resolving that is Tier 3's business. This file just makes sure the
//! guarantee cannot vanish while the question is still open.

use kirra_world_store::{
    Adjudication, ClaimStatus, Corroboration, EventId, NewEvent, ObservationId, Origin,
    ProjectedClaim, TrustAxes, TrustGrade, Validity, WorldStore, WriterClass,
};

const T0: i64 = 1_700_000_000_000;
const BUDGET: Option<u64> = Some(60_000);

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-world-inadmissible-{}-{}.sqlite",
        name,
        std::process::id()
    ));
    for suffix in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

fn axes(corr: Corroboration, adj: Adjudication) -> TrustAxes {
    TrustAxes::new(Origin::Observed, corr, adj).expect("constructible")
}

/// Append one claim. Returns the store's verdict so a refusal can be asserted
/// rather than unwrapped away.
fn append(
    store: &mut WorldStore,
    n: i64,
    subject: &str,
    valid_to_ms: Option<i64>,
    trust: Option<&TrustAxes>,
    claim_status: ClaimStatus,
) -> Result<i64, kirra_world_store::StoreError> {
    let event = EventId::new(format!("evt-{n}")).unwrap();
    let observation = ObservationId::new(format!("obs-{n}")).unwrap();
    store.append(&NewEvent {
        event_id: &event,
        observation_id: &observation,
        txn_time_ms: T0,
        valid_from_ms: T0,
        valid_to_ms,
        source: "sensor-a",
        source_version: "0.1.0",
        writer_class: WriterClass::Sensor,
        claim_status,
        provenance: &[],
        frame_id: None,
        map_id: None,
        kind: "observation",
        subject,
        subject_ref: None,
        predicate: Some("colour"),
        object: Some("red"),
        payload: r#"{"n":1}"#,
        payload_schema: 1,
        retention_class: "raw",
        trust,
    })
}

fn seed(
    store: &mut WorldStore,
    n: i64,
    subject: &str,
    valid_to_ms: Option<i64>,
    trust: Option<&TrustAxes>,
    claim_status: ClaimStatus,
) {
    append(store, n, subject, valid_to_ms, trust, claim_status).expect("append");
    store.rebuild_projections().expect("project");
}

fn open(name: &str) -> WorldStore {
    WorldStore::open(&tmp(name)).expect("open")
}

fn grade(c: &ProjectedClaim, now_ms: i64) -> Option<TrustGrade> {
    c.grade_at(now_ms.max(0).unsigned_abs(), BUDGET)
}

// ---------------------------------------------------------------------------
// Non-vacuity anchor — read this first
// ---------------------------------------------------------------------------

/// **The anchor for every assertion below.** A confirmed, in-force claim IS
/// returned by `current()`.
///
/// Without this, every "not returned" test in this file would pass just as
/// happily against a `current()` that returned nothing at all, or a projection
/// that never folded anything. A guarantee proved only by absences is not
/// proved.
#[test]
fn a_confirmed_in_force_claim_is_readable() {
    let mut s = open("anchor");
    let a = axes(Corroboration::Corroborated(2), Adjudication::Confirmed);
    seed(&mut s, 1, "cup-1", None, Some(&a), ClaimStatus::Confirmed);

    let claims = s.current("cup-1", T0).expect("read");
    assert_eq!(claims.len(), 1, "the projection and read path do work");
    assert_eq!(grade(&claims[0], T0), Some(TrustGrade::Strong));
}

// ---------------------------------------------------------------------------
// The three routes to Inadmissible, each closed
// ---------------------------------------------------------------------------

/// Filters 1+2: a `Rejected` claim is storable but never projected.
///
/// Rule 7 makes `Rejected` terminal for the record, so this is not a temporary
/// state that later becomes readable — it is permanent exclusion from what a
/// reader can see.
#[test]
fn a_rejected_claim_never_reaches_a_reader() {
    let mut s = open("rejected");
    let a = axes(Corroboration::Contradicted(2), Adjudication::Rejected);
    seed(&mut s, 1, "cup-1", None, Some(&a), ClaimStatus::Candidate);

    assert!(
        s.current("cup-1", T0).expect("read").is_empty(),
        "a rejected claim must not be readable"
    );
    // ...and it really is in the log. Excluded from the read path, not dropped.
    s.verify_chain()
        .expect("the evidence is retained and verifies");
}

/// Filters 1+2: an `Ambiguous` claim is never projected either.
///
/// Rule 3 makes `Ambiguous` a **stable, reportable** state rather than an error
/// — "I have conflicting information about that". Worth pinning separately from
/// `Rejected` because the two are opposites in intent: one is a terminal ruling,
/// the other is an open conflict that may yet resolve. Both are equally
/// unreadable today, and a future change might reasonably want to surface the
/// second while still hiding the first.
#[test]
fn an_ambiguous_claim_never_reaches_a_reader() {
    let mut s = open("ambiguous");
    let a = axes(Corroboration::Contradicted(2), Adjudication::Ambiguous);
    seed(&mut s, 1, "cup-1", None, Some(&a), ClaimStatus::Candidate);

    assert!(
        s.current("cup-1", T0).expect("read").is_empty(),
        "an ambiguous claim must not be readable as though settled"
    );
    s.verify_chain()
        .expect("the evidence is retained and verifies");
}

/// Filter 3: a claim past its valid interval is filtered by `holds_at`.
///
/// Both sides asserted, so this pins the boundary rather than just the exclusion
/// — a `holds_at` that excluded everything would pass a one-sided test.
#[test]
fn an_expired_claim_never_reaches_a_reader() {
    let mut s = open("expired");
    let a = axes(Corroboration::Corroborated(2), Adjudication::Confirmed);
    seed(
        &mut s,
        1,
        "cup-1",
        Some(T0 + 1_000),
        Some(&a),
        ClaimStatus::Confirmed,
    );

    assert_eq!(
        s.current("cup-1", T0 + 500).expect("read").len(),
        1,
        "in force: readable"
    );
    assert!(
        s.current("cup-1", T0 + 5_000).expect("read").is_empty(),
        "past its valid interval: not readable"
    );
}

// ---------------------------------------------------------------------------
// Filter 2's teeth
// ---------------------------------------------------------------------------

/// The schema **refuses** to file a rejected axis under confirmed status.
///
/// This is what makes filter 1 safe. The projection trusts `claim_status`; if
/// that column could disagree with `adjudication`, a rejected claim wearing a
/// confirmed status would be folded straight into `world_current` and the whole
/// guarantee would collapse.
///
/// Asserted as a refusal at the storage layer rather than a convention in the
/// write path, because a convention is exactly what a future writer can bypass.
#[test]
fn the_schema_refuses_a_rejected_axis_under_confirmed_status() {
    let mut s = open("check-teeth");
    let a = axes(Corroboration::Contradicted(2), Adjudication::Rejected);

    assert_eq!(s.count().expect("count"), 0, "empty to begin with");

    let out = append(&mut s, 1, "cup-1", None, Some(&a), ClaimStatus::Confirmed);

    // Refused BY THE CHECK, not merely "some error happened". A bare
    // `is_err()` would pass on a transient IO fault and prove nothing about
    // the constraint -- which is the same vacuity the anchor test above
    // exists to rule out, so this file should not tolerate it here.
    let err = out.expect_err("claim_status must not drift from the axis it stands for");
    assert!(
        matches!(err, kirra_world_store::StoreError::Sqlite(_)),
        "expected the storage layer's constraint refusal, got {err:?}"
    );
    assert!(
        format!("{err:?}").contains("CHECK"),
        "expected a CHECK constraint violation, got {err:?}"
    );

    // Refused AND not partially written. An evidence log that keeps a row it
    // said it rejected is worse than one that never refused: the chain would
    // carry a record no rule admits.
    assert_eq!(
        s.count().expect("count"),
        0,
        "a refused append must leave nothing behind"
    );
    s.verify_chain()
        .expect("the chain is intact after a refusal");

    // The legal filing of the same axis still works, so the refusal above is
    // about the disagreement and not about rejected claims being unwritable.
    append(&mut s, 2, "cup-1", None, Some(&a), ClaimStatus::Candidate)
        .expect("a rejected claim is storable under candidate status");
    assert_eq!(s.count().expect("count"), 1, "and the legal one did land");
}

// ---------------------------------------------------------------------------
// The invariant itself
// ---------------------------------------------------------------------------

/// **The property, stated directly** over a store holding all four kinds of
/// claim at once.
///
/// The per-route tests above say *how* it holds. This says *what* holds, so the
/// invariant survives even if the mechanisms behind it are rearranged — a
/// refactor that keeps this green has kept the guarantee, whatever it did to the
/// filters.
#[test]
fn no_claim_a_reader_can_see_is_ever_inadmissible() {
    let mut s = open("invariant");

    let confirmed = axes(Corroboration::Corroborated(2), Adjudication::Confirmed);
    let rejected = axes(Corroboration::Contradicted(2), Adjudication::Rejected);
    let ambiguous = axes(Corroboration::Contradicted(2), Adjudication::Ambiguous);

    // readable
    append(
        &mut s,
        1,
        "a",
        None,
        Some(&confirmed),
        ClaimStatus::Confirmed,
    )
    .expect("a");
    // expired
    append(
        &mut s,
        2,
        "b",
        Some(T0 + 1_000),
        Some(&confirmed),
        ClaimStatus::Confirmed,
    )
    .expect("b");
    // unreadable by adjudication
    append(
        &mut s,
        3,
        "c",
        None,
        Some(&rejected),
        ClaimStatus::Candidate,
    )
    .expect("c");
    append(
        &mut s,
        4,
        "d",
        None,
        Some(&ambiguous),
        ClaimStatus::Candidate,
    )
    .expect("d");
    // unlabelled: no axes at all
    append(&mut s, 5, "e", None, None, ClaimStatus::Confirmed).expect("e");
    s.rebuild_projections().expect("project");

    let now = T0 + 5_000; // past b's valid interval
    let visible = s.current_all(now).expect("read");

    // Non-vacuous: something is visible, or the assertion below proves nothing.
    assert!(!visible.is_empty(), "the store must serve something");

    for c in &visible {
        assert_ne!(
            grade(c, now),
            Some(TrustGrade::Inadmissible),
            "subject {} reached a reader while grading Inadmissible",
            c.subject
        );
    }

    // And specifically: the two unreadable subjects and the expired one are
    // absent, while the readable and the unlabelled ones are present. Named
    // individually so a change in *which* claims are hidden is visible here,
    // not just a change in how many.
    let subjects: Vec<&str> = visible.iter().map(|c| c.subject.as_str()).collect();
    assert!(subjects.contains(&"a"), "confirmed and in force");
    assert!(
        subjects.contains(&"e"),
        "unlabelled: admitted, because an absent axis is not a negative judgement"
    );
    assert!(!subjects.contains(&"b"), "expired");
    assert!(!subjects.contains(&"c"), "rejected");
    assert!(!subjects.contains(&"d"), "ambiguous");
}

/// An unlabelled claim is readable and grades to `None`, not to `Inadmissible`.
///
/// The boundary of the invariant, and it cuts the other way: *"I hold no trust
/// judgement about this"* must not be read as *"I hold a negative one"*.
/// Grading an unlabelled claim inadmissible would invent a judgement from the
/// absence of one, which is the mirror of manufacturing a default grade.
#[test]
fn an_unlabelled_claim_is_readable_and_ungraded() {
    let mut s = open("unlabelled");
    seed(&mut s, 1, "cup-1", None, None, ClaimStatus::Confirmed);

    let claims = s.current("cup-1", T0).expect("read");
    assert_eq!(claims.len(), 1);
    assert_eq!(grade(&claims[0], T0), None);
    assert_eq!(
        claims[0].validity_at(T0.unsigned_abs(), BUDGET),
        Validity::Fresh
    );
}
