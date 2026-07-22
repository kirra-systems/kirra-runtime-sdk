use std::sync::Arc;

use kirra_persistence::VerifierStore;
use kirra_verifier::gateway::policy::OperationalCommand;
use kirra_verifier::posture_cache::{
    should_route_command, CachedFleetPosture, SharedPostureCache, POSTURE_CACHE_TTL_MS,
};
use kirra_verifier::posture_engine_v2::{resolve_post_promotion_posture, LockoutReason};
use kirra_verifier::verifier::{AppState, FleetPosture, VerifierOperationMode};

#[tokio::test]
async fn concurrent_epoch_claims_have_single_durable_winner() {
    let app = Arc::new(AppState::new(
        VerifierStore::new(":memory:").expect("in-memory store"),
        VerifierOperationMode::PassiveStandby,
    ));

    let observed = app
        .store
        .call_read(|store| store.current_epoch())
        .await
        .expect("store task must complete")
        .expect("current_epoch sql must succeed");

    let mut joins = Vec::new();
    for i in 0..12u64 {
        let app_b = Arc::clone(&app);
        joins.push(tokio::spawn(async move {
            let id = format!("candidate-{i}");
            app_b
                .store
                .call(move |store| store.try_claim_epoch(observed, &id, 1_000 + i))
                .await
        }));
    }

    let mut winner_count = 0u64;
    let mut won_epoch = None;
    for join in joins {
        let outcome = join.await.expect("claim task must run");
        match outcome {
            Ok(Ok(Some(epoch))) => {
                winner_count += 1;
                won_epoch = Some(epoch);
            }
            Ok(Ok(None)) => {}
            Ok(Err(e)) => panic!("unexpected SQL error in epoch claim: {e}"),
            Err(e) => panic!("unexpected store actor error in epoch claim: {e}"),
        }
    }

    assert_eq!(winner_count, 1, "epoch CAS must have exactly one winner");
    assert_eq!(won_epoch, Some(observed + 1));
}

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
        epoch: 0,
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
        epoch: 0,
    };
    let snapshot = Some(stale);
    let now = 1 + POSTURE_CACHE_TTL_MS + 1;
    assert!(
        !should_route_command(&snapshot, now, OperationalCommand::WriteState,),
        "stale cache must fail closed for mutating commands"
    );
}
