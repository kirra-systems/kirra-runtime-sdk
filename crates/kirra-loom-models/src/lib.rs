//! Loom concurrency-model tests for Kirra's posture-generation protocol
//! (stop-gate review M5 hardening).
//!
//! Each model mirrors a real atomic protocol in the verifier and asserts its
//! safety invariant under EVERY thread interleaving loom can construct. Without
//! `--cfg loom` this crate is empty (see `Cargo.toml` for how to run).
//!
//! Modeled protocols:
//!   * `POSTURE_GENERATION` — `next_generation()` (`fetch_add`) issues unique,
//!     monotone ids, and `init_generation_from_store()` (`fetch_max`) never lowers
//!     the counter (`src/posture_engine.rs:57,64`; the "B6: fetch_max not store"
//!     monotonicity concern).
//!   * `replace_cache_if_newer` — the GENERATION compare-and-swap under the cache
//!     write lock never lets a lower (or equal) generation clobber a higher one
//!     (`src/posture_engine.rs:462-497`). This models the generation-ordering half
//!     of the #688 defense only. The other half — the sticky-lockout downgrade
//!     guard (`sticky_lockout && candidate.posture != LockedOut` refuses a
//!     non-LockedOut candidate) — is enforced by the posture guard in production
//!     and is NOT modeled here; a faithful adversarial model of its read-vs-trip
//!     window is tracked follow-up.

// The whole crate is loom-only; keep it out of non-loom builds entirely.
#![cfg(loom)]

use loom::sync::atomic::{AtomicU64, Ordering::SeqCst};
use loom::sync::{Arc, RwLock};
use loom::thread;

/// Faithful model of `replace_cache_if_newer`'s generation CAS under the write
/// lock (`src/posture_engine.rs:462-497`), reduced to the generation field. A
/// candidate is committed only if it is strictly newer than what is cached.
fn replace_cache_if_newer(cache: &RwLock<Option<u64>>, candidate_gen: u64) -> bool {
    let mut guard = cache.write().unwrap();
    let cur_gen = guard.unwrap_or(0);
    if candidate_gen > cur_gen {
        *guard = Some(candidate_gen);
        true
    } else {
        false
    }
}

/// INV: `next_generation()` (`fetch_add`) hands every concurrent caller a UNIQUE
/// generation, and a racing `init_generation_from_store()` (`fetch_max`) never
/// lowers the counter below an already-issued generation. Loom explores every
/// ordering of the two threads' atomic ops.
#[test]
fn generations_are_unique_and_init_is_monotone() {
    loom::model(|| {
        // POSTURE_GENERATION starts at 1 (src/posture_engine.rs:17).
        let gen = Arc::new(AtomicU64::new(1));

        // Thread A: two recalcs, each taking a generation.
        let ga = gen.clone();
        let a = thread::spawn(move || (ga.fetch_add(1, SeqCst), ga.fetch_add(1, SeqCst)));

        // Thread B: a store-seeded init (fetch_max) racing one recalc.
        let gb = gen.clone();
        let b = thread::spawn(move || {
            gb.fetch_max(5, SeqCst); // init_generation_from_store seed
            gb.fetch_add(1, SeqCst) // a recalc
        });

        let (a1, a2) = a.join().unwrap();
        let b1 = b.join().unwrap();

        // Uniqueness: no two recalcs ever share a generation.
        let ids = [a1, a2, b1];
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(ids[i], ids[j], "generations must be unique: {ids:?}");
            }
        }

        // Monotonicity: the counter ended strictly above every issued id AND at or
        // above the fetch_max seed — a racing recalc can never undo the init floor.
        let final_gen = gen.load(SeqCst);
        assert!(final_gen > *ids.iter().max().unwrap());
        assert!(final_gen >= 5, "fetch_max init floor must survive a racing recalc");
    });
}

/// INV: two concurrent writers, each stamping the cache with its own generation
/// via `replace_cache_if_newer`, leave the HIGHER generation cached regardless of
/// which one wins the write lock first — the cache never regresses. This models
/// the generation-CAS half of the #688 defense (a later-committing but
/// lower-generation write cannot clobber a higher one); the sticky-lockout
/// posture guard that #688 also relies on is enforced in production code and is
/// not modeled here.
#[test]
fn cache_holds_highest_generation_under_concurrent_replace() {
    loom::model(|| {
        let gen = Arc::new(AtomicU64::new(1));
        let cache: Arc<RwLock<Option<u64>>> = Arc::new(RwLock::new(None));

        let (g1, c1) = (gen.clone(), cache.clone());
        let t1 = thread::spawn(move || {
            let v = g1.fetch_add(1, SeqCst);
            replace_cache_if_newer(&c1, v);
            v
        });
        let (g2, c2) = (gen.clone(), cache.clone());
        let t2 = thread::spawn(move || {
            let v = g2.fetch_add(1, SeqCst);
            replace_cache_if_newer(&c2, v);
            v
        });

        let v1 = t1.join().unwrap();
        let v2 = t2.join().unwrap();

        let cached = cache.read().unwrap().expect("a generation was committed");
        assert_eq!(
            cached,
            v1.max(v2),
            "cache must hold the highest committed generation, never a lower one"
        );
    });
}
