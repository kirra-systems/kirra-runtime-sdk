//! Loom concurrency-model tests for Kirra's posture-generation protocol
//! (stop-gate review M5 hardening).
//!
//! Each model mirrors a real atomic protocol in the verifier and asserts its
//! safety invariant under EVERY thread interleaving loom can construct. Without
//! `--cfg loom` this crate is empty (see `Cargo.toml` for how to run).
//!
//! Symbols below name functions/fields in `src/posture_engine.rs` rather than
//! absolute line numbers, which drift.
//!
//! Modeled protocols:
//!   * `POSTURE_GENERATION` — `next_generation()` (`fetch_add`) issues unique,
//!     monotone ids, and `init_generation_from_store()` (`fetch_max`) never lowers
//!     the counter (the "B6: fetch_max not store" monotonicity concern).
//!   * `replace_cache_if_newer` — the GENERATION compare-and-swap under the cache
//!     write lock never lets a lower (or equal) generation clobber a higher one.
//!   * The FULL #688 defense — a supervisor `force_lockout` is never transiently
//!     downgraded by a racing recalc, under every interleaving. Both jointly
//!     necessary halves are modeled with production's exact ordering: the LATE
//!     sticky read (in `recalculate_and_broadcast` the recalc grabs its generation
//!     via `next_generation()` BEFORE reading the `supervisor_tripped` /
//!     `frame_lockout_active` sticky flag just before the write) AND the sticky
//!     downgrade guard in `replace_cache_if_newer`. See
//!     `sticky_lockout_never_downgraded_under_recalc_race`.

// The whole crate is loom-only; keep it out of non-loom builds entirely.
#![cfg(loom)]

use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use loom::sync::{Arc, RwLock};
use loom::thread;

/// Faithful model of `replace_cache_if_newer`'s generation CAS under the write
/// lock (`src/posture_engine.rs`), reduced to the generation field. A candidate is
/// committed only if it is strictly newer than what is cached.
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
        // POSTURE_GENERATION starts at 1 (see `posture_engine::POSTURE_GENERATION`).
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
/// lower-generation write cannot clobber a higher one). The sticky-lockout
/// posture guard that #688 ALSO relies on is modeled separately, with the full
/// force-vs-recalc race, in `sticky_lockout_never_downgraded_under_recalc_race`.
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

/// The posture subset the #688 sticky-lockout model needs: a healthy recalc
/// result vs. the forced supervisor lockout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Posture {
    Nominal,
    LockedOut,
}

/// Faithful model of `replace_cache_if_newer` INCLUDING the #688 sticky guard
/// (`src/posture_engine.rs`): a sticky lockout refuses ANY non-LockedOut candidate;
/// otherwise the generation compare-and-swap applies.
fn replace_cache_if_newer_posture(
    cache: &RwLock<Option<(u64, Posture)>>,
    candidate: (u64, Posture),
    sticky_lockout: bool,
) -> bool {
    let mut guard = cache.write().unwrap();
    // The sticky guard — a supervisor/frame sticky LockedOut is never downgraded.
    if sticky_lockout && candidate.1 != Posture::LockedOut {
        return false;
    }
    let cur_gen = guard.map(|(g, _)| g).unwrap_or(0);
    if candidate.0 > cur_gen {
        *guard = Some(candidate);
        true
    } else {
        false
    }
}

/// INV (#688, FULL defense): a supervisor `force_lockout` can NEVER be transiently
/// downgraded by a racing recalc that computed a healthy (non-LockedOut) posture —
/// under EVERY interleaving loom can build. The defense is two jointly-necessary
/// halves, both modeled here with production's EXACT ordering:
///
///   1. The LATE sticky read. In `recalculate_and_broadcast` the recalc grabs its
///      generation (`next_generation()`) BEFORE reading the sticky flag. So if it
///      reads the flag as UNSET, `force_lockout`'s generation — grabbed only AFTER
///      the caller set the flag — is necessarily HIGHER, and the generation CAS
///      rejects the recalc's now-stale write.
///   2. The sticky guard. If the recalc instead reads the flag as SET, the guard
///      in `replace_cache_if_newer` refuses its non-LockedOut candidate regardless
///      of generation.
///
/// If either half were dropped (e.g. the sticky read moved BEFORE the generation
/// grab, or the guard removed) loom would find the downgrade interleaving. With
/// both, the cache is LockedOut in every schedule.
#[test]
fn sticky_lockout_never_downgraded_under_recalc_race() {
    loom::model(|| {
        // A prior Nominal posture at generation 1; the counter hands out 2 and 3.
        let gen = Arc::new(AtomicU64::new(2));
        let sticky = Arc::new(AtomicBool::new(false));
        let cache: Arc<RwLock<Option<(u64, Posture)>>> =
            Arc::new(RwLock::new(Some((1, Posture::Nominal))));

        // Recalc (e.g. the Step-C worker) recomputes a healthy posture. FAITHFUL
        // ORDERING (recalculate_and_broadcast): grab the generation BEFORE the
        // sticky read.
        let (gr, sr, cr) = (gen.clone(), sticky.clone(), cache.clone());
        let recalc = thread::spawn(move || {
            let g = gr.fetch_add(1, SeqCst); // next_generation()
            let s = sr.load(SeqCst); // the LATE sticky_lockout read
            replace_cache_if_newer_posture(&cr, (g, Posture::Nominal), s);
        });

        // Force: the C2 supervisor escalation. The caller sets supervisor_tripped
        // BEFORE force_lockout grabs its generation.
        let (gf, sf, cf) = (gen.clone(), sticky.clone(), cache.clone());
        let force = thread::spawn(move || {
            sf.store(true, SeqCst); // caller sets supervisor_tripped
            let g = gf.fetch_add(1, SeqCst); // force_lockout's next_generation()
            replace_cache_if_newer_posture(&cf, (g, Posture::LockedOut), true);
        });

        recalc.join().unwrap();
        force.join().unwrap();

        // Once the supervisor has tripped, the cache MUST be LockedOut — the
        // healthy recalc can never leave a non-LockedOut posture behind it.
        let (_, posture) = cache.read().unwrap().expect("cache populated");
        assert_eq!(
            posture,
            Posture::LockedOut,
            "a racing recalc must never downgrade a forced supervisor LockedOut"
        );
    });
}
