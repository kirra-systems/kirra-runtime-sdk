use std::sync::Arc;
use std::time::Duration;

use kirra_verifier::store_handle::StoreError;
use kirra_verifier::verifier::{AppState, VerifierOperationMode};
use kirra_verifier::verifier_store::VerifierStore;

fn app() -> Arc<AppState> {
    Arc::new(AppState::new(
        VerifierStore::new(":memory:").expect("in-memory store"),
        VerifierOperationMode::Active,
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn db_call_is_offloaded_and_runtime_stays_responsive() {
    let app = app();
    let store = app.store.clone();

    let db_work = tokio::spawn(async move {
        store
            .call(|store| {
                std::thread::sleep(Duration::from_millis(150));
                store
                    .save_engine_state("dbactortests_probe", "ok")
                    .expect("save_engine_state");
            })
            .await
    });

    let responsive = tokio::time::timeout(Duration::from_millis(80), async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        1u8
    })
    .await;

    assert_eq!(
        responsive.expect("runtime must stay responsive while DB work runs"),
        1
    );
    assert!(matches!(db_work.await.expect("join"), Ok(())));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn db_call_failure_surfaces_fail_closed_error() {
    let app = app();
    let result = app
        .store
        .call(|_store| -> u64 { panic!("intentional panic to exercise fail-closed task failure") })
        .await;

    assert!(matches!(result, Err(StoreError::TaskFailed)));
}
