use std::sync::Arc;
use std::time::Duration;

use kirra_core::NodeTrustState;
use kirra_verifier::posture_cache::SharedPostureCache;
use kirra_verifier::posture_engine::recalculate_and_broadcast;
use kirra_verifier::verifier::{AppState, RegisteredNode, VerifierOperationMode};
use kirra_verifier::verifier_store::VerifierStore;

fn mk_node(node_id: &str) -> RegisteredNode {
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
    let app = Arc::new(AppState::new(
        VerifierStore::new(":memory:").expect("in-memory store"),
        VerifierOperationMode::Active,
    ));
    let cache: SharedPostureCache = Arc::new(std::sync::RwLock::new(None));

    app.nodes.insert("root".into(), mk_node("root"));
    app.nodes.insert("dep_a".into(), mk_node("dep_a"));
    app.dependency_graph
        .insert("root".into(), vec!["dep_a".into()]);

    let writer_app = Arc::clone(&app);
    let writer = tokio::spawn(async move {
        for i in 0..250u32 {
            let temp = format!("temp-{i}");
            writer_app.nodes.insert(temp.clone(), mk_node(&temp));
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
