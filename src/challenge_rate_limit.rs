//! Rate limiting for the UNAUTHENTICATED attestation-challenge endpoint
//! (`POST /attestation/challenge/{node_id}`) — Bug 3.
//!
//! The challenge endpoint is public (challenge-response is its own guarantee) and
//! per request draws a CSPRNG nonce, prunes the pending-challenge map (an O(n)
//! `retain`), and OVERWRITES the target node's pending nonce. Two consequences
//! make an unbounded issue-rate a real hazard:
//!   1. **Targeted nonce-churn DoS.** Because a new challenge overwrites the
//!      node's pending nonce, an attacker who floods `challenge/{victim}` keeps
//!      invalidating the nonce the legitimate node is about to sign — the victim
//!      can never complete `/attestation/verify`.
//!   2. **CPU amplification.** Each call costs a CSPRNG draw + an O(pending)
//!      prune; a flood turns that into sustained cost.
//!
//! The global `KIRRA_HTTP_MAX_CONCURRENCY` backpressure caps TOTAL in-flight
//! requests but is neither per-node nor per-endpoint, so it does not stop a
//! sustained under-concurrency flood aimed at one victim node's nonce. This
//! module adds that finer control: a two-tier token-bucket limiter (mirroring
//! `kirra-fleet-transport`'s ingress limiter, kept in-crate to avoid pulling a
//! carrier crate into the verifier dep tree).
//!
//! Two tiers, BOTH of which must permit an issuance:
//! - a **per-node** bucket bounds any single node's challenge rate (defeats the
//!   targeted nonce-churn DoS);
//! - a **global** bucket bounds the TOTAL issue rate — the backstop against a
//!   many-node flood, and the reason the per-node map is memory-bounded (a node
//!   seen while the map is full is admitted on the global bucket alone, never
//!   allocated).
//!
//! Pure and clock-injected (`now_ms` is always supplied — no wall-clock read), so
//! the limiter is deterministically testable.

use std::collections::HashMap;

/// Per-node burst allowance: a node may request this many challenges back-to-back
/// (generous — a node needs ~one challenge per attestation).
pub const CHALLENGE_PER_NODE_BURST: u32 = 5;
/// Per-node sustained rate (challenges/sec) once the burst is spent.
pub const CHALLENGE_PER_NODE_REFILL_PER_SEC: f64 = 1.0;
/// Fleet-wide burst allowance across all nodes.
pub const CHALLENGE_GLOBAL_BURST: u32 = 200;
/// Fleet-wide sustained issue rate (challenges/sec).
pub const CHALLENGE_GLOBAL_REFILL_PER_SEC: f64 = 100.0;
/// Cap on the per-node bucket map — bounds memory under a distinct-node flood.
pub const CHALLENGE_MAX_TRACKED_NODES: usize = 8192;
/// `Retry-After` (seconds) advertised on a 429 — one per-node token refills in
/// `1 / CHALLENGE_PER_NODE_REFILL_PER_SEC` s.
pub const CHALLENGE_RETRY_AFTER_SECS: u64 = 1;

/// A single token bucket. `capacity` tokens of burst; refills at `refill_per_ms`
/// tokens/ms. Pure — the caller supplies `now_ms`.
#[derive(Debug, Clone)]
struct TokenBucket {
    capacity: f64,
    refill_per_ms: f64,
    tokens: f64,
    last_ms: u64,
}

impl TokenBucket {
    /// A bucket starting FULL at `now_ms`. `refill_per_sec` is the steady-state
    /// rate; `capacity` the burst allowance.
    fn new(capacity: u32, refill_per_sec: f64, now_ms: u64) -> Self {
        let capacity = capacity.max(1) as f64;
        Self {
            capacity,
            refill_per_ms: refill_per_sec.max(0.0) / 1000.0,
            tokens: capacity,
            last_ms: now_ms,
        }
    }

    /// Add tokens for elapsed time (clamped to `capacity`). A non-advancing or
    /// BACKWARD clock adds nothing — never fabricates tokens, never panics.
    fn refill(&mut self, now_ms: u64) {
        if now_ms > self.last_ms {
            let elapsed = (now_ms - self.last_ms) as f64;
            self.tokens = (self.tokens + elapsed * self.refill_per_ms).min(self.capacity);
            self.last_ms = now_ms;
        }
    }

    /// Tokens available after refilling to `now_ms`, WITHOUT consuming — lets two
    /// buckets be checked jointly so neither is charged when the other denies.
    fn available_at(&mut self, now_ms: u64) -> f64 {
        self.refill(now_ms);
        self.tokens
    }

    fn consume_one(&mut self) {
        self.tokens -= 1.0;
    }
}

/// Two-tier challenge-issue rate limiter: a global backstop bucket plus a bounded
/// map of per-node buckets. For a TRACKED node, an issuance is allowed iff BOTH
/// the global and that node's bucket have a token; only then is one consumed from
/// each (a denial charges neither). Once `max_tracked_nodes` is reached, a
/// previously-unseen node is NOT allocated a bucket — it is admitted purely on the
/// global backstop (the memory bound). The global bucket is checked FIRST and
/// short-circuits: when it is empty nothing can be admitted, so no per-node bucket
/// is even allocated.
#[derive(Debug)]
pub struct ChallengeRateLimiter {
    global: TokenBucket,
    per_node: HashMap<String, TokenBucket>,
    per_node_capacity: u32,
    per_node_refill_per_sec: f64,
    max_tracked_nodes: usize,
}

impl ChallengeRateLimiter {
    /// Build a limiter with explicit tiers (used by tests).
    #[must_use]
    pub fn new(
        global_capacity: u32,
        global_refill_per_sec: f64,
        per_node_capacity: u32,
        per_node_refill_per_sec: f64,
        max_tracked_nodes: usize,
        now_ms: u64,
    ) -> Self {
        Self {
            global: TokenBucket::new(global_capacity, global_refill_per_sec, now_ms),
            per_node: HashMap::new(),
            per_node_capacity,
            per_node_refill_per_sec,
            max_tracked_nodes: max_tracked_nodes.max(1),
        }
    }

    /// Build a limiter from the module's default tiers (the production path).
    #[must_use]
    pub fn with_defaults(now_ms: u64) -> Self {
        Self::new(
            CHALLENGE_GLOBAL_BURST,
            CHALLENGE_GLOBAL_REFILL_PER_SEC,
            CHALLENGE_PER_NODE_BURST,
            CHALLENGE_PER_NODE_REFILL_PER_SEC,
            CHALLENGE_MAX_TRACKED_NODES,
            now_ms,
        )
    }

    /// Should a challenge issuance for `node_id` at `now_ms` be admitted?
    /// `true` → admit (tokens consumed); `false` → rate-limit (respond 429).
    pub fn allow(&mut self, node_id: &str, now_ms: u64) -> bool {
        // Global backstop FIRST, short-circuit: an empty total-rate bucket denies
        // cheaply WITHOUT touching the per-node map, so a global-overload flood
        // cannot allocate per-node buckets for issuances that can't be admitted.
        if self.global.available_at(now_ms) < 1.0 {
            return false;
        }

        let node_ok = if let Some(b) = self.per_node.get_mut(node_id) {
            b.available_at(now_ms) >= 1.0
        } else if self.per_node.len() < self.max_tracked_nodes {
            let mut b =
                TokenBucket::new(self.per_node_capacity, self.per_node_refill_per_sec, now_ms);
            let ok = b.available_at(now_ms) >= 1.0;
            self.per_node.insert(node_id.to_string(), b);
            ok
        } else {
            // Untracked node under a full map → global-only (memory-bounded).
            true
        };

        if node_ok {
            self.global.consume_one();
            if let Some(b) = self.per_node.get_mut(node_id) {
                b.consume_one();
            }
            true
        } else {
            false
        }
    }

    /// Number of currently-tracked per-node buckets (observability/tests).
    #[must_use]
    pub fn tracked_nodes(&self) -> usize {
        self.per_node.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_node_burst_then_limited_until_refill() {
        // burst 3, 1/sec; generous global.
        let mut lim = ChallengeRateLimiter::new(1_000, 1_000.0, 3, 1.0, 64, 1_000);
        assert!(lim.allow("n1", 1_000));
        assert!(lim.allow("n1", 1_000));
        assert!(lim.allow("n1", 1_000));
        assert!(!lim.allow("n1", 1_000), "burst exhausted → 429");
        // Half a second later: not yet one token.
        assert!(!lim.allow("n1", 1_500));
        // One second after the burst: exactly one token back.
        assert!(lim.allow("n1", 2_000));
    }

    #[test]
    fn per_node_buckets_are_isolated_defeating_targeted_flood() {
        let mut lim = ChallengeRateLimiter::new(1_000, 1_000.0, 1, 1.0, 64, 0);
        // A victim node is flooded and limited; a DIFFERENT node is unaffected —
        // an attacker hammering one node cannot starve challenge issuance for
        // every other node.
        assert!(lim.allow("victim", 0));
        assert!(!lim.allow("victim", 0), "victim's per-node bucket is empty");
        assert!(lim.allow("other", 0), "other node has its own bucket");
    }

    #[test]
    fn global_backstop_bounds_total_across_nodes() {
        // Global allows only 2 in a burst; generous per-node. A third distinct
        // node is denied by the GLOBAL bucket even though its own is full.
        let mut lim = ChallengeRateLimiter::new(2, 0.0, 10, 0.0, 64, 0);
        assert!(lim.allow("a", 0));
        assert!(lim.allow("b", 0));
        assert!(!lim.allow("c", 0), "global backstop denies the 3rd node");
    }

    #[test]
    fn denial_charges_neither_bucket() {
        // Global has 5; node x has capacity 1 (no refill).
        let mut lim = ChallengeRateLimiter::new(5, 0.0, 1, 0.0, 16, 0);
        assert!(lim.allow("x", 0)); // 1 global + x's only token
        assert!(!lim.allow("x", 0), "x denied by its own bucket");
        // The denial must NOT have spent a global token: 4 remain for others.
        assert!(lim.allow("y", 0));
        assert!(lim.allow("z", 0));
    }

    #[test]
    fn global_overload_does_not_allocate_per_node_buckets() {
        // Global capacity 1, no refill: the first node is admitted (and tracked);
        // once global is empty, further DISTINCT nodes are denied by the
        // short-circuit WITHOUT allocation — a flood cannot poison the map.
        let mut lim = ChallengeRateLimiter::new(1, 0.0, 10, 0.0, 100_000, 0);
        assert!(lim.allow("a", 0));
        assert_eq!(lim.tracked_nodes(), 1);
        for i in 0..500 {
            assert!(
                !lim.allow(&format!("flood-{i}"), 0),
                "global empty → denied"
            );
        }
        assert_eq!(
            lim.tracked_nodes(),
            1,
            "no per-node buckets allocated under global overload"
        );
    }

    #[test]
    fn per_node_map_is_memory_bounded() {
        // Cap = 2 tracked nodes; generous rates so nothing is rate-limited.
        let mut lim = ChallengeRateLimiter::new(1_000_000, 1_000.0, 1_000, 1_000.0, 2, 0);
        assert!(lim.allow("s1", 0));
        assert!(lim.allow("s2", 0));
        assert_eq!(lim.tracked_nodes(), 2);
        // A 3rd/4th distinct node is admitted (global-only) but NOT allocated.
        assert!(lim.allow("s3", 0));
        assert!(lim.allow("s4", 0));
        assert_eq!(lim.tracked_nodes(), 2, "map stays bounded under many nodes");
    }

    #[test]
    fn backward_clock_never_fabricates_tokens_or_panics() {
        let mut lim = ChallengeRateLimiter::new(1_000, 1_000.0, 1, 1.0, 16, 5_000);
        assert!(lim.allow("n", 5_000));
        assert!(!lim.allow("n", 5_000));
        // Clock jumps backward — no refill, still denied, no panic.
        assert!(!lim.allow("n", 4_000));
        assert!(!lim.allow("n", 1));
    }

    #[test]
    fn defaults_admit_a_normal_attestation_burst() {
        // The production defaults must comfortably admit a node's ordinary
        // back-to-back challenge burst.
        let mut lim = ChallengeRateLimiter::with_defaults(0);
        for _ in 0..CHALLENGE_PER_NODE_BURST {
            assert!(lim.allow("node-1", 0));
        }
        assert!(
            !lim.allow("node-1", 0),
            "the (n+1)th immediate request is limited"
        );
    }
}
