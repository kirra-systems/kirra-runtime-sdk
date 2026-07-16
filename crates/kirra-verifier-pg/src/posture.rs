//! `PgVerifierStore` — PostureEngineStateStore seam (de-monolith split of lib.rs).
//!
//! Additional impl block(s); behaviour unchanged. Shared internals (`lock`,
//! `row_to_node`) are `pub(crate)` in the parent module.

use super::*;

impl PostureEngineStateStore for PgVerifierStore {
    type Error = PgStoreError;

    fn load_last_generation(&self) -> Result<u64, PgStoreError> {
        let row = self.lock().query_opt(
            "SELECT value FROM posture_engine_state WHERE key = 'last_generation'",
            &[],
        )?;
        match row {
            // Genuinely absent row → 0 (fresh store), like SQLite's
            // `QueryReturnedNoRows => Ok(0)`.
            None => Ok(0),
            // Present but non-numeric OR out-of-domain (`>= i64::MAX`) → CORRUPTION,
            // fail closed. Same verdict as `VerifierStore::load_last_generation`.
            Some(r) => {
                let s: String = r.get(0);
                let parsed = s
                    .parse::<u64>()
                    .map_err(|_| PgStoreError::CorruptGeneration(s.clone()))?;
                if parsed >= i64::MAX as u64 {
                    return Err(PgStoreError::CorruptGeneration(s));
                }
                Ok(parsed)
            }
        }
    }

    fn save_last_generation(&self, generation: u64) -> Result<bool, PgStoreError> {
        // Single-statement conditional upsert — ATOMIC, so it is race-safe across
        // connections exactly like the SQLite backend's monotonic upsert: there is no
        // read-then-write window in which two racers could both read the old value and
        // a lower generation clobber a higher one. The `WHERE` gates only the DO UPDATE
        // (conflict) path; a first insert is unconditional. `rows == 1` on insert or an
        // accepted strict advance, `0` when the guard rejects a stale/equal generation.
        //
        // The `CASE` guards the cast of the STORED value: a non-numeric existing value
        // counts as 0, so a positive generation OVERWRITES it — the exact
        // heal-on-corrupt-save parity SQLite gives via `CAST('garbage') = 0` (and the
        // in-memory backend via `parse().unwrap_or(0)`). Only `load_last_generation`
        // fails closed on a corrupt high-water; `save` heals, matching every backend.
        //
        // The comparison is `::numeric` (arbitrary precision), NOT `::bigint`: a stored
        // value that is all-digits but out of the i64/u64 domain (a huge digit string
        // written via a blind `save_engine_state`) would OVERFLOW a `::bigint` cast and
        // re-raise the very crash this heals. `::numeric` never overflows — such a value
        // simply compares greater than any real generation, so the monotonic guard keeps
        // it (never a crash). `EXCLUDED.value` is `$1` (a valid u64 string), likewise
        // overflow-free as `numeric`.
        let n = self.lock().execute(
            "INSERT INTO posture_engine_state (key, value) VALUES ('last_generation', $1) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value \
             WHERE (CASE WHEN posture_engine_state.value ~ '^[0-9]+$' \
                         THEN posture_engine_state.value::numeric ELSE 0 END) \
                   < EXCLUDED.value::numeric",
            &[&generation.to_string()],
        )?;
        Ok(n == 1)
    }

    fn load_engine_state(&self, key: &str) -> Result<Option<String>, PgStoreError> {
        let row = self.lock().query_opt(
            "SELECT value FROM posture_engine_state WHERE key = $1",
            &[&key],
        )?;
        Ok(row.map(|r| r.get::<_, String>(0)))
    }

    fn save_engine_state(&self, key: &str, value: &str) -> Result<(), PgStoreError> {
        // Blind upsert (INSERT-OR-REPLACE) — NOT the monotonic guard.
        self.lock().execute(
            "INSERT INTO posture_engine_state (key, value) VALUES ($1, $2) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            &[&key, &value],
        )?;
        Ok(())
    }
}
