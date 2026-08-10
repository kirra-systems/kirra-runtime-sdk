//! **Tier 3 box 3a — the consumer's half of the answer boundary.**
//!
//! `mission_context` used to read [`ProjectedClaim`]'s public fields directly:
//! no validity, no trust axes, no provenance handle, and — because the boundary
//! had no `object()` — no way to see the very field the fact turns on. It now
//! routes through `WorldView::ask`, and this file is what makes that migration
//! more than a change of import.
//!
//! Two things are asserted, and they are different claims:
//!
//! 1. **The three silences stay distinct.** *Never heard of it*, *heard of it
//!    but not servable*, and *heard of it and it named something off the menu*
//!    are three different facts about the world. Collapsing them into "no
//!    preference" is exactly the information loss the answer boundary exists to
//!    stop, and it is the loss the pre-migration code committed.
//! 2. **The categorical judgements the boundary attaches actually cross the
//!    seam.** A grade and a freshness verdict that the boundary computes and the
//!    consumer drops are the same defect one layer along.
//!
//! # On reaching for raw SQL
//!
//! [`WorldSilence::NoneAdmissible`] cannot be produced through the sanctioned
//! write path, and that is a real guarantee rather than an oversight —
//! `kirra-world-store/tests/inadmissible_never_read.rs` pins it, and
//! `UnknownReason::NoneAdmissible`'s own docs call the variant unreachable and
//! say why it is kept anyway. So the test below plants the state by writing
//! `world_current` directly, which bypasses the projection fold's
//! `claim_status = 'confirmed'` filter — one of the three mechanisms that
//! guarantee holds on.
//!
//! What that test therefore proves is narrow and worth stating: **the
//! consumer's mapping does not collapse the variant**, so if any of those three
//! mechanisms is ever relaxed for a good local reason, a rejected fact surfaces
//! as `NoneAdmissible` rather than as silence indistinguishable from ignorance.
//! It does not claim the store can be made to serve a rejected claim today, and
//! it must not be read as weakening that guarantee.

use kirra_proposal_context::{mission_context, ContextId, FactGrade, FactValidity, WorldSilence};
use kirra_world_store::{
    Adjudication, ClaimStatus, Corroboration, EventId, NewEvent, ObservationId, Origin, TrustAxes,
    WorldStore, WriterClass,
};

const T0: i64 = 1_700_000_000_000;

/// A store path unique to THIS call — cargo runs these as parallel threads of
/// one process, so a pid-only path has two tests opening one SQLite file.
fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-answer-boundary-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    for suffix in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

fn id(s: &str) -> ContextId {
    ContextId::new(s).expect("non-empty id")
}

fn candidates() -> Vec<ContextId> {
    vec![id("dock_a"), id("dock_b")]
}

/// Record `package_17 last_seen_at <object>`, valid from `valid_from_ms`.
fn record(store: &mut WorldStore, object: &str, valid_from_ms: i64, trust: Option<&TrustAxes>) {
    let event_id = EventId::new("ev-last-seen").expect("event id");
    let observation_id = ObservationId::new("obs-last-seen").expect("observation id");
    store
        .append(&NewEvent {
            event_id: &event_id,
            observation_id: &observation_id,
            txn_time_ms: valid_from_ms,
            valid_from_ms,
            valid_to_ms: None,
            source: "warehouse-scanner",
            source_version: "1.0.0",
            writer_class: WriterClass::Sensor,
            claim_status: ClaimStatus::Confirmed,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "mission",
            subject: "package_17",
            subject_ref: None,
            predicate: Some("last_seen_at"),
            object: Some(object),
            payload: "{}",
            payload_schema: 1,
            retention_class: "raw",
            trust,
        })
        .expect("append");
    store.fold().expect("fold the current projection");
}

/// Ask the one question this file is about.
fn ask(
    store: &WorldStore,
    now_ms: i64,
    budget: Option<u64>,
) -> kirra_proposal_context::ProposalContext {
    mission_context(
        store,
        &id("package_17"),
        &id("last_seen_at"),
        &candidates(),
        now_ms,
        budget,
    )
    .expect("context")
}

// ---------------------------------------------------------------------------
// The three silences stay distinct
// ---------------------------------------------------------------------------

/// Nothing is known about the subject.
#[test]
fn an_unknown_subject_reports_no_claim() {
    let path = tmp("noclaim");
    let store = WorldStore::open(&path).expect("open");

    let ctx = ask(&store, T0, None);
    assert_eq!(ctx.silence(), Some(WorldSilence::NoClaim));
    assert_eq!(ctx.preferred_destination(), None);
    // Silence is not emptiness: the candidates still cross the seam, in the
    // caller's own order.
    assert_eq!(ctx.candidate_priority(), Some(candidates().as_slice()));

    drop(store);
    let _ = std::fs::remove_file(&path);
}

/// A claim exists and is servable — it just names somewhere that is not on the
/// menu. The world spoke; the consumer has to say so.
#[test]
fn an_off_menu_object_reports_no_candidate_matched() {
    let path = tmp("offmenu");
    let mut store = WorldStore::open(&path).expect("open");
    record(&mut store, "dock_zzz", T0, None);

    let ctx = ask(&store, T0, None);
    assert_eq!(ctx.silence(), Some(WorldSilence::NoCandidateMatched));
    assert_eq!(ctx.preferred_destination(), None);

    drop(store);
    let _ = std::fs::remove_file(&path);
}

/// The same shape with a predicate the caller did not ask about is *also*
/// `NoCandidateMatched` — the boundary answered, the relation did not match.
///
/// Worth its own test because the predicate filter and the object filter are
/// two different early-outs in the matcher, and only one of them was exercised
/// above.
#[test]
fn a_different_relation_reports_no_candidate_matched() {
    let path = tmp("otherrel");
    let mut store = WorldStore::open(&path).expect("open");
    record(&mut store, "dock_b", T0, None);

    let ctx = mission_context(
        &store,
        &id("package_17"),
        &id("destined_for"), // a relation the store holds nothing for
        &candidates(),
        T0,
        None,
    )
    .expect("context");
    assert_eq!(ctx.silence(), Some(WorldSilence::NoCandidateMatched));

    drop(store);
    let _ = std::fs::remove_file(&path);
}

/// **`NoneAdmissible` is not collapsed into `NoClaim`.**
///
/// Provoked by writing `world_current` directly — see the module docs for why
/// that is necessary and what it does and does not prove. The point is the
/// mapping: the consumer must be able to say *"the world holds something about
/// this and refuses to serve it"* rather than *"the world has never heard of
/// it"*, because those send an operator to two different questions.
#[test]
fn a_rejected_claim_reports_none_admissible_not_no_claim() {
    let path = tmp("inadmissible");
    let mut store = WorldStore::open(&path).expect("open");

    // A perfectly ordinary confirmed claim first. It has to be labelled: the
    // axes columns are nullable *together*, so flipping one on an unlabelled
    // row would produce a corrupt row rather than a rejected one.
    let axes = TrustAxes::new(
        Origin::Observed,
        Corroboration::Uncorroborated,
        Adjudication::Confirmed,
    )
    .expect("constructible");
    record(&mut store, "dock_b", T0, Some(&axes));

    // Baseline: as written, the claim IS served and DOES move the proposal.
    // Without this the assertion below would pass against a store that had
    // simply failed to project anything.
    let before = ask(&store, T0, None);
    assert_eq!(before.silence(), None, "the claim is servable as written");
    assert_eq!(before.preferred_destination(), Some(&id("dock_b")));

    // Now demote the projected row's adjudication behind the write path's back.
    // `world_current` carries no `claim_status` column and therefore none of the
    // CHECKs that make this shape unwritable in `world_events`.
    store
        .raw_execute_for_test(
            "UPDATE world_current SET adjudication = 'rejected' WHERE subject = 'package_17'",
        )
        .expect("plant the inadmissible grade");

    let ctx = ask(&store, T0, None);
    assert_eq!(
        ctx.silence(),
        Some(WorldSilence::NoneAdmissible),
        "a claim the boundary refuses to serve must not read as an unknown subject"
    );
    assert_ne!(
        ctx.silence(),
        Some(WorldSilence::NoClaim),
        "collapsing these is the information loss box 3a exists to close"
    );
    assert_eq!(ctx.preferred_destination(), None);

    drop(store);
    let _ = std::fs::remove_file(&path);
}

/// The three silences are three values, and a match on them is exhaustive over
/// distinct arms. Cheap, and it fails immediately if a future edit aliases two
/// of them to the same variant.
#[test]
fn the_three_silences_are_pairwise_distinct() {
    let all = [
        WorldSilence::NoClaim,
        WorldSilence::NoneAdmissible,
        WorldSilence::NoCandidateMatched,
    ];
    for (i, a) in all.iter().enumerate() {
        for (j, b) in all.iter().enumerate() {
            assert_eq!(i == j, a == b, "{a:?} vs {b:?}");
        }
    }
}

/// A context that DID express a preference reports no silence at all —
/// `silence()` is not a field that is always populated with a nearest reason.
#[test]
fn a_preference_carries_no_silence() {
    let path = tmp("nosilence");
    let mut store = WorldStore::open(&path).expect("open");
    record(&mut store, "dock_b", T0, None);

    let ctx = ask(&store, T0, None);
    assert_eq!(ctx.silence(), None);
    assert_eq!(ctx.preferred_destination(), Some(&id("dock_b")));

    drop(store);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// The boundary's categorical judgements cross the seam
// ---------------------------------------------------------------------------

/// An unlabelled claim is `Ungraded` — **not** a default grade.
///
/// Manufacturing one would invent a trust judgement out of the absence of one,
/// which is the failure the world's separate axes exist to prevent; the consumer
/// has to preserve the distinction the boundary preserved.
#[test]
fn an_unlabelled_claim_is_ungraded_rather_than_defaulted() {
    let path = tmp("ungraded");
    let mut store = WorldStore::open(&path).expect("open");
    record(&mut store, "dock_b", T0, None);

    let ctx = ask(&store, T0, None);
    assert_eq!(ctx.fact_trust(), Some(FactGrade::Ungraded));

    drop(store);
    let _ = std::fs::remove_file(&path);
}

/// A labelled claim carries the boundary's grade across the seam, categorically.
#[test]
fn a_labelled_claim_carries_its_grade() {
    let path = tmp("graded");
    let mut store = WorldStore::open(&path).expect("open");
    let axes = TrustAxes::new(
        Origin::Observed,
        Corroboration::Corroborated(2),
        Adjudication::Confirmed,
    )
    .expect("constructible");
    record(&mut store, "dock_b", T0, Some(&axes));

    let ctx = ask(&store, T0, None);
    assert_eq!(ctx.fact_trust(), Some(FactGrade::Graded("strong")));

    // The SAME claim grades lower once the caller's budget makes it stale —
    // without this the assertion above would hold just as well against a mapping
    // that returned one constant. Note what this also shows: the grade is a
    // collapse of the axes AND the validity, so the caller's freshness contract
    // moves the trust verdict too.
    let stale = ask(&store, T0 + 60_000, Some(1_000));
    assert_eq!(stale.fact_trust(), Some(FactGrade::Graded("adequate")));
    assert_ne!(stale.fact_trust(), ctx.fact_trust());

    drop(store);
    let _ = std::fs::remove_file(&path);
}

/// **The freshness contract is the caller's, and all three verdicts cross.**
///
/// One claim, one clock, three budgets. `Timeless` is reported rather than
/// silently treated as fresh, which is the whole reason `staleness_budget_ms` is
/// a required parameter: for "last seen at", *age does not matter* is a false
/// claim, and the consumer can only notice it if the boundary's verdict survives
/// the trip.
#[test]
fn the_callers_budget_decides_freshness_and_all_three_verdicts_cross() {
    let path = tmp("freshness");
    let mut store = WorldStore::open(&path).expect("open");
    record(&mut store, "dock_b", T0, None);

    // Asked 60 s after the claim came into force.
    let now = T0 + 60_000;

    let generous = ask(&store, now, Some(120_000));
    assert_eq!(generous.fact_freshness(), Some(FactValidity::Fresh));

    let tight = ask(&store, now, Some(1_000));
    assert_eq!(tight.fact_freshness(), Some(FactValidity::Stale));

    let none = ask(&store, now, None);
    assert_eq!(
        none.fact_freshness(),
        Some(FactValidity::Timeless),
        "no budget is a positive claim of time-independence, and must be visible \
         as one rather than passing for Fresh"
    );

    // The fact itself is the same in all three — only the caller's contract
    // changed, so the difference cannot be attributed to the world.
    for ctx in [&generous, &tight, &none] {
        assert_eq!(ctx.preferred_destination(), Some(&id("dock_b")));
    }

    drop(store);
    let _ = std::fs::remove_file(&path);
}

/// A stale fact is still SERVED — the boundary reports staleness, it does not
/// swallow the claim, and neither does the consumer. Deciding what to do about
/// a stale fact is the caller's business; hiding it would take that decision
/// away.
#[test]
fn a_stale_fact_is_reported_rather_than_withheld() {
    let path = tmp("stale-served");
    let mut store = WorldStore::open(&path).expect("open");
    record(&mut store, "dock_b", T0, None);

    let ctx = ask(&store, T0 + 60_000, Some(1_000));
    assert_eq!(ctx.fact_freshness(), Some(FactValidity::Stale));
    assert_eq!(
        ctx.preferred_destination(),
        Some(&id("dock_b")),
        "stale is a label on an answer, not a refusal to answer"
    );
    assert_eq!(ctx.silence(), None);

    drop(store);
    let _ = std::fs::remove_file(&path);
}

/// A silent world carries no grade and no freshness — there is no fact to
/// describe, and reporting one would describe a fact that was never read.
#[test]
fn silence_carries_no_grade_and_no_freshness() {
    let path = tmp("silent-nojudgement");
    let store = WorldStore::open(&path).expect("open");

    let ctx = ask(&store, T0, Some(1_000));
    assert_eq!(ctx.fact_trust(), None);
    assert_eq!(ctx.fact_freshness(), None);

    drop(store);
    let _ = std::fs::remove_file(&path);
}
