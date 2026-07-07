//! WP-18 (MGA G-20) — versioned SQLite schema migration framework.
//!
//! Before this, schema evolution was `CREATE TABLE IF NOT EXISTS` + a scatter of
//! idempotent "tolerate duplicate column name" `ALTER TABLE … ADD COLUMN` blocks,
//! run unconditionally on every open — with NO version stamp, NO ordering, and
//! critically NO downgrade protection: an OLDER binary would happily open a DB an
//! newer binary had migrated, and could misread a column it doesn't understand.
//!
//! This module keys migrations off SQLite's `PRAGMA user_version` (a 32-bit int in
//! the DB header, `0` on every pre-framework database — so adopting it is purely
//! additive and non-conflicting):
//!
//! - **[`SCHEMA_VERSION`]** — the schema version this binary targets. `1` is the
//!   current baseline (the whole existing `CREATE`/`ALTER` DDL). Future changes bump
//!   this and register a [`Migration`] step.
//! - **[`assert_schema_not_future`]** — the FAIL-CLOSED gate, called BEFORE the DDL:
//!   a DB whose `user_version` exceeds [`SCHEMA_VERSION`] was written by a newer
//!   binary, so we refuse to open it rather than risk a destructive misread
//!   (VERSIONING_POLICY.md classifies a destructive schema change as MAJOR; the
//!   safety-asymmetry clause says refuse-closed on the ambiguous direction).
//! - **[`run_migrations`]** — called AFTER the baseline DDL: applies any registered
//!   [`MIGRATIONS`] whose version is newer than the DB's, each in order, and stamps
//!   `user_version` up to [`SCHEMA_VERSION`]. The baseline DDL is idempotent, so a
//!   pre-framework DB (`user_version 0`) is upgraded in place and stamped to `1`.
//!
//! The decision logic ([`decide_migration`]) is pure and unit-tested; the DB-touching
//! functions are thin wrappers over a `Connection`.

use rusqlite::Connection;

/// The schema version this binary targets. Version `1` is the current baseline
/// schema (every `CREATE TABLE`/`ADD COLUMN` in `VerifierStore::new` +
/// `init_audit_chain_schema`). BUMP this and push a [`Migration`] onto
/// [`MIGRATIONS`] for any future schema change — additive → MINOR, destructive →
/// MAJOR per `docs/VERSIONING_POLICY.md`.
pub const SCHEMA_VERSION: i64 = 1;

/// One registered schema migration to a specific target `version` (≥ 2 — version 1
/// is the unconditional idempotent baseline DDL, not a registered step). `apply`
/// performs the DDL/data change; the framework stamps `user_version` afterwards.
#[derive(Clone, Copy)]
pub struct Migration {
    pub version: i64,
    pub apply: fn(&Connection) -> rusqlite::Result<()>,
}

/// The ordered (ascending by `version`) registry of post-baseline migrations. Empty
/// today: version 1 is the baseline the existing DDL establishes. A future schema
/// change adds a `Migration { version: 2, apply: … }` here (and bumps
/// [`SCHEMA_VERSION`] to 2) — the framework then upgrades a v1 DB to v2 in order.
pub const MIGRATIONS: &[Migration] = &[];

/// What to do given a database's current schema version vs the binary's target.
/// Pure — the whole policy, unit-tested without a DB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationDecision {
    /// `db_version == target` — nothing to do.
    UpToDate,
    /// `db_version < target` — apply the steps in `(from, to]`.
    Migrate { from: i64, to: i64 },
    /// `db_version > target` — the DB is from a NEWER binary; refuse (fail-closed).
    RefuseFuture { db_version: i64, target: i64 },
}

/// The pure migration decision. `RefuseFuture` is the fail-closed direction: never
/// silently operate a database a newer binary has migrated beyond us.
#[must_use]
pub fn decide_migration(db_version: i64, target: i64) -> MigrationDecision {
    use std::cmp::Ordering;
    match db_version.cmp(&target) {
        Ordering::Equal => MigrationDecision::UpToDate,
        Ordering::Less => MigrationDecision::Migrate { from: db_version, to: target },
        Ordering::Greater => MigrationDecision::RefuseFuture { db_version, target },
    }
}

/// Read `PRAGMA user_version` (0 on a database never stamped by this framework).
pub fn read_user_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
}

/// Set `PRAGMA user_version`. `PRAGMA` cannot be parameterized, so the value is
/// formatted inline — it is a trusted i64 the framework controls, never user input.
fn set_user_version(conn: &Connection, v: i64) -> rusqlite::Result<()> {
    conn.execute_batch(&format!("PRAGMA user_version = {v};"))
}

/// Build a fail-closed migration error carrying an operator-readable reason.
fn migration_error(reason: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR), Some(reason))
}

/// The fail-closed "future schema" error (a DB migrated past this binary).
fn future_schema_error(db_version: i64, target: i64) -> rusqlite::Error {
    migration_error(format!(
        "database schema version {db_version} is newer than this binary supports \
         (max {target}) — refusing to open (fail-closed downgrade protection); run a \
         binary at schema version {db_version} or higher"
    ))
}

/// FAIL-CLOSED gate — call BEFORE running the schema DDL. Refuses (does not open) a
/// database whose `user_version` exceeds [`SCHEMA_VERSION`] (written by a newer
/// binary). Returns the DB's current schema version on success (`0` for a
/// pre-framework database).
pub fn assert_schema_not_future(conn: &Connection) -> rusqlite::Result<i64> {
    let db_version = read_user_version(conn)?;
    match decide_migration(db_version, SCHEMA_VERSION) {
        MigrationDecision::RefuseFuture { db_version, target } => {
            Err(future_schema_error(db_version, target))
        }
        _ => Ok(db_version),
    }
}

/// Apply the registered [`MIGRATIONS`] and stamp the DB up to [`SCHEMA_VERSION`].
/// Call AFTER the baseline DDL (which brings a fresh/pre-framework DB up to the v1
/// baseline idempotently). Returns the DB's schema version BEFORE this call ran.
pub fn run_migrations(conn: &Connection) -> rusqlite::Result<i64> {
    run_migrations_with(conn, MIGRATIONS, SCHEMA_VERSION)
}

/// The engine behind [`run_migrations`], with the step list + target injected so it
/// is unit-tested with synthetic migrations.
///
/// - **Ordering is ENFORCED, not just documented** (Copilot #863): the registry must
///   be strictly ascending by version and every step `> 1` (version 1 is the
///   unconditional baseline) — a misordered/duplicate/≤-baseline registry is a
///   fail-closed error, so it can never silently skip or double-apply a migration.
/// - **Each step is ATOMIC with its version stamp** (Copilot #863): the `apply` DDL
///   and the `user_version` bump commit in ONE transaction, so a crash between them
///   rolls back cleanly — the step is never left applied-but-unstamped (which would
///   re-run it on restart, corrupting a non-idempotent migration).
///
/// Fail-closed on a future DB; applies each pending step in `(start, target]` in
/// order; finally stamps to `target` so a fresh/pre-framework DB with no pending
/// step still lands on the baseline version (that stamp is its own transaction).
fn run_migrations_with(
    conn: &Connection,
    steps: &[Migration],
    target: i64,
) -> rusqlite::Result<i64> {
    // Enforce the registry contract before touching the DB.
    let mut prev = 1i64; // the baseline; the first registered step must exceed it
    for m in steps {
        if m.version <= prev {
            return Err(migration_error(format!(
                "migration registry is malformed: version {} is not strictly greater than \
                 the previous ({prev}) — steps must be ascending and above the v1 baseline",
                m.version
            )));
        }
        prev = m.version;
    }

    let start = read_user_version(conn)?;
    if start > target {
        return Err(future_schema_error(start, target));
    }
    let mut applied = start;
    for m in steps.iter().filter(|m| m.version > start && m.version <= target) {
        // Atomic: the step's DDL AND its version stamp commit together (or neither).
        // `unchecked_transaction` because we hold only `&Connection` at open time;
        // startup is single-threaded on this connection.
        let tx = conn.unchecked_transaction()?;
        (m.apply)(&tx)?;
        set_user_version(&tx, m.version)?;
        tx.commit()?;
        applied = m.version;
    }
    // Stamp the baseline version even when no step ran (fresh / pre-framework DB).
    if applied < target {
        set_user_version(conn, target)?;
    }
    Ok(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        Connection::open_in_memory().expect("in-memory conn")
    }

    #[test]
    fn decide_covers_the_three_directions() {
        assert_eq!(decide_migration(1, 1), MigrationDecision::UpToDate);
        assert_eq!(decide_migration(0, 1), MigrationDecision::Migrate { from: 0, to: 1 });
        assert_eq!(decide_migration(3, 5), MigrationDecision::Migrate { from: 3, to: 5 });
        assert_eq!(
            decide_migration(2, 1),
            MigrationDecision::RefuseFuture { db_version: 2, target: 1 }
        );
    }

    #[test]
    fn a_fresh_db_reads_version_zero_then_stamps_to_baseline() {
        let c = mem();
        assert_eq!(read_user_version(&c).unwrap(), 0, "an unstamped DB is version 0");
        let before = run_migrations(&c).unwrap();
        assert_eq!(before, 0);
        assert_eq!(read_user_version(&c).unwrap(), SCHEMA_VERSION, "stamped to the baseline");
        // Idempotent: re-running keeps it at the baseline.
        assert_eq!(run_migrations(&c).unwrap(), SCHEMA_VERSION);
        assert_eq!(read_user_version(&c).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn a_future_db_is_refused_fail_closed() {
        let c = mem();
        set_user_version(&c, SCHEMA_VERSION + 1).unwrap();
        assert!(assert_schema_not_future(&c).is_err(), "a newer-binary DB must be refused");
        assert!(run_migrations(&c).is_err(), "run_migrations also refuses a future DB");
        // The refusal did NOT downgrade the stamp.
        assert_eq!(read_user_version(&c).unwrap(), SCHEMA_VERSION + 1);
    }

    #[test]
    fn registered_steps_apply_in_order_and_advance_the_stamp() {
        // A synthetic v2 + v3 registry to exercise the engine (the real MIGRATIONS is
        // empty at the v1 baseline). Each step creates a marker table.
        fn mk_v2(c: &Connection) -> rusqlite::Result<()> {
            c.execute_batch("CREATE TABLE m2 (x INTEGER)")
        }
        fn mk_v3(c: &Connection) -> rusqlite::Result<()> {
            c.execute_batch("CREATE TABLE m3 (x INTEGER)")
        }
        let steps = [
            Migration { version: 2, apply: mk_v2 },
            Migration { version: 3, apply: mk_v3 },
        ];
        let c = mem();
        // Start at the v1 baseline; migrate up to v3.
        set_user_version(&c, 1).unwrap();
        let before = run_migrations_with(&c, &steps, 3).unwrap();
        assert_eq!(before, 1);
        assert_eq!(read_user_version(&c).unwrap(), 3, "stamped to the newest applied step");
        // Both marker tables exist → both steps ran.
        for t in ["m2", "m3"] {
            let n: i64 = c
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "step {t} must have created its table");
        }
        // Re-running is a no-op (already at target); an older step is never re-applied.
        assert_eq!(run_migrations_with(&c, &steps, 3).unwrap(), 3);
    }

    #[test]
    fn a_malformed_registry_is_refused_fail_closed() {
        fn noop(_c: &Connection) -> rusqlite::Result<()> {
            Ok(())
        }
        let c = mem();
        // Not strictly ascending.
        let descending = [Migration { version: 3, apply: noop }, Migration { version: 2, apply: noop }];
        assert!(run_migrations_with(&c, &descending, 5).is_err(), "descending registry refused");
        // Duplicate version.
        let dup = [Migration { version: 2, apply: noop }, Migration { version: 2, apply: noop }];
        assert!(run_migrations_with(&c, &dup, 5).is_err(), "duplicate version refused");
        // At/below the v1 baseline (versions ≤ 1 are the baseline, not steps).
        let at_baseline = [Migration { version: 1, apply: noop }];
        assert!(run_migrations_with(&c, &at_baseline, 5).is_err(), "a step at the baseline is refused");
    }

    #[test]
    fn a_failing_step_rolls_back_atomically_and_does_not_stamp() {
        // The step writes a table THEN fails — proving the DDL and the version stamp
        // commit together: on failure BOTH roll back, so a restart re-runs a clean
        // step (never applied-but-unstamped). Copilot #863 atomicity guard.
        fn write_then_fail(c: &Connection) -> rusqlite::Result<()> {
            c.execute_batch("CREATE TABLE m2 (x INTEGER)")?;
            Err(rusqlite::Error::ExecuteReturnedResults) // synthetic mid-step failure
        }
        let steps = [Migration { version: 2, apply: write_then_fail }];
        let c = mem();
        set_user_version(&c, 1).unwrap();
        assert!(run_migrations_with(&c, &steps, 2).is_err(), "the failing step propagates");
        assert_eq!(read_user_version(&c).unwrap(), 1, "version not advanced on a failed step");
        let tables: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='m2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0, "the failed step's DDL rolled back with the stamp (atomic)");
    }

    #[test]
    fn only_steps_newer_than_the_db_are_applied() {
        // DB already at v2 → the v2 step must NOT re-run; only v3 applies.
        fn boom_v2(_c: &Connection) -> rusqlite::Result<()> {
            panic!("the v2 step must not run when the DB is already at v2");
        }
        fn mk_v3(c: &Connection) -> rusqlite::Result<()> {
            c.execute_batch("CREATE TABLE m3 (x INTEGER)")
        }
        let steps = [
            Migration { version: 2, apply: boom_v2 },
            Migration { version: 3, apply: mk_v3 },
        ];
        let c = mem();
        set_user_version(&c, 2).unwrap();
        assert_eq!(run_migrations_with(&c, &steps, 3).unwrap(), 2);
        assert_eq!(read_user_version(&c).unwrap(), 3);
    }
}
