use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use kirra_verifier::store_handle::StoreError;
use kirra_verifier::store_handle::StoreHandle;
use kirra_verifier::verifier_store::VerifierStore;

#[tokio::test(flavor = "current_thread")]
async fn db_actor_call_read_does_not_block_tokio_worker() {
    let handle = StoreHandle::new(VerifierStore::new(":memory:").expect("in-memory store"));
    let ticks = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicBool::new(false));

    let ticks_bg = Arc::clone(&ticks);
    let done_bg = Arc::clone(&done);
    let ticker = tokio::spawn(async move {
        while !done_bg.load(Ordering::Relaxed) {
            ticks_bg.fetch_add(1, Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });

    let started = Instant::now();
    let value = handle
        .call_read(|_| {
            std::thread::sleep(Duration::from_millis(200));
            7usize
        })
        .await
        .expect("call_read task must complete");
    done.store(true, Ordering::Relaxed);
    ticker.await.expect("ticker task must finish");

    assert_eq!(value, 7);
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert!(
        ticks.load(Ordering::Relaxed) >= 5,
        "tokio worker should keep scheduling while DB actor does blocking work"
    );
}

#[tokio::test]
async fn db_actor_panics_fail_closed_with_task_failed() {
    let handle = StoreHandle::new(VerifierStore::new(":memory:").expect("in-memory store"));
    let err = handle
        .call(|_store| {
            panic!("intentional panic to verify fail-closed StoreError");
        })
        .await
        .expect_err("panic in DB actor closure must fail closed");

    assert_eq!(err, StoreError::TaskFailed);
}
