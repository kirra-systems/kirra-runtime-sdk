use std::sync::Arc;

use kirra_verifier::verifier::{AppState, VerifierOperationMode};
use kirra_verifier::verifier_store::VerifierStore;

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
