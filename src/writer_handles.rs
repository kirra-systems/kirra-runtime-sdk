//! ADR-0035 Stage 3 (slice 3g) — the off-verdict-path async writer handles,
//! lifted off the `AppState` god-object into a cohesive field façade (the 3c/3e/3f
//! pattern), byte-identical.
//!
//! Unlike the earlier slices — which moved *safety-decision* state the posture
//! engine / supervisor / fence READS (escalation flags, drop counters, the HA
//! split-brain predicate) into `kirra-safety-authority` — these three are pure
//! off-verdict-path async I/O plumbing:
//!
//! - `audit_writer_tx` — the bounded mpsc Sender for the audit-writer task. The
//!   deny arm does `.get().try_send(job)`; `None` (writer not installed) falls
//!   back to the inline lock+save path. NEVER gates the verdict.
//! - `capture_writer_tx` — the learning-loop capture channel (default-OFF side
//!   channel); `None` → the gateway emit is a pure no-op.
//! - `capture_decision_seq` — the monotonic per-decision join-key sequence for
//!   capture. Non-safety (capture only).
//!
//! They are therefore grouped in a ROOT-crate leaf (not `kirra-safety-authority`,
//! whose deliberately std-only-plus-crypto character keeps it Kani/loom/MSRV-clean
//! — an async-runtime handle is the wrong kind of thing to put there). The move
//! adds NO dependency edge: `tokio`, `AuditWriteJob` and `CaptureRecord` already
//! live in the root tree. Embedded on `AppState` as `app.writers`; every field is
//! interior-mutable (`OnceLock` / `Arc<AtomicU64>`), so the move is pure
//! relocation — no `&mut self`, no ordering change. `AppState` keeps
//! `install_audit_writer` / `install_capture_writer` as thin delegators, so those
//! call-sites are unchanged.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, OnceLock};

use tokio::sync::mpsc::Sender;

use crate::audit_writer::AuditWriteJob;
use crate::capture::CaptureRecord;

/// The off-verdict-path async writer handles (ADR-0035 slice 3g). None of the
/// three ever gates the verdict path.
pub struct WriterHandles {
    /// Pass B2 (S3 / #115): bounded mpsc Sender for the audit-writer task.
    /// The deny arm of the actuator-safety-envelope middleware does
    /// `audit_writer_tx.get().try_send(job)` to push the kinematic-violation
    /// audit record off the verdict path. `None` (writer not installed)
    /// causes the deny arm to fall back to the previous inline lock+save
    /// path — production main always installs the writer at startup; tests
    /// that don't may still exercise the verdict path. Use
    /// `install_audit_writer` once to install.
    pub audit_writer_tx: OnceLock<Sender<AuditWriteJob>>,
    /// Learning-loop capture channel (Phase 1, #190) — sibling of
    /// `audit_writer_tx`. The actuator gateway `try_send`s a small
    /// `CaptureRecord` here off the verdict path. `None` (writer not installed,
    /// e.g. capture disabled or tests) → the gateway emit is a pure no-op.
    /// Installed once via `install_capture_writer` at startup, only when
    /// `capture::capture_enabled()`.
    pub capture_writer_tx: OnceLock<Sender<CaptureRecord>>,
    /// Monotonic per-decision sequence for the capture join key. Incremented at
    /// the gateway emit; non-safety (capture only).
    pub capture_decision_seq: Arc<AtomicU64>,
}

impl WriterHandles {
    /// Both writers uninstalled (`None`), the capture sequence at 0 — the exact
    /// defaults the prior `AppState::new` set inline (byte-identical initial state).
    pub fn new() -> Self {
        Self {
            audit_writer_tx: OnceLock::new(),
            capture_writer_tx: OnceLock::new(),
            capture_decision_seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Install the audit-writer mpsc Sender. Called once at startup, after
    /// `audit_writer::spawn_audit_writer`. Subsequent calls are ignored
    /// (OnceLock semantics) and logged as a duplicate-install warning.
    pub fn install_audit_writer(&self, tx: Sender<AuditWriteJob>) {
        if self.audit_writer_tx.set(tx).is_err() {
            tracing::warn!("audit writer Sender already installed — ignoring duplicate install");
        }
    }

    /// Install the capture-writer mpsc Sender (learning-loop Phase 1, #190).
    /// Called once at startup, after `capture::spawn_capture_writer`, and only
    /// when `capture::capture_enabled()`. Mirrors `install_audit_writer`.
    pub fn install_capture_writer(&self, tx: Sender<CaptureRecord>) {
        if self.capture_writer_tx.set(tx).is_err() {
            tracing::warn!("capture writer Sender already installed — ignoring duplicate install");
        }
    }
}

impl Default for WriterHandles {
    fn default() -> Self {
        Self::new()
    }
}
