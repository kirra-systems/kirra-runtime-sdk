// src/store_handle.rs
//
// StoreHandle — the single access seam for the durable `VerifierStore`.
//
// WRITE PATH (phase 1): every mutating / read-then-write access goes through one
// of two methods against the single WRITER connection:
//
//   * `with(|s| ...)`  — synchronous; for background OS threads, tests, and
//                        sync helpers. Blocks the calling thread for the closure.
//   * `call(|s| ...).await` — async; runs the closure OFF the tokio worker pool
//                        (`spawn_blocking`) so the runtime stays responsive.
//
// READ PATH (phase 2 / review P3): read-only routes go through `with_read` /
// `call_read`, which dispatch to a POOL of independent READ-ONLY replica
// connections on the same WAL file — OUTSIDE the writer mutex. In WAL mode a
// read-only connection sees committed snapshots and neither blocks nor is
// blocked by the writer, so a read route (fleet posture, history, a full backup
// export, an audit-chain verify) no longer serializes behind a slow write. The
// read closures take `&VerifierStore` (read methods are all `&self`), so the
// type system forbids a write on the read path. For `":memory:"` stores (tests)
// or if the replicas fail to open, the read path FALLS BACK to the writer —
// correctness preserved, just without the concurrency benefit.
//
// POISON HANDLING: `with` / `call` / `with_read` / `call_read` all RECOVER a
// poisoned lock via `into_inner` rather than panicking or failing closed.
// rusqlite data is not corrupted by a panicking lock holder (Rust mutex
// poisoning is conservative, and an aborted SQLite transaction rolls back on
// drop), so recovery keeps the safety governor evaluating instead of wedging on
// a one-off panic. This matches what `telemetry_watchdog` already did.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::verifier_store::VerifierStore;

/// Number of independent read-only replica connections opened per file-backed
/// store. Bounds read-among-read concurrency (each is a separate WAL reader);
/// reads never serialize behind the writer regardless. Small: read routes are
/// light and a handful of extra read-only fds is cheap.
const READ_REPLICA_POOL_SIZE: usize = 4;

/// The read-only side of a `StoreHandle`: a pool of independent replica
/// connections, or a fallback to the writer when replicas are unavailable.
enum ReadReplica {
    /// File-backed: a round-robin pool of read-only connections, independent of
    /// the writer mutex.
    Pool {
        conns: Vec<Mutex<VerifierStore>>,
        next: AtomicUsize,
    },
    /// `":memory:"` (a 2nd open is a distinct empty db) or an open failure: reads
    /// fall back to the writer. Correct, just not concurrent.
    Fallback,
}

impl ReadReplica {
    /// Open a read-replica pool for `path`, or `Fallback` for in-memory / on any
    /// open failure (a degraded-concurrency, never-incorrect outcome).
    fn open(path: &str) -> Self {
        if path == ":memory:" {
            return ReadReplica::Fallback;
        }
        let mut conns = Vec::with_capacity(READ_REPLICA_POOL_SIZE);
        for _ in 0..READ_REPLICA_POOL_SIZE {
            match VerifierStore::open_read_replica(path) {
                Ok(replica) => conns.push(Mutex::new(replica)),
                // A partial pool is still useful; a fully empty one falls back.
                Err(_) => break,
            }
        }
        if conns.is_empty() {
            ReadReplica::Fallback
        } else {
            ReadReplica::Pool {
                conns,
                next: AtomicUsize::new(0),
            }
        }
    }

    /// Run a read-only closure against a pooled replica (round-robin), or the
    /// writer if no replica is available. `writer` is only touched on `Fallback`.
    fn dispatch<F, R>(&self, writer: &Mutex<VerifierStore>, f: F) -> R
    where
        F: FnOnce(&VerifierStore) -> R,
    {
        match self {
            ReadReplica::Pool { conns, next } => {
                let i = next.fetch_add(1, Ordering::Relaxed) % conns.len();
                let guard = conns[i].lock().unwrap_or_else(|p| p.into_inner());
                f(&guard)
            }
            ReadReplica::Fallback => {
                let guard = writer.lock().unwrap_or_else(|p| p.into_inner());
                f(&guard)
            }
        }
    }
}

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
/// (shares the underlying store + read pool); clone it into background tasks
/// freely.
#[derive(Clone)]
pub struct StoreHandle {
    /// The single writer connection — all mutations and read-then-write groups.
    writer: Arc<Mutex<VerifierStore>>,
    /// The read-only side: a pool of independent replica connections (or a
    /// fallback to the writer for in-memory stores).
    readers: Arc<ReadReplica>,
}

impl StoreHandle {
    /// Build a handle that owns `store` (the writer) and opens a read-replica
    /// pool against the same database file (or `Fallback` for `":memory:"`).
    pub fn new(store: VerifierStore) -> Self {
        let readers = Arc::new(ReadReplica::open(store.path()));
        Self {
            writer: Arc::new(Mutex::new(store)),
            readers,
        }
    }

    /// Wrap an existing `Arc<Mutex<VerifierStore>>` as the writer and open a
    /// read-replica pool against its database file. (Transitional helper for the
    /// few call sites that already hold an `Arc<Mutex<VerifierStore>>`, e.g.
    /// `FabricCausalLog::new`.)
    pub fn from_arc(writer: Arc<Mutex<VerifierStore>>) -> Self {
        let path = writer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .path()
            .to_string();
        let readers = Arc::new(ReadReplica::open(&path));
        Self { writer, readers }
    }

    /// Run `f` against the WRITER from a SYNCHRONOUS context and return its value.
    /// Poison-tolerant. Use off the async runtime (background OS threads, tests,
    /// sync helpers). Calling this from an async task blocks the worker for the
    /// closure's duration — async callers should use [`StoreHandle::call`].
    pub fn with<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut VerifierStore) -> R,
    {
        let mut guard = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut guard)
    }

    /// Run `f` against the WRITER OFF the async worker threads and await the
    /// result. The lock acquisition + SQLite work runs on a blocking thread, so a
    /// long write cannot pin a tokio worker. Fail-closed: a panicked / cancelled
    /// task surfaces as `Err(StoreError::TaskFailed)`.
    pub async fn call<F, R>(&self, f: F) -> Result<R, StoreError>
    where
        F: FnOnce(&mut VerifierStore) -> R + Send + 'static,
        R: Send + 'static,
    {
        let writer = Arc::clone(&self.writer);
        match tokio::task::spawn_blocking(move || {
            let mut guard = writer.lock().unwrap_or_else(|p| p.into_inner());
            f(&mut guard)
        })
        .await
        {
            Ok(r) => Ok(r),
            Err(_) => Err(StoreError::TaskFailed),
        }
    }

    /// Run a READ-ONLY closure `f` against a read-replica connection (or the
    /// writer on `Fallback`) from a SYNCHRONOUS context. `f` gets `&VerifierStore`
    /// (immutable), so only `&self` read methods are reachable — no write can ride
    /// the read path. Decouples reads from the writer mutex.
    pub fn with_read<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&VerifierStore) -> R,
    {
        self.readers.dispatch(&self.writer, f)
    }

    /// Async read-only access OFF the worker pool against a read-replica
    /// connection (or the writer on `Fallback`). The heavy-read win: a full
    /// backup export / audit-chain verify runs on a blocking thread against a
    /// replica, never pinning a worker NOR contending the writer mutex.
    pub async fn call_read<F, R>(&self, f: F) -> Result<R, StoreError>
    where
        F: FnOnce(&VerifierStore) -> R + Send + 'static,
        R: Send + 'static,
    {
        let readers = Arc::clone(&self.readers);
        let writer = Arc::clone(&self.writer);
        match tokio::task::spawn_blocking(move || readers.dispatch(&writer, f)).await {
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
        assert_eq!(
            n, 0,
            "the handle recovers a poisoned lock instead of wedging"
        );
    }

    #[test]
    fn memory_store_read_falls_back_to_writer() {
        // `:memory:` → ReadReplica::Fallback; with_read still reads the writer.
        let handle = StoreHandle::new(temp_store());
        let n = handle.with_read(|s| s.load_nodes().map(|v| v.len()).unwrap_or(usize::MAX));
        assert_eq!(
            n, 0,
            "in-memory read falls back to the writer and reads consistently"
        );
    }

    fn file_db_path(tag: &str) -> std::path::PathBuf {
        // Unique-per-test file under the OS temp dir; pid keeps parallel runs apart.
        let mut p = std::env::temp_dir();
        p.push(format!(
            "kirra_store_handle_{}_{}.sqlite",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn file_read_replica_sees_committed_writer_data() {
        use crate::verifier::RegisteredNode;
        let path = file_db_path("replica_visibility");
        let handle = StoreHandle::new(VerifierStore::new(path.to_str().unwrap()).expect("store"));

        // Pre-write: the replica pool sees zero nodes.
        assert_eq!(handle.with_read(|s| s.load_nodes().unwrap().len()), 0);

        // Commit a node through the WRITER.
        handle.with(|s| {
            s.save_node(&RegisteredNode {
                node_id: "node-A".to_string(),
                status: crate::verifier::NodeTrustState::Unknown,
                registered_at_ms: 1,
                last_trust_update_ms: 1,
                ak_public_pem: None,
                expected_pcr16_digest_hex: None,
                site: None,
                firmware_version: None,
            })
            .expect("save_node");
        });

        // A fresh read transaction on a replica connection must observe the
        // committed write (WAL read-committed visibility).
        let nodes = handle.with_read(|s| s.load_nodes().expect("load_nodes"));
        assert_eq!(
            nodes.len(),
            1,
            "read replica must see the committed writer node"
        );
        assert_eq!(nodes[0].node_id, "node-A");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn call_read_offloads_and_reads_replica() {
        let path = file_db_path("call_read");
        let handle = StoreHandle::new(VerifierStore::new(path.to_str().unwrap()).expect("store"));
        let n = handle
            .call_read(|s| s.load_nodes().map(|v| v.len()).unwrap_or(usize::MAX))
            .await
            .expect("call_read completes");
        assert_eq!(n, 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_read_path_is_independent_of_the_writer_lock() {
        // Deterministic proof (no timing/sleep) that a file-backed read does NOT
        // route through the writer mutex: hold the writer guard on THIS thread and
        // issue a read inside it. The std Mutex is non-reentrant, so if `with_read`
        // touched the writer lock this would DEADLOCK; because the file store has an
        // independent replica pool, the read proceeds on a separate connection.
        let path = file_db_path("isolation");
        let handle = StoreHandle::new(VerifierStore::new(path.to_str().unwrap()).expect("store"));
        handle.with(|_writer_guard_held| {
            let n = handle.with_read(|s| s.load_nodes().map(|v| v.len()).unwrap_or(usize::MAX));
            assert_eq!(
                n, 0,
                "a file-backed read must proceed on the replica while the writer lock is held"
            );
        });
        let _ = std::fs::remove_file(&path);
    }
}
