// C2 (#1031) — per-node trust read-modify-write atomicity.
//
// A trust mutation used to be two unsynchronized steps: `mark_node_untrusted`
// read (`nodes.get`) → built an update → `persist_and_insert_node`, which itself
// wrote disk (`save_node`) then memory (`nodes.insert`) as SEPARATE ops. Two
// concurrent same-node mutators (a watchdog downgrade vs an `/attestation/verify`
// re-trust or a recovery re-trust) could interleave and either lose the
// downgrade (fail-open) OR invert to disk=Trusted / memory=Untrusted — so a
// restart hydrating from disk resurrected revoked trust the running system
// believed gone.
//
// The fix makes the whole read-modify-write ONE critical section under the
// per-key DashMap entry (shard) lock (`AppState::update_node_atomic`), with the
// disk write and the memory write both inside it. These tests pin the resulting
// invariant: disk and memory move together, always.

use std::sync::Arc;
use std::thread;

use kirra_verifier::verifier::{AppState, NodeTrustState, RegisteredNode, VerifierOperationMode};
use kirra_verifier::verifier_store::VerifierStore;

fn app() -> AppState {
    AppState::new(
        VerifierStore::new(":memory:").expect("in-memory store"),
        VerifierOperationMode::Active,
    )
}

fn seed_trusted(app: &AppState, id: &str) {
    app.persist_and_insert_node(RegisteredNode {
        node_id: id.to_string(),
        status: NodeTrustState::Trusted,
        registered_at_ms: 1,
        last_trust_update_ms: 1,
        ak_public_pem: None,
        expected_pcr16_digest_hex: None,
        site: None,
        firmware_version: None,
    })
    .expect("seed node");
}

/// The durable (on-disk) trust state, read straight from the store.
fn disk_status(app: &AppState, id: &str) -> Option<NodeTrustState> {
    app.store
        .with(|s| s.load_node(id))
        .expect("load_node")
        .map(|n| n.status)
}

/// An atomic update on a node that isn't registered is a fail-closed no-op:
/// `Ok(false)`, and it creates no row in memory OR on disk.
#[test]
fn update_node_atomic_is_a_noop_on_an_absent_node() {
    let app = app();
    let r = app.update_node_atomic("ghost", |n| n.clone());
    assert_eq!(r, Ok(false));
    assert!(app.nodes.get("ghost").is_none(), "no memory row created");
    assert!(disk_status(&app, "ghost").is_none(), "no disk row created");
}

/// A single downgrade lands on BOTH disk and memory — never one without the
/// other (the disk-before-memory write is inside the entry lock).
#[test]
fn a_downgrade_writes_disk_and_memory_together() {
    let app = app();
    seed_trusted(&app, "n1");

    assert_eq!(app.mark_node_untrusted("n1", "FAULT", 42), Ok(true));

    let mem = app.nodes.get("n1").expect("memory row").status.clone();
    assert!(
        matches!(mem, NodeTrustState::Untrusted(_)),
        "memory reflects the downgrade"
    );
    assert!(
        matches!(disk_status(&app, "n1"), Some(NodeTrustState::Untrusted(_))),
        "disk reflects the downgrade"
    );
}

/// Under many interleaved same-node downgrades and re-trusts, disk and memory
/// can never diverge: the per-key atomic RMW commits each write to BOTH stores
/// under one lock, so at quiescence the last write is reflected identically in
/// each — a restart hydrating from disk can never resurrect a superseded state.
#[test]
fn concurrent_downgrade_and_retrust_keep_disk_and_memory_consistent() {
    let app = Arc::new(app());
    seed_trusted(&app, "n1");

    let mut handles = Vec::new();
    for i in 0u64..8 {
        let app = Arc::clone(&app);
        handles.push(thread::spawn(move || {
            for j in 0u64..200 {
                if (i + j) % 2 == 0 {
                    let _ = app.mark_node_untrusted("n1", "FAULT", i * 1000 + j);
                } else {
                    let _ = app.update_node_atomic("n1", |n| RegisteredNode {
                        status: NodeTrustState::Trusted,
                        last_trust_update_ms: i * 1000 + j,
                        ..n.clone()
                    });
                }
            }
        }));
    }
    for h in handles {
        h.join().expect("thread");
    }

    let mem = app.nodes.get("n1").expect("memory row").status.clone();
    let disk = disk_status(&app, "n1").expect("disk row");
    assert_eq!(
        std::mem::discriminant(&mem),
        std::mem::discriminant(&disk),
        "disk and memory trust state must never diverge (mem={mem:?} disk={disk:?})"
    );
}
