// tests/audit_row_durability_on_abort.rs (renamed from audit_power_loss.rs, EP-23)
//
// WS-0.3 (#G10 audit axis) — incident-class audit rows survive an ungraceful
// process death, proven by a REAL kill: a child process commits a posture
// TRANSITION (written DIRECTLY on the `synchronous=FULL` connection since #772
// F2, so the commit itself fsyncs the WAL) and then `std::process::abort()`s —
// no destructors, no shutdown `durable_checkpoint`, no SIGTERM path. The parent
// reopens the SQLite file and must find the transition row and an intact chain.
//
// HONESTY NOTE on what this can and cannot prove: a userspace test can kill a
// PROCESS, not the power. WAL-committed data survives a process kill even at
// `synchronous=NORMAL` (the OS page cache persists), so process death alone
// cannot distinguish NORMAL from FULL. The hard-power-loss half of the WS-0.3
// claim therefore rests on the MECHANISM — since #772 F2 the incident ROW is
// itself written on the FULL connection (`durable_connection_is_full_main_is_
// normal` pins the pragma; `save_posture_event_chained_with_generation_durable`
// rides `durable_conn`), so the row IS the fsync'd operation rather than a
// neighbouring marker's side effect. This test pins the abort-path end-to-end
// behaviour (the child truly died by SIGABRT, not a graceful unwind) and the
// survival of the incident row it committed a moment before death.

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

    // #772 F10: assert the child died by SIGABRT specifically, not merely
    // "non-success". A child that PANICS after the recalc (e.g. the cache assert)
    // also exits non-zero, but unwinding runs destructors — the exact graceful
    // path this test exists to EXCLUDE. Only a true `abort()` proves the row was
    // durable with no shutdown hook in between.
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        // SIGABRT == 6 on every common unix (Linux/macOS/BSD). Avoiding a `libc`
        // dev-dependency for a single well-known constant.
        const SIGABRT: i32 = 6;
        assert_eq!(
            output.status.signal(),
            Some(SIGABRT),
            "the child must die by SIGABRT (abort), not a panic-unwind or clean exit \
             (status: {:?}, stdout: {}, stderr: {})",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[cfg(not(unix))]
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

    // The chain the incident row landed on is intact after the kill.
    assert!(
        store
            .verify_audit_chain_integrity()
            .expect("parent: chain verifiable"),
        "audit-chain hash linkage must verify after an ungraceful death"
    );
}
