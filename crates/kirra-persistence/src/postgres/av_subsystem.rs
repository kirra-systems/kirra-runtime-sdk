//! `PgVerifierStore` — AvSubsystemStore seam (de-monolith split of lib.rs).
//!
//! Additional impl block(s); behaviour unchanged. Shared internals (`lock`,
//! `row_to_node`) are `pub(crate)` in the parent module.

use super::*;

impl PgVerifierStore {
    fn row_to_av_subsystem(row: &postgres::Row) -> AvSubsystemRecord {
        AvSubsystemRecord {
            node_id: row.get(0),
            subsystem_type: row.get(1),
            hardware_id: row.get(2),
            confidence_floor: row.get(3),
            // Fail-closed reads (negative → 0), as everywhere else in this backend.
            last_telemetry_ms: u64::try_from(row.get::<_, i64>(4)).unwrap_or(0),
            recovery_streak_count: u32::try_from(row.get::<_, i64>(5)).unwrap_or(0),
            recovery_streak_start_ms: u64::try_from(row.get::<_, i64>(6)).unwrap_or(0),
        }
    }
}

impl AvSubsystemStore for PgVerifierStore {
    type Error = PgStoreError;

    fn register_av_subsystem_meta(
        &self,
        node_id: &str,
        subsystem_type: &str,
        hardware_id: &str,
        confidence_floor: f64,
        initial_telemetry_ms: u64,
    ) -> Result<(), PgStoreError> {
        let tel_ms =
            i64::try_from(initial_telemetry_ms).map_err(|_| PgStoreError::OutOfDomain {
                field: "initial_telemetry_ms",
                value: initial_telemetry_ms,
            })?;
        // Faithful INSERT-OR-REPLACE: a re-register overwrites the row AND resets the
        // streak counters to 0 (SQLite's INSERT OR REPLACE drops the old row, so the
        // streak columns fall back to DEFAULT 0). The `DO UPDATE` therefore explicitly
        // zeroes the streak — they are not in the insert projection.
        self.lock().execute(
            "INSERT INTO av_subsystem_meta \
                 (node_id, subsystem_type, hardware_id, confidence_floor, last_telemetry_ms, \
                  recovery_streak_count, recovery_streak_start_ms) \
             VALUES ($1, $2, $3, $4, $5, 0, 0) \
             ON CONFLICT (node_id) DO UPDATE SET \
                 subsystem_type           = EXCLUDED.subsystem_type, \
                 hardware_id              = EXCLUDED.hardware_id, \
                 confidence_floor         = EXCLUDED.confidence_floor, \
                 last_telemetry_ms        = EXCLUDED.last_telemetry_ms, \
                 recovery_streak_count    = 0, \
                 recovery_streak_start_ms = 0",
            &[
                &node_id,
                &subsystem_type,
                &hardware_id,
                &confidence_floor,
                &tel_ms,
            ],
        )?;
        Ok(())
    }

    fn load_av_confidence_floor(&self, node_id: &str) -> Result<Option<f64>, PgStoreError> {
        let rows = self.lock().query(
            "SELECT confidence_floor FROM av_subsystem_meta WHERE node_id = $1",
            &[&node_id],
        )?;
        Ok(rows.first().map(|r| r.get::<_, f64>(0)))
    }

    fn touch_av_telemetry_timestamp(&self, node_id: &str, now_ms: u64) -> Result<(), PgStoreError> {
        let now = i64::try_from(now_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "now_ms",
            value: now_ms,
        })?;
        // No-op if the node is unregistered (0 rows updated), exactly as SQLite.
        self.lock().execute(
            "UPDATE av_subsystem_meta SET last_telemetry_ms = $1 WHERE node_id = $2",
            &[&now, &node_id],
        )?;
        Ok(())
    }

    fn get_last_telemetry_timestamp(&self, node_id: &str) -> Result<u64, PgStoreError> {
        let rows = self.lock().query(
            "SELECT last_telemetry_ms FROM av_subsystem_meta WHERE node_id = $1",
            &[&node_id],
        )?;
        // Absent → 0 (matches SQLite's QueryReturnedNoRows → 0); negative → 0.
        Ok(rows
            .first()
            .map(|r| u64::try_from(r.get::<_, i64>(0)).unwrap_or(0))
            .unwrap_or(0))
    }

    fn load_all_registered_av_node_ids(&self) -> Result<Vec<String>, PgStoreError> {
        // Ordered by node_id (deterministic; the contract sorts anyway).
        let rows = self.lock().query(
            "SELECT node_id FROM av_subsystem_meta ORDER BY node_id",
            &[],
        )?;
        Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
    }

    fn load_av_subsystems(&self) -> Result<Vec<AvSubsystemRecord>, PgStoreError> {
        let rows = self.lock().query(
            "SELECT node_id, subsystem_type, hardware_id, confidence_floor, last_telemetry_ms, \
                    recovery_streak_count, recovery_streak_start_ms \
             FROM av_subsystem_meta ORDER BY node_id",
            &[],
        )?;
        Ok(rows.iter().map(Self::row_to_av_subsystem).collect())
    }

    fn load_recovery_streak(&self, node_id: &str) -> Result<(u32, u64), PgStoreError> {
        let rows = self.lock().query(
            "SELECT recovery_streak_count, recovery_streak_start_ms \
             FROM av_subsystem_meta WHERE node_id = $1",
            &[&node_id],
        )?;
        // Absent → (0, 0); fail-closed reads (negative → 0).
        Ok(rows
            .first()
            .map(|r| {
                (
                    u32::try_from(r.get::<_, i64>(0)).unwrap_or(0),
                    u64::try_from(r.get::<_, i64>(1)).unwrap_or(0),
                )
            })
            .unwrap_or((0, 0)))
    }

    fn reset_recovery_streak(&self, node_id: &str, now_ms: u64) -> Result<(), PgStoreError> {
        let now = i64::try_from(now_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "now_ms",
            value: now_ms,
        })?;
        self.lock().execute(
            "UPDATE av_subsystem_meta \
                SET recovery_streak_count = 0, recovery_streak_start_ms = 0, \
                    last_telemetry_ms = $1 \
              WHERE node_id = $2",
            &[&now, &node_id],
        )?;
        Ok(())
    }

    fn reset_recovery_streak_preserving_telemetry(
        &self,
        node_id: &str,
    ) -> Result<(), PgStoreError> {
        self.lock().execute(
            "UPDATE av_subsystem_meta \
                SET recovery_streak_count = 0, recovery_streak_start_ms = 0 \
              WHERE node_id = $1",
            &[&node_id],
        )?;
        Ok(())
    }

    fn increment_recovery_streak(&self, node_id: &str, now_ms: u64) -> Result<u32, PgStoreError> {
        let now = i64::try_from(now_ms).map_err(|_| PgStoreError::OutOfDomain {
            field: "now_ms",
            value: now_ms,
        })?;
        // Single-statement UPDATE … RETURNING: `start_ms` stamps only on the 0→1 edge
        // (the CASE mirrors SQLite exactly), telemetry advances, and the new count is
        // returned. Zero rows returned = the node has no row → fail-closed
        // `AvNodeNotRegistered` (SQLite surfaces this as QueryReturnedNoRows, the
        // in-memory backend as NodeNotRegistered).
        let rows = self.lock().query(
            "UPDATE av_subsystem_meta \
                SET recovery_streak_count = recovery_streak_count + 1, \
                    recovery_streak_start_ms = CASE \
                        WHEN recovery_streak_count = 0 THEN $1 \
                        ELSE recovery_streak_start_ms END, \
                    last_telemetry_ms = $1 \
              WHERE node_id = $2 \
              RETURNING recovery_streak_count",
            &[&now, &node_id],
        )?;
        match rows.first() {
            Some(r) => Ok(u32::try_from(r.get::<_, i64>(0)).unwrap_or(0)),
            None => Err(PgStoreError::AvNodeNotRegistered(node_id.to_string())),
        }
    }
}
