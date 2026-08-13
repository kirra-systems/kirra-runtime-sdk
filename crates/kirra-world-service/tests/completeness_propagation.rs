//! **The 3g follow-up — completeness for `history` and `subject_summary`.**
//!
//! 3g closed with a stated limit: *"`history` and `subject_summary` remain
//! unpropagated, and a boundary query for them is the follow-up."* This is that
//! follow-up, held to one acceptance rule:
//!
//! > A boundary answer must never report `Full` when evidence required by that
//! > family may have been removed. Conservative `Degraded` is allowed; silent
//! > loss is not.
//!
//! # The two families needed DIFFERENT mechanisms, and that is the finding
//!
//! Both propagate rather than recompute — but they propagate different types,
//! because the store computes their completeness from different things.
//!
//! | Family | Signal | Computed from |
//! |---|---|---|
//! | `history` | `Resolution` | citations overlapping a queried RANGE |
//! | `subject_summary` | `SummaryCoverage` | the evidence behind ONE folded row |
//!
//! Coercing the second into the first would mean passing `spans: vec![]`, which
//! reads as *"no compacted span bore on this"* — false, in the reassuring
//! direction. So each crosses the boundary in its native type. One source of
//! truth per family beats one type across families.
//!
//! # Why history's two arms need two SEPARATE stores
//!
//! Not tidiness. `WorldStore::resolution_for` checks citations **store-wide** —
//! `load_citations()` takes no subject — so one retained citation degrades every
//! subject's history, including subjects the compacted span never touched. The
//! `Full` arm is therefore only reachable in a store with no citations at all.
//!
//! That conservatism is deliberate and this suite PINS it rather than sharpening
//! it. Narrowing the check to the queried subject would turn a conservative
//! signal into an exact-loss detector, and an exact-loss detector that is wrong
//! is silent — the failure the acceptance rule forbids. A future reader who sees
//! the two-store split and "simplifies" it into one store will find the Full arm
//! unreachable, which is the intended tripwire.
//!
//! # Both history arms return non-empty claims, deliberately
//!
//! A forced-`Full` mutation must red for the RIGHT reason. If the compacted arm
//! returned nothing, it could red for "no claims" while completeness propagation
//! was entirely broken, and the control would be measuring emptiness.
//!
//! # The mutation battery
//!
//! Run against the shipped code, not reasoned about:
//!
//! | # | Mutation | Reds |
//! |---|---|---|
//! | 1 | history: force `Resolution::Full` in the carry | `a_compacted_history_reports_degraded` |
//! | 2 | history: force `Degraded` everywhere | `an_uncompacted_history_reports_full` |
//! | 3 | summary: `is_degraded` returns `false` | the two summary-degraded controls |
//! | 4 | summary: `is_degraded` defers to reconciliation | the two summary-degraded controls |
//! | 5 | history: swallow the `policy_for` refusal | `an_unruled_claim_refuses_the_whole_history` |
//! | 6 | fixture: drop the post-compaction rebuild | `reconciliation_does_not_upgrade_completeness` ONLY |
//!
//! **Mutations 3 and 4 red the same pair**, so on mutation evidence alone
//! `reconciliation_does_not_upgrade_completeness` looks redundant against
//! `a_compacted_summary_reports_degraded`. It is not, and mutation 6 is the
//! separating case: it reds that test alone, at `left: 4, right: 3`. What the
//! extra test buys is the guarantee that the reconstruction genuinely
//! SUCCEEDS in the fixture — so the degraded verdict is being held *despite*
//! working reconciliation rather than alongside broken reconciliation. Without
//! it, a fixture that quietly double-counted would still report `Degraded` and
//! the control would be green for the wrong reason.
//!
//! Mutation 4 is the one worth keeping in view. `reconciled_observation_count`
//! and `reconciled_first_observed_ms` genuinely DO reconstruct their
//! pre-compaction values from citations — so an implementation reading "the
//! numbers add up" as "nothing was lost" would look correct. It is wrong
//! because a citation names a SPAN, not the events inside it:
//! `provenance_head` and `last_event_id` cannot be reconstructed at all, and
//! for a fully compacted subject `retained` is `None` and they do not exist.
//! `Full` has to describe the whole answer contract, not the two numbers that
//! survived.
//!
//! # Three fixture facts that were measured, not assumed
//!
//! Each cost a red test first, and each is recorded because the wrong version
//! looks entirely reasonable:
//!
//! * `fold()` does **not** build `subject_summary` — `fold_subject_summary()`
//!   is a separate call. A fixture running only the first produces ZERO
//!   summaries, and every subject-summary control would pass vacuously against
//!   a store that had never summarised anything.
//! * The summary must be rebuilt **after** compaction, not merely folded before
//!   it — measured at `reconciled = 4` against a true 3. The argument is on
//!   `holed_store`, which is where someone changing the fixture will be.
//! * `is_admissible` drops `Expired` validity and `Inadmissible` grade — **not**
//!   staleness. A stale claim is served by `ask`, carrying a `Stale` grade. The
//!   first draft of `history_keeps_a_claim_that_ask_refuses_to_serve` used
//!   staleness and failed on its own premise.

use kirra_world_service::freshness::FreshnessSource;
use kirra_world_service::read_view::{AskError, WorldLookup, WorldView};
use kirra_world_store::lineage::LineagePage;
use kirra_world_store::{ClaimStatus, EventId, NewEvent, ObservationId, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-3gf-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    for suffix in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

fn claim(store: &mut WorldStore, tag: &str, object: &str, at_ms: i64) {
    store
        .append(&NewEvent {
            event_id: &EventId::new(format!("ev-{tag}")).expect("event id"),
            observation_id: &ObservationId::new(format!("obs-{tag}")).expect("obs"),
            txn_time_ms: at_ms,
            valid_from_ms: at_ms,
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
            trust: None,
        })
        .expect("append");
}

/// The RULED table classifies `mission`/`last_seen_at` with a five-minute
/// budget, so this view is fail-closed AND the fixture's claims are classified.
fn view(store: &WorldStore) -> WorldView<'_> {
    WorldView::new(store, FreshnessSource::Ruled)
}

/// Three observations, all retained. No citations anywhere in the store.
///
/// `fold_subject_summary` is a SEPARATE call from `fold` and both are needed:
/// the first builds `world_current`, the second builds `subject_summary`. A
/// fixture that ran only `fold` produces zero summaries, and the
/// subject-summary controls below would then pass vacuously against a store
/// that has never summarised anything.
fn intact_store(name: &str) -> (WorldStore, std::path::PathBuf) {
    let path = tmp(name);
    let mut store = WorldStore::open(&path).expect("open");
    claim(&mut store, "alpha", "dock_alpha", T0);
    claim(&mut store, "beta", "dock_beta", T0 + 100);
    claim(&mut store, "gamma", "dock_gamma", T0 + 200);
    store.fold().expect("fold");
    store.fold_subject_summary().expect("summary fold");
    (store, path)
}

/// The same three observations, with the superseded middle one compacted away.
///
/// # The rebuild after compaction is load-bearing, and was measured
///
/// The summary must be REBUILT after the compaction, not merely folded before
/// it. Both orders were run:
///
/// | Order | `retained` | `reconciled_observation_count` |
/// |---|---|---|
/// | fold summary → compact | 3 | **4** — the removed event counted twice |
/// | fold summary → compact → rebuild | 2 | 3 — the identity holds |
///
/// Folding first leaves the pre-compaction total in the row, and the citation
/// then adds the removed event back on top of a count that never lost it. A
/// fixture built that way would make `reconciliation_does_not_upgrade_completeness`
/// assert a wrong number while still passing its completeness check — the
/// control would look green and be measuring a double-count.
fn holed_store(name: &str) -> (WorldStore, std::path::PathBuf) {
    let path = tmp(name);
    let mut store = WorldStore::open(&path).expect("open");
    claim(&mut store, "alpha", "dock_alpha", T0);
    claim(&mut store, "beta", "dock_beta", T0 + 100);
    claim(&mut store, "gamma", "dock_gamma", T0 + 200);
    store.fold().expect("fold");
    store.fold_subject_summary().expect("summary fold");
    let outcome = store
        .compact_range(2, 2, T0 + 9_000)
        .expect("generation 2 is superseded, so compactable");
    assert_eq!(outcome.removed, 1, "the fixture must remove an observation");
    store
        .rebuild_subject_summary()
        .expect("rebuild over surviving evidence");
    (store, path)
}

fn answered(lookup: &WorldLookup) -> usize {
    match lookup {
        WorldLookup::Answered(a) => a.len(),
        WorldLookup::Unknown(_) => 0,
    }
}

// ---------------------------------------------------------------- history ---

/// **CONTROL 1.** Compacted evidence must never read `Full`.
///
/// The load-bearing direction of the acceptance rule. Forcing `Full` in the
/// boundary's carry reds this.
#[test]
fn a_compacted_history_reports_degraded() {
    let (store, _p) = holed_store("hist-degraded");
    let lookup = view(&store)
        .history("package_17", LineagePage::first())
        .expect("history");

    assert!(
        lookup.is_degraded(),
        "evidence was compacted away, so this history is not the whole record"
    );
}

/// **CONTROL 5.** Both arms answer non-emptily, so control 1 reds for the right
/// reason.
///
/// Without this, forcing `Full` could be caught by an assertion that the
/// compacted arm merely returned nothing — which would still pass with
/// propagation completely broken.
#[test]
fn both_history_arms_return_a_plausible_non_empty_record() {
    let (holed, _p1) = holed_store("hist-nonempty-holed");
    let (intact, _p2) = intact_store("hist-nonempty-intact");

    let degraded = view(&holed)
        .history("package_17", LineagePage::first())
        .expect("history");
    let full = view(&intact)
        .history("package_17", LineagePage::first())
        .expect("history");

    assert_eq!(
        answered(full.lookup()),
        3,
        "the intact arm holds every observation"
    );
    assert_eq!(
        answered(degraded.lookup()),
        2,
        "the compacted arm is SHORTER but still a real record — one observation \
         was removed, and the two that remain are genuine history"
    );
}

/// **CONTROL 2.** The `Full` arm is demonstrably reachable.
///
/// Non-vacuity. Without it, an implementation reporting `Degraded`
/// unconditionally would satisfy every other history control here.
///
/// Uses its OWN store, because `resolution_for`'s citation check is store-wide:
/// sharing a store with the compacted case would make this arm unreachable and
/// the control silently untestable. See this file's header.
#[test]
fn an_uncompacted_history_reports_full() {
    let (store, _p) = intact_store("hist-full");
    let lookup = view(&store)
        .history("package_17", LineagePage::first())
        .expect("history");

    assert!(
        !lookup.is_degraded(),
        "nothing was compacted in this store, so the record is whole"
    );
    assert!(
        answered(lookup.lookup()) > 0,
        "a Full arm over an empty answer would prove nothing"
    );
}

/// **History does not FILTER on admissibility — a refused claim is still history.**
///
/// The trap the version set invites: `answer_admissibility` is in history's
/// dependency set, so the natural inference is that history filters on it.
///
/// Pinned as a CONTRAST against `ask` over the same store, because a bare count
/// would also pass if nothing in the fixture were inadmissible at all.
///
/// The claim is made EXPIRED rather than merely stale, and that distinction was
/// measured rather than assumed: `is_admissible` drops a claim whose validity is
/// `Expired` or whose grade is `Inadmissible`, and a *stale* claim is neither —
/// `ask` serves it, carrying a `Stale` grade. A first draft of this test used
/// staleness and failed, asserting `ask` returned nothing when it returned the
/// claim. Expiry is what actually makes `ask` decline.
#[test]
fn history_keeps_a_claim_that_ask_refuses_to_serve() {
    let path = tmp("hist-expired");
    let mut store = WorldStore::open(&path).expect("open");
    store
        .append(&NewEvent {
            event_id: &EventId::new("ev-expired").expect("event id"),
            observation_id: &ObservationId::new("obs-expired").expect("obs"),
            txn_time_ms: T0,
            valid_from_ms: T0,
            // Valid for one second, and the read below is an hour later.
            valid_to_ms: Some(T0 + 1_000),
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
            object: Some("dock_alpha"),
            payload: "{}",
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append");
    store.fold().expect("fold");

    let v = view(&store);
    let late = T0 + 3_600_000;

    let served = v.ask("package_17", late).expect("ask");
    assert_eq!(
        answered(served.lookup()),
        0,
        "the claim must genuinely be inadmissible at this clock, or the \
         contrast below is vacuous"
    );

    let record = v
        .history("package_17", LineagePage::first())
        .expect("history");
    assert_eq!(
        answered(record.lookup()),
        1,
        "the claim is in the record even though `ask` declines to serve it; a \
         history that hid it would be lying about the past"
    );
}

/// **CONTROL for mutation 5.** An unruled claim refuses the whole history.
///
/// 3e's fail-closed rule reaches this family. This is the only reason
/// `answer_admissibility` is in history's version set, so if the `policy_for`
/// call were dropped as "unused" — it filters nothing — this is what catches it.
#[test]
fn an_unruled_claim_refuses_the_whole_history() {
    let path = tmp("hist-unruled");
    let mut store = WorldStore::open(&path).expect("open");
    // A semantic class the RULED table does not name.
    store
        .append(&NewEvent {
            event_id: &EventId::new("ev-unruled").expect("event id"),
            observation_id: &ObservationId::new("obs-unruled").expect("obs"),
            txn_time_ms: T0,
            valid_from_ms: T0,
            valid_to_ms: None,
            source: "warehouse-scanner",
            source_version: "1.0.0",
            writer_class: WriterClass::Sensor,
            claim_status: ClaimStatus::Confirmed,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "not_a_ruled_kind",
            subject: "package_17",
            subject_ref: None,
            predicate: Some("whatever"),
            object: Some("dock_alpha"),
            payload: "{}",
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append");
    store.fold().expect("fold");

    let err = view(&store)
        .history("package_17", LineagePage::first())
        .expect_err("an unclassified claim must refuse the whole query");
    assert!(
        matches!(err, AskError::UnclassifiedFreshness { .. }),
        "expected a fail-closed refusal, got {err:?}"
    );
}

// --------------------------------------------------------- subject summary ---

/// **CONTROL 3.** Existing degraded coverage must not be swallowed.
///
/// The boundary propagates the fold's `SummaryCoverage` verbatim; mapping
/// `Degraded` → `Complete` reds this.
#[test]
fn a_compacted_summary_reports_degraded() {
    let (store, _p) = holed_store("sum-degraded");
    let lookup = view(&store)
        .subject_summary("package_17")
        .expect("subject summary");

    assert!(
        lookup.summary().is_some(),
        "the subject is summarised; a missing summary would make the coverage \
         assertion below vacuous"
    );
    assert!(
        lookup.is_degraded(),
        "evidence behind these aggregates was compacted away"
    );
}

/// **CONTROL 2's counterpart for summaries.** The `Complete` arm is reachable.
#[test]
fn an_uncompacted_summary_reports_complete() {
    let (store, _p) = intact_store("sum-complete");
    let lookup = view(&store)
        .subject_summary("package_17")
        .expect("subject summary");

    assert!(lookup.summary().is_some(), "the subject is summarised");
    assert!(
        !lookup.is_degraded(),
        "nothing was compacted, so the aggregates rest on all their evidence"
    );
}

/// **CONTROL 4 — the invariant this half exists for.**
///
/// > Successful numerical reconciliation does not imply complete evidence
/// > coverage.
///
/// `reconciled_observation_count` genuinely reconstructs the pre-compaction
/// total from citations, and `reconciled_first_observed_ms` genuinely
/// reconstructs the earliest transaction time. Both succeed here — asserted,
/// so this test cannot pass because reconciliation quietly failed.
///
/// Completeness must remain `Degraded` anyway, because a citation names a span
/// and not the events inside it: `provenance_head` and `last_event_id` are
/// unrecoverable. An implementation reading "the counts agree" as "nothing was
/// lost" passes every other control in this file and fails only here.
#[test]
fn reconciliation_does_not_upgrade_completeness() {
    let (store, _p) = holed_store("sum-reconcile");
    let lookup = view(&store)
        .subject_summary("package_17")
        .expect("subject summary");
    let summary = lookup.summary().expect("summarised");

    // The reconstruction really does work — otherwise the assertion below would
    // hold for the boring reason rather than the load-bearing one.
    assert_eq!(
        summary.reconciled_observation_count(),
        3,
        "all three observations are accounted for: two retained, one via its \
         citation — the reconstruction this test exists to NOT trust"
    );
    assert_eq!(
        summary.reconciled_first_observed_ms(),
        Some(T0),
        "the earliest transaction time is recoverable too"
    );

    assert!(
        lookup.is_degraded(),
        "the counts reconcile and the answer is STILL degraded: provenance_head \
         and last_event_id name events that no longer exist, and Full has to \
         describe the whole answer contract rather than the two numbers that \
         happened to survive"
    );
}

/// An unsummarised subject is not degraded — there is no evidence missing.
///
/// Keeps "never heard of it" distinct from "its evidence was removed", which is
/// a distinction the store goes to real trouble to preserve.
#[test]
fn an_unknown_subject_is_absent_rather_than_degraded() {
    let (store, _p) = intact_store("sum-absent");
    let lookup = view(&store)
        .subject_summary("package_99")
        .expect("subject summary");

    assert!(lookup.summary().is_none(), "never observed");
    assert!(
        !lookup.is_degraded(),
        "nothing was claimed about this subject, so nothing is missing"
    );
}

// ------------------------------------------------- box 3d: history pages ---

/// **A short page returns the page, not the record.**
///
/// The bound must reach the STORE. `history` used to fetch every confirmed claim
/// about a subject and hand it all back — the third instance of the defect class
/// box 3d closes, and the one written in the PR that documented the other two.
#[test]
fn a_history_page_returns_at_most_its_limit() {
    let (store, _p) = intact_store("hist-page-limit");
    let page = LineagePage::new(2, None).expect("valid page");
    let lookup = view(&store).history("package_17", page).expect("history");

    assert_eq!(
        answered(lookup.lookup()),
        2,
        "three claims exist; a limit of 2 must return 2"
    );
    assert!(
        lookup.boundary().is_truncated(),
        "a third claim follows, so the page must say so"
    );
}

/// **Paginating to exhaustion returns the whole record exactly once.**
///
/// The cursor walk. A page that reported `More` forever, or one whose cursor was
/// inclusive and repeated a claim, would both still pass the limit test above.
#[test]
fn paginating_a_history_walks_the_record_without_gaps_or_repeats() {
    let (store, _p) = intact_store("hist-page-walk");
    let v = view(&store);

    let mut seen: Vec<String> = Vec::new();
    let mut cursor = None;
    for _ in 0..10 {
        let page = LineagePage::new(1, cursor).expect("valid page");
        let lookup = v.history("package_17", page).expect("history");
        if let WorldLookup::Answered(answers) = lookup.lookup() {
            for a in answers {
                seen.push(a.event_id().to_string());
            }
        }
        match lookup.boundary() {
            kirra_world_store::lineage::PageBoundary::More {
                next_after_generation,
            } => cursor = Some(*next_after_generation),
            kirra_world_store::lineage::PageBoundary::Complete => break,
        }
    }

    assert_eq!(
        seen.len(),
        3,
        "the walk must visit every claim exactly once"
    );
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 3, "an inclusive cursor would repeat a claim");
}

/// **A page that exactly fills is NOT `More`.**
///
/// The off-by-one `lineage::boundary_for` owns. Asking for exactly the record's
/// length must report `Complete`, or a caller paginating to exhaustion makes one
/// wasted round trip on every record whose length divides the page size.
///
/// Sized to the record deliberately — #1440's first draft of this control asked
/// for a 256-limit page over two events, a page that could not have been full,
/// and the mutation survived it.
#[test]
fn a_history_page_that_exactly_fills_is_complete() {
    let (store, _p) = intact_store("hist-page-exact");
    let page = LineagePage::new(3, None).expect("valid page");
    let lookup = view(&store).history("package_17", page).expect("history");

    assert_eq!(answered(lookup.lookup()), 3, "the whole record");
    assert!(
        !lookup.boundary().is_truncated(),
        "the page holds the entire record, so nothing follows it"
    );
}

/// **Page boundary and record completeness are independent.**
///
/// 3g's rule, applied to a paginated family. A page cut short by the caller's
/// own limit is complete evidence; a record missing a compacted span is evidence
/// that no longer exists. Reporting one as the other in either direction would
/// make a routine first page look like data loss, or data loss look routine.
#[test]
fn a_truncated_page_over_a_degraded_record_reports_both() {
    let (store, _p) = holed_store("hist-page-degraded");
    let page = LineagePage::new(1, None).expect("valid page");
    let lookup = view(&store).history("package_17", page).expect("history");

    assert!(
        lookup.boundary().is_truncated(),
        "two claims survive and the limit is 1, so more follows"
    );
    assert!(
        lookup.is_degraded(),
        "evidence was compacted away, which the page bound says nothing about"
    );
}
