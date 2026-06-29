use std::sync::Arc;
use std::time::Duration;

use kirra_verifier::posture_cache::{CachedFleetPosture, SharedPostureCache};
use kirra_verifier::posture_engine::recalculate_and_broadcast;
use kirra_verifier::posture_engine_v2::{start_posture_engine_worker, PostureRecalcTrigger};
use kirra_verifier::verifier::{
    AppState, FleetPosture, NodeTrustState, RegisteredNode, VerifierOperationMode,
};
use kirra_verifier::verifier_store::VerifierStore;

fn app() -> Arc<AppState> {
    Arc::new(AppState::new(
        VerifierStore::new(":memory:").expect("in-memory store"),
        VerifierOperationMode::Active,
    ))
}

fn cache() -> SharedPostureCache {
    Arc::new(std::sync::RwLock::new(Some(CachedFleetPosture::new(
        FleetPosture::Nominal,
    ))))
}

fn node(node_id: &str) -> RegisteredNode {
    RegisteredNode {
        node_id: node_id.to_string(),
        status: NodeTrustState::Trusted,
        registered_at_ms: 1,
        last_trust_update_ms: 1,
        ak_public_pem: None,
        expected_pcr16_digest_hex: None,
        site: None,
        firmware_version: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn posture_worker_handles_burst_triggers_without_deadlock() {
    let app = app();
    let cache = cache();
    let initial_generation = cache
        .read()
        .expect("cache lock")
        .as_ref()
        .map(|c| c.generation)
        .unwrap_or(0);
    let tx = start_posture_engine_worker(Arc::clone(&app), Arc::clone(&cache));

    for i in 0..64 {
        let trigger = if i % 2 == 0 {
            PostureRecalcTrigger::PeriodicRefresh
        } else {
            PostureRecalcTrigger::NodeTrustChanged {
                node_id: format!("node-{i}"),
                reason: "burst".to_string(),
            }
        };
        tx.send(trigger).await.expect("send trigger");
    }

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let generation = cache
                .read()
                .expect("cache lock")
                .as_ref()
                .map(|c| c.generation)
                .unwrap_or(0);
            if generation > initial_generation {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("worker should complete burst recalc");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn posture_dag_recalc_completes_under_dashmap_contention() {
    let app = app();
    let cache = cache();

    for i in 0..24 {
        let id = format!("n{i}");
        app.persist_and_insert_node(node(&id)).expect("insert node");
        if i > 0 {
            app.dependency_graph.insert(id, vec![format!("n{}", i - 1)]);
        }
    }

    let app_w = Arc::clone(&app);
    let writer = tokio::spawn(async move {
        for i in 0..200usize {
            let id = format!("w{}", i % 24);
            app_w
                .dependency_graph
                .insert(id.clone(), vec![format!("n{}", i % 24)]);
            if i % 3 == 0 {
                app_w.dependency_graph.remove(&id);
            }
            tokio::task::yield_now().await;
        }
    });

    let app_r = Arc::clone(&app);
    let cache_r = Arc::clone(&cache);
    let reader = tokio::task::spawn_blocking(move || {
        for _ in 0..100 {
            recalculate_and_broadcast(&app_r, &cache_r);
        }
    });

    tokio::time::timeout(Duration::from_secs(3), async {
        writer.await.expect("writer join");
        reader.await.expect("reader join");
    })
    .await
    .expect("contention run should finish without deadlock");
}
