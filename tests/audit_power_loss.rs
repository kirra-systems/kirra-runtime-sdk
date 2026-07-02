// tests/audit_power_loss.rs
//
// WS-0.3 (#G10 audit axis) — incident-class audit rows survive an ungraceful
// process death, proven by a REAL kill: a child process commits a posture
// TRANSITION (which triggers the WS-0.3 `fsync_wal_durable` marker commit on
// the `synchronous=FULL` connection) and then `std::process::abort()`s —
// no destructors, no shutdown `durable_checkpoint`, no SIGTERM path. The
// parent reopens the SQLite file and must find the transition row, the
// durability marker, and an intact audit chain.
//
// HONESTY NOTE on what this can and cannot prove: a userspace test can kill a
// PROCESS, not the power. WAL-committed data survives a process kill even at
// `synchronous=NORMAL` (the OS page cache persists), so process death alone
// cannot distinguish NORMAL from FULL. The hard-power-loss half of the WS-0.3
// claim therefore rests on the asserted MECHANISM — the marker commit rides
// the FULL connection (`durable_connection_is_full_main_is_normal` pins the
// pragma; `fsync_wal_durable` rides `durable_ref()`), and a FULL commit
// fsyncs the shared WAL file, carrying every previously committed frame with
// it. This test pins the abort-path end-to-end behaviour AND the presence of
// the fsync marker at the moment of death.

use std::process::Command;

use kirra_verifier::posture_cache::SharedPostureCache;
use kirra_verifier::posture_engine::recalculate_and_broadcast;
use kirra_verifier::verifier::{AppState, VerifierOperationMode};
use kirra_verifier::verifier_store::VerifierStore;

const CHILD_ENV: &str = "KIRRA_AUDIT_POWER_LOSS_CHILD_DB";

fn empty_cache() -> SharedPostureCache {
    std::sync::Arc::new(std::sync::RwLock::new(None))
}

/// CHILD half: open the store at the given path, commit exactly one posture
/// TRANSITION (an empty Active fleet forces the M-9 None→LockedOut transition
/// on the first recalc — incident-class by construction), then die without
/// any graceful path.
fn child_write_incident_and_abort(db_path: &str) -> ! {
    let store = VerifierStore::new(db_path).expect("child: file store");
    let app = std::sync::Arc::new(AppState::new(store, VerifierOperationMode::Active));
    let cache = empty_cache();
    recalculate_and_broadcast(&app, &cache);
    assert!(
        cache.read().unwrap().is_some(),
        "child: recalc must have populated the cache before the kill"
    );
    // Ungraceful death: no Drop, no shutdown checkpoint, no WAL truncate.
    std::process::abort();
}

#[test]
fn incident_row_survives_ungraceful_process_death() {
    // ---- child mode (re-exec'd below) ------------------------------------
    if let Ok(db_path) = std::env::var(CHILD_ENV) {
        child_write_incident_and_abort(&db_path);
    }

    // ---- parent mode -------------------------------------------------------
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("audit_power_loss.sqlite");
    let db_str = db.to_str().expect("utf8 temp path").to_string();

    let exe = std::env::current_exe().expect("test binary path");
    let output = Command::new(&exe)
        .arg("incident_row_survives_ungraceful_process_death")
        .arg("--exact")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(CHILD_ENV, &db_str)
        .output()
        .expect("spawn child test process");

    assert!(
        !output.status.success(),
        "the child must die by abort, not exit cleanly (status: {:?}, stdout: {}, stderr: {})",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Reopen the store the way a restarted service would.
    let store = VerifierStore::new(&db_str).expect("parent: reopen after kill");

    // DoD: the incident row is present on reopen.
    let events = store
        .load_all_posture_events()
        .expect("parent: events readable after kill");
    assert!(
        events
            .iter()
            .any(|e| e["event_type"] == "SYSTEM_POSTURE_TRANSITION"),
        "the posture-transition row committed immediately before the kill must \
         be present on reopen; got events: {events:?}"
    );

    // The WS-0.3 durability marker was committed on the FULL connection at the
    // moment of the incident — its presence pins that the fsync path ran
    // before the death.
    let marker = store
        .load_engine_state("last_incident_durable_ms")
        .expect("parent: engine state readable");
    assert!(
        marker.is_some(),
        "the incident-durability fsync marker must exist — the WS-0.3 fsync \
         path did not run before the kill"
    );

    // The chain the incident row landed on is intact after the kill.
    assert!(
        store
            .verify_audit_chain_integrity()
            .expect("parent: chain verifiable"),
        "audit-chain hash linkage must verify after an ungraceful death"
    );
}
