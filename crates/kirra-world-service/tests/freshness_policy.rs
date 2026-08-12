//! **Box 3e — freshness is ruled, and unclassified semantics refuse.**
//!
//! `KIRRA-WM-FRESHNESS-POLICY-001`:
//!
//! > Freshness semantics are centrally ruled by claim kind. `Timeless` must be
//! > explicitly granted. Bounded facts require an explicit age limit.
//! > Unclassified semantics refuse.
//!
//! And the invariant that follows:
//!
//! > **`Timeless` is an affirmative semantic classification, never the absence
//! > of a freshness policy.**
//!
//! # The defect, which was live and self-documented
//!
//! `WM_SCOPE.md`'s FINDING 2 recorded it before this box was built:
//!
//! > *"Where was the package last seen" is about as recency-sensitive as this
//! > domain gets, so a year-old observation is currently served with the same
//! > standing as a fresh one, under a label asserting that is fine.*
//!
//! `WorldView` took an `Option<u64>` and `None` meant `Validity::Timeless` — a
//! **positive claim that the fact's age does not matter**, asserted by the
//! engine about every fact in the store whenever nobody supplied a budget.
//!
//! # Why the adversarial pair is the same age
//!
//! The two arms below use claims with the **same `valid_from`**, read at the
//! **same clock**. Nothing about the data distinguishes them; only the ruling
//! does. A pair at different ages would pass on arithmetic and prove nothing
//! about the table.

use kirra_world_service::freshness::{FreshnessPolicy, FreshnessSource};
use kirra_world_service::read_view::{AskError, WorldLookup, WorldView};
use kirra_world_store::{
    ClaimStatus, EventId, NewEvent, ObservationId, Validity, WorldStore, WriterClass,
};

const T0: i64 = 1_700_000_000_000;
/// One hour after the facts became valid. Past every bound in the ruled table,
/// so a `Bounded` class is unambiguously stale and a `Timeless` one is not.
const MUCH_LATER: i64 = T0 + 60 * 60 * 1_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-fresh-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    cleanup(&p);
    p
}

fn cleanup(path: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut q = path.as_os_str().to_os_string();
        q.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
}

fn claim(store: &mut WorldStore, tag: &str, kind: &str, predicate: &str) {
    store
        .append(&NewEvent {
            event_id: &EventId::new(format!("ev-{tag}")).expect("event id"),
            observation_id: &ObservationId::new(format!("obs-{tag}")).expect("obs"),
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
            kind,
            subject: "package_17",
            subject_ref: None,
            predicate: Some(predicate),
            object: None,
            payload: "{}",
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append");
}

/// Two claims about one subject, **the same age**, with different rulings:
/// `mission/last_seen_at` is `Bounded`, `observation/colour` is `Timeless`.
fn store_with_both_dispositions(name: &str) -> (WorldStore, std::path::PathBuf) {
    let path = tmp(name);
    let mut store = WorldStore::open(&path).expect("open");
    claim(&mut store, "seen", "mission", "last_seen_at");
    claim(&mut store, "colour", "observation", "colour");
    store.fold().expect("fold");
    (store, path)
}

fn validity_of(store: &WorldStore, predicate: &str) -> Validity {
    let view = WorldView::new(store, FreshnessSource::Ruled);
    let composed = view.ask("package_17", MUCH_LATER).expect("ask");
    let WorldLookup::Answered(answers) = composed.lookup() else {
        panic!("the fixture must answer, got {:?}", composed.lookup());
    };
    answers
        .iter()
        .find(|a| a.predicate() == Some(predicate))
        .unwrap_or_else(|| panic!("no answer for {predicate}"))
        .validity()
}

// ---------------------------------------------------------------------------
// The adversarial pair
// ---------------------------------------------------------------------------

/// **An old `last_seen_at` is Stale.** The claim FINDING 2 said was served as
/// though its age did not matter.
#[test]
fn an_old_recency_sensitive_fact_is_stale() {
    let (store, path) = store_with_both_dispositions("stale");
    assert_eq!(
        validity_of(&store, "last_seen_at"),
        Validity::Stale,
        "an hour-old `last_seen_at` is past its ruled bound and must say so"
    );
    drop(store);
    cleanup(&path);
}

/// **An equally old genuinely timeless fact is Timeless.**
///
/// Same `valid_from`, same clock, opposite verdict — so the difference is the
/// ruling and nothing else.
#[test]
fn an_equally_old_timeless_fact_is_timeless() {
    let (store, path) = store_with_both_dispositions("timeless");
    assert_eq!(
        validity_of(&store, "colour"),
        Validity::Timeless,
        "colour is ruled Timeless; ageing it would refuse valid answers for no \
         safety gain"
    );
    drop(store);
    cleanup(&path);
}

/// The pair asserted together, so neither arm can be edited into passing alone.
#[test]
fn the_two_dispositions_differ_at_identical_age() {
    let (store, path) = store_with_both_dispositions("pair");
    assert_ne!(
        validity_of(&store, "last_seen_at"),
        validity_of(&store, "colour"),
        "two claims of the SAME age got the same verdict — the ruled table is \
         not distinguishing anything"
    );
    drop(store);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// Unclassified semantics refuse
// ---------------------------------------------------------------------------

/// **THE BOX.** A claim whose semantics are not ruled REFUSES the query.
///
/// Not `Timeless`, not an infinite budget, and not a hidden default.
#[test]
fn an_unclassified_claim_refuses_rather_than_defaulting_to_timeless() {
    let path = tmp("unruled");
    let mut store = WorldStore::open(&path).expect("open");
    claim(&mut store, "unruled", "mission", "invented_predicate");
    store.fold().expect("fold");

    let view = WorldView::new(&store, FreshnessSource::Ruled);
    match view.ask("package_17", MUCH_LATER) {
        Err(AskError::UnclassifiedFreshness { kind, predicate }) => {
            assert_eq!(kind, "mission");
            assert_eq!(predicate.as_deref(), Some("invented_predicate"));
        }
        Ok(answer) => panic!(
            "an unruled class was SERVED rather than refused: {:?}",
            answer.lookup()
        ),
        Err(other) => panic!("wrong refusal: {other:?}"),
    }

    drop(store);
    cleanup(&path);
}

/// The refusal is not "nothing is known" — it is a policy fault.
///
/// Rule 3 says `Unknown` is a success meaning absence of knowledge. A claim
/// exists here; what is missing is the ruling about what its age means. Landing
/// in `Unknown` would tell a caller the store held nothing, which is false.
#[test]
fn the_refusal_is_an_error_not_an_unknown_answer() {
    let path = tmp("notunknown");
    let mut store = WorldStore::open(&path).expect("open");
    claim(&mut store, "unruled", "mission", "invented_predicate");
    store.fold().expect("fold");

    let view = WorldView::new(&store, FreshnessSource::Ruled);
    assert!(
        view.ask("package_17", MUCH_LATER).is_err(),
        "a missing policy is a policy-resolution failure, not a freshness state \
         and not an absence of knowledge"
    );

    drop(store);
    cleanup(&path);
}

/// One unruled claim refuses the WHOLE query, rather than being dropped.
///
/// Dropping it would silently narrow the answer: the caller would receive a
/// well-formed result missing a row it was never told about, which is the
/// under-reporting failure `SummaryKindError` refuses for the same reason.
#[test]
fn one_unruled_claim_refuses_the_whole_query_rather_than_narrowing_it() {
    let path = tmp("mixed");
    let mut store = WorldStore::open(&path).expect("open");
    claim(&mut store, "seen", "mission", "last_seen_at"); // ruled
    claim(&mut store, "unruled", "mission", "invented_predicate"); // not
    store.fold().expect("fold");

    let view = WorldView::new(&store, FreshnessSource::Ruled);
    assert!(
        view.ask("package_17", MUCH_LATER).is_err(),
        "a partially-ruled subject must refuse, not serve the ruled half and \
         quietly omit the rest"
    );

    drop(store);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// The caller source stays an explicit act
// ---------------------------------------------------------------------------

/// The caller path answers where the table refuses — and says so in a value.
///
/// This is the interim `mission_context` relies on. It is safer than the global
/// default it replaced because the classification is now something a caller
/// wrote, greppable and reviewable, rather than a branch nobody took.
#[test]
fn the_caller_source_can_classify_what_the_table_has_not_ruled() {
    let path = tmp("caller");
    let mut store = WorldStore::open(&path).expect("open");
    claim(&mut store, "unruled", "mission", "invented_predicate");
    store.fold().expect("fold");

    let view = WorldView::new(
        &store,
        FreshnessSource::Caller(FreshnessPolicy::Bounded { max_age_ms: 1_000 }),
    );
    let composed = view
        .ask("package_17", MUCH_LATER)
        .expect("caller classified it");
    let WorldLookup::Answered(answers) = composed.lookup() else {
        panic!("must answer");
    };
    assert_eq!(answers[0].validity(), Validity::Stale);

    drop(store);
    cleanup(&path);
}

/// **`Timeless` can only come from an affirmative grant.**
///
/// The invariant, stated as a test over both sources: every path that yields
/// `Timeless` traces to somebody classifying the fact that way — the ruled
/// table, or a caller writing `FreshnessPolicy::Timeless`. There is no third
/// path, because `FreshnessSource` has no variant meaning "nothing supplied".
#[test]
fn timeless_is_always_traceable_to_a_grant() {
    let (store, path) = store_with_both_dispositions("grant");

    // From the ruled table.
    assert_eq!(validity_of(&store, "colour"), Validity::Timeless);

    // From an explicit caller grant, on a class the table rules BOUNDED — so
    // the value cannot have leaked from the table.
    let view = WorldView::new(&store, FreshnessSource::Caller(FreshnessPolicy::Timeless));
    let composed = view.ask("package_17", MUCH_LATER).expect("ask");
    let WorldLookup::Answered(answers) = composed.lookup() else {
        panic!("must answer");
    };
    let seen = answers
        .iter()
        .find(|a| a.predicate() == Some("last_seen_at"))
        .expect("last_seen_at");
    assert_eq!(
        seen.validity(),
        Validity::Timeless,
        "an explicit caller grant overrides the table — and is the ONLY other \
         way to reach Timeless"
    );

    drop(store);
    cleanup(&path);
}
