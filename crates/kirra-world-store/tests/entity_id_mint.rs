//! `entity_id` minting — `WM_SCOPE.md` §5, against a real store.
//!
//! §6.1 requires an id to be *"stable, opaque, monotonic. Never reused, never
//! encodes semantics."* Stable and opaque are properties of the **type**; the
//! other two are properties of the **generator**, and this is where they are
//! tested because a generator cannot promise them without durable state.
//!
//! The two tests that matter are not the happy path:
//!
//! 1. **Monotonic across a clock that goes backwards.** An NTP step or a VM
//!    restore is not exotic, and an id derived from the clock alone would
//!    regress.
//! 2. **Never reused, enforced by the schema.** The ledger's `PRIMARY KEY` is
//!    the guarantee; the mint does not check.

use kirra_world_store::{StoreError, WorldStore};

const T0: i64 = 1_700_000_000_000;

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "kirra-world-mint-{}-{}.sqlite",
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

/// Distinct ids, strictly increasing, and recorded.
#[test]
fn minted_ids_are_distinct_and_strictly_increasing() {
    let path = tmp("basic");
    let mut s = WorldStore::open(&path).expect("open");

    let mut seen: Vec<String> = Vec::new();
    for n in 0..25 {
        let id = s.mint_entity_id(T0 + n).expect("mint");
        seen.push(id.as_str().to_owned());
    }

    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(sorted, seen, "ids must come out in increasing order");
    sorted.dedup();
    assert_eq!(sorted.len(), seen.len(), "and every one must be distinct");
    assert_eq!(s.minted_entity_id_count().expect("count"), 25);
    let _ = std::fs::remove_file(&path);
}

/// **The same millisecond does not repeat an id.**
///
/// The branch a clock-only generator gets wrong. Every id here is built from an
/// identical `now_ms`, so the timestamp half is constant and only the ULID
/// increment rule separates them.
#[test]
fn ids_minted_within_one_millisecond_still_advance() {
    let path = tmp("same-ms");
    let mut s = WorldStore::open(&path).expect("open");

    let mut seen: Vec<String> = Vec::new();
    for _ in 0..50 {
        seen.push(s.mint_entity_id(T0).expect("mint").as_str().to_owned());
    }

    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(sorted, seen, "strictly increasing inside one millisecond");
    sorted.dedup();
    assert_eq!(sorted.len(), 50, "no repeats inside one millisecond");
    let _ = std::fs::remove_file(&path);
}

/// **A clock that goes backwards does not regress the id.**
///
/// An NTP step or a VM restore moves `now_ms` back. An id derived from the
/// clock alone would then be *lower* than one already handed out — and the next
/// reader to sort by id would see history reorder itself.
///
/// This is the test that makes monotonicity a property of the STORE rather than
/// of the caller's clock being well-behaved: the mint takes its floor from the
/// durable high-water, so the earlier timestamp is simply ignored.
#[test]
fn a_clock_that_goes_backwards_does_not_regress_the_id() {
    let path = tmp("backwards");
    let mut s = WorldStore::open(&path).expect("open");

    let forward = s.mint_entity_id(T0 + 10_000).expect("mint");
    // ...and now the clock jumps back ten seconds.
    let after_step = s.mint_entity_id(T0).expect("mint");

    assert!(
        after_step.as_str() > forward.as_str(),
        "an id minted after a backwards clock step must still exceed its \
         predecessor: {} !> {}",
        after_step.as_str(),
        forward.as_str()
    );
    let _ = std::fs::remove_file(&path);
}

/// Monotonic **across a reopen**, because the floor is on disk.
///
/// A generator holding its high-water in memory would restart from the clock
/// after a process restart, which is the same regression as above with a longer
/// fuse.
#[test]
fn the_monotonic_floor_survives_a_reopen() {
    let path = tmp("reopen");
    let last = {
        let mut s = WorldStore::open(&path).expect("open");
        s.mint_entity_id(T0 + 10_000)
            .expect("mint")
            .as_str()
            .to_owned()
    };

    let mut s = WorldStore::open(&path).expect("reopen");
    let after = s.mint_entity_id(T0).expect("mint");
    assert!(
        after.as_str() > last.as_str(),
        "the floor must come from disk, not from process memory"
    );
    assert_eq!(
        s.minted_entity_id_count().expect("count"),
        2,
        "the ledger carried across the reopen"
    );
    let _ = std::fs::remove_file(&path);
}

/// **Never-reuse is the schema's guarantee, and it is real.**
///
/// The mint does not check for reuse; the `PRIMARY KEY` refuses it. Asserted by
/// writing a minted id a second time and finding the constraint fire, so the
/// guarantee is demonstrated rather than described.
#[test]
fn the_ledger_refuses_a_second_write_of_the_same_id() {
    let path = tmp("reuse");
    let mut s = WorldStore::open(&path).expect("open");
    let id = s.mint_entity_id(T0).expect("mint");

    let err = s
        .raw_execute_for_test(&format!(
            "INSERT INTO entity_id_mint (entity_id, minted_at_ms) VALUES ('{}', {})",
            id.as_str(),
            T0
        ))
        .expect_err("a second write of the same id must be refused");
    assert!(
        matches!(err, StoreError::Sqlite(_)),
        "expected a constraint failure, got {err}"
    );
    assert_eq!(s.minted_entity_id_count().expect("count"), 1);
    let _ = std::fs::remove_file(&path);
}

/// A minted id passes the same validator a stored one must.
///
/// The mint constructs through `EntityId::new` rather than wrapping a string,
/// so an id it hands out is admissible by exactly the rule the read path
/// applies — length and control characters included.
#[test]
fn a_minted_id_is_admissible_by_the_types_own_rule() {
    let path = tmp("valid");
    let mut s = WorldStore::open(&path).expect("open");
    let id = s.mint_entity_id(T0).expect("mint");

    assert_eq!(
        kirra_world_store::EntityId::new(id.as_str()).expect("re-admits"),
        id,
        "a minted id must re-admit as itself"
    );
    assert_eq!(id.as_str().len(), 26, "a ULID is 26 Crockford base32 chars");
    let _ = std::fs::remove_file(&path);
}
