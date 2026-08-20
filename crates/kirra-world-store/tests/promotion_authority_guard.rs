//! **`KIRRA-WM-PROMOTION-001` at the write door** — a matcher may propose, never confirm.
//!
//! > *Clustering may PROPOSE co-reference; it may never CONFIRM identity.*
//! > A heuristic or learned matcher is authorized as a candidate producer;
//! > confirmed identity still arrives only through explicit adjudication over
//! > recorded evidence.
//!
//! Until schema v8 that rule was **policy, not enforcement**. SD-2's `CHECK`
//! names `llm_candidate` and only `llm_candidate`, so `derivation` +
//! `confirmed` was accepted by the store. `WM_SCOPE.md` §2a had flagged it, and
//! also recorded that an earlier draft of that same section wrongly claimed the
//! boundary was already "extended to `derivation`" — which is why the hole was
//! established by probing the store rather than by reading either text.
//!
//! The rule held only because no `derivation`-class producer existed yet. Box
//! 2a is exactly such a producer, so the guard lands before it.
//!
//! # What these tests are careful about
//!
//! The rule now has **two** enforcement sites for **two** writer classes: a v1
//! `CHECK` for `llm_candidate`, a v8 trigger for `derivation`. A test that only
//! went through `append` would prove the Rust early-refusals fire and say
//! nothing about whether the store itself is safe — and the store is what a
//! future producer written by someone else will meet. So the load-bearing test
//! goes around Rust entirely, in raw SQL, and asserts the COMBINATION is
//! complete rather than trusting either half.

use kirra_world::reference::{EventId, ObservationId};
use kirra_world_store::{ClaimStatus, NewEvent, StoreError, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-promo-guard-{name}-{}-{n}.sqlite",
        std::process::id()
    ));
    for s in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(s);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

/// One `same_as` claim — the exact shape box 2a's matcher will emit.
fn claim<'a>(
    event_id: &'a EventId,
    observation_id: &'a ObservationId,
    writer_class: WriterClass,
    claim_status: ClaimStatus,
) -> NewEvent<'a> {
    NewEvent {
        event_id,
        observation_id,
        txn_time_ms: T0,
        valid_from_ms: T0,
        valid_to_ms: None,
        source: "track-matcher",
        source_version: "2.3.1",
        writer_class,
        claim_status,
        provenance: &[],
        frame_id: None,
        map_id: None,
        kind: "observation",
        subject: "track-a",
        subject_ref: None,
        predicate: Some("same_as"),
        object: Some("track-b"),
        payload: "{}",
        payload_schema: 1,
        retention_class: "raw",
        trust: None,
    }
}

fn ids(n: &str) -> (EventId, ObservationId) {
    (
        EventId::new(format!("ev-{n}")).expect("event id"),
        ObservationId::new(format!("obs-{n}")).expect("observation id"),
    )
}

/// A raw `INSERT` naming every `NOT NULL` column, going around `append` and
/// every Rust-side refusal with it.
///
/// Built here rather than as a new `*_for_test` seam on the store: the existing
/// [`WorldStore::raw_execute_for_test`] is precisely the "arbitrary SQL, so a
/// test can attempt what the writer refuses" hatch, and adding a second one
/// would widen the test surface to say something it can already say.
fn raw_insert(
    store: &WorldStore,
    n: &str,
    writer_class: &str,
    claim_status: &str,
) -> Result<(), StoreError> {
    store.raw_execute_for_test(&format!(
        "INSERT INTO world_events (
             event_id, observation_id, txn_time_ms, valid_from_ms,
             source, source_version, writer_class, claim_status, provenance,
             kind, subject, predicate, object,
             payload, payload_schema, payload_digest,
             retention_class, chain_digest
         ) VALUES (
             'ev-{n}-{writer_class}', 'obs-{n}-{writer_class}', {T0}, {T0},
             'track-matcher', '2.3.1', '{writer_class}', '{claim_status}', '[]',
             'observation', 'track-a', 'same_as', 'track-b',
             '{{}}', 1, 'digest',
             'raw', 'chain'
         )"
    ))
}

// ---------------------------------------------------------------------------
// The load-bearing test: the STORE refuses, not merely the Rust wrapper
// ---------------------------------------------------------------------------

/// **Neither unauthorized class can self-confirm, even bypassing `append`.**
///
/// Raw `INSERT`s, going around every Rust-side refusal. This is the test that
/// speaks to the threat the rule actually names — a producer written later, by
/// someone else, against the same store — because a policy enforced only in a
/// function is enforced only for callers of that function.
///
/// Asserting BOTH classes in one test is the point: the rule is split across a
/// `CHECK` and a trigger, and checking only the new half would leave "one rule,
/// two mechanisms, one of them silently dropped" undetectable.
#[test]
fn neither_unauthorized_class_can_self_confirm_through_raw_sql() {
    let path = tmp("raw-sql");
    let store = WorldStore::open(&path).expect("open");

    for class in ["derivation", "llm_candidate"] {
        let err = raw_insert(&store, "conf", class, "confirmed")
            .expect_err(&format!("{class} + confirmed must be refused by the store"));
        assert!(
            format!("{err:?}").contains("Sqlite"),
            "{class} must be refused at the SQL layer, not by a Rust guard: {err:?}"
        );
    }
}

/// **The proposing shape those same classes exist for is still admitted.**
///
/// The non-vacuity control for the test above. Without it, a guard that refused
/// every `derivation` write — or every write at all — would look identical.
/// A matcher must remain able to do its job.
#[test]
fn the_same_classes_may_still_propose_candidates() {
    let path = tmp("propose");
    let store = WorldStore::open(&path).expect("open");

    for class in ["derivation", "llm_candidate"] {
        raw_insert(&store, "cand", class, "candidate")
            .unwrap_or_else(|e| panic!("{class} must still be able to propose: {e:?}"));
    }
}

/// **An authorized class is untouched.** The guard is about WHO confirms, not
/// about confirmation. A `sensor` and an `operator` still write confirmed
/// claims — the second being the adjudicator `KIRRA-WM-PROMOTION-001` names as
/// the v1 promotion authority, so refusing it would break the rule it enforces.
#[test]
fn authorized_classes_still_confirm() {
    let path = tmp("authorized");
    let store = WorldStore::open(&path).expect("open");

    for class in ["sensor", "operator"] {
        raw_insert(&store, "auth", class, "confirmed")
            .unwrap_or_else(|e| panic!("{class} must still be able to confirm: {e:?}"));
    }
}

// ---------------------------------------------------------------------------
// The Rust half: the caller gets a message naming the rule
// ---------------------------------------------------------------------------

/// `append` refuses before building the statement, so the error names the RULE
/// rather than surfacing a constraint index — the shape `LlmCannotConfirm`
/// already had, now with the sibling it was always missing.
#[test]
fn append_names_the_rule_rather_than_the_constraint() {
    let path = tmp("named");
    let mut s = WorldStore::open(&path).expect("open");

    let (e, o) = ids("deriv");
    assert!(
        matches!(
            s.append(&claim(
                &e,
                &o,
                WriterClass::Derivation,
                ClaimStatus::Confirmed
            )),
            Err(StoreError::DerivationCannotConfirm)
        ),
        "derivation + confirmed must be refused by name"
    );

    let (e2, o2) = ids("llm");
    assert!(
        matches!(
            s.append(&claim(
                &e2,
                &o2,
                WriterClass::LlmCandidate,
                ClaimStatus::Confirmed
            )),
            Err(StoreError::LlmCannotConfirm)
        ),
        "the sibling refusal must be unchanged"
    );

    // Non-vacuity: the very same claim, proposed rather than confirmed, lands.
    let (e3, o3) = ids("candidate");
    s.append(&claim(
        &e3,
        &o3,
        WriterClass::Derivation,
        ClaimStatus::Candidate,
    ))
    .expect("a derivation-class CANDIDATE is exactly what 2a will write");
}

/// The message a reader gets must name the ruling, not a SQLite constraint
/// index. `KIRRA-WM-PROMOTION-001` is the searchable token that leads to the
/// rationale, so the test pins it rather than the prose around it.
#[test]
fn the_message_carries_the_ruling_id() {
    let text = format!("{}", StoreError::DerivationCannotConfirm);
    assert!(
        text.contains("KIRRA-WM-PROMOTION-001"),
        "the refusal must name the ruling that motivates it: {text}"
    );
    assert!(
        text.contains("derivation") && text.contains("confirmed"),
        "and the pairing it refused: {text}"
    );
}

// ---------------------------------------------------------------------------
// Migration: inherited rows survive and are nameable
// ---------------------------------------------------------------------------

/// **A fresh store has nothing to report.** The floor for the test below: if
/// `unauthorized_confirmation_rows` returned rows on a clean store, its
/// non-empty result would mean nothing.
#[test]
fn a_clean_store_reports_no_unauthorized_confirmations() {
    let path = tmp("clean");
    let mut s = WorldStore::open(&path).expect("open");
    let (e, o) = ids("ok");
    s.append(&claim(&e, &o, WriterClass::Sensor, ClaimStatus::Confirmed))
        .expect("append");
    assert!(
        s.unauthorized_confirmation_rows()
            .expect("query")
            .is_empty(),
        "a store created at v8 cannot hold one"
    );
}

/// **The store stamps v8**, so a reader can tell a guarded store from an
/// unguarded one without probing its behaviour.
#[test]
fn the_schema_stamps_the_guarded_version() {
    let path = tmp("version");
    let s = WorldStore::open(&path).expect("open");
    assert_eq!(s.schema_version().expect("version"), 8);
}

/// Return a store to its pre-guard state: drop the v8 trigger and roll the
/// stamp back to 7, so the next `open` takes the migration path a real v7
/// store would.
fn unguard(store: &WorldStore) {
    store
        .raw_execute_for_test("DROP TRIGGER world_events_derivation_cannot_confirm")
        .expect("drop trigger");
    store
        .raw_execute_for_test(
            "UPDATE world_store_meta SET value = '7' WHERE key = 'schema_version'",
        )
        .expect("roll back the stamp");
}

/// **A store migrated from v7 ends up as guarded as one born at v8.**
///
/// This test exists because the bug it describes was made and caught here.
/// `open` has TWO paths — an explicit enumeration for a new store and
/// `migrate` for an existing one — and a migration added to only one of them
/// produces a store **stamped v8 with no v8 trigger**. That store passes a
/// version check and fails the rule.
///
/// Which is the wider point: `the_schema_stamps_the_guarded_version` passed
/// throughout that bug. A version stamp records what someone said the schema
/// was. Only behaviour records what it is, so this asserts behaviour.
#[test]
fn a_store_migrated_from_v7_gains_the_guard() {
    let path = tmp("migrated");
    {
        let store = WorldStore::open(&path).expect("open");
        unguard(&store);
        // Non-vacuity: while unguarded, the write the guard forbids succeeds.
        // Without this the test could not tell "migration installed the guard"
        // from "the guard was never actually removed".
        raw_insert(&store, "pre", "derivation", "confirmed")
            .expect("an unguarded store is exactly what v8 exists to end");
    }

    let store = WorldStore::open(&path).expect("reopen runs the migration");
    assert_eq!(store.schema_version().expect("version"), 8);
    raw_insert(&store, "post", "derivation", "confirmed")
        .expect_err("after migrating, the store must refuse it");
}

/// **A row inherited from before the guard survives, and can be named.**
///
/// The migration's stated contract: it constrains future inserts and touches
/// nothing already written, because repairing a hash-chained append-only log is
/// a much larger decision than a migration is entitled to make. So the honest
/// outcome is a store that stops the next one and can report the ones it
/// inherited — which is what makes them a finding rather than a silence.
#[test]
fn rows_inherited_from_before_the_guard_survive_and_are_nameable() {
    let path = tmp("inherited");
    {
        let store = WorldStore::open(&path).expect("open");
        unguard(&store);
        raw_insert(&store, "legacy", "derivation", "confirmed").expect("pre-guard write");
    }

    let store = WorldStore::open(&path).expect("reopen runs the migration");
    let found = store.unauthorized_confirmation_rows().expect("query");
    assert_eq!(
        found.len(),
        1,
        "the inherited row must SURVIVE migration, not be coerced or dropped: {found:?}"
    );
    assert_eq!(found[0].1, "track-a", "and be named by subject: {found:?}");
}
