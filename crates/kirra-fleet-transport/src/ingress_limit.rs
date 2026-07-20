//! Ingest rate limiting for the fleet lane (WS-4 / Track 3 transport hardening).
//!
//! Zenoh is an untrusted carrier (ADR-0007 Clause 1): every ingest verifies an
//! Ed25519 signature before use. That makes forged/tampered payloads *safe*, but a
//! flood of them is still a DoS — each bogus message forces an expensive signature
//! verify. This module gates the ingest BEFORE that verify with a token-bucket rate
//! limiter, so a flood is dropped cheaply (and counted as
//! [`RejectReason::RateLimited`](crate::RejectReason::RateLimited)).
//!
//! Two tiers, both of which must permit an ingest:
//! - a **per-source** bucket bounds any single source's rate (the source is the
//!   Zenoh key-expression's node id — untrusted, but fine for *bucketing*). The
//!   per-source map is memory-bounded by `max_tracked_sources`; at the cap a new
//!   source EVICTS the longest-idle bucket (LRU-by-idle, M2 · #1041), so an
//!   id-churn flood cannot strip per-source isolation from genuine active peers —
//!   its single-use spoofed ids are the eviction victims, not the active sources;
//! - a **global** bucket bounds the TOTAL ingest rate — the backstop against a
//!   many-source spoofing flood, checked first and short-circuiting (an
//!   over-global flood is denied before any per-source alloc/eviction).
//!
//! Pure and clock-injected (`now_ms` is always supplied — no wall-clock read), so
//! the limiter is deterministically testable and reused across the sync/async paths.

use std::collections::HashMap;

/// A single token bucket. `capacity` tokens of burst; refills at `refill_per_ms`
/// tokens per millisecond. Pure — the caller supplies `now_ms`.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_ms: f64,
    tokens: f64,
    last_ms: u64,
}

impl TokenBucket {
    /// A bucket starting FULL (`capacity` tokens) at `now_ms`. `refill_per_sec` is
    /// the steady-state allowed rate; `capacity` is the burst allowance.
    #[must_use]
    pub fn new(capacity: u32, refill_per_sec: f64, now_ms: u64) -> Self {
        let capacity = capacity.max(1) as f64;
        Self {
            capacity,
            refill_per_ms: (refill_per_sec.max(0.0)) / 1000.0,
            tokens: capacity,
            last_ms: now_ms,
        }
    }

    /// Add tokens for time elapsed since the last refill (clamped to `capacity`). A
    /// non-advancing or BACKWARD clock adds nothing (never fabricates tokens, never
    /// panics).
    fn refill(&mut self, now_ms: u64) {
        if now_ms > self.last_ms {
            let elapsed = (now_ms - self.last_ms) as f64;
            self.tokens = (self.tokens + elapsed * self.refill_per_ms).min(self.capacity);
            self.last_ms = now_ms;
        }
    }

    /// Refill then try to take one token. `true` → allowed (a token was consumed);
    /// `false` → rate-limited (nothing consumed).
    pub fn try_take(&mut self, now_ms: u64) -> bool {
        self.refill(now_ms);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Tokens available after refilling to `now_ms`, without consuming — used to
    /// check two buckets jointly so neither is charged when the other denies.
    fn available_at(&mut self, now_ms: u64) -> f64 {
        self.refill(now_ms);
        self.tokens
    }

    fn consume_one(&mut self) {
        self.tokens -= 1.0;
    }

    /// Timestamp of this bucket's last refill/activity (ms). Advances every time
    /// the bucket is queried via a later `now_ms`; a bucket that is never touched
    /// again keeps its old value — so the SMALLEST `last_activity_ms` across the
    /// map is the longest-idle (LRU) entry. Used by the per-source-map eviction
    /// (M2 · #1041).
    #[must_use]
    pub fn last_activity_ms(&self) -> u64 {
        self.last_ms
    }
}

/// Two-tier ingest rate limiter: a global backstop bucket plus a bounded map of
/// per-source buckets. For a TRACKED source, an ingest is allowed iff BOTH the
/// global and that source's bucket have a token; only then is one consumed from
/// each (so a denial charges neither). Once `max_tracked_sources` is reached, a
/// previously-unseen source EVICTS the longest-idle tracked bucket (LRU-by-idle,
/// M2 · #1041) and takes its slot — so the map stays memory-bounded AND every
/// genuine source keeps its own bucket, while a churn-flood's single-use spoofed
/// ids are the eviction victims. The global bucket is checked FIRST and short-
/// circuits: when it is empty nothing can be admitted, so no per-source bucket is
/// even allocated (nor evicted).
#[derive(Debug)]
pub struct IngressRateLimiter {
    global: TokenBucket,
    per_source: HashMap<String, TokenBucket>,
    per_source_capacity: u32,
    per_source_refill_per_sec: f64,
    max_tracked_sources: usize,
}

impl IngressRateLimiter {
    /// Build a limiter. `global_*` bound the total ingest rate; `per_source_*` bound
    /// any single source; `max_tracked_sources` caps the per-source map (a new
    /// source seen while the map is full evicts the longest-idle bucket, M2 · #1041).
    #[must_use]
    pub fn new(
        global_capacity: u32,
        global_refill_per_sec: f64,
        per_source_capacity: u32,
        per_source_refill_per_sec: f64,
        max_tracked_sources: usize,
        now_ms: u64,
    ) -> Self {
        Self {
            global: TokenBucket::new(global_capacity, global_refill_per_sec, now_ms),
            per_source: HashMap::new(),
            per_source_capacity,
            per_source_refill_per_sec,
            max_tracked_sources: max_tracked_sources.max(1),
        }
    }

    /// Should an ingest from `source` at `now_ms` be admitted? `true` → admit
    /// (tokens consumed); `false` → rate-limit (drop before the expensive verify).
    pub fn allow(&mut self, source: &str, now_ms: u64) -> bool {
        // Global backstop FIRST, short-circuit: if the total-rate bucket is empty
        // nothing can be admitted, so deny cheaply WITHOUT touching the per-source
        // map. This stops a global-overload flood from allocating per-source
        // buckets (poisoning the map / burning CPU) for ingests that can't be
        // admitted anyway.
        if self.global.available_at(now_ms) < 1.0 {
            return false;
        }

        // Per-source bucket: reuse an existing one; else allocate. When the map is
        // at its tracking cap and the source is unknown, EVICT the longest-idle
        // (smallest `last_activity_ms`) bucket and admit this source into a fresh
        // one, rather than falling through to global-only.
        //
        // M2 (#1041): the old "full map → global-only" fallthrough let an id-churn
        // flood (a fresh spoofed source id per message — the Zenoh key-expr id is
        // untrusted) SATURATE the map with buckets it never revisits, after which
        // every genuine new source lost per-source isolation until restart. LRU-by-
        // idle inverts that: an ACTIVE genuine source (recently touched → large
        // `last_activity_ms`) is preserved, while the churned-once spoofed ids
        // (never touched again → smallest `last_activity_ms`) are the eviction
        // victims. The map stays memory-bounded (evict-one-insert-one holds the cap)
        // and the global backstop still bounds total rate. Eviction only runs for a
        // globally-admitted ingest (the global check above short-circuits a flood
        // first), so the O(n) scan is bounded by the global rate, not the flood.
        let source_ok = if let Some(b) = self.per_source.get_mut(source) {
            b.available_at(now_ms) >= 1.0
        } else {
            if self.per_source.len() >= self.max_tracked_sources {
                if let Some(victim) = self
                    .per_source
                    .iter()
                    .min_by_key(|(_, b)| b.last_activity_ms())
                    .map(|(k, _)| k.clone())
                {
                    self.per_source.remove(&victim);
                }
            }
            let mut b = TokenBucket::new(
                self.per_source_capacity,
                self.per_source_refill_per_sec,
                now_ms,
            );
            let ok = b.available_at(now_ms) >= 1.0;
            self.per_source.insert(source.to_string(), b);
            ok
        };

        if source_ok {
            self.global.consume_one();
            if let Some(b) = self.per_source.get_mut(source) {
                b.consume_one();
            }
            true
        } else {
            false
        }
    }

    /// Number of currently-tracked per-source buckets (for observability/tests).
    #[must_use]
    pub fn tracked_sources(&self) -> usize {
        self.per_source.len()
    }

    /// Whether `source` currently has its own per-source bucket (observability /
    /// tests — e.g. proving M2 eviction kept an active source and dropped the
    /// longest-idle one).
    #[must_use]
    pub fn is_tracked(&self, source: &str) -> bool {
        self.per_source.contains_key(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_allows_burst_up_to_capacity_then_denies() {
        let mut b = TokenBucket::new(3, 1.0, 1_000);
        // Full bucket → 3 immediate takes, then empty.
        assert!(b.try_take(1_000));
        assert!(b.try_take(1_000));
        assert!(b.try_take(1_000));
        assert!(!b.try_take(1_000), "burst exhausted → denied");
    }

    #[test]
    fn bucket_refills_over_time() {
        let mut b = TokenBucket::new(2, 10.0, 1_000); // 10 tokens/sec = 1 per 100ms
        assert!(b.try_take(1_000));
        assert!(b.try_take(1_000));
        assert!(!b.try_take(1_000));
        // 100ms later → +1 token.
        assert!(b.try_take(1_100));
        assert!(!b.try_take(1_100));
        // 1s later → refill clamped to capacity (2), not unbounded.
        assert!(b.try_take(2_100));
        assert!(b.try_take(2_100));
        assert!(!b.try_take(2_100), "refill clamps at capacity");
    }

    #[test]
    fn backward_clock_never_fabricates_tokens_or_panics() {
        let mut b = TokenBucket::new(1, 1.0, 5_000);
        assert!(b.try_take(5_000));
        // Clock jumps backward — no refill, still denied, no panic.
        assert!(!b.try_take(4_000));
        assert!(!b.try_take(1));
    }

    #[test]
    fn per_source_buckets_are_isolated() {
        let mut lim = IngressRateLimiter::new(100, 100.0, 1, 1.0, 16, 0);
        // Source A burns its single token; A is now limited, B is unaffected.
        assert!(lim.allow("a", 0));
        assert!(!lim.allow("a", 0), "a's per-source bucket is empty");
        assert!(lim.allow("b", 0), "b has its own bucket");
    }

    #[test]
    fn global_backstop_bounds_total_across_sources() {
        // Global allows only 2 in a burst; generous per-source. A third distinct
        // source is denied by the GLOBAL bucket even though its own is full.
        let mut lim = IngressRateLimiter::new(2, 0.0, 10, 0.0, 64, 0);
        assert!(lim.allow("a", 0));
        assert!(lim.allow("b", 0));
        assert!(!lim.allow("c", 0), "global backstop denies the 3rd source");
    }

    #[test]
    fn denial_charges_neither_bucket() {
        // Global has 1 token; source X's bucket is empty (capacity 1, already used).
        let mut lim = IngressRateLimiter::new(5, 0.0, 1, 0.0, 16, 0);
        assert!(lim.allow("x", 0)); // consumes 1 global + x's only token
                                    // x now denied by its own bucket; the global token must NOT be spent.
        assert!(!lim.allow("x", 0));
        // A fresh source still gets through — proof the earlier denial left global
        // tokens intact (4 remain).
        assert!(lim.allow("y", 0));
        assert!(lim.allow("z", 0));
    }

    #[test]
    fn global_overload_does_not_allocate_per_source_buckets() {
        // Global capacity 1, no refill: the first source is admitted (and tracked);
        // once the global bucket is empty, further DISTINCT sources are denied by
        // the short-circuit WITHOUT being allocated a per-source bucket — a
        // global-overload flood cannot poison the map.
        let mut lim = IngressRateLimiter::new(1, 0.0, 10, 0.0, 1_000, 0);
        assert!(lim.allow("a", 0));
        assert_eq!(lim.tracked_sources(), 1);
        for i in 0..100 {
            assert!(
                !lim.allow(&format!("flood-{i}"), 0),
                "global empty → denied"
            );
        }
        assert_eq!(
            lim.tracked_sources(),
            1,
            "no per-source buckets allocated under global overload"
        );
    }

    #[test]
    fn per_source_map_is_memory_bounded() {
        // Cap = 2 tracked sources; generous rates so nothing is rate-limited.
        let mut lim = IngressRateLimiter::new(1_000, 1_000.0, 1_000, 1_000.0, 2, 0);
        assert!(lim.allow("s1", 0));
        assert!(lim.allow("s2", 0));
        assert_eq!(lim.tracked_sources(), 2);
        // A 3rd distinct source is admitted (global-only) but NOT allocated — the
        // map cannot grow past the cap under a spoofing flood.
        assert!(lim.allow("s3", 0));
        assert!(lim.allow("s4", 0));
        assert_eq!(
            lim.tracked_sources(),
            2,
            "map stays bounded under many sources"
        );
    }

    #[test]
    fn rate_limited_ingest_is_counted_before_verify() {
        // Simulate the pre-verify gate: on a limiter denial, the caller records
        // RejectReason::RateLimited and drops WITHOUT verifying. Prove the counter
        // reflects the drops.
        use crate::{RejectReason, RejectionCounter};
        let counter = RejectionCounter::new();
        let mut lim = IngressRateLimiter::new(1_000, 0.0, 2, 0.0, 16, 0); // 2/burst per source

        let mut admitted = 0u32;
        for _ in 0..5 {
            if lim.allow("robot-01", 0) {
                admitted += 1;
                // (verify would happen here)
            } else {
                counter.record(&RejectReason::RateLimited);
            }
        }
        assert_eq!(admitted, 2, "only the burst capacity is admitted");
        let snap = counter.snapshot();
        assert_eq!(snap.rate_limited, 3, "the 3 over-rate ingests are counted");
        assert_eq!(counter.total_rejected(), 3);
        // The verify-path counters stayed at zero — nothing reached verification.
        assert_eq!(snap.bad_signature, 0);
        assert_eq!(snap.accepted, 0);
    }

    #[test]
    fn steady_rate_admits_at_the_configured_rate() {
        // 1 token/sec per source, capacity 1: one admit per second sustained.
        let mut lim = IngressRateLimiter::new(1_000, 1_000.0, 1, 1.0, 16, 0);
        assert!(lim.allow("s", 0));
        assert!(!lim.allow("s", 0));
        assert!(!lim.allow("s", 500), "half a second → not yet refilled");
        assert!(lim.allow("s", 1_000), "one second → one token back");
    }

    #[test]
    fn full_map_evicts_longest_idle_and_keeps_active_sources() {
        // M2 (#1041): at the tracking cap, a new source must EVICT the longest-idle
        // bucket (not fall through to global-only), preserving per-source isolation
        // for genuine active sources against an id-churn flood.
        //
        // Generous global rate so the global backstop never masks the per-source
        // behaviour under test; per-source cap 1; MAP CAP = 2.
        let mut lim = IngressRateLimiter::new(1_000_000, 1_000_000.0, 1, 1.0, 2, 0);

        // Fill both slots.
        assert!(lim.allow("a", 0));
        assert!(lim.allow("b", 0));
        assert_eq!(lim.tracked_sources(), 2);

        // Touch "a" later so "b" becomes the longest-idle (last_activity: a=100,
        // b=0). The admit RESULT is irrelevant (a's cap-1 bucket is empty 0.1 s
        // after its first use) — the point is that querying it REFRESHES a's idle
        // timestamp, which is what protects it from eviction.
        lim.allow("a", 100);

        // A NEW source "c" arrives at the cap → evict the longest-idle ("b"),
        // admit "c" into a fresh per-source bucket. The map stays at the cap.
        assert!(lim.allow("c", 200));
        assert_eq!(lim.tracked_sources(), 2, "cap held — eviction, not growth");
        assert!(lim.is_tracked("a"), "the recently-active source survives");
        assert!(
            lim.is_tracked("c"),
            "the new source got its OWN per-source bucket"
        );
        assert!(!lim.is_tracked("b"), "the longest-idle source was evicted");

        // Isolation restored for "c": its own capacity-1 bucket rate-limits it,
        // rather than it riding global-only (which the old fallthrough allowed).
        assert!(
            !lim.allow("c", 200),
            "c's per-source bucket is now empty → denied"
        );
    }

    #[test]
    fn id_churn_flood_cannot_strip_isolation_from_an_active_source() {
        // The end-to-end M2 property: a genuine active source keeps its per-source
        // bucket even while an attacker churns a fresh spoofed id every message.
        let mut lim = IngressRateLimiter::new(1_000_000, 1_000_000.0, 1, 1.0, 4, 0);

        // Genuine source "good" is active every tick.
        for t in 0..50u64 {
            // Keep "good" warm (its bucket refills at 1/s; capacity 1).
            let _ = lim.allow("good", t * 1_000);
            // Attacker: a brand-new id each tick.
            assert!(lim.allow(&format!("spoof-{t}"), t * 1_000 + 1));
        }
        assert!(
            lim.is_tracked("good"),
            "the continuously-active source retained its per-source bucket through the flood"
        );
        assert!(
            lim.tracked_sources() <= 4,
            "the map stayed memory-bounded at the cap"
        );
    }
}
