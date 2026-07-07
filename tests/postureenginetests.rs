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

#[tokio::test]
async fn posture_recalc_remains_bounded_under_dashmap_contention() {
    let app = app();
    let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));

    app.nodes.insert("root".into(), node("root"));
    app.nodes.insert("dep_a".into(), node("dep_a"));
    app.dependency_graph
        .insert("root".into(), vec!["dep_a".into()]);

    let writer_app = Arc::clone(&app);
    let writer = tokio::spawn(async move {
        for i in 0..250u32 {
            let temp = format!("temp-{i}");
            writer_app.nodes.insert(temp.clone(), node(&temp));
            writer_app
                .dependency_graph
                .insert("root".into(), vec!["dep_a".into(), temp.clone()]);
            writer_app.nodes.remove(temp.as_str());
            tokio::task::yield_now().await;
        }
    });

    for _ in 0..40 {
        let app_b = Arc::clone(&app);
        let cache_b = Arc::clone(&cache);
        tokio::time::timeout(
            Duration::from_secs(2),
            tokio::task::spawn_blocking(move || recalculate_and_broadcast(&app_b, &cache_b)),
        )
        .await
        .expect("recalc should not deadlock")
        .expect("recalc task should not panic");
    }

    writer.await.expect("writer task should finish");
    assert!(
        cache.read().expect("cache lock").is_some(),
        "at least one recalc should publish posture cache state"
    );
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


/// S-DG1 end-to-end through the REAL worker: a GovernorDivergence trigger
/// (as the parko `PostureEngineSenderSink` sends it) drives the fleet cache to
/// Degraded on the first significant tick, to LockedOut on an escalated tick,
/// and the lockout survives subsequent healthy recalcs (sticky, human reset).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn governor_divergence_trigger_drives_fleet_posture_through_worker() {
    let app = app();
    let cache = cache();
    // A trusted node so the DAG itself is Nominal (not the M-9 empty-set lockout).
    app.persist_and_insert_node(node("robot-01")).expect("insert node");
    let tx = start_posture_engine_worker(Arc::clone(&app), Arc::clone(&cache));

    let posture = |cache: &SharedPostureCache| {
        cache.read().expect("cache lock").as_ref().map(|c| c.posture)
    };
    let wait_for = |cache: SharedPostureCache, want: FleetPosture| async move {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if cache.read().expect("cache lock").as_ref().map(|c| c.posture) == Some(want) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
    };

    // First significant tick → Degraded, immediately (no grace period).
    tx.send(PostureRecalcTrigger::GovernorDivergence { significant: true, escalated: false })
        .await
        .expect("send");
    wait_for(Arc::clone(&cache), FleetPosture::Degraded)
        .await
        .expect("significant divergence must degrade the fleet");

    // Escalated tick (the comparator's own sustained-divergence decision) →
    // sticky LockedOut.
    tx.send(PostureRecalcTrigger::GovernorDivergence { significant: true, escalated: true })
        .await
        .expect("send");
    wait_for(Arc::clone(&cache), FleetPosture::LockedOut)
        .await
        .expect("escalated divergence must lock the fleet out");

    // Sticky: a healthy periodic refresh must NOT downgrade it.
    tx.send(PostureRecalcTrigger::PeriodicRefresh).await.expect("send");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        posture(&cache),
        Some(FleetPosture::LockedOut),
        "the divergence lockout is human-reset sticky — a healthy recalc must not clear it"
    );
}
