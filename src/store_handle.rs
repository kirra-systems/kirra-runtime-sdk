// src/store_handle.rs
//
// StoreHandle — the single access seam for the durable `VerifierStore`.
//
// DB-ACTOR MIGRATION, PHASE 1 (this commit): this type replaces the bare
// `Arc<Mutex<VerifierStore>>` that `AppState` and `FabricCausalLog` used to
// expose. Every store access now goes through one of two methods:
//
//   * `with(|s| ...)`  — synchronous; for background OS threads, tests, and
//                        sync helpers. Blocks the calling thread for the closure.
//   * `call(|s| ...).await` — async; runs the closure OFF the tokio worker pool
//                        (`spawn_blocking`) so the runtime stays responsive.
//
// In THIS phase the handle still wraps an `Arc<Mutex<VerifierStore>>`, so the
// behavior is materially identical to the prior direct-lock code. PHASE 2 swaps
// the internals for a dedicated-thread actor that OWNS the connection outright —
// removing the mutex and the lock-poison surface — WITHOUT touching any call
// site (they keep calling `with` / `call`).
//
// POISON HANDLING — the one intentional behavior delta in phase 1: `with` and
// `call` both RECOVER a poisoned lock via `into_inner` rather than panicking
// (`.lock().unwrap()`) or failing closed (`match .lock() { Err => ... }`).
// rusqlite data is not corrupted by a panicking lock holder (Rust mutex
// poisoning is conservative, and an aborted SQLite transaction rolls back on
// drop), so recovery keeps the safety governor evaluating instead of wedging on
// a one-off panic. This matches what `telemetry_watchdog` already did, and
// phase 2 removes lock poisoning entirely.

use std::sync::{Arc, Mutex};

use crate::verifier_store::VerifierStore;

/// A store access could not run to completion. Distinct from a DB-level error
/// (which the closure returns itself). Fail-closed: callers map this to a 500 /
/// safe-state, exactly like the prior inline `store.lock()` `Err` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    /// The async store task panicked or was cancelled. In phase 2 this also
    /// covers "the store actor thread is gone".
    TaskFailed,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            StoreError::TaskFailed => "store task failed",
        })
    }
}

impl std::error::Error for StoreError {}

/// The single access seam for the durable `VerifierStore`. Cheaply cloneable
/// (shares the underlying store); clone it into background tasks freely.
#[derive(Clone)]
pub struct StoreHandle {
    inner: Arc<Mutex<VerifierStore>>,
}

impl StoreHandle {
    /// Build a handle that owns `store`.
    pub fn new(store: VerifierStore) -> Self {
        Self {
            inner: Arc::new(Mutex::new(store)),
        }
    }

    /// Wrap an existing `Arc<Mutex<VerifierStore>>` (transitional helper for the
    /// few call sites that already hold one, e.g. `FabricCausalLog::new`). Phase 2
    /// removes this along with the mutex.
    pub fn from_arc(inner: Arc<Mutex<VerifierStore>>) -> Self {
        Self { inner }
    }

    /// Run `f` against the store from a SYNCHRONOUS context and return its value.
    /// Poison-tolerant. Use off the async runtime (background OS threads, tests,
    /// sync helpers). Calling this from an async task blocks the worker for the
    /// closure's duration — async callers should use [`StoreHandle::call`].
    pub fn with<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut VerifierStore) -> R,
    {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut guard)
    }

    /// Run `f` against the store OFF the async worker threads and await the
    /// result. The lock acquisition + SQLite work runs on a blocking thread, so a
    /// long write cannot pin a tokio worker. Fail-closed: a panicked / cancelled
    /// task surfaces as `Err(StoreError::TaskFailed)`.
    pub async fn call<F, R>(&self, f: F) -> Result<R, StoreError>
    where
        F: FnOnce(&mut VerifierStore) -> R + Send + 'static,
        R: Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        match tokio::task::spawn_blocking(move || {
            let mut guard = inner.lock().unwrap_or_else(|p| p.into_inner());
            f(&mut guard)
        })
        .await
        {
            Ok(r) => Ok(r),
            Err(_) => Err(StoreError::TaskFailed),
        }
    }
}

#[cfg(test)]
mod store_handle_tests {
    use super::*;

    fn temp_store() -> VerifierStore {
        // In-memory store keeps the test hermetic.
        VerifierStore::new(":memory:").expect("open in-memory store")
    }

    #[test]
    fn with_runs_closure_and_returns_value() {
        let handle = StoreHandle::new(temp_store());
        let n = handle.with(|s| s.load_nodes().map(|v| v.len()).unwrap_or(usize::MAX));
        assert_eq!(n, 0, "a fresh store has no nodes");
    }

    #[tokio::test]
    async fn call_offloads_and_returns_value() {
        let handle = StoreHandle::new(temp_store());
        let n = handle
            .call(|s| s.load_nodes().map(|v| v.len()).unwrap_or(usize::MAX))
            .await
            .expect("call must complete");
        assert_eq!(n, 0);
    }

    #[test]
    fn with_recovers_a_poisoned_lock() {
        let handle = StoreHandle::new(temp_store());
        // Poison the lock by panicking while holding it.
        let h2 = handle.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            h2.with(|_s| panic!("poison the lock"));
        }));
        // A subsequent access still works (recovered via into_inner).
        let n = handle.with(|s| s.load_nodes().map(|v| v.len()).unwrap_or(usize::MAX));
        assert_eq!(n, 0, "the handle recovers a poisoned lock instead of wedging");
    }
}
