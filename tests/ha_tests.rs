use std::sync::Arc;

use kirra_verifier::gateway::policy::OperationalCommand;
use kirra_verifier::posture_cache::{
    should_route_command, CachedFleetPosture, SharedPostureCache, POSTURE_CACHE_TTL_MS,
};
use kirra_verifier::posture_engine_v2::{resolve_post_promotion_posture, LockoutReason};
use kirra_verifier::verifier::FleetPosture;

#[test]
fn post_promotion_empty_cache_is_fail_closed_locked_out() {
    let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));
    let (posture, reason) = resolve_post_promotion_posture(&cache);
    assert_eq!(posture, FleetPosture::LockedOut);
    assert_eq!(reason, Some(LockoutReason::PostureCacheEmpty));
}

#[test]
fn stale_cache_is_fail_closed_locked_out() {
    let stale = CachedFleetPosture {
        posture: FleetPosture::Nominal,
        generated_at_ms: 1,
        ttl_ms: POSTURE_CACHE_TTL_MS,
        generation: 1,
    };
    let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(Some(stale)));
    let (posture, reason) = resolve_post_promotion_posture(&cache);
    assert_eq!(posture, FleetPosture::LockedOut);
    assert_eq!(reason, Some(LockoutReason::PostureCacheStale));
}

#[test]
fn routing_gate_blocks_when_cache_is_stale() {
    let stale = CachedFleetPosture {
        posture: FleetPosture::Nominal,
        generated_at_ms: 1,
        ttl_ms: POSTURE_CACHE_TTL_MS,
        generation: 1,
    };
    let snapshot = Some(stale);
    let now = 1 + POSTURE_CACHE_TTL_MS + 1;
    assert!(
        !should_route_command(&snapshot, now, OperationalCommand::WriteState,),
        "stale cache must fail closed for mutating commands"
    );
}
