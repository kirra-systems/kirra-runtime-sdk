//! EP-15 proofs — the HA lease timing algebra (`src/lease.rs`).
//!
//! These are the split-brain guarantees the EP-03 lease failover trigger rests
//! on. Integer-only, loop-free code: Kani proves each property for EVERY `u64`
//! input, not a sampled subset.
//!
//! Properties (cited from `docs/safety/GOVERNOR_INTEGRITY_EVIDENCE.md` §2):
//!  * L1 `from_ttl` is TOTAL and every output satisfies the split-brain
//!    invariant (`demote_before_promote`): clamping makes `renew ≥ 1` and
//!    `promote_after = ttl + ttl/2` exact (no wrap), for all `u64` TTLs.
//!  * L2 a challenger may promote ONLY strictly after the holder's own lease
//!    expired — the two `mode_active` windows cannot overlap — and the guard
//!    margin between the two deadlines equals the renew half-life.
//!  * L3 wall-clock skew fails SAFE: an observer whose clock reads at-or-behind
//!    the stored renewal timestamp neither promotes nor self-demotes.
//!  * L4 a holder that renews on cadence can never see its own lease expire
//!    (`renew_interval < ttl` for every derived parameter set).

#[allow(unused_imports)]
use crate::lease::{
    holder_must_self_demote, lease_expired, promotion_due_since_renew, should_promote,
    should_renew, LeaseParams, MAX_LEASE_TTL_MS, MIN_LEASE_TTL_MS,
};

#[cfg(kani)]
mod proofs {
    use super::*;

    /// L1 — `from_ttl` totality: for EVERY u64 TTL the derived parameters are
    /// clamped into range, arithmetically exact (no overflow wrap), and satisfy
    /// the split-brain invariant.
    #[kani::proof]
    fn l1_from_ttl_total_and_split_brain_safe() {
        let ttl: u64 = kani::any();
        let p = LeaseParams::from_ttl(ttl);

        assert!(p.ttl_ms >= MIN_LEASE_TTL_MS && p.ttl_ms <= MAX_LEASE_TTL_MS);
        assert!(p.renew_interval_ms >= 1, "renew cadence never degenerates to 0");
        assert_eq!(p.renew_interval_ms, p.ttl_ms / 2, "renew at half-life, exact");
        assert_eq!(
            p.promote_after_ms,
            p.ttl_ms + p.renew_interval_ms,
            "promote deadline is exact — the MAX clamp forbids overflow wrap"
        );
        assert!(p.demote_before_promote(), "THE split-brain invariant, all u64");
    }

    /// L2 — window ordering: promotion is possible only strictly after the
    /// holder's lease expired, and the guard margin between the two deadlines
    /// is exactly the renew half-life (positive).
    #[kani::proof]
    fn l2_promotion_only_after_holder_expiry() {
        let ttl: u64 = kani::any();
        let elapsed: u64 = kani::any();
        let p = LeaseParams::from_ttl(ttl);

        if should_promote(elapsed, &p) {
            assert!(
                lease_expired(elapsed, &p),
                "a promoting challenger implies an already-expired holder lease"
            );
        }
        assert_eq!(p.guard_margin_ms(), p.renew_interval_ms);
        assert!(p.guard_margin_ms() > 0, "the deadlines never coincide");
    }

    /// L3 — skew fails safe: whenever the observer's clock reads at-or-behind
    /// the stored renewal timestamp (`now ≤ last_renew`, the cross-node skew
    /// case), the durable-lease wrappers treat the lease as freshly renewed —
    /// no promotion over a live holder, no spurious self-demotion.
    #[kani::proof]
    fn l3_clock_skew_fails_safe() {
        let ttl: u64 = kani::any();
        let now: u64 = kani::any();
        let last_renew: u64 = kani::any();
        kani::assume(now <= last_renew);
        let p = LeaseParams::from_ttl(ttl);

        assert!(!promotion_due_since_renew(now, last_renew, &p));
        assert!(!holder_must_self_demote(now, last_renew, &p));
    }

    /// L4 — on-cadence renewal keeps the lease alive: the renew deadline is
    /// strictly inside the expiry deadline for every derived parameter set, so
    /// a holder that renews when `should_renew` first fires has not expired.
    #[kani::proof]
    fn l4_on_cadence_renewal_never_expires() {
        let ttl: u64 = kani::any();
        let p = LeaseParams::from_ttl(ttl);

        assert!(p.renew_interval_ms < p.ttl_ms);
        // At the exact renew deadline the lease is still valid.
        assert!(should_renew(p.renew_interval_ms, &p));
        assert!(!lease_expired(p.renew_interval_ms, &p));
    }
}

// ---------------------------------------------------------------------------
// Concrete mirrors — the same properties exercised at boundary points under
// plain `cargo test`, so the harness logic is validated without Kani.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod mirrors {
    use super::*;

    /// The u64 boundary points every proof above covers symbolically.
    const PROBES: &[u64] = &[
        0,
        1,
        MIN_LEASE_TTL_MS,
        3,
        DEFAULT_PROBE,
        MAX_LEASE_TTL_MS - 1,
        MAX_LEASE_TTL_MS,
        MAX_LEASE_TTL_MS + 1,
        u64::MAX - 1,
        u64::MAX,
    ];
    const DEFAULT_PROBE: u64 = 3_000;

    #[test]
    fn l1_mirror_totality_at_boundaries() {
        for &ttl in PROBES {
            let p = LeaseParams::from_ttl(ttl);
            assert!(p.ttl_ms >= MIN_LEASE_TTL_MS && p.ttl_ms <= MAX_LEASE_TTL_MS);
            assert!(p.renew_interval_ms >= 1);
            assert_eq!(p.renew_interval_ms, p.ttl_ms / 2);
            assert_eq!(p.promote_after_ms, p.ttl_ms + p.renew_interval_ms);
            assert!(p.demote_before_promote(), "ttl={ttl}");
        }
    }

    #[test]
    fn l2_mirror_ordering_at_boundaries() {
        for &ttl in PROBES {
            let p = LeaseParams::from_ttl(ttl);
            for elapsed in [0, p.ttl_ms - 1, p.ttl_ms, p.promote_after_ms - 1, p.promote_after_ms]
            {
                if should_promote(elapsed, &p) {
                    assert!(lease_expired(elapsed, &p), "ttl={ttl} elapsed={elapsed}");
                }
            }
            assert_eq!(p.guard_margin_ms(), p.renew_interval_ms);
            assert!(p.guard_margin_ms() > 0);
        }
    }

    #[test]
    fn l3_mirror_skew_fails_safe() {
        for &ttl in PROBES {
            let p = LeaseParams::from_ttl(ttl);
            for (now, last) in [(0, 0), (0, u64::MAX), (5, 5), (100, 101), (u64::MAX, u64::MAX)] {
                assert!(!promotion_due_since_renew(now, last, &p));
                assert!(!holder_must_self_demote(now, last, &p));
            }
        }
    }

    #[test]
    fn l4_mirror_on_cadence_renewal() {
        for &ttl in PROBES {
            let p = LeaseParams::from_ttl(ttl);
            assert!(p.renew_interval_ms < p.ttl_ms, "ttl={ttl}");
            assert!(should_renew(p.renew_interval_ms, &p));
            assert!(!lease_expired(p.renew_interval_ms, &p));
        }
    }
}
