//! **The bounded neighbour read behind box 5b's `Related` query.**
//!
//! These are the STORE-level properties: the bound, the index that makes it a
//! bound, the disjointness the `UNION ALL` rests on, and the D-20 guard. The
//! end-to-end behaviour over the real production chain is
//! `kirra-world-service/tests/related_query.rs`.

use kirra_world::reference::EntityId;
use kirra_world_store::relationship_projection::MAX_RELATED;
use kirra_world_store::WorldStore;

fn tmp(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!("kirra-5b-{name}-{}-{n}.sqlite", std::process::id()));
    for s in ["", "-wal", "-shm"] {
        let mut q = p.as_os_str().to_os_string();
        q.push(s);
        let _ = std::fs::remove_file(std::path::PathBuf::from(q));
    }
    p
}

fn eid(s: &str) -> EntityId {
    EntityId::new(s).expect("entity id")
}

/// Force the projection table into existence, then write rows directly.
///
/// Raw SQL deliberately: these tests are about the READ's bound and index, and
/// driving 257 neighbours through the real adjudication door would make the
/// fixture the subject of the test. The end-to-end proof that real production
/// data reaches this read lives in the service crate.
fn store_with_projection(name: &str) -> WorldStore {
    let mut store = WorldStore::open(&tmp(name)).expect("open");
    store
        .fold_relationship_projection()
        .expect("install + fold");
    store
}

fn insert(store: &WorldStore, low: &str, high: &str, generation: i64) {
    store
        .raw_execute_for_test(&format!(
            "INSERT INTO relationships_projection
                 (low, high, decided_generation, candidate_observation_id,
                  adjudicator, decided_at_ms, decided_at_domain)
             VALUES ('{low}', '{high}', {generation}, 'cand-obs-1', 'op', 1, 'system')"
        ))
        .expect("insert");
}

/// **`related` on a store that never folded installs no projection table.**
///
/// The D-20 guard, and the control for a claim `related`'s own docs make. The
/// read self-heals a missing INDEX, and the obvious way to write that —
/// calling `ensure_relationship_projection()` — would also install the TABLE,
/// putting its root pages into every store that merely asked a question.
/// ADR-0041 D-20's `log_only_bytes` is the size of a store holding only the
/// event log, and D-2's retention horizons rest on that comparison.
#[test]
fn related_on_an_unfolded_store_is_empty_and_installs_nothing() {
    let store = WorldStore::open(&tmp("unfolded")).expect("open");
    let answer = store.related(&eid("track-a")).expect("related");
    assert!(answer.is_empty());
    assert!(!answer.truncated);
    assert!(
        !store
            .has_relationship_projection()
            .expect("catalogue readable"),
        "asking a question must not create a projection table — D-20's \
         log_only_bytes is measured on a store that never projected"
    );
}

/// **A missing reverse index is restored by the read.**
///
/// A projection installed before `relationships_projection_high` existed would
/// otherwise answer correctly while SCANNING for the `high = ?` half — a
/// correct answer with unbounded work, which is precisely the shape of the
/// three defects the boundedness gate exists for, and invisible in the result.
#[test]
fn related_heals_a_missing_reverse_index() {
    let store = store_with_projection("heal");
    insert(&store, "track-a", "track-b", 1);

    store
        .raw_execute_for_test("DROP INDEX relationships_projection_high")
        .expect("drop the index");
    assert_eq!(
        store
            .query_scalar_for_test(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' \
                 AND name='relationships_projection_high'"
            )
            .expect("count"),
        0,
        "the index must actually be gone, or this test proves nothing"
    );

    let answer = store.related(&eid("track-b")).expect("related");
    assert_eq!(answer.len(), 1, "the answer must still be correct");
    assert_eq!(
        store
            .query_scalar_for_test(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' \
                 AND name='relationships_projection_high'"
            )
            .expect("count"),
        1,
        "and the index must be back, or the bound is not a bound"
    );
}

/// **Found from EITHER side of the canonical pair.**
///
/// A pair is stored once as `(low, high)`, so an implementation that looked in
/// one column would answer correctly for whichever entity happened to sort
/// first and report the other as related to nothing. The two assertions are
/// deliberately asymmetric in what they expect back — each must see the OTHER
/// entity, which also catches the `other` computation being inverted.
#[test]
fn a_pair_is_found_from_either_side() {
    let store = store_with_projection("either-side");
    insert(&store, "track-a", "track-b", 1);

    let from_low = store.related(&eid("track-a")).expect("related");
    assert_eq!(from_low.len(), 1);
    assert_eq!(
        from_low.neighbours[0].other.as_str(),
        "track-b",
        "asking about the low entity must return the HIGH one"
    );

    let from_high = store.related(&eid("track-b")).expect("related");
    assert_eq!(from_high.len(), 1);
    assert_eq!(
        from_high.neighbours[0].other.as_str(),
        "track-a",
        "asking about the high entity must return the LOW one"
    );
}

/// **The two index halves are disjoint, so `UNION ALL` cannot double-count.**
///
/// The read uses `UNION ALL` rather than `UNION`, resting on the domain
/// guarantee that a pair names two DISTINCT entities — `CandidatePair::new`
/// refuses `(x, x)` — so no row can satisfy both `low = ?` and `high = ?`.
/// This asserts the guarantee rather than trusting it, because a `UNION ALL`
/// over overlapping arms would report one relationship as two.
#[test]
fn the_two_index_halves_are_disjoint_so_union_all_cannot_double_count() {
    // The domain refusal the disjointness rests on.
    assert!(
        kirra_world::same_as_candidate::CandidatePair::new(eid("x"), eid("x")).is_err(),
        "a pair of one entity must be unrepresentable, or the arms can overlap"
    );

    let store = store_with_projection("disjoint");
    insert(&store, "track-a", "track-b", 1);
    insert(&store, "track-a", "track-c", 2);
    insert(&store, "track-b", "track-c", 3);

    // `track-b` is the HIGH of one row and the LOW of another — the case where
    // a double-count would show, and a plain count would miss if both arms
    // returned the same row.
    let answer = store.related(&eid("track-b")).expect("related");
    assert_eq!(answer.len(), 2);
    let mut others: Vec<&str> = answer.neighbours.iter().map(|n| n.other.as_str()).collect();
    others.sort_unstable();
    assert_eq!(others, ["track-a", "track-c"]);
}

/// **The page ceiling holds, and truncation is CARRIED rather than inferred.**
///
/// `MAX_RELATED + 1` neighbours are inserted, so the answer is one short of
/// what exists and must say so. A caller that inferred truncation from
/// `len() == MAX_RELATED` would be right here and wrong for an entity with
/// exactly `MAX_RELATED` neighbours, which is why the flag exists.
#[test]
fn related_is_capped_and_carries_its_truncation() {
    let store = store_with_projection("cap");
    for i in 0..=MAX_RELATED {
        // Zero-padded so the sort order is the numeric one, and `subject`
        // sorts below every neighbour — so the subject is the LOW column
        // throughout and the cap is exercised on one arm rather than split.
        insert(
            &store,
            "aaa-subject",
            &format!("track-{i:04}"),
            i as i64 + 1,
        );
    }

    let answer = store.related(&eid("aaa-subject")).expect("related");
    assert_eq!(answer.len(), MAX_RELATED, "the ceiling must hold");
    assert!(
        answer.truncated,
        "more neighbours exist than were returned, and the answer must say so"
    );
}

/// **Exactly `MAX_RELATED` neighbours is a COMPLETE answer, not a truncated one.**
///
/// The non-vacuity twin of the cap test, and the reason the read probes one
/// over the ceiling instead of fetching exactly `MAX_RELATED`. Without this,
/// an implementation that always set `truncated` on a full page would pass the
/// test above while lying about every complete answer at the boundary.
#[test]
fn exactly_the_ceiling_is_not_reported_as_truncated() {
    let store = store_with_projection("exact");
    for i in 0..MAX_RELATED {
        insert(
            &store,
            "aaa-subject",
            &format!("track-{i:04}"),
            i as i64 + 1,
        );
    }

    let answer = store.related(&eid("aaa-subject")).expect("related");
    assert_eq!(answer.len(), MAX_RELATED);
    assert!(
        !answer.truncated,
        "a full page is not a cut-short page — nothing was withheld"
    );
}
