// src/posture_cache.rs — CachedFleetPosture definition
//
// v2.2.2 — Temporal hardening patch
//
// CORRECTIONS vs. milestone doc:
//
//   1. Field name is `generated_at_ms`, NOT `updated_at_ms`.
//      Every existing patch (policy_layer.rs, posture_engine.rs) reads
//      `cached.generated_at_ms`. Renaming it breaks all those call sites.
//      We keep the field name consistent with what the engine writes.
//
//   2. `ttl_ms` field is RETAINED. The milestone doc dropped it entirely.
//      policy_layer.rs reads `cached.ttl_ms` to evaluate staleness.
//      The TTL is owned by the entry (set by the engine), not hardcoded
//      in the middleware. Dropping it would re-centralize TTL knowledge
//      in the wrong layer.
//
//   3. `CachedFleetPosture::new()` signature kept compatible with existing
//      test infrastructure. A `new_with_generation(posture, generation, now_ms)`
//      constructor is added for engine use without breaking existing callers.
//
// This file is the single definition of CachedFleetPosture.
// SharedPostureCache = Arc<RwLock<Option<CachedFleetPosture>>> (unchanged).

use crate::verifier::FleetPosture;
use crate::posture_engine::POSTURE_CACHE_TTL_MS;

// ---------------------------------------------------------------------------
// CachedFleetPosture
// ---------------------------------------------------------------------------

/// A complete, immutable snapshot of the fleet posture at a point in time.
///
/// This struct is atomically replaced (never field-mutated) by
/// `recalculate_and_broadcast`. Readers always observe a consistent snapshot.
///
/// Field ownership:
///   - `posture`          — derived by `derive_fleet_posture` in posture_engine.rs
///   - `generated_at_ms`  — timestamp set by the engine at write time
///   - `ttl_ms`           — staleness window set by the engine (POSTURE_CACHE_TTL_MS)
///   - `generation`       — monotonic counter from `next_generation()` in posture_engine.rs
///
/// The middleware (`resolve_posture`) reads `generated_at_ms` and `ttl_ms`
/// to evaluate staleness. It does not own either value.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CachedFleetPosture {
    /// The current aggregated system-wide posture derived from the DAG.
    pub posture: FleetPosture,

    /// Absolute timestamp (ms since UNIX epoch) when this snapshot was computed.
    /// Named `generated_at_ms` to distinguish from any external "update" event.
    /// This is when the *engine* computed it, not when a sensor last reported.
    pub generated_at_ms: u64,

    /// Staleness TTL in milliseconds. After `generated_at_ms + ttl_ms < now`,
    /// `resolve_posture` treats this entry as stale and fails closed.
    /// Set by the engine from `POSTURE_CACHE_TTL_MS` — not by the middleware.
    pub ttl_ms: u64,

    /// Monotonically increasing generation counter. Strictly increasing within
    /// a process lifetime; persisted across restarts (see posture_engine_v2.rs).
    /// Useful for ordering guarantees, stale-cache debugging, and federation
    /// reconciliation.
    pub generation: u64,
}

impl CachedFleetPosture {
    /// Constructs a new cache entry with engine-assigned fields.
    ///
    /// Used by `recalculate_and_broadcast` — callers supply the generation
    /// (from `next_generation()`) and timestamp (from `now_ms()`).
    pub fn new_with_generation(posture: FleetPosture, generation: u64, now_ms: u64) -> Self {
        Self {
            posture,
            generated_at_ms: now_ms,
            ttl_ms: POSTURE_CACHE_TTL_MS,
            generation,
        }
    }

    /// Convenience constructor for tests and cold-start initialization.
    /// Uses generation=1 and the current system time.
    /// For production engine writes, use `new_with_generation` instead.
    pub fn new(posture: FleetPosture) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            posture,
            generated_at_ms: now,
            ttl_ms: POSTURE_CACHE_TTL_MS,
            generation: 1,
        }
    }

    /// Returns true if this entry has exceeded its TTL relative to `now_ms`.
    pub fn is_stale(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.generated_at_ms) >= self.ttl_ms
    }
}

/// The shared posture cache type.
///
/// `Arc<RwLock<Option<CachedFleetPosture>>>`:
///   - `Arc` — shared ownership across ServiceState, handlers, middleware
///   - `RwLock` — concurrent reads, exclusive writes
///   - `Option` — `None` = cold start / cache cleared (fail-closed in middleware)
///   - `CachedFleetPosture` — complete atomic snapshot (never partially updated)
pub type SharedPostureCache = std::sync::Arc<tokio::sync::RwLock<Option<CachedFleetPosture>>>;

#[cfg(test)]
mod posture_cache_tests {
    use super::*;
    use crate::verifier::FleetPosture;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    #[test]
    fn test_new_entry_is_not_stale() {
        let entry = CachedFleetPosture::new(FleetPosture::Nominal);
        assert!(!entry.is_stale(now_ms()), "brand-new entry must not be stale");
    }

    #[test]
    fn test_entry_beyond_ttl_is_stale() {
        let old_ts = now_ms().saturating_sub(POSTURE_CACHE_TTL_MS + 1);
        let entry = CachedFleetPosture {
            posture: FleetPosture::Nominal,
            generated_at_ms: old_ts,
            ttl_ms: POSTURE_CACHE_TTL_MS,
            generation: 1,
        };
        assert!(entry.is_stale(now_ms()), "entry older than TTL must be stale");
    }

    #[test]
    fn test_entry_exactly_at_ttl_boundary_is_stale() {
        // At exactly TTL age the entry is stale (>=, not >).
        let boundary_ts = now_ms().saturating_sub(POSTURE_CACHE_TTL_MS);
        let entry = CachedFleetPosture {
            posture: FleetPosture::Nominal,
            generated_at_ms: boundary_ts,
            ttl_ms: POSTURE_CACHE_TTL_MS,
            generation: 1,
        };
        assert!(entry.is_stale(now_ms()));
    }

    #[test]
    fn test_new_with_generation_sets_all_fields() {
        let ts = now_ms();
        let entry = CachedFleetPosture::new_with_generation(FleetPosture::Degraded, 42, ts);
        assert_eq!(entry.posture, FleetPosture::Degraded);
        assert_eq!(entry.generation, 42);
        assert_eq!(entry.generated_at_ms, ts);
        assert_eq!(entry.ttl_ms, POSTURE_CACHE_TTL_MS);
    }

    #[test]
    fn test_new_convenience_constructor_uses_generation_1() {
        let entry = CachedFleetPosture::new(FleetPosture::Nominal);
        assert_eq!(entry.generation, 1);
        assert_eq!(entry.ttl_ms, POSTURE_CACHE_TTL_MS);
    }

    #[test]
    fn test_cached_posture_is_serializable() {
        let entry = CachedFleetPosture::new(FleetPosture::Nominal);
        let json = serde_json::to_string(&entry).expect("must serialize");
        let rt: CachedFleetPosture = serde_json::from_str(&json).expect("must deserialize");
        assert_eq!(entry.posture, rt.posture);
        assert_eq!(entry.generation, rt.generation);
    }
}
