//! **Tier 3 — the generation-pinned read.**
//!
//! `KIRRA-WM-ANSWER-IDENTITY-001` rules that resolving an `AnswerRef` means
//! *"re-execute this exact deterministic query against the same snapshot"*.
//! Until this existed the ruling had no mechanism behind it:
//! `projection_generation()` could report the coordinate, and nothing could read
//! AT it. `WM_SCOPE.md` carried the gap as its own open box.
//!
//! Four claims are pinned here, and the third is the one the box turns on:
//!
//! 1. A pinned read returns the state at that generation — **not** current
//!    state. Proved by pinning at a generation whose answer DIFFERS from now.
//! 2. A pinned read at the head equals the live read, so the reconstruction is
//!    the same fold and not a second implementation that happens to agree on
//!    old data.
//! 3. **Compaction makes a generation irreproducible, and the read REFUSES**
//!    rather than falling forward to current state.
//! 4. A generation ahead of the head refuses too, rather than clamping.
//!
//! # Why case 3 is the sharp one
//!
//! Falling forward is not merely wrong, it is wrong in the way that looks right:
//! the caller asked what was true at generation 1 and receives what is true at
//! generation 2, with nothing in the value to say so. The fixture is built so a
//! fall-forward implementation returns a *plausible, non-empty, wrong* answer —
//! `dock_a` where `dock_old` was the truth — because a refusal that only ever
//! fired when the answer would have been empty anyway proves nothing.

use kirra_world_store::snapshot::{Irreproducible, PinnedRead};
use kirra_world_store::{ClaimStatus, EventId, NewEvent, ObservationId, WorldStore, WriterClass};

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-pinned-read-{name}-{}-{n}.sqlite",
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

/// Record `package_17 last_seen_at <object>` at `at_ms`.
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

/// The object of the single `package_17` claim, as of a pinned read.
fn pinned_object(store: &WorldStore, generation: i64) -> Option<String> {
    match store.read_at_generation(generation).expect("pinned read") {
        PinnedRead::Reproduced(p) => p
            .current("package_17", T0 + 1_000)
            .first()
            .and_then(|c| c.object.clone()),
        PinnedRead::Irreproducible(r) => panic!("expected a reproduction, got {r:?}"),
    }
}

/// A store where the answer CHANGED: `dock_old` at generation 1, `dock_a` at 2.
///
/// The supersession is what makes generation 1 both interesting and compactable
/// — `compact_range` refuses to remove a live projection head, so only a
/// superseded event can be taken away.
fn store_that_changed_its_mind(name: &str) -> (WorldStore, std::path::PathBuf) {
    let path = tmp(name);
    let mut store = WorldStore::open(&path).expect("open");
    claim(&mut store, "1", "dock_old", T0);
    store.fold().expect("fold");
    claim(&mut store, "2", "dock_a", T0 + 1);
    store.fold().expect("fold");
    (store, path)
}

// ---------------------------------------------------------------------------
// 1 & 2. The pin actually pins
// ---------------------------------------------------------------------------

/// **A pinned read answers as of THEN, not as of now.**
///
/// The whole point in one assertion. Generation 1 said `dock_old`; generation 2
/// says `dock_a`. A pinned read at 1 that returned `dock_a` would be reporting
/// current state under a past coordinate.
#[test]
fn a_pinned_read_returns_the_state_at_that_generation() {
    let (store, path) = store_that_changed_its_mind("changed");

    assert_eq!(
        pinned_object(&store, 1).as_deref(),
        Some("dock_old"),
        "generation 1 held dock_old — a pinned read must not report today's answer"
    );
    assert_eq!(
        pinned_object(&store, 2).as_deref(),
        Some("dock_a"),
        "generation 2 held dock_a"
    );

    // Non-vacuity: the two really are different, so the assertion above is
    // distinguishing something.
    assert_ne!(pinned_object(&store, 1), pinned_object(&store, 2));

    drop(store);
    cleanup(&path);
}

/// **At the head, the pinned read and the live read agree.**
///
/// Guards the other direction: a reconstruction that agreed with history but
/// disagreed with the present would be a second, divergent implementation of the
/// projection. It is the same reducer over a bounded input, and this says so.
#[test]
fn a_pinned_read_at_the_head_equals_the_live_read() {
    let (store, path) = store_that_changed_its_mind("head");
    let head = store
        .projection_coordinate()
        .expect("coordinate")
        .world_current()
        .generation();

    let live = store.current("package_17", T0 + 1_000).expect("live read");
    let pinned = match store.read_at_generation(head).expect("pinned") {
        PinnedRead::Reproduced(p) => p.current("package_17", T0 + 1_000),
        PinnedRead::Irreproducible(r) => panic!("the head must be reproducible, got {r:?}"),
    };

    assert_eq!(live.len(), pinned.len());
    assert_eq!(
        live.first().and_then(|c| c.object.clone()),
        pinned.first().and_then(|c| c.object.clone()),
        "the pinned reconstruction at the head must equal the live projection"
    );

    drop(store);
    cleanup(&path);
}

/// **Generation 0 is the empty projection that preceded every event.**
///
/// Legal, not an error: "before anything was known" is a real state and it
/// reconstructs trivially.
#[test]
fn generation_zero_reconstructs_the_empty_projection() {
    let (store, path) = store_that_changed_its_mind("zero");

    match store.read_at_generation(0).expect("pinned") {
        PinnedRead::Reproduced(p) => {
            assert!(p.is_empty(), "nothing was known before the first event");
            assert_eq!(p.generation(), 0);
        }
        PinnedRead::Irreproducible(r) => panic!("generation 0 is reproducible, got {r:?}"),
    }

    drop(store);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// 3. The sharp case: compaction ends reproducibility, and it REFUSES
// ---------------------------------------------------------------------------

/// **A compacted generation refuses — it does not fall forward.**
///
/// Generation 1's evidence is deleted. The fixture is deliberately built so
/// falling forward yields a plausible non-empty answer (`dock_a`), because a
/// refusal that only fired when the fallback was empty would be
/// indistinguishable from doing nothing.
#[test]
fn a_compacted_generation_refuses_rather_than_falling_forward() {
    let (mut store, path) = store_that_changed_its_mind("compacted");

    // Before: reproducible, and it says dock_old.
    assert_eq!(pinned_object(&store, 1).as_deref(), Some("dock_old"));

    let outcome = store
        .compact_range(1, 1, T0 + 5_000)
        .expect("generation 1 is superseded, so compactable");
    assert_eq!(
        outcome.removed, 1,
        "the fixture must actually remove an event"
    );

    match store.read_at_generation(1).expect("pinned read") {
        PinnedRead::Reproduced(p) => panic!(
            "fell forward: returned {:?} for a generation whose evidence is gone",
            p.current("package_17", T0 + 1_000)
                .first()
                .and_then(|c| c.object.clone())
        ),
        PinnedRead::Irreproducible(Irreproducible::Compacted { spans }) => {
            assert!(!spans.is_empty(), "the refusal must name what was removed");
            assert_eq!(spans[0].lo_generation, 1);
        }
        PinnedRead::Irreproducible(other) => {
            panic!("compaction must be reported as such, got {other:?}")
        }
    }

    // And the live read still works — the refusal is about REPRODUCING a past
    // generation, not about the store being broken.
    assert_eq!(
        store
            .current("package_17", T0 + 1_000)
            .expect("live read")
            .first()
            .and_then(|c| c.object.clone())
            .as_deref(),
        Some("dock_a"),
        "current state is unaffected; only the pinned reconstruction is refused"
    );

    drop(store);
    cleanup(&path);
}

/// **Compaction ends reproducibility for generations ABOVE the removed span too.**
///
/// Rebuilding at generation 2 folds every confirmed event `<= 2`, and one of
/// them is gone. The fold cannot be reproduced, whatever its result would have
/// been — here it would in fact have been identical, since generation 1 was
/// superseded. That is the documented over-refusal, and it is pinned so the
/// behaviour is a decision rather than a surprise: the compaction floor is also
/// the floor on how far back answers stay reproducible.
#[test]
fn compaction_also_ends_reproducibility_above_the_removed_span() {
    let (mut store, path) = store_that_changed_its_mind("above");
    store.compact_range(1, 1, T0 + 5_000).expect("compact");

    let head = store
        .projection_coordinate()
        .expect("coordinate")
        .world_current()
        .generation();
    assert!(head >= 2);

    assert!(
        matches!(
            store.read_at_generation(head).expect("pinned"),
            PinnedRead::Irreproducible(Irreproducible::Compacted { .. })
        ),
        "a generation above the removed span still folds the removed event"
    );

    drop(store);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// 4. The future refuses rather than clamping
// ---------------------------------------------------------------------------

/// **A generation ahead of the head refuses.**
///
/// Clamping to the head would answer a question that was not asked, and would
/// do it silently — the same class of defect as falling forward.
#[test]
fn a_generation_ahead_of_the_head_refuses() {
    let (store, path) = store_that_changed_its_mind("future");
    let head = store
        .projection_coordinate()
        .expect("coordinate")
        .world_current()
        .generation();

    match store.read_at_generation(head + 100).expect("pinned") {
        PinnedRead::Irreproducible(Irreproducible::NotYetReached { head: reported }) => {
            assert_eq!(reported, head, "the refusal reports how far the store got");
        }
        other => panic!("a future generation must refuse, got {other:?}"),
    }

    drop(store);
    cleanup(&path);
}

/// **A negative generation is a malformed query, not an outcome.**
///
/// Rule 3's split: `Irreproducible` reports facts about the DATA; a negative
/// generation is neither compacted nor in the future, it is nonsense.
#[test]
fn a_negative_generation_is_an_error_not_an_outcome() {
    let (store, path) = store_that_changed_its_mind("negative");
    assert!(
        store.read_at_generation(-1).is_err(),
        "a negative generation belongs in the error channel"
    );

    drop(store);
    cleanup(&path);
}

// ---------------------------------------------------------------------------
// Determinism — the property step 2's AnswerRef will rest on
// ---------------------------------------------------------------------------

/// **Re-executing at the same generation returns the same answer.**
///
/// This is the property a reproducible descriptor needs: the same coordinate
/// twice must not drift, even across appends and folds that move the head. A
/// pinned read that quietly tracked the head would fail this the moment
/// something else wrote.
#[test]
fn re_execution_at_the_same_generation_is_stable_across_later_writes() {
    let (mut store, path) = store_that_changed_its_mind("stable");

    let first = pinned_object(&store, 1);

    // The world moves on: two more claims and a fold.
    claim(&mut store, "3", "dock_b", T0 + 2);
    claim(&mut store, "4", "dock_c", T0 + 3);
    store.fold().expect("fold");

    let second = pinned_object(&store, 1);
    assert_eq!(
        first, second,
        "the same coordinate must answer the same way after unrelated writes"
    );
    assert_eq!(first.as_deref(), Some("dock_old"));

    // Non-vacuity: the head really did move, so "stable" is not "nothing
    // happened".
    assert_eq!(
        store
            .current("package_17", T0 + 1_000)
            .expect("live")
            .first()
            .and_then(|c| c.object.clone())
            .as_deref(),
        Some("dock_c"),
        "the store moved on, or this test proves nothing"
    );

    drop(store);
    cleanup(&path);
}
