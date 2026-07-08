//! WP-19 (MGA G-21) — lease-based HA failover TIMING MODEL (pure).
//!
//! The live promotion path (`standby_monitor`) today detects a dead primary by a
//! heartbeat TOKEN going unchanged past `PROMOTION_TIMEOUT_MS` (10 s), with a
//! `MAX_CONSECUTIVE_HEARTBEAT_FAILURES × interval < timeout` clamp guaranteeing the
//! wedged primary self-demotes before the standby promotes. That is safe but slow
//! (~12 s detection) and the timing is a set of ad-hoc constants.
//!
//! This module is the PRINCIPLED lease-timing contract WP-19 introduces, derived
//! from a single **TTL**:
//! - the Active holder **renews at half-life** (`ttl/2`) — so one missed renewal
//!   still leaves the lease valid (two attempts before expiry);
//! - the holder **self-demotes (fail-closed) once its own lease expires** (`ttl`) —
//!   past that it can no longer prove it holds the lease;
//! - a challenger **promotes only after `ttl + ttl/2`** — a full TTL for the lease
//!   to expire PLUS a half-TTL guard, so the holder's self-demote strictly precedes
//!   the challenger's promotion (no two-`mode_active` window, independent of the
//!   durable epoch fence that backs it).
//!
//! With `DEFAULT_LEASE_TTL_MS = 3 s` the promote deadline is 4.5 s (≤ the 5 s
//! failover target and ≤ `POSTURE_CACHE_TTL_MS`), a big cut from ~12 s.
//!
//! **Scope (WP-19 slice 1):** this is the pure, unit-tested timing model + its
//! split-brain non-overlap PROOF. WIRING it into the live `standby_monitor`
//! promotion loop (replacing the heartbeat token with a renewed lease on the
//! `ha_state` epoch machinery) is the recorded follow-up — the proven epoch fence
//! and heartbeat writer stay intact until then, so this slice changes NO runtime
//! behaviour. Everything here is pure over injected elapsed-times (measured on the
//! challenger's / holder's OWN monotonic clock, like `HeartbeatFreshness`), so it
//! is skew-immune and unit-tested without timers.

use crate::posture_cache::POSTURE_CACHE_TTL_MS;

/// Default HA lease TTL. Chosen so a failed primary's lease expires AND the standby
/// promotes within ~5 s (the WP-19 failover target). MUST stay ≤ `POSTURE_CACHE_TTL_MS`
/// so an expired lease is bounded by the posture-cache staleness window.
pub const DEFAULT_LEASE_TTL_MS: u64 = 3_000;

/// Arithmetic floor for a lease TTL: `from_ttl` clamps UP to this so
/// `renew_interval_ms = ttl/2 ≥ 1`, keeping the demote-before-promote invariant
/// total. This is a correctness floor, not a policy one — a real lease is seconds
/// (`DEFAULT_LEASE_TTL_MS`); a sub-floor value is a misconfiguration.
pub const MIN_LEASE_TTL_MS: u64 = 2;

/// Arithmetic ceiling for a lease TTL: `from_ttl` clamps DOWN to this so
/// `ttl + ttl/2` can never overflow `u64` and wrap `promote_after_ms` below
/// `ttl_ms` (which would be a premature-promotion split-brain hazard). `u64::MAX/2`
/// keeps `ttl + ttl/2 ≤ ¾·u64::MAX < u64::MAX`. Astronomically above any real TTL.
pub const MAX_LEASE_TTL_MS: u64 = u64::MAX / 2;

// Compile-time guards: the TTL is bounded by the posture-cache TTL, and the derived
// promote deadline meets the ≤ 5 s failover target.
const _: () = assert!(
    DEFAULT_LEASE_TTL_MS <= POSTURE_CACHE_TTL_MS,
    "lease TTL must be ≤ the posture-cache TTL (an expired lease is bounded by cache staleness)"
);
const _: () = assert!(
    LeaseParams::from_ttl(DEFAULT_LEASE_TTL_MS).promote_after_ms <= 5_000,
    "default lease promote deadline must meet the ≤5 s failover target"
);
const _: () = assert!(
    LeaseParams::from_ttl(DEFAULT_LEASE_TTL_MS).demote_before_promote(),
    "default lease must satisfy the demote-before-promote split-brain invariant"
);

/// Lease timing parameters, all DERIVED from a single TTL. Copy/const so the derived
/// deadlines are usable in `const` asserts and cheap to pass around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseParams {
    /// Lease lifetime: the holder must renew within this, else its lease expires.
    pub ttl_ms: u64,
    /// Renew cadence — half the TTL, so a single missed renewal still leaves the
    /// lease valid (fail-operational on one blip, fail-closed on sustained failure).
    pub renew_interval_ms: u64,
    /// How long a challenger waits, since it last saw the lease refreshed, before it
    /// may promote: `ttl + ttl/2` (a full TTL to expire + a half-TTL guard margin).
    pub promote_after_ms: u64,
}

impl LeaseParams {
    /// Derive all timings from a TTL. `renew = ttl/2`, `promote_after = ttl + ttl/2`.
    ///
    /// TOTAL by construction (Copilot #864): the TTL is clamped into
    /// `[MIN_LEASE_TTL_MS, MAX_LEASE_TTL_MS]` first, so `renew_interval_ms ≥ 1` (a
    /// tiny TTL can never make it 0, which would break `demote_before_promote`) and
    /// `ttl + ttl/2` can never overflow `u64` and wrap `promote_after_ms` below
    /// `ttl_ms` (a premature-promotion hazard). Every `from_ttl` output therefore
    /// satisfies the split-brain invariant — clamping UP/DOWN is the safe direction.
    #[must_use]
    pub const fn from_ttl(ttl_ms: u64) -> Self {
        let ttl_ms = if ttl_ms < MIN_LEASE_TTL_MS {
            MIN_LEASE_TTL_MS
        } else if ttl_ms > MAX_LEASE_TTL_MS {
            MAX_LEASE_TTL_MS
        } else {
            ttl_ms
        };
        let renew_interval_ms = ttl_ms / 2; // ≥ 1 by the MIN clamp
        // No overflow by the MAX clamp, so this is exact (not saturating).
        Self { ttl_ms, renew_interval_ms, promote_after_ms: ttl_ms + renew_interval_ms }
    }

    /// The default lease parameters (`DEFAULT_LEASE_TTL_MS`).
    #[must_use]
    pub const fn default_params() -> Self {
        Self::from_ttl(DEFAULT_LEASE_TTL_MS)
    }

    /// THE split-brain safety invariant: the holder's self-demote deadline (its lease
    /// expiry, `ttl_ms`) strictly precedes the challenger's promotion deadline
    /// (`promote_after_ms`), with a positive guard margin AND a positive renew
    /// cadence. When this holds the two `mode_active` windows can never overlap even
    /// before the durable epoch fence catches a stale holder.
    #[must_use]
    pub const fn demote_before_promote(&self) -> bool {
        self.renew_interval_ms > 0 && self.ttl_ms < self.promote_after_ms
    }

    /// The guard window between the holder's lease expiry and the challenger's
    /// promotion (`promote_after_ms − ttl_ms` = a half-TTL for the default).
    #[must_use]
    pub const fn guard_margin_ms(&self) -> u64 {
        self.promote_after_ms.saturating_sub(self.ttl_ms)
    }
}

/// Holder-side: should the Active lease-holder RENEW now? Renew once at least a
/// half-life has elapsed since its last successful renewal (`elapsed` measured on
/// the holder's own monotonic clock).
#[must_use]
pub fn should_renew(elapsed_since_renew_ms: u64, params: &LeaseParams) -> bool {
    elapsed_since_renew_ms >= params.renew_interval_ms
}

/// Holder-side: has this holder's OWN lease EXPIRED — meaning it can no longer prove
/// it holds the lease and MUST self-demote (fail-closed), exactly as the disk-wedge
/// path does today?
#[must_use]
pub fn lease_expired(elapsed_since_renew_ms: u64, params: &LeaseParams) -> bool {
    elapsed_since_renew_ms >= params.ttl_ms
}

/// Challenger-side: may a standby PROMOTE? Only once the lease it observes has gone
/// unrefreshed for at least `promote_after_ms`, measured on the challenger's OWN
/// monotonic clock (like `HeartbeatFreshness`) — so cross-machine wall-clock skew
/// can never trigger a premature promotion.
#[must_use]
pub fn should_promote(elapsed_since_fresh_ms: u64, params: &LeaseParams) -> bool {
    elapsed_since_fresh_ms >= params.promote_after_ms
}

/// A challenger must observe EVERY renewal to keep re-anchoring its freshness, so its
/// poll must be strictly faster than the holder's renew cadence. True iff the poll
/// interval is short enough that a live holder can never be promoted over.
#[must_use]
pub fn poll_fast_enough(poll_interval_ms: u64, params: &LeaseParams) -> bool {
    poll_interval_ms > 0 && poll_interval_ms < params.renew_interval_ms
}

// ---------------------------------------------------------------------------
// WP-19 slice 2 — durable-lease decision wrappers
//
// The helpers above take an ELAPSED time (measured on the caller's own monotonic
// clock). Slice 2 makes the lease DURABLE on `ha_state.updated_at_ms` (see
// `VerifierStore::renew_lease` / `read_ha_lease`), so the live decisions read an
// ABSOLUTE last-renew timestamp instead. These wrappers map that timestamp to the
// elapsed form with a SATURATING subtraction: if `now < last_renew` (the observer's
// clock briefly reads behind the stored renewal — cross-node wall-clock skew), the
// elapsed is 0, i.e. "freshly renewed" — the fail-SAFE direction (never a spurious
// giant elapsed that would promote over a live holder).
// ---------------------------------------------------------------------------

/// Challenger-side, durable-lease form: may a standby PROMOTE, given the durable
/// last-renew timestamp (`ha_state.updated_at_ms`) and the observer's `now_ms`?
/// Wrapper over [`should_promote`]; skew fails safe (see the module note above).
#[must_use]
pub fn promotion_due_since_renew(now_ms: u64, last_renew_ms: u64, params: &LeaseParams) -> bool {
    should_promote(now_ms.saturating_sub(last_renew_ms), params)
}

/// Holder-side, durable-lease form: must the Active holder self-demote because its
/// own lease (last renewed at `last_renew_ms`) has EXPIRED? Wrapper over
/// [`lease_expired`]; skew fails safe (a clock blip never spuriously demotes a
/// just-renewed holder).
#[must_use]
pub fn holder_must_self_demote(now_ms: u64, last_renew_ms: u64, params: &LeaseParams) -> bool {
    lease_expired(now_ms.saturating_sub(last_renew_ms), params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::standby_monitor::PROMOTION_POLL_MS;

    #[test]
    fn timings_derive_from_the_ttl() {
        let p = LeaseParams::from_ttl(3_000);
        assert_eq!(p.ttl_ms, 3_000);
        assert_eq!(p.renew_interval_ms, 1_500, "renew at half-life");
        assert_eq!(p.promote_after_ms, 4_500, "ttl + half-ttl guard");
        assert_eq!(p.guard_margin_ms(), 1_500);
    }

    #[test]
    fn default_lease_meets_the_failover_and_cache_bounds() {
        let p = LeaseParams::default_params();
        assert!(p.ttl_ms <= POSTURE_CACHE_TTL_MS, "TTL ≤ posture-cache TTL");
        assert!(p.promote_after_ms <= 5_000, "promote within the ≤5s failover target");
        // A big cut from the legacy ~12 s (PROMOTION_TIMEOUT_MS 10 s + interval).
        assert!(
            p.promote_after_ms < crate::standby_monitor::PROMOTION_TIMEOUT_MS,
            "the lease promote deadline is faster than the legacy heartbeat timeout"
        );
    }

    /// THE split-brain non-overlap invariant, over a range of TTLs: the holder's
    /// self-demote deadline (lease expiry) strictly precedes the challenger's
    /// promotion deadline, with a positive guard margin.
    #[test]
    fn demote_deadline_strictly_precedes_promote_deadline() {
        for ttl in [2_000u64, 3_000, 4_000, 5_000, 10_000] {
            let p = LeaseParams::from_ttl(ttl);
            assert!(p.demote_before_promote(), "invariant must hold for ttl={ttl}");
            // The holder is gone (lease_expired) by ttl; the challenger only promotes
            // at promote_after > ttl → a positive guard window with no overlap.
            assert!(lease_expired(p.ttl_ms, &p), "holder's lease has expired at ttl");
            assert!(!should_promote(p.ttl_ms, &p), "challenger must NOT promote yet at ttl");
            assert!(should_promote(p.promote_after_ms, &p), "challenger promotes at promote_after");
            assert!(p.guard_margin_ms() > 0, "positive guard margin for ttl={ttl}");
        }
    }

    /// `from_ttl` is TOTAL (Copilot #864): for ANY u64 input — including the
    /// degenerate 0/1 and the overflow-prone extremes — the derived params satisfy
    /// the demote-before-promote invariant (renew > 0, promote_after > ttl, no wrap).
    #[test]
    fn from_ttl_is_total_and_never_violates_the_invariant() {
        for ttl in [0u64, 1, 2, 3, 100, 3_000, MAX_LEASE_TTL_MS, MAX_LEASE_TTL_MS + 1, u64::MAX] {
            let p = LeaseParams::from_ttl(ttl);
            assert!(p.renew_interval_ms >= 1, "renew must be ≥ 1 for input {ttl}");
            assert!(
                p.promote_after_ms > p.ttl_ms,
                "promote_after ({}) must exceed ttl ({}) for input {ttl} — no overflow wrap",
                p.promote_after_ms,
                p.ttl_ms
            );
            assert!(p.demote_before_promote(), "invariant must hold for input {ttl}");
        }
        // The degenerate inputs clamp to the floor; the huge inputs clamp to the ceiling.
        assert_eq!(LeaseParams::from_ttl(0).ttl_ms, MIN_LEASE_TTL_MS);
        assert_eq!(LeaseParams::from_ttl(u64::MAX).ttl_ms, MAX_LEASE_TTL_MS);
    }

    #[test]
    fn renew_at_half_life_tolerates_one_missed_renewal() {
        let p = LeaseParams::from_ttl(3_000);
        // Renew fires at half-life; the lease is still valid then (one miss survivable).
        assert!(!should_renew(p.renew_interval_ms - 1, &p));
        assert!(should_renew(p.renew_interval_ms, &p));
        assert!(!lease_expired(p.renew_interval_ms, &p), "one missed renewal: lease still valid");
        // A second consecutive miss reaches expiry → self-demote.
        assert!(lease_expired(p.ttl_ms, &p), "sustained renewal failure expires the lease");
    }

    #[test]
    fn durable_lease_wrappers_map_the_timestamp_and_fail_safe_on_skew() {
        let p = LeaseParams::default_params();
        let renew_at = 100_000u64;

        // Fresh: now just after the renewal → neither promote nor self-demote.
        assert!(!promotion_due_since_renew(renew_at + 10, renew_at, &p));
        assert!(!holder_must_self_demote(renew_at + 10, renew_at, &p));

        // At ttl the holder's lease has expired (self-demote) but the challenger is
        // still inside the guard window (must NOT promote yet).
        assert!(holder_must_self_demote(renew_at + p.ttl_ms, renew_at, &p));
        assert!(!promotion_due_since_renew(renew_at + p.ttl_ms, renew_at, &p));

        // At promote_after the challenger may finally promote.
        assert!(promotion_due_since_renew(renew_at + p.promote_after_ms, renew_at, &p));

        // Skew fails SAFE: an observer whose clock reads BEFORE the stored renewal
        // sees elapsed 0 (freshly renewed), never a spurious huge elapsed.
        assert!(!promotion_due_since_renew(renew_at - 5_000, renew_at, &p), "skew must not promote");
        assert!(!holder_must_self_demote(renew_at - 5_000, renew_at, &p), "skew must not self-demote");
    }

    #[test]
    fn the_default_poll_cadence_observes_every_renewal() {
        // The live monitor polls at PROMOTION_POLL_MS; it must be faster than the
        // lease renew cadence so a live holder is never promoted over.
        let p = LeaseParams::default_params();
        assert!(
            poll_fast_enough(PROMOTION_POLL_MS, &p),
            "PROMOTION_POLL_MS ({PROMOTION_POLL_MS}) must be < the renew interval ({})",
            p.renew_interval_ms
        );
        assert!(!poll_fast_enough(0, &p), "a zero poll interval is rejected");
        assert!(!poll_fast_enough(p.renew_interval_ms, &p), "poll == renew is too slow");
    }
}
