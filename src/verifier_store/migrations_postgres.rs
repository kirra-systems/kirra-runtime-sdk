//! WP-18 slice 3 (MGA G-20) — a **Postgres** [`SchemaBackend`] for the shared
//! migration engine.
//!
//! The engine ([`run_migrations_generic`]) is dialect-independent; this module
//! supplies the Postgres half of the seam: a `schema_version` table stands in for
//! SQLite's `PRAGMA user_version`, and each step's DDL + its version stamp commit
//! in one transaction. The **same** fail-closed guarantees hold — refuse a
//! future DB, enforce ascending registry ordering, never leave a step
//! applied-but-unstamped.
//!
//! **No driver dependency.** Rather than hard-wire `tokio-postgres` (a heavy,
//! runtime-coupled tree the on-device SQLite build must not carry), the backend
//! runs its SQL against an injected [`PgExecutor`] seam. An integrator binds that
//! to their concrete client in a ~10-line adapter — `execute` → `Client::batch_execute`,
//! `query_version` → a `SELECT`, `transaction` → `BEGIN`/`COMMIT`/`ROLLBACK`.
//! Because the version value the framework stamps is a trusted `i64` it controls
//! (never user input), it is formatted inline, so the seam needs no parameter
//! binding. The pure SQL + transaction semantics are exercised here against a
//! modelled executor; binding a real server is the recorded follow-up.

use super::migrations::{run_migrations_generic, SchemaBackend};

/// One Postgres migration step: its target `version` and the dialect-specific DDL.
#[derive(Clone, Copy, Debug)]
pub struct PgMigration {
    /// The version this step migrates the schema TO (≥ 2; v1 is the baseline).
    pub version: i64,
    /// The Postgres DDL/data change applied inside the step's transaction.
    pub sql: &'static str,
}

/// The minimal Postgres seam the backend drives. An integrator implements this
/// over their concrete client; the tests implement it over an in-memory model.
pub trait PgExecutor {
    /// The executor's error type.
    type Error;
    /// Run a statement that returns no rows.
    fn execute(&mut self, sql: &str) -> Result<(), Self::Error>;
    /// Read the single schema version (`0` when the row/table is absent).
    fn query_version(&mut self) -> Result<i64, Self::Error>;
    /// Run `f` inside a transaction: COMMIT on `Ok`, ROLLBACK on `Err`. This is
    /// what makes a step's DDL + its version stamp atomic.
    fn transaction<R>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<R, Self::Error>,
    ) -> Result<R, Self::Error>;
}

/// DDL for the single-row version table (idempotent — safe on every open).
pub const CREATE_VERSION_TABLE: &str =
    "CREATE TABLE IF NOT EXISTS kirra_schema_version (id INTEGER PRIMARY KEY, version BIGINT NOT NULL)";

/// The version upsert (single row, `id = 1`). `version` is a trusted framework
/// `i64`, so it is formatted inline — never user input, so no injection surface.
#[must_use]
pub fn upsert_version_sql(version: i64) -> String {
    format!(
        "INSERT INTO kirra_schema_version (id, version) VALUES (1, {version}) \
         ON CONFLICT (id) DO UPDATE SET version = {version}"
    )
}

/// A fail-closed migration error for the Postgres backend: an executor error, a
/// future DB, or a malformed registry. Mirrors the SQLite path's fail-closed
/// verdicts without pinning a concrete driver error type.
#[derive(Debug, PartialEq, Eq)]
pub enum PgMigrationError<E> {
    /// The underlying executor failed.
    Executor(E),
    /// The DB schema is NEWER than this binary supports — refuse (downgrade guard).
    FutureSchema { db_version: i64, target: i64 },
    /// The migration registry is malformed (not strictly ascending / ≤ baseline).
    MalformedRegistry(String),
}

impl<E: core::fmt::Display> core::fmt::Display for PgMigrationError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PgMigrationError::Executor(e) => write!(f, "postgres executor error: {e}"),
            PgMigrationError::FutureSchema { db_version, target } => write!(
                f,
                "database schema version {db_version} is newer than this binary supports \
                 (max {target}) — refusing to open (fail-closed downgrade protection)"
            ),
            PgMigrationError::MalformedRegistry(r) => write!(f, "{r}"),
        }
    }
}

/// The Postgres [`SchemaBackend`], wrapping a [`PgExecutor`].
pub struct PostgresBackend<E: PgExecutor> {
    exec: E,
}

impl<E: PgExecutor> PostgresBackend<E> {
    /// Wrap an executor as a schema backend.
    pub fn new(exec: E) -> Self {
        Self { exec }
    }

    /// Recover the executor (e.g. to keep using the connection after migrating).
    pub fn into_executor(self) -> E {
        self.exec
    }

    /// Drive the shared engine against this backend and `steps`, targeting `target`.
    pub fn migrate(
        &mut self,
        steps: &[PgMigration],
        target: i64,
    ) -> Result<i64, PgMigrationError<E::Error>> {
        run_migrations_generic(self, steps, target)
    }
}

impl<E: PgExecutor> SchemaBackend for PostgresBackend<E> {
    type Error = PgMigrationError<E::Error>;
    type Step = PgMigration;

    fn step_version(step: &PgMigration) -> i64 {
        step.version
    }

    fn read_version(&mut self) -> Result<i64, Self::Error> {
        // Idempotently ensure the version table exists, then read the single row.
        self.exec.execute(CREATE_VERSION_TABLE).map_err(PgMigrationError::Executor)?;
        self.exec.query_version().map_err(PgMigrationError::Executor)
    }

    fn apply_and_stamp(&mut self, step: &PgMigration) -> Result<(), Self::Error> {
        let sql = step.sql;
        let stamp = upsert_version_sql(step.version);
        // The step's DDL and its version stamp commit together (or neither).
        self.exec
            .transaction(|e| {
                e.execute(sql)?;
                e.execute(&stamp)?;
                Ok(())
            })
            .map_err(PgMigrationError::Executor)
    }

    fn stamp(&mut self, version: i64) -> Result<(), Self::Error> {
        self.exec.execute(&upsert_version_sql(version)).map_err(PgMigrationError::Executor)
    }

    fn future_error(db_version: i64, target: i64) -> Self::Error {
        PgMigrationError::FutureSchema { db_version, target }
    }

    fn malformed_error(reason: String) -> Self::Error {
        PgMigrationError::MalformedRegistry(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// An in-memory model of a Postgres store: the version row + the set of DDL
    /// objects that have been created. `transaction` snapshots and restores on
    /// error, modelling COMMIT/ROLLBACK so the atomicity contract is testable
    /// without a server. `fail_on` makes a matching `execute` fail (to drive the
    /// rollback path).
    #[derive(Clone, Default)]
    struct MockPg {
        table_exists: bool,
        version: Option<i64>,
        objects: BTreeSet<String>,
        fail_on: Option<&'static str>,
    }

    impl PgExecutor for MockPg {
        type Error = String;

        fn execute(&mut self, sql: &str) -> Result<(), String> {
            if let Some(marker) = self.fail_on {
                if sql.contains(marker) {
                    return Err(format!("mock executor failed on: {sql}"));
                }
            }
            if sql.contains("CREATE TABLE IF NOT EXISTS kirra_schema_version") {
                self.table_exists = true;
            } else if let Some(rest) = sql.strip_prefix("INSERT INTO kirra_schema_version (id, version) VALUES (1, ") {
                // Parse the inline version out of "…VALUES (1, <n>) ON CONFLICT…".
                let n: i64 = rest.split(')').next().unwrap().trim().parse().unwrap();
                self.version = Some(n);
            } else {
                // A schema DDL step — record its identity for assertions.
                self.objects.insert(sql.to_string());
            }
            Ok(())
        }

        fn query_version(&mut self) -> Result<i64, String> {
            assert!(self.table_exists, "read_version must ensure the table first");
            Ok(self.version.unwrap_or(0))
        }

        fn transaction<R>(
            &mut self,
            f: impl FnOnce(&mut Self) -> Result<R, String>,
        ) -> Result<R, String> {
            let snapshot = self.clone();
            match f(self) {
                Ok(r) => Ok(r),
                Err(e) => {
                    *self = snapshot; // ROLLBACK
                    Err(e)
                }
            }
        }
    }

    fn step(version: i64, sql: &'static str) -> PgMigration {
        PgMigration { version, sql }
    }

    #[test]
    fn a_fresh_store_reads_zero_then_stamps_to_baseline() {
        let mut b = PostgresBackend::new(MockPg::default());
        let before = b.migrate(&[], 1).unwrap();
        assert_eq!(before, 0, "an unstamped store is version 0");
        assert_eq!(b.exec.version, Some(1), "stamped to the baseline");
        // Idempotent.
        assert_eq!(b.migrate(&[], 1).unwrap(), 1);
        assert_eq!(b.exec.version, Some(1));
    }

    #[test]
    fn a_future_store_is_refused_fail_closed() {
        let mut b = PostgresBackend::new(MockPg { table_exists: true, version: Some(5), ..Default::default() });
        let err = b.migrate(&[], 1).unwrap_err();
        assert_eq!(err, PgMigrationError::FutureSchema { db_version: 5, target: 1 });
        assert_eq!(b.exec.version, Some(5), "the refusal did not downgrade the stamp");
    }

    #[test]
    fn registered_steps_apply_in_order_and_advance_the_stamp() {
        let steps = [
            step(2, "CREATE TABLE m2 (x INTEGER)"),
            step(3, "CREATE TABLE m3 (x INTEGER)"),
        ];
        let mut b = PostgresBackend::new(MockPg { table_exists: true, version: Some(1), ..Default::default() });
        let before = b.migrate(&steps, 3).unwrap();
        assert_eq!(before, 1);
        assert_eq!(b.exec.version, Some(3), "stamped to the newest applied step");
        assert!(b.exec.objects.contains("CREATE TABLE m2 (x INTEGER)"));
        assert!(b.exec.objects.contains("CREATE TABLE m3 (x INTEGER)"));
        // Re-running is a no-op.
        assert_eq!(b.migrate(&steps, 3).unwrap(), 3);
    }

    #[test]
    fn a_failing_step_rolls_back_atomically_and_does_not_stamp() {
        // The step's DDL "succeeds" but the version stamp inside the same tx fails
        // (fail_on matches the upsert) → the whole tx rolls back: neither the DDL
        // object nor the version advance survive.
        let steps = [step(2, "CREATE TABLE m2 (x INTEGER)")];
        let mut b = PostgresBackend::new(MockPg {
            table_exists: true,
            version: Some(1),
            fail_on: Some("INSERT INTO kirra_schema_version"),
            ..Default::default()
        });
        assert!(b.migrate(&steps, 2).is_err(), "the failing step propagates");
        assert_eq!(b.exec.version, Some(1), "version not advanced on a failed step");
        assert!(!b.exec.objects.contains("CREATE TABLE m2 (x INTEGER)"), "DDL rolled back with the stamp");
    }

    #[test]
    fn a_malformed_registry_is_refused_fail_closed() {
        let mut b = PostgresBackend::new(MockPg { table_exists: true, version: Some(1), ..Default::default() });
        let descending = [step(3, "CREATE TABLE a (x INTEGER)"), step(2, "CREATE TABLE b (x INTEGER)")];
        let err = b.migrate(&descending, 5).unwrap_err();
        assert!(matches!(err, PgMigrationError::MalformedRegistry(_)), "descending registry refused: {err:?}");
        // Nothing was applied.
        assert!(b.exec.objects.is_empty());
    }

    #[test]
    fn only_steps_newer_than_the_store_are_applied() {
        // Store already at v2 → the v2 step must NOT re-run; only v3 applies.
        let steps = [
            step(2, "SELECT will_not_run"),
            step(3, "CREATE TABLE m3 (x INTEGER)"),
        ];
        let mut b = PostgresBackend::new(MockPg { table_exists: true, version: Some(2), ..Default::default() });
        assert_eq!(b.migrate(&steps, 3).unwrap(), 2);
        assert_eq!(b.exec.version, Some(3));
        assert!(!b.exec.objects.contains("SELECT will_not_run"), "the already-applied v2 step did not re-run");
        assert!(b.exec.objects.contains("CREATE TABLE m3 (x INTEGER)"));
    }

    #[test]
    fn upsert_sql_carries_the_version_both_places() {
        // The mock parses the VALUES clause; guard the format the parser relies on.
        let sql = upsert_version_sql(7);
        assert!(sql.contains("VALUES (1, 7)"));
        assert!(sql.contains("DO UPDATE SET version = 7"));
    }
}
