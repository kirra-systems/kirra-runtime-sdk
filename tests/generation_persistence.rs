// tests/generation_persistence.rs
//
// WS-0.2 / #G10 — posture-generation persistence across restarts, proven as a
// RESTART SIMULATION over one file-backed SQLite store.
//
// The invariant (CLAUDE.md "Generation Persistence"): every generation emitted
// after a restart is strictly greater than every generation emitted before it.
// Federation peers and SSE consumers order posture reports by generation, so a
// reset-to-1 after restart is a time-reversal, not a cosmetic blip.
//
// Why a simulation and not two literal processes: `POSTURE_GENERATION` is a
// process-global atomic, so an in-process test cannot observe the pre-fix
// reset itself. What it CAN prove — and what this file pins — is the boot
// CONTRACT the binary now follows (init from the persisted high-water BEFORE
// the first recalc): given a store whose high-water is ahead of the live
// counter (exactly the state a fresh process finds after a predecessor died),
// the init+recalc sequence emits generations strictly above the high-water
// and re-persists the new maximum.

use std::sync::Arc;

use kirra_verifier::posture_cache::SharedPostureCache;
use kirra_verifier::posture_engine::{init_generation_from_store, recalculate_and_broadcast};
use kirra_verifier::verifier::{AppState, NodeTrustState, RegisteredNode, VerifierOperationMode};
use kirra_verifier::verifier_store::VerifierStore;

fn app_on(path: &std::path::Path) -> Arc<AppState> {
    let store = VerifierStore::new(path.to_str().expect("utf8 temp path")).expect("file store");
    let app = Arc::new(AppState::new(store, VerifierOperationMode::Active));
    let node = RegisteredNode {
        node_id: "gen-node".to_string(),
        status: NodeTrustState::Trusted,
        registered_at_ms: 1,
        last_trust_update_ms: 1,
        ak_public_pem: None,
        expected_pcr16_digest_hex: None,
        site: None,
        firmware_version: None,
    };
    app.fleet.nodes.insert(node.node_id.clone(), node);
    app
}

fn empty_cache() -> SharedPostureCache {
    Arc::new(std::sync::RwLock::new(None))
}

fn cache_generation(cache: &SharedPostureCache) -> u64 {
    cache
        .read()
        .unwrap()
        .as_ref()
        .expect("cache populated by recalc")
        .generation
}

/// The restart contract, end to end over one SQLite file:
///
/// "Process #1": recalc against a file-backed store → the emitted generation
/// is persisted as the high-water. Simulated death: everything but the file is
/// dropped, and the high-water is pushed far AHEAD of the live counter (the
/// state a fresh process finds — its counter is behind the store).
///
/// "Process #2": a NEW AppState on the SAME file runs the binary's boot order
/// (`init_generation_from_store`, then the first recalc). The emitted
/// generation must be strictly above the persisted high-water, and the store
/// must now hold the new maximum.
#[test]
fn generation_survives_restart_and_never_time_reverses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("gen_restart.sqlite");

    // ---- "process #1": live service, emits + persists generations --------
    let persisted_high_water = {
        let app1 = app_on(&db);
        let cache1 = empty_cache();
        recalculate_and_broadcast(&app1, &cache1);
        recalculate_and_broadcast(&app1, &cache1);
        let last_emitted = cache_generation(&cache1);

        let stored = app1
            .store
            .with(|s| s.load_last_generation())
            .expect("readable store");
        assert!(
            stored >= last_emitted,
            "recalc must persist its generation (stored {stored} < emitted {last_emitted})"
        );

        // Simulate the predecessor having run far ahead of THIS process's
        // live counter before dying — the exact restart state (a fresh
        // process's counter is behind the store's high-water). The offset
        // also makes the test robust to other tests bumping the shared
        // global counter concurrently.
        let ahead = kirra_verifier::posture_engine::POSTURE_GENERATION
            .load(std::sync::atomic::Ordering::SeqCst)
            + 50_000;
        assert!(
            app1.store
                .with(|s| s.save_last_generation(ahead))
                .expect("writable store"),
            "pushing the high-water ahead must be accepted"
        );
        ahead
    }; // app1 + cache1 dropped — only the SQLite file survives the "crash".

    // ---- "process #2": fresh state on the same file, binary's boot order --
    let app2 = app_on(&db);
    let loaded = init_generation_from_store(&app2).expect("readable store at boot");
    assert_eq!(
        loaded, persisted_high_water,
        "boot init must load the predecessor's high-water"
    );

    let cache2 = empty_cache();
    recalculate_and_broadcast(&app2, &cache2);
    let post_restart = cache_generation(&cache2);
    assert!(
        post_restart > persisted_high_water,
        "the first post-restart generation must be strictly above the persisted \
         high-water (got {post_restart}, high-water {persisted_high_water}) — \
         anything else is the pre-fix time-reversal"
    );

    // The store's high-water must have advanced to the new maximum (the #695
    // monotonic UPSERT accepted the strictly-greater post-restart write).
    let stored_after = app2
        .store
        .with(|s| s.load_last_generation())
        .expect("readable store");
    assert!(
        stored_after >= post_restart,
        "the post-restart generation must be re-persisted (stored {stored_after}, \
         emitted {post_restart})"
    );
}
