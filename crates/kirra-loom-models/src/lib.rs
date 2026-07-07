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
//!   * The HVCHAN-001 odd/even seqlock (`crates/kirra-contract-channel/src/seqlock.rs`)
//!     — an accepted cross-partition snapshot is never torn, weak-memory outcomes
//!     included; models the driver's two fences (WP-01 / MGA G-13). See
//!     `seqlock_accepted_snapshot_is_never_torn`.

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

// ---------------------------------------------------------------------------
// HVCHAN-001 §3 odd/even seqlock (WP-01 / MGA G-13)
// ---------------------------------------------------------------------------

use loom::sync::atomic::fence;
use loom::sync::atomic::Ordering::{Acquire, Relaxed, Release};

/// The shared contract region reduced to what the invariant needs: the seqlock
/// `generation` plus a two-field body carrying the cross-field consistency
/// invariant `b == 2 * a`. Orderings mirror the production bindings
/// (`crates/kirra-contract-channel/src/reference.rs::InProcessRegion` and the
/// `kirra-hv-carrier` POSIX-SHM regions): generation Acquire/Release, body
/// fields Relaxed.
struct SeqlockRegion {
    generation: AtomicU64,
    a: AtomicU64,
    b: AtomicU64,
}

/// Faithful mirror of `kirra_contract_channel::publish`
/// (`crates/kirra-contract-channel/src/seqlock.rs`), including the WP-01
/// release fence (ordering edge 3): odd marker → release fence → Relaxed body
/// stores → even commit (Release).
fn seqlock_publish(region: &SeqlockRegion, committed: u64, a: u64, b: u64) -> u64 {
    region.generation.store(committed + 1, Release); // odd: write in progress
    fence(Release); // edge 3 — body stores must not float above the odd marker
    region.a.store(a, Relaxed);
    region.b.store(b, Relaxed);
    let next = committed + 2;
    region.generation.store(next, Release); // even: commit
    next
}

/// Faithful mirror of `kirra_contract_channel::read_coherent_snapshot`
/// (`crates/kirra-contract-channel/src/seqlock.rs`), including the WP-01
/// acquire fence (ordering edge 4): g1 (Acquire) → Relaxed body copy →
/// acquire fence → g2 re-read; accept iff even and unchanged; bounded
/// retries fail closed (`None` = the `SnapshotFault::RetryExhausted` reject).
fn seqlock_read(region: &SeqlockRegion, max_retries: u32) -> Option<(u64, u64, u64)> {
    let mut failures = 0u32;
    loop {
        let g1 = region.generation.load(Acquire);
        if g1 & 1 == 0 {
            let a = region.a.load(Relaxed);
            let b = region.b.load(Relaxed);
            fence(Acquire); // edge 4 — body loads must not sink below the re-read
            let g2 = region.generation.load(Acquire);
            if g2 == g1 {
                return Some((g1, a, b));
            }
        }
        if failures >= max_retries {
            return None; // fail-closed: never a stale/torn accept
        }
        failures += 1;
    }
}

/// INV (HVCHAN-001 §3 steps 2-3): a snapshot the reader ACCEPTS is never torn —
/// under every interleaving AND every weak-memory outcome loom can construct
/// (loom models Relaxed loads observing stale values, which is exactly the
/// aarch64 hazard the seqlock's two fences exist to close). The writer
/// publishes two sessions whose fields satisfy `b == 2 * a`; a torn mix of two
/// sessions (or of a session with the zeroed initial state) breaks the
/// invariant. Rejection (`None`) is always a legal outcome — fail-closed —
/// but an accepted snapshot must be internally consistent and even-generation.
///
/// This models the exact protocol shipped in
/// `crates/kirra-contract-channel/src/seqlock.rs` INCLUDING its two fences;
/// dropping either fence from the mirrors above makes this model fail (loom
/// finds the torn outcome), which is how the fences' necessity was
/// established — the model is not vacuous. (Verified 2026-07: removing edge 3
/// or edge 4 makes loom report `b != 2 * a`.)
///
/// The search is preemption-bounded (loom's standard state-space control,
/// cf. tokio's `LOOM_MAX_PREEMPTIONS=2`): exhaustive within 2 preemptions per
/// thread, which is where seqlock tear counterexamples live (verified: the
/// fence-removal counterexamples above are found WITHIN this bound).
#[test]
fn seqlock_accepted_snapshot_is_never_torn() {
    let mut model = loom::model::Builder::new();
    model.preemption_bound = Some(2);
    model.check(|| {
        let region = Arc::new(SeqlockRegion {
            generation: AtomicU64::new(0),
            a: AtomicU64::new(0),
            b: AtomicU64::new(0),
        });

        let w = Arc::clone(&region);
        let writer = thread::spawn(move || {
            let committed = seqlock_publish(&w, 0, 1, 2);
            seqlock_publish(&w, committed, 2, 4);
        });

        let r = Arc::clone(&region);
        let reader = thread::spawn(move || {
            if let Some((g, a, b)) = seqlock_read(&r, 2) {
                assert_eq!(g % 2, 0, "accepted snapshot must be even-generation");
                assert_eq!(b, 2 * a, "torn snapshot accepted: a={a} b={b} g={g}");
                assert!(
                    matches!((a, b), (0, 0) | (1, 2) | (2, 4)),
                    "snapshot must be one whole published session: a={a} b={b}"
                );
            }
        });

        writer.join().unwrap();
        reader.join().unwrap();
    });
}
