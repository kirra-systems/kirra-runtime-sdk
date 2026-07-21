// store_offload_tests — extracted verbatim from kirra_verifier_service.rs (L3 bin decomposition, pure move).
// ---------------------------------------------------------------------------
// Store offload helper (heavy-op spawn_blocking path).
//
// `StoreHandle::call` moves the long-held SQLite ops (backup export,
// audit-chain verify, federation commit) off the tokio worker pool. These tests
// pin its contract: a closure runs to completion against the real store, a write
// is visible to a subsequent offloaded read, and `&mut self` writes + `&self`
// reads both work through the handle. Each runs on a multi-thread runtime so the
// spawn_blocking offload is actually exercised.
// ---------------------------------------------------------------------------

use kirra_persistence::VerifierStore;
use kirra_verifier::store_handle::StoreError;
use kirra_verifier::verifier::{AppState, VerifierOperationMode};
use std::sync::Arc;

fn app() -> Arc<AppState> {
    let store = VerifierStore::new(":memory:").expect("in-memory store");
    Arc::new(AppState::new(store, VerifierOperationMode::Active))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offloaded_write_is_visible_to_an_offloaded_read() {
    let app = app();

    let wrote = app
        .store
        .call(|store| store.save_engine_state("offload_probe", "42").is_ok())
        .await;
    assert!(
        matches!(wrote, Ok(true)),
        "offloaded write must run to completion: {wrote:?}"
    );

    let read = app
        .store
        .call(|store| store.load_engine_state("offload_probe").ok().flatten())
        .await;
    assert!(
        matches!(read, Ok(Some(ref v)) if v == "42"),
        "an offloaded read must observe the offloaded write; got {read:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offloaded_closure_return_value_is_propagated() {
    let app = app();
    // A pure read that computes a value off-thread and returns it intact.
    let n: Result<u64, StoreError> = app.store.call(|_store| 7u64 * 6).await;
    assert!(
        matches!(n, Ok(42)),
        "closure return value must propagate; got {n:?}"
    );
}
