// src/clock.rs
//
// Clock abstraction for deterministic time injection.
//
// This is the foundation that makes the scenario harness actually work.
//
// THE CORE PROBLEM with the milestone doc's approach:
// Every time-dependent function in the system (now_ms() in posture_engine.rs,
// telemetry_watchdog.rs, recovery_hysteresis.rs) calls SystemTime::now()
// directly. The virtual clock on ScenarioRunner is just a field — nothing
// reads it. Tests using the doc's harness would compare streak timestamps
// written with real wall time against assertions evaluated at virtual time.
// The hysteresis window check would always pass or always fail depending on
// real elapsed time, not scenario time.
//
// THE FIX: A Clock trait with two implementations:
//   - SystemClock: calls SystemTime::now() — used in production
//   - VirtualClock: returns a controlled value — used in tests
//
// Functions that need the current time accept &dyn Clock (or Arc<dyn Clock>)
// instead of calling now_ms() internally. The scenario runner holds a
// VirtualClock and advances it by calling set_ms(). All time-dependent
// operations in the scenario use the injected clock.
//
// This pattern is standard in safety-critical embedded and systems software.
// It makes temporal behavior deterministic, repeatable, and independent of
// host CPU load, scheduler jitter, or CI machine speed.

use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Clock trait
// ---------------------------------------------------------------------------

/// Abstraction over the current time.
///
/// Implement this trait to provide a time source to any function that needs
/// the current timestamp. Production code uses `SystemClock`; tests use
/// `VirtualClock`.
///
/// All timestamps are milliseconds since the UNIX epoch (u64), consistent
/// with the rest of the Kirra codebase.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

// ---------------------------------------------------------------------------
// SystemClock — production implementation
// ---------------------------------------------------------------------------

/// Production clock. Delegates to `SystemTime::now()`.
#[derive(Debug, Clone, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
}

// ---------------------------------------------------------------------------
// VirtualClock — test implementation
// ---------------------------------------------------------------------------

/// Deterministic clock for test scenarios.
///
/// Time does not advance automatically. The test controls time explicitly
/// by calling `set_ms()` or `advance_ms()`. All reads via `now_ms()` return
/// the current virtual time.
///
/// `VirtualClock` is `Clone` and `Arc`-safe. Multiple holders of the same
/// `Arc<VirtualClock>` see the same time and can advance it.
///
/// # Usage in ScenarioRunner
/// The runner holds `Arc<VirtualClock>`. All time-dependent operations
/// (hysteresis evaluation, watchdog checks, cache staleness) receive the
/// same clock instance. Advancing the clock in the runner advances it for
/// all operations.
#[derive(Debug, Default)]
pub struct VirtualClock {
    current_ms: AtomicU64,
}

impl VirtualClock {
    /// Creates a new VirtualClock starting at t=0.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            current_ms: AtomicU64::new(0),
        })
    }

    /// Creates a new VirtualClock starting at the given timestamp.
    pub fn starting_at(ms: u64) -> Arc<Self> {
        Arc::new(Self {
            current_ms: AtomicU64::new(ms),
        })
    }

    /// Sets the virtual clock to an absolute timestamp.
    pub fn set_ms(&self, ms: u64) {
        self.current_ms.store(ms, Ordering::SeqCst);
    }

    /// Advances the virtual clock by a relative duration.
    pub fn advance_ms(&self, delta_ms: u64) {
        self.current_ms.fetch_add(delta_ms, Ordering::SeqCst);
    }
}

impl Clock for VirtualClock {
    fn now_ms(&self) -> u64 {
        self.current_ms.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------
// Convenience type alias
// ---------------------------------------------------------------------------

/// Shared, injectable clock used throughout the system.
pub type SharedClock = Arc<dyn Clock>;

/// Creates a production clock.
pub fn system_clock() -> SharedClock {
    Arc::new(SystemClock)
}

/// Creates a virtual clock for tests, starting at t=0.
pub fn virtual_clock() -> Arc<VirtualClock> {
    VirtualClock::new()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod clock_tests {
    use super::*;

    #[test]
    fn test_system_clock_returns_nonzero() {
        let clock = SystemClock;
        assert!(clock.now_ms() > 0, "system clock must return positive timestamp");
    }

    #[test]
    fn test_virtual_clock_starts_at_zero() {
        let clock = VirtualClock::new();
        assert_eq!(clock.now_ms(), 0);
    }

    #[test]
    fn test_virtual_clock_set_ms() {
        let clock = VirtualClock::new();
        clock.set_ms(5000);
        assert_eq!(clock.now_ms(), 5000);
    }

    #[test]
    fn test_virtual_clock_advance_ms() {
        let clock = VirtualClock::new();
        clock.set_ms(1000);
        clock.advance_ms(500);
        assert_eq!(clock.now_ms(), 1500);
    }

    #[test]
    fn test_virtual_clock_advance_is_additive() {
        let clock = VirtualClock::new();
        clock.advance_ms(100);
        clock.advance_ms(200);
        clock.advance_ms(300);
        assert_eq!(clock.now_ms(), 600);
    }

    #[test]
    fn test_virtual_clock_set_ms_overwrites_previous() {
        let clock = VirtualClock::new();
        clock.advance_ms(9999);
        clock.set_ms(100); // Hard set to 100 — discards the advance
        assert_eq!(clock.now_ms(), 100);
    }

    #[test]
    fn test_virtual_clock_shared_across_arc_clones() {
        let clock = VirtualClock::new();
        let clock2 = Arc::clone(&clock);

        clock.set_ms(42);
        assert_eq!(clock2.now_ms(), 42, "arc clone must see same time");

        clock2.advance_ms(8);
        assert_eq!(clock.now_ms(), 50, "advance via clone must be visible to original");
    }

    #[test]
    fn test_virtual_clock_starting_at() {
        let clock = VirtualClock::starting_at(10_000);
        assert_eq!(clock.now_ms(), 10_000);
    }

    #[test]
    fn test_virtual_clock_used_as_shared_clock_trait_object() {
        let clock: SharedClock = VirtualClock::starting_at(999);
        assert_eq!(clock.now_ms(), 999);
    }
}
