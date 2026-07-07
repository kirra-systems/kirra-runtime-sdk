//! WP-18 (MGA G-20) — the versioned SQLite schema migration framework, driven
//! through `VerifierStore::new` end-to-end: a fresh store stamps `user_version` to
//! the baseline, real data + the version survive a reopen, a PRE-FRAMEWORK
//! (`user_version 0`) database upgrades in place without losing data, and a FUTURE
//! (`user_version > SCHEMA_VERSION`) database is REFUSED fail-closed.
//!
//! The migration policy itself is unit-tested in `verifier_store::migrations`; this
//! drill proves `new()` actually wires the fail-closed gate + the stamp, on a real
//! on-disk WAL database with committed rows.

use kirra_verifier::verifier_store::migrations::SCHEMA_VERSION;
use kirra_verifier::verifier_store::VerifierStore;

const FP: &str = "aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44ee55ff66aa11bb22cc33dd44";

/// Set `PRAGMA user_version` on a store's DB FILE via a raw connection (the store
/// must be closed first). Simulates a database written by a different binary.
fn force_user_version(path: &str, version: i64) {
    let conn = rusqlite::Connection::open(path).expect("raw open");
    conn.execute_batch(&format!("PRAGMA user_version = {version};")).expect("set version");
}

#[test]
fn fresh_store_stamps_baseline_and_data_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kirra.sqlite");
    let path = path.to_str().unwrap();

    // 1. Fresh store → stamped at the baseline; write a committed row.
    {
        let mut s = VerifierStore::new(path).expect("open fresh");
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION, "fresh DB stamped to baseline");
        s.register_cert_principal("svc", FP, "integrator", None, 1_000).expect("write");
    }

    // 2. Reopen → version stable AND the committed row survived.
    {
        let s = VerifierStore::new(path).expect("reopen");
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        assert!(
            s.load_cert_principal_by_fingerprint(FP).unwrap().is_some(),
            "the committed row must survive a reopen"
        );
    }
}

#[test]
fn pre_framework_database_upgrades_in_place_without_data_loss() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kirra.sqlite");
    let path = path.to_str().unwrap();

    // A store with data, then force it back to the pre-framework state (version 0),
    // as an older binary that never used user_version would have left it.
    {
        let mut s = VerifierStore::new(path).expect("open");
        s.register_cert_principal("svc", FP, "auditor", None, 1_000).expect("write");
    }
    force_user_version(path, 0);

    // Reopen through the framework → upgraded in place (stamped to the baseline) and
    // the pre-existing data is intact (the baseline DDL is idempotent).
    let s = VerifierStore::new(path).expect("reopen upgrades");
    assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION, "version-0 DB upgraded to baseline");
    let rec = s.load_cert_principal_by_fingerprint(FP).unwrap().expect("data preserved");
    assert_eq!(rec.role, "auditor");
}

#[test]
fn future_schema_database_is_refused_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kirra.sqlite");
    let path = path.to_str().unwrap();

    // Create a valid store, then stamp it as if a NEWER binary migrated it.
    {
        let _ = VerifierStore::new(path).expect("open");
    }
    force_user_version(path, SCHEMA_VERSION + 1);

    // This binary must REFUSE to open it (fail-closed downgrade protection) rather
    // than risk misreading a schema it does not understand.
    let err = VerifierStore::new(path);
    assert!(err.is_err(), "a future-schema database must be refused, not opened");
    let msg = format!("{}", err.err().unwrap());
    assert!(
        msg.contains("newer than this binary supports"),
        "the refusal must name the downgrade-protection reason; got: {msg}"
    );
}
