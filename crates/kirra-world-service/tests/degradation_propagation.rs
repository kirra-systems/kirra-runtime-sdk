//! **Tier 3 box 3g — degradation propagation across the answer boundary.**
//!
//! > Every answer family preserves `Full`/`Degraded` **independently of the
//! > payload outcome**. Retention may reduce answer precision; Tier 3 makes the
//! > loss observable.
//!
//! # What was already true, and what this adds
//!
//! The STORE already decides `Full` vs `Degraded` correctly, on both temporal
//! axes, and `kirra-world-store/tests/degraded_answers.rs` pins it — including
//! an `as_of` pair that cannot pass by always answering `Full`. Rebuilding that
//! here would be duplication wearing a closed box.
//!
//! What did not exist is the **propagation**. `WorldLookup` is
//! `{Answered, Unknown}` and carries no completeness at all, and `ask` reads
//! `world_current`, which `compact_range` structurally protects by refusing to
//! remove a live projection head — so its completeness would be `Full` by
//! construction and could never fail. `WorldView::ask_as_of` is the first
//! boundary query that can genuinely degrade, and `TemporalLookup` is the first
//! boundary type that carries the verdict.
//!
//! # The property, as agreed
//!
//! > If retained evidence is sufficient for the query, completeness is `Full`.
//! > If the query depends on evidence removed by compaction, completeness is
//! > `Degraded`. Tier 3 may over-report degradation, but it must **never**
//! > report `Full` after relevant evidence has been lost.
//!
//! That asymmetry is `Resolution`'s own documented contract, so **3g proves
//! non-loss of degradation information, not minimal classification.** A test
//! demanding "Degraded exactly when it mattered" would contradict the contract
//! and force a precision the retained evidence cannot justify — the removed rows
//! are the only record of themselves.
//!
//! # This suite is deliberately STRICTER than the contract, in one direction
//!
//! Measured, not assumed. Three mutations were run:
//!
//! | Mutation | Result |
//! |---|---|
//! | force `Full` in the degraded arm | 3 tests fail — the load-bearing direction |
//! | drop completeness on `Unknown` | the independence test fails |
//! | force `Degraded` everywhere | **the Full arms fail** |
//!
//! The third is the one worth stating plainly. `Resolution`'s contract PERMITS
//! over-reporting — it may say `Degraded` where a full answer would have been
//! identical — so a move in that direction is legal, and these tests would red
//! anyway. They pin current BEHAVIOUR, not the contract's outer bound.
//!
//! That is the right trade rather than an oversight: without asserting `Full`,
//! an implementation that answered `Degraded` unconditionally would pass every
//! remaining case, and the degraded arm would prove nothing. The Full arms are
//! what stop the pair collapsing.
//!
//! So if a future change legitimately makes an arm here `Degraded`, the correct
//! response is to update the test **with the reasoning written down**, not to
//! treat it as a regression — and emphatically not to relax the degraded arm to
//! match. Only one direction is a bug: `Full` after evidence was lost.
//!
//! # Both arms return a plausible, non-empty answer
//!
//! Deliberately. A pair that distinguished "got an answer" from "got nothing"
//! would be measuring emptiness, not completeness. In the degraded arm the
//! answer is not merely non-empty — it is **plausible and wrong**: the query
//! instant falls inside the compacted window, so the surviving evidence yields
//! `dock_alpha` when `dock_beta` was the truth. Only the completeness axis
//! reveals that, which is precisely the loss 3g exists to make observable.

use kirra_world_service::read_view::{WorldLookup, WorldView};
use kirra_world_store::{ClaimStatus, EventId, NewEvent, ObservationId, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!("kirra-3g-{name}-{}-{n}.sqlite", std::process::id()));
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

/// `package_17 last_seen_at <object>`, valid and known from `at_ms`.
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

/// Three observations, the middle one compacted away.
///
/// ```text
///   gen 1   T0+0     dock_alpha    survives  (earliest evidence)
///   gen 2   T0+100   dock_beta     COMPACTED (the removed window)
///   gen 3   T0+200   dock_gamma    survives  (the projection head)
/// ```
///
/// Generation 2 is compactable precisely because it is superseded — the head is
/// protected, so only a superseded observation can be taken away, which is also
/// why the surviving answer stays plausible.
fn store_with_a_hole(name: &str) -> (WorldStore, std::path::PathBuf) {
    let path = tmp(name);
    let mut store = WorldStore::open(&path).expect("open");
    claim(&mut store, "alpha", "dock_alpha", T0);
    claim(&mut store, "beta", "dock_beta", T0 + 100);
    claim(&mut store, "gamma", "dock_gamma", T0 + 200);
    store.fold().expect("fold");

    let outcome = store
        .compact_range(2, 2, T0 + 9_000)
        .expect("generation 2 is superseded, so compactable");
    assert_eq!(outcome.removed, 1, "the fixture must remove an observation");
    (store, path)
}

fn objects(lookup: &WorldLookup) -> Vec<String> {
    match lookup {
        WorldLookup::Answered(a) => a
            .iter()
            .filter_map(|x| x.object().map(String::from))
            .collect(),
        WorldLookup::Unknown(_) => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// The pair
// ---------------------------------------------------------------------------

/// **Evidence entirely outside the compacted span → `Full`, with a real answer.**
///
/// At `T0+50` the only observation that can bear on the answer is `dock_alpha`,
/// which survives. The removed window begins at `T0+100`, later on BOTH axes, so
/// nothing lost could have changed this answer and reporting `Full` is honest.
#[test]
fn evidence_outside_the_compacted_span_is_full_and_still_answers() {
    let (store, path) = store_with_a_hole("full");
    let view = WorldView::new(&store, None);

    let out = view
        .ask_as_of("package_17", T0 + 50, T0 + 50)
        .expect("ask_as_of");

    assert_eq!(
        objects(out.lookup()),
        vec!["dock_alpha".to_string()],
        "the full arm must carry a real answer, not an empty one"
    );
    assert!(
        !out.is_degraded(),
        "the removed window begins after this query on both axes; nothing lost \
         could have borne on it — got {:?}",
        out.completeness()
    );

    drop(store);
    cleanup(&path);
}

/// **Evidence inside the compacted span → `Degraded`, with a plausible WRONG
/// answer.**
///
/// The sharp half. At `T0+150` the truth was `dock_beta` — and `dock_beta` has
/// been deleted. The replay finds `dock_alpha`, which is non-empty, well-formed
/// and entirely believable. Nothing in the payload betrays the loss; only the
/// completeness axis does.
///
/// This is why 3g is not satisfied by a test that distinguishes an answer from
/// silence: here the wrong answer and the right one are the same shape.
#[test]
fn evidence_inside_the_compacted_span_is_degraded_and_the_answer_looks_fine() {
    let (store, path) = store_with_a_hole("degraded");
    let view = WorldView::new(&store, None);

    let out = view
        .ask_as_of("package_17", T0 + 150, T0 + 9_000)
        .expect("ask_as_of");

    assert_eq!(
        objects(out.lookup()),
        vec!["dock_alpha".to_string()],
        "the surviving evidence yields a plausible answer — that is the point"
    );
    assert!(
        out.is_degraded(),
        "dock_beta held at T0+150 and was deleted; reporting Full here would be \
         the silent rewrite the box forbids — got {:?}",
        out.completeness()
    );
    assert!(
        !out.completeness().spans().is_empty(),
        "a degraded answer must name the span it is degraded by"
    );

    // The two arms genuinely differ, so the pair is distinguishing completeness
    // rather than agreeing by accident.
    let full = view
        .ask_as_of("package_17", T0 + 50, T0 + 50)
        .expect("ask_as_of");
    assert_ne!(full.is_degraded(), out.is_degraded());
    assert_eq!(
        objects(full.lookup()),
        objects(out.lookup()),
        "and they differ ONLY in completeness — same payload, opposite verdict, \
         which is what 'independently of the payload outcome' means"
    );

    drop(store);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// Independence from the payload outcome
// ---------------------------------------------------------------------------

/// **An `Unknown` still carries completeness, and can be `Degraded`.**
///
/// The tempting shape is to attach a resolution only to `Answered` — an empty
/// answer has nothing to be incomplete about. But then *"nothing was known"* and
/// *"we deleted it"* become the same value, which is exactly the confusion an
/// incident reconstruction cannot afford.
///
/// Here the subject's ONLY observation is compacted away, so the answer is
/// empty AND lossy. A boundary that dropped completeness on `Unknown` would
/// report this identically to a subject the store had never heard of.
#[test]
fn an_empty_answer_still_reports_that_evidence_was_lost() {
    let path = tmp("unknown-degraded");
    let mut store = WorldStore::open(&path).expect("open");
    // `solo` is superseded by a later observation so it can be compacted, and
    // the query instant is chosen BEFORE the survivor's valid time, so the
    // answer is genuinely empty at that instant.
    claim(&mut store, "solo", "dock_solo", T0 + 100);
    claim(&mut store, "later", "dock_later", T0 + 200);
    store.fold().expect("fold");
    store.compact_range(1, 1, T0 + 9_000).expect("compact");

    let view = WorldView::new(&store, None);
    let out = view
        .ask_as_of("package_17", T0 + 150, T0 + 9_000)
        .expect("ask_as_of");

    assert!(
        matches!(out.lookup(), WorldLookup::Unknown(_)),
        "the only observation holding at T0+150 was deleted, so the payload is \
         empty — got {:?}",
        objects(out.lookup())
    );
    assert!(
        out.is_degraded(),
        "an empty answer that is empty BECAUSE evidence was deleted must say so; \
         otherwise it is indistinguishable from a subject never heard of"
    );

    drop(store);
    cleanup(&path);
}

/// **A subject the compacted window never held is `Full`.**
///
/// The other half of independence: completeness is not a property of the STORE
/// having compacted something, it is a property of THIS query's evidence. A
/// boundary that degraded every answer once any compaction had run would be
/// conservative to the point of uselessness — and would pass the degraded arm
/// above for the wrong reason.
#[test]
fn a_subject_the_compacted_window_never_held_is_full() {
    let path = tmp("other-subject");
    let mut store = WorldStore::open(&path).expect("open");
    claim(&mut store, "alpha", "dock_alpha", T0);
    claim(&mut store, "beta", "dock_beta", T0 + 100);
    store
        .append(&NewEvent {
            event_id: &EventId::new("ev-other").expect("id"),
            observation_id: &ObservationId::new("obs-other").expect("obs"),
            txn_time_ms: T0 + 50,
            valid_from_ms: T0 + 50,
            valid_to_ms: None,
            source: "warehouse-scanner",
            source_version: "1.0.0",
            writer_class: WriterClass::Sensor,
            claim_status: ClaimStatus::Confirmed,
            provenance: &[],
            frame_id: None,
            map_id: None,
            kind: "mission",
            subject: "pallet_9",
            subject_ref: None,
            predicate: Some("last_seen_at"),
            object: Some("bay_3"),
            payload: "{}",
            payload_schema: 1,
            retention_class: "raw",
            trust: None,
        })
        .expect("append other subject");
    store.fold().expect("fold");
    store.compact_range(1, 1, T0 + 9_000).expect("compact");

    let view = WorldView::new(&store, None);
    let out = view
        .ask_as_of("pallet_9", T0 + 1_000, T0 + 9_000)
        .expect("ask_as_of");

    assert_eq!(objects(out.lookup()), vec!["bay_3".to_string()]);
    assert!(
        !out.is_degraded(),
        "the compacted window held nothing about pallet_9 — degrading here would \
         make every answer degraded forever after the first compaction"
    );

    drop(store);
    cleanup(&path);
}

/// **The boundary propagates the store's verdict rather than forming its own.**
///
/// A second judgement at the boundary would be a second implementation of the
/// rule that decides whether an answer can be trusted, and the two would drift —
/// the same defect the `AnswerRef` corpus caught when supersession turned out to
/// have two implementations. This pins them equal on both arms.
#[test]
fn the_boundary_verdict_equals_the_stores_verdict() {
    let (store, path) = store_with_a_hole("propagates");
    let view = WorldView::new(&store, None);

    for (valid_at, known_at) in [(T0 + 50, T0 + 50), (T0 + 150, T0 + 9_000)] {
        let store_side = store
            .as_of("package_17", valid_at, known_at)
            .expect("store as_of");
        let boundary = view
            .ask_as_of("package_17", valid_at, known_at)
            .expect("ask_as_of");
        assert_eq!(
            &store_side.resolution,
            boundary.completeness(),
            "the boundary must carry the store's verdict unchanged at \
             ({valid_at}, {known_at})"
        );
    }

    drop(store);
    cleanup(&path);
}
