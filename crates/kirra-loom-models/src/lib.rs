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
//!   * `replace_cache_if_newer` — the `(epoch, generation)` lexicographic
//!     compare-and-swap under the cache write lock (#791 I1) never lets a lower
//!     (or equal) tuple clobber a higher one.
//!   * The FULL #688 defense — a supervisor `force_lockout` is never transiently
//!     downgraded by a racing recalc, under every interleaving. Both jointly
//!     necessary halves are modeled with production's exact ordering: the LATE
//!     sticky read (in `recalculate_and_broadcast` the recalc grabs its generation
//!     via `next_generation()` BEFORE reading the `supervisor_tripped` /
//!     `frame_lockout_active` sticky flag just before the write) AND the sticky
//!     downgrade guard in `replace_cache_if_newer`. See
//!     `sticky_lockout_never_downgraded_under_recalc_race`.
//!   * The #791 I1 lift of #688 to the epoch tuple — a PROMOTION bumping the
//!     fence epoch concurrently with a supervisor trip must not let a
//!     higher-epoch healthy recalc outrank the forced lockout on the epoch
//!     rung. Two jointly-sufficient defenses, both modeled with production's
//!     ordering: `force_lockout` loads `held_epoch` only AFTER the caller set
//!     the sticky flag (so its epoch is ≥ any recalc's whose sticky read was
//!     stale-false), and the CAS's sticky epoch-inherit fallback. See
//!     `sticky_lockout_never_downgraded_under_promotion_race`.
//!   * The HVCHAN-001 odd/even seqlock (`crates/kirra-contract-channel/src/seqlock.rs`)
//!     — an accepted cross-partition snapshot is never torn, weak-memory outcomes
//!     included; models the driver's two fences (WP-01 / MGA G-13). See
//!     `seqlock_accepted_snapshot_is_never_torn`.

// The whole crate is loom-only; keep it out of non-loom builds entirely.
#![cfg(loom)]

use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use loom::sync::{Arc, RwLock};
use loom::thread;

/// Faithful model of `replace_cache_if_newer`'s `(epoch, generation)` tuple CAS
/// under the write lock (`src/posture_engine.rs`, #791 I1), reduced to the stamp
/// fields. A candidate is committed only if its tuple is lexicographically
/// strictly newer than what is cached.
fn replace_cache_if_newer(cache: &RwLock<Option<(u64, u64)>>, candidate: (u64, u64)) -> bool {
    let mut guard = cache.write().unwrap();
    let cur = guard.unwrap_or((0, 0));
    if candidate > cur {
        *guard = Some(candidate);
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
        assert!(
            final_gen >= 5,
            "fetch_max init floor must survive a racing recalc"
        );
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
        let cache: Arc<RwLock<Option<(u64, u64)>>> = Arc::new(RwLock::new(None));

        // Both writers stamp under the same held epoch (1) — the tuple CAS must
        // degenerate exactly to the proven generation CAS.
        let (g1, c1) = (gen.clone(), cache.clone());
        let t1 = thread::spawn(move || {
            let v = g1.fetch_add(1, SeqCst);
            replace_cache_if_newer(&c1, (1, v));
            v
        });
        let (g2, c2) = (gen.clone(), cache.clone());
        let t2 = thread::spawn(move || {
            let v = g2.fetch_add(1, SeqCst);
            replace_cache_if_newer(&c2, (1, v));
            v
        });

        let v1 = t1.join().unwrap();
        let v2 = t2.join().unwrap();

        let (_, cached) = cache.read().unwrap().expect("a generation was committed");
        assert_eq!(
            cached,
            v1.max(v2),
            "cache must hold the highest committed generation, never a lower one"
        );
    });
}

/// INV (#791 I1): under concurrent same-generation-stream writers stamped with
/// DIFFERENT epochs (a promotion bumped the fence between their stamps), the
/// cache ends at the lexicographically-highest tuple — the higher-epoch entry
/// wins even when the lower-epoch writer committed last.
#[test]
fn cache_holds_highest_tuple_under_cross_epoch_replace() {
    loom::model(|| {
        let cache: Arc<RwLock<Option<(u64, u64)>>> = Arc::new(RwLock::new(None));

        // Pre-promotion writer: epoch 1, HIGH generation (a long-lived primary).
        let c1 = cache.clone();
        let t1 = thread::spawn(move || replace_cache_if_newer(&c1, (1, 100)));
        // Post-promotion writer: epoch 2, LOW generation — newer BY CONSTRUCTION.
        let c2 = cache.clone();
        let t2 = thread::spawn(move || replace_cache_if_newer(&c2, (2, 5)));

        t1.join().unwrap();
        t2.join().unwrap();

        let cached = cache.read().unwrap().expect("a stamp was committed");
        assert_eq!(
            cached,
            (2, 5),
            "the higher-epoch stamp must win regardless of generation and commit order"
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
/// AND the #791 I1 tuple CAS + sticky epoch-inherit (`src/posture_engine.rs`):
/// a sticky lockout refuses ANY non-LockedOut candidate; a sticky LockedOut
/// candidate inherits the cached epoch under the lock; otherwise the
/// `(epoch, generation)` lexicographic compare-and-swap applies.
/// Candidate/cached shape: `(epoch, generation, posture)`.
fn replace_cache_if_newer_posture(
    cache: &RwLock<Option<(u64, u64, Posture)>>,
    candidate: (u64, u64, Posture),
    sticky_lockout: bool,
    // #791 I1: the AUTHORITATIVE under-lock sticky re-read (production passes
    // the escalation flags; `None` models the force path, which passes `true`).
    sticky_flag: Option<&AtomicBool>,
) -> bool {
    let mut candidate = candidate;
    let mut guard = cache.write().unwrap();
    // Under-lock re-read: if a forced LockedOut already committed, its caller's
    // flag store happens-before this critical section (lock release→acquire),
    // so the re-read observes it regardless of the caller's stale pre-read.
    let sticky_lockout = sticky_lockout || sticky_flag.map(|f| f.load(SeqCst)).unwrap_or(false);
    // The sticky guard — a supervisor/frame sticky LockedOut is never downgraded.
    if sticky_lockout && candidate.2 != Posture::LockedOut {
        return false;
    }
    // The sticky epoch-inherit fallback (#791 I1) — a forced lockout can never
    // lose the CAS to the epoch rung of what is already cached.
    if sticky_lockout && candidate.2 == Posture::LockedOut {
        if let Some((cur_epoch, _, _)) = *guard {
            candidate.0 = candidate.0.max(cur_epoch);
        }
    }
    let cur = guard.map(|(e, g, _)| (e, g)).unwrap_or((0, 0));
    if (candidate.0, candidate.1) > cur {
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
        // A prior Nominal posture at (epoch 1, generation 1); the counter hands
        // out 2 and 3. The fence epoch is steady at 1 (no promotion in this
        // model — the promotion race is the next test).
        let gen = Arc::new(AtomicU64::new(2));
        let epoch = Arc::new(AtomicU64::new(1));
        let sticky = Arc::new(AtomicBool::new(false));
        let cache: Arc<RwLock<Option<(u64, u64, Posture)>>> =
            Arc::new(RwLock::new(Some((1, 1, Posture::Nominal))));

        // Recalc (e.g. the Step-C worker) recomputes a healthy posture. FAITHFUL
        // ORDERING (recalculate_and_broadcast): grab the generation, then the
        // epoch, BEFORE the sticky read.
        let (gr, er, sr, cr) = (gen.clone(), epoch.clone(), sticky.clone(), cache.clone());
        let recalc = thread::spawn(move || {
            let g = gr.fetch_add(1, SeqCst); // next_generation()
            let e = er.load(SeqCst); // held_epoch stamp read
            let s = sr.load(SeqCst); // the LATE sticky_lockout read
            replace_cache_if_newer_posture(&cr, (e, g, Posture::Nominal), s, Some(&sr));
        });

        // Force: the C2 supervisor escalation. The caller sets supervisor_tripped
        // BEFORE force_lockout loads the epoch and grabs its generation.
        let (gf, ef, sf, cf) = (gen.clone(), epoch.clone(), sticky.clone(), cache.clone());
        let force = thread::spawn(move || {
            sf.store(true, SeqCst); // caller sets supervisor_tripped
            let e = ef.load(SeqCst); // force_lockout's held_epoch load (AFTER the flag)
            let g = gf.fetch_add(1, SeqCst); // force_lockout's next_generation()
            replace_cache_if_newer_posture(&cf, (e, g, Posture::LockedOut), true, None);
        });

        recalc.join().unwrap();
        force.join().unwrap();

        // Once the supervisor has tripped, the cache MUST be LockedOut — the
        // healthy recalc can never leave a non-LockedOut posture behind it.
        let (_, _, posture) = cache.read().unwrap().expect("cache populated");
        assert_eq!(
            posture,
            Posture::LockedOut,
            "a racing recalc must never downgrade a forced supervisor LockedOut"
        );
    });
}

/// INV (#791 I1 — #688 lifted to the epoch tuple): a PROMOTION bumping the
/// fence epoch CONCURRENTLY with a supervisor trip must never let a
/// higher-epoch healthy recalc outrank the forced LockedOut on the epoch rung.
/// The closure argument, modeled with production's exact ordering:
///
///   * If the recalc read the sticky flag as SET → the guard refuses it.
///   * If it read the flag as UNSET, then its own epoch load happened before
///     the trip, while `force_lockout` loads `held_epoch` AFTER the trip — so
///     (the fence epoch being monotonic) force's epoch ≥ the recalc's, and
///     force's generation (grabbed after the flag) is strictly higher: force's
///     TUPLE strictly outranks the recalc's.
///   * The CAS-side sticky epoch-inherit closes the residual case where the
///     recalc committed first (force then inherits the higher cached epoch).
///
/// If `force_lockout` instead pre-loaded the epoch before the flag store, or
/// the inherit fallback were dropped together with it, loom finds the
/// downgrade interleaving (higher-epoch Nominal cached after the trip).
#[test]
fn sticky_lockout_never_downgraded_under_promotion_race() {
    // Preemption-bounded like the seqlock model (loom's standard state-space
    // control): three threads over an epoch atomic, a flag, a counter and the
    // lock explode combinatorially unbounded (~8 min); bound 3 explores every
    // schedule within 3 preemptions per thread in seconds. VERIFIED NON-VACUOUS
    // within this bound: removing the under-lock sticky re-read (pass `None`
    // in the recalc arm below) OR pre-loading force's epoch before the flag
    // store makes loom report the Nominal-cached downgrade counterexample.
    let mut model = loom::model::Builder::new();
    model.preemption_bound = Some(3);
    model.check(|| {
        // Steady state: epoch 1, prior Nominal at (1, 1). Counter hands out 2, 3.
        let gen = Arc::new(AtomicU64::new(2));
        let epoch = Arc::new(AtomicU64::new(1));
        let sticky = Arc::new(AtomicBool::new(false));
        let cache: Arc<RwLock<Option<(u64, u64, Posture)>>> =
            Arc::new(RwLock::new(Some((1, 1, Posture::Nominal))));

        // Promotion: the fence epoch advances (perform_promotion after a
        // successful try_claim_epoch CAS). Free-running vs both other threads.
        let ep = epoch.clone();
        let promotion = thread::spawn(move || {
            ep.store(2, SeqCst);
        });

        // Recalc: healthy posture, production ordering (gen → epoch → sticky).
        let (gr, er, sr, cr) = (gen.clone(), epoch.clone(), sticky.clone(), cache.clone());
        let recalc = thread::spawn(move || {
            let g = gr.fetch_add(1, SeqCst);
            let e = er.load(SeqCst);
            let s = sr.load(SeqCst);
            let w = replace_cache_if_newer_posture(&cr, (e, g, Posture::Nominal), s, Some(&sr));
            (g, e, s, w)
        });

        // Force: flag first, THEN the epoch load, then the generation grab.
        let (gf, ef, sf, cf) = (gen.clone(), epoch.clone(), sticky.clone(), cache.clone());
        let force = thread::spawn(move || {
            sf.store(true, SeqCst);
            let e = ef.load(SeqCst);
            let g = gf.fetch_add(1, SeqCst);
            let w = replace_cache_if_newer_posture(&cf, (e, g, Posture::LockedOut), true, None);
            (e, g, w)
        });

        promotion.join().unwrap();
        let rv = recalc.join().unwrap();
        let fv = force.join().unwrap();

        let (ce, cg, posture) = cache.read().unwrap().expect("cache populated");
        assert_eq!(
            posture,
            Posture::LockedOut,
            "a promotion-epoch bump must never let a healthy recalc outrank the forced lockout: cached=({ce},{cg}) recalc={rv:?} force={fv:?}"
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

// ---------------------------------------------------------------------------
// W10 (#1050) — LOCKSTEP-SEQLOCK-HVCHAN-001 drift tripwire.
//
// `seqlock_publish` / `seqlock_read` above are a HAND-COPIED mirror of the
// production `kirra_contract_channel::{publish,read_coherent_snapshot}`. The
// loom model only proves the MIRROR sound — if the production ordering edges
// drift without the mirror following, loom would be validating a stale protocol.
// This test reads the production source at compile time and asserts its four
// load-bearing ordering edges (the ones the mirror replicates) are still present,
// so a change there reds this test and forces the mirror to be re-checked. It is
// a drift reminder, not a full-equivalence proof (that is what the loom model is,
// against the mirror). Pure `include_str!` + substring checks — no loom runtime.
// ---------------------------------------------------------------------------
#[test]
fn production_seqlock_edges_are_in_lockstep() {
    const PROD: &str = include_str!("../../kirra-contract-channel/src/seqlock.rs");
    for needle in [
        "LOCKSTEP-SEQLOCK-HVCHAN-001", // the reciprocal marker naming this test
        "committed_gen.wrapping_add(1)", // odd marker: write in progress
        "committed_gen.wrapping_add(2)", // even commit
        "fence(Ordering::Release)",    // edge 3: publisher release fence
        "fence(Ordering::Acquire)",    // edge 4: reader acquire fence before g2 re-read
    ] {
        assert!(
            PROD.contains(needle),
            "LOCKSTEP DRIFT: production seqlock.rs no longer contains `{needle}` — \
             the loom mirror (seqlock_publish/seqlock_read) may now model a stale \
             protocol. Update the mirror in lock-step (W10 #1050) and re-pin this test."
        );
    }
}
