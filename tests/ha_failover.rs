//! WS-4 (Fleet Plane GA) — HA topology failover + anti-split-brain drill.
//!
//! The shared-file HA topology has one Active primary and one PassiveStandby sharing
//! the durable store. Availability comes from the standby PROMOTING when the primary
//! goes silent; SAFETY comes from the durable HA epoch fence (`ha_state`) — a
//! compare-and-set that guarantees only ONE instance owns writes at a time, so a
//! revived old primary can never double-write (split brain).
//!
//! This drill exercises that guarantee DETERMINISTICALLY at the store level (the real
//! `try_claim_epoch` CAS + `assert_actuator_epoch_held` fence over two connections to
//! one file) — no async monitors, no 10 s wall-clock timers — plus the pure
//! heartbeat-timing invariants the live monitors rely on. It proves: a standby
//! promotes by claiming the next durable epoch, and the old primary is then FENCED
//! out of writing.

use kirra_verifier::lease::{lease_expired, should_promote, LeaseParams, DEFAULT_LEASE_TTL_MS};
use kirra_verifier::posture_cache::POSTURE_CACHE_TTL_MS;
use kirra_verifier::standby_monitor::{
    should_self_demote_on_heartbeat_failures, HEARTBEAT_INTERVAL_MS, HEARTBEAT_KEY,
    MAX_CONSECUTIVE_HEARTBEAT_FAILURES, PROMOTION_TIMEOUT_MS,
};
use kirra_verifier::verifier_store::VerifierStore;

#[test]
fn ha_failover_promotes_standby_and_fences_the_old_primary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("verifier.sqlite");
    let path = db.to_str().unwrap();

    // Two instances share the durable store (the WS-4 shared-file HA topology).
    let mut a = VerifierStore::new(path).expect("open primary A");
    let mut b = VerifierStore::new(path).expect("open standby B");

    // --- Primary A claims the epoch → it owns writes ---
    let e0 = a.current_epoch().unwrap();
    let e1 = a
        .try_claim_epoch(e0, "A", 1_000)
        .unwrap()
        .expect("A claims the epoch at startup");
    assert_eq!(e1, e0 + 1);
    a.assert_actuator_epoch_held(e1)
        .expect("A holds the epoch → its writes are admitted");

    // A heartbeats into the shared durable store.
    a.save_engine_state(HEARTBEAT_KEY, &1_000u64.to_string()).unwrap();

    // --- Standby B reads a FRESH heartbeat → it must NOT promote ---
    let hb: u64 = b
        .load_engine_state(HEARTBEAT_KEY)
        .unwrap()
        .expect("heartbeat present")
        .parse()
        .unwrap();
    let now_fresh = 1_000 + PROMOTION_TIMEOUT_MS - 1;
    assert!(
        now_fresh.saturating_sub(hb) < PROMOTION_TIMEOUT_MS,
        "a fresh heartbeat keeps B in standby"
    );

    // --- A dies (stops heartbeating); time advances past the promotion timeout ---
    let now_stale = 1_000 + PROMOTION_TIMEOUT_MS + 1;
    assert!(
        now_stale.saturating_sub(hb) >= PROMOTION_TIMEOUT_MS,
        "a stale heartbeat is B's promotion trigger"
    );

    // --- B promotes by claiming the NEXT durable epoch (the real CAS) ---
    let observed = b.current_epoch().unwrap();
    assert_eq!(observed, e1, "B observes A's epoch before promoting");
    let e2 = b
        .try_claim_epoch(observed, "B", now_stale)
        .unwrap()
        .expect("B wins the epoch claim");
    assert_eq!(e2, e1 + 1);
    b.assert_actuator_epoch_held(e2)
        .expect("B now holds the epoch → its writes are admitted");

    // --- SPLIT-BRAIN FENCE: the old primary A revives and tries to act at its STALE
    // epoch. It still believes it holds e1, but the durable epoch is now e2. ---
    assert!(
        a.assert_actuator_epoch_held(e1).is_err(),
        "the fenced old primary CANNOT write (epoch superseded)"
    );
    assert!(
        a.try_claim_epoch(e1, "A", now_stale + 1).unwrap().is_none(),
        "A's stale-epoch re-claim is refused by the durable CAS"
    );

    // Exactly ONE writer (B) at a time — split brain prevented.
    let (cur, holder) = a.current_active_holder().unwrap();
    assert_eq!(cur, e2);
    assert_eq!(holder.as_deref(), Some("B"), "B is the sole active holder");
}

#[test]
fn heartbeat_timing_leaves_no_split_brain_window() {
    // A primary self-demotes after MAX_CONSECUTIVE_HEARTBEAT_FAILURES failed ticks.
    assert!(!should_self_demote_on_heartbeat_failures(0));
    assert!(!should_self_demote_on_heartbeat_failures(MAX_CONSECUTIVE_HEARTBEAT_FAILURES - 1));
    assert!(should_self_demote_on_heartbeat_failures(MAX_CONSECUTIVE_HEARTBEAT_FAILURES));

    // The safety-critical timing invariant: a wedged primary self-demotes STRICTLY
    // BEFORE a standby's promotion window opens, so the two mode_active windows never
    // overlap (no transient double-primary even before the epoch fence catches it).
    assert!(
        (MAX_CONSECUTIVE_HEARTBEAT_FAILURES as u64) * HEARTBEAT_INTERVAL_MS < PROMOTION_TIMEOUT_MS,
        "self-demote window ({} ms) must close before the promotion window ({} ms) opens",
        (MAX_CONSECUTIVE_HEARTBEAT_FAILURES as u64) * HEARTBEAT_INTERVAL_MS,
        PROMOTION_TIMEOUT_MS
    );
}

/// WP-19 (G-21) — the LEASE timing model carries the SAME split-brain non-overlap
/// guarantee as the heartbeat clamp above, but as a first-class contract derived
/// from one TTL, and it is FASTER: the default lease promotes within the ≤5 s
/// failover target (bounded by the posture-cache TTL) instead of the legacy ~12 s.
/// This is the pure timing proof; wiring the lease into the live promotion loop is
/// the recorded WP-19 follow-up (the epoch fence + heartbeat writer stay intact).
#[test]
fn lease_timing_leaves_no_split_brain_window_and_is_faster() {
    let p = LeaseParams::default_params();

    // Non-overlap: the holder's lease has expired (it must have self-demoted) by
    // `ttl_ms`, while a challenger is not yet allowed to promote until `promote_after_ms`.
    assert!(lease_expired(p.ttl_ms, &p), "the holder's lease has expired at ttl");
    assert!(
        !should_promote(p.ttl_ms, &p),
        "a challenger must NOT promote at ttl — the holder may only just have demoted"
    );
    assert!(should_promote(p.promote_after_ms, &p), "the challenger promotes at promote_after");
    assert!(
        p.ttl_ms < p.promote_after_ms && p.guard_margin_ms() > 0,
        "demote deadline ({} ms) strictly precedes promote deadline ({} ms), guard {} ms",
        p.ttl_ms,
        p.promote_after_ms,
        p.guard_margin_ms()
    );

    // Faster + bounded: the lease promote deadline meets the ≤5 s target, stays within
    // the posture-cache staleness window, and beats the legacy heartbeat timeout.
    assert_eq!(p.ttl_ms, DEFAULT_LEASE_TTL_MS);
    assert!(p.promote_after_ms <= 5_000, "≤5 s failover target");
    assert!(p.ttl_ms <= POSTURE_CACHE_TTL_MS, "TTL bounded by the posture-cache TTL");
    assert!(
        p.promote_after_ms < PROMOTION_TIMEOUT_MS,
        "the lease promote deadline ({} ms) is faster than the legacy heartbeat timeout ({} ms)",
        p.promote_after_ms,
        PROMOTION_TIMEOUT_MS
    );
}
