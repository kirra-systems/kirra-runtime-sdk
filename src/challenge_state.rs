//! ADR-0035 Stage 3 (slice 3i) — the volatile challenge/nonce state, lifted off
//! the `AppState` god-object into a cohesive field façade (the 3f/3g/3h pattern),
//! byte-identical.
//!
//! These three fields are the verifier's VOLATILE, never-persisted challenge
//! surface — the attestation and operator-clearance nonce maps plus the
//! public-endpoint rate limiter that protects the attestation-challenge issuance:
//!
//! - `pending_challenges` — the attestation-challenge nonce map (INVARIANT #5):
//!   volatile, NEVER persisted to SQLite, TTL-bounded (`CHALLENGE_TTL_MS`),
//!   single-use. The field declaration is kept VERBATIM (name + type) so the
//!   invariant's grep still resolves here after the relocation.
//! - `pending_clearance_challenges` — the operator-clearance nonce map (#314
//!   Phase 1), keyed `"{operator_id}|{node_id}"`; same volatility discipline
//!   (INVARIANT #5): never persisted, TTL-bounded, single-use.
//! - `challenge_rate_limiter` — the Bug 3 two-tier (per-node + global) token
//!   bucket bounding `POST /attestation/challenge/{node_id}` issuance, defeating
//!   a nonce-churn DoS. `&mut`-checked under its own `Mutex`.
//!
//! They are grouped in a ROOT-crate leaf (not `kirra-safety-authority`, whose
//! std-only-plus-crypto character keeps it Kani/loom/MSRV-clean — a `DashMap` /
//! runtime rate limiter is the wrong kind of thing to put there). The move adds
//! NO dependency edge: `dashmap`, `ChallengeEntry` / `ClearanceChallengeEntry`
//! (root `verifier`) and `ChallengeRateLimiter` (root `challenge_rate_limit`)
//! already live in the root tree. Embedded on `AppState` as `app.challenges`;
//! per-field semantics are UNCHANGED — pure relocation, no `&mut self` on
//! `AppState`, no behaviour change. Call sites read `app.challenges.<field>`.

use std::sync::{Arc, Mutex};

use dashmap::DashMap;

use crate::challenge_rate_limit::ChallengeRateLimiter;
use crate::verifier::{ChallengeEntry, ClearanceChallengeEntry};

/// The volatile challenge/nonce state (ADR-0035 slice 3i). None of the three is
/// ever persisted; the two maps are TTL-bounded single-use nonce stores and the
/// limiter is pure ingress protection. See the module docs for the per-field
/// invariants.
pub struct ChallengeState {
    /// Volatile in-memory attestation-challenge map — nonces are never persisted
    /// to SQLite (INVARIANT #5). Field name/type kept verbatim across the
    /// relocation so the invariant's grep resolves here.
    pub pending_challenges: DashMap<String, ChallengeEntry>,
    /// Volatile operator clearance-challenge map (#314 Phase 1) — keyed by
    /// `"{operator_id}|{node_id}"`. Same volatility discipline as
    /// `pending_challenges` (INVARIANT #5): never persisted, TTL-bounded, single-use.
    pub pending_clearance_challenges: DashMap<String, ClearanceChallengeEntry>,
    /// Bug 3 — rate limiter for the UNAUTHENTICATED
    /// `POST /attestation/challenge/{node_id}` endpoint. A two-tier token bucket
    /// (per-node + global backstop) bounding challenge issuance; `&mut`-checked
    /// under this mutex (critical section is a hashmap lookup + a few float ops).
    pub challenge_rate_limiter: Arc<Mutex<ChallengeRateLimiter>>,
}

impl ChallengeState {
    /// Both nonce maps empty and the limiter seeded clock-free (`last_ms = 0`) —
    /// the exact defaults the prior `AppState::new` set inline (byte-identical
    /// initial state; the buckets start full, and the first real `allow(_, now)`
    /// refills from 0 clamped to capacity, so a 0 baseline is a no-op vs. `now`).
    pub fn new() -> Self {
        Self {
            pending_challenges: DashMap::new(),
            pending_clearance_challenges: DashMap::new(),
            challenge_rate_limiter: Arc::new(Mutex::new(ChallengeRateLimiter::with_defaults(0))),
        }
    }
}

impl Default for ChallengeState {
    fn default() -> Self {
        Self::new()
    }
}
