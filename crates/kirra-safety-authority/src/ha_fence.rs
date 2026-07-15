//! ADR-0035 Stage 3 (slice 3f) — the HA split-brain fence state + the pure fence
//! decision (SG-009 / HA-L3), lifted VERBATIM off the `AppState` god-object into
//! the safety-authority crate.
//!
//! Two things live here, and they belong together — the atomics the mutation gate
//! reads, and the pure predicate that reads them:
//!
//! - [`HaFenceState`] — the three interior-mutable atomics that coordinate this
//!   process's HA role (`mode_active`) and its durable-epoch fencing (`held_epoch`,
//!   `cached_db_epoch`). Embedded on `AppState` as `app.ha_fence`, reached as
//!   `app.ha_fence.<field>`; each field's semantics are UNCHANGED (documented on
//!   the struct below). All are `Arc<Atomic*>` interior-mutable (shared-ref access
//!   only — no `&mut self`), so the move is pure relocation — no behaviour change.
//! - [`mutation_fence_verdict`] + [`MutationFence`] — the pure fence predicate
//!   (stop-gate review H3): no env, no store, exhaustively unit-tested over the
//!   finite domain. This is the genuine safety-authority decision surface, so it
//!   moves here beside the state it reads (the 3b/3d "move the test suite as the
//!   behaviour-preservation proof" precedent — the exhaustive test moves with it).
//!
//! `AppState` keeps `is_active()` / `current_mode()` as thin delegators, so every
//! `app.is_active()` / `app.current_mode()` caller is unchanged; the crate never
//! sees `StoreHandle` (the `store.current_epoch()` seed stays in `AppState::new`,
//! passed into [`HaFenceState::new`] as a plain `u64`) or `VerifierOperationMode`.
//! std-only — no new crate dependency.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// The HA split-brain fence state (ADR-0035 slice 3f). Interior-mutable atomics
/// coordinating this process's HA role and its durable-epoch fencing.
pub struct HaFenceState {
    /// Runtime-mutable operational mode.
    /// true = Active (accepts mutations); false = PassiveStandby (read-only).
    /// LOCAL only — coordinates this process. Distributed split-brain is
    /// prevented by `held_epoch` against the durable `ha_state` row.
    pub mode_active: Arc<AtomicBool>,
    /// HA fencing token (durable epoch) currently claimed by this instance.
    /// 0 = no claim yet. The mutation gate compares this to the DB epoch on
    /// every state-mutating request; if they diverge this node has been
    /// fenced (another instance promoted) and must self-demote.
    pub held_epoch: Arc<AtomicU64>,
    /// Pass B1 cache (S3 / #115): the most recently observed durable `ha_state`
    /// epoch. The mutation gate (`policy_layer.rs::enforce_posture_routing`)
    /// reads this atomically instead of taking `store.lock()` + `current_epoch()`
    /// per request. Re-stamped by `perform_promotion` after a successful
    /// `try_claim_epoch` (Release) and by the heartbeat writer on every
    /// `HEARTBEAT_INTERVAL_MS` tick (Release). 0 = "not yet observed";
    /// the gate treats 0 the same way the previous DB-read path treated
    /// an unreadable epoch — fall through and rely on the existing
    /// `held == 0` / non-Active checks for fail-closed.
    pub cached_db_epoch: Arc<AtomicU64>,
}

impl HaFenceState {
    /// Seed the fence: `initial_active` sets `mode_active` (Active vs
    /// PassiveStandby), `initial_db_epoch` seeds `cached_db_epoch` from the
    /// store's current durable epoch (read in `AppState::new` before the store
    /// moves into the handle; unreadable → 0). `held_epoch` always starts at 0
    /// (no claim yet). Byte-identical to the prior inline `AppState::new` init.
    pub fn new(initial_active: bool, initial_db_epoch: u64) -> Self {
        Self {
            mode_active: Arc::new(AtomicBool::new(initial_active)),
            held_epoch: Arc::new(AtomicU64::new(0)),
            cached_db_epoch: Arc::new(AtomicU64::new(initial_db_epoch)),
        }
    }

    /// Returns true if this instance is currently Active (accepting mutations).
    /// Reads the atomic — reflects runtime promotion that occurred after startup.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.mode_active.load(Ordering::SeqCst)
    }
}

/// The HA fence verdict for a `WriteState`/`SystemMutation` at the outer gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationFence {
    /// Active instance with a fresh/uncontested epoch — admit (subject to the
    /// posture check downstream).
    Admit,
    /// Not Active (demoted or standby) — refuse. This is the authoritative HA
    /// fence: it does NOT depend on the epoch cache, which is defeated on a
    /// disk-wedge self-demotion (the heartbeat loop stops, freezing
    /// `cached_db_epoch == held_epoch` so the stale-epoch check can never fire).
    DenyNotActive,
    /// Active but the held epoch is stale vs the cached durable epoch — a newer
    /// primary exists; self-demote and refuse (the fast first-line fence for the
    /// non-top-tier writes, which have no in-transaction epoch re-check).
    DenyStaleEpoch,
}

/// Pure fence predicate (stop-gate review H3) — no env, no store, unit-tested
/// exhaustively over the finite domain {active, standby} × {0,1,2}² (the epoch
/// class representatives). A non-Active instance is ALWAYS fenced (independent of
/// the epoch cache); an Active instance is fenced only when its held epoch is
/// present and disagrees with the present cached durable epoch (a `0` on either
/// side = cold start / unclaimed → admit).
pub fn mutation_fence_verdict(
    is_active: bool,
    held_epoch: u64,
    cached_db_epoch: u64,
) -> MutationFence {
    if !is_active {
        return MutationFence::DenyNotActive;
    }
    if cached_db_epoch != 0 && held_epoch != 0 && held_epoch != cached_db_epoch {
        return MutationFence::DenyStaleEpoch;
    }
    MutationFence::Admit
}

#[cfg(test)]
mod tests {
    use super::*;

    // Stop-gate review H3 — the mutation fence must refuse a non-Active instance
    // INDEPENDENT of the epoch cache (which is frozen == held on a disk-wedge
    // self-demotion), and still self-demote an Active instance whose held epoch is
    // stale. This drives the predicate over the FULL finite domain
    // {active, standby} × {0,1,2}² (18 cases): `{0,1,2}` spans every distinction
    // the predicate makes — held==0, db==0, held==db (both nonzero), and
    // held!=db (both nonzero) — so it is genuinely exhaustive over the class
    // representatives, not a hand-picked sample.
    #[test]
    fn mutation_fence_exhaustive_over_finite_domain() {
        for is_active in [false, true] {
            for held in 0u64..=2 {
                for db in 0u64..=2 {
                    // Expected verdict computed independently of the predicate,
                    // straight from the H3 spec.
                    let expected = if !is_active {
                        // Not Active → ALWAYS fenced, including the frozen-cache
                        // case (held == db) the stale-epoch check cannot catch.
                        MutationFence::DenyNotActive
                    } else if db != 0 && held != 0 && held != db {
                        // Active + both epochs present + disagree → self-demote.
                        MutationFence::DenyStaleEpoch
                    } else {
                        // Active + fresh/uncontested (a 0 on either side = cold
                        // start / unclaimed, or held == db) → admit.
                        MutationFence::Admit
                    };
                    assert_eq!(
                        mutation_fence_verdict(is_active, held, db),
                        expected,
                        "is_active={is_active}, held={held}, db={db}"
                    );
                }
            }
        }
    }

    #[test]
    fn ha_fence_state_new_seeds_and_is_active_reads() {
        let s = HaFenceState::new(true, 5);
        assert!(s.is_active());
        assert_eq!(s.held_epoch.load(Ordering::SeqCst), 0);
        assert_eq!(s.cached_db_epoch.load(Ordering::SeqCst), 5);
        let standby = HaFenceState::new(false, 0);
        assert!(!standby.is_active());
    }
}
