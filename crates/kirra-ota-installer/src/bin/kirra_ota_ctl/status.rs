//! `kirra-ota-ctl` — slot status (de-monolith split of kirra_ota_ctl.rs).
//!
//! Behaviour unchanged. Shared plumbing (Cfg, http/*, now_ms, write_atomic,
//! exec_governor, …) stays in the bin root and is visible to this submodule.

use crate::*;

/// `status` — print the record + which slot `run` would launch. READ-ONLY: it does
/// NOT create the record or its directory (unlike the mutating commands), so
/// querying an uninitialized node has no side effects.
pub(crate) fn cmd_status() -> Result<(), String> {
    let cfg = Cfg::from_env();
    if !cfg.record.exists() {
        println!(
            "no boot record at {} (uninitialized; `stage`/`run` will create it, defaulting active=a)",
            cfg.record.display()
        );
        return Ok(());
    }
    // The file exists, so `open` reads it without writing a default.
    let ctrl = FileBootController::open(&cfg.record, Slot::A)
        .map_err(|e| format!("open boot record: {e}"))?;
    let record = ctrl
        .record()
        .map_err(|e| format!("read boot record: {e}"))?;
    let (would_run, _next) = plan_run(&record);
    println!(
        "active={} try_boot={:?} trying={:?} -> run would launch slot {} ({})",
        record.active.as_str(),
        record.try_boot.map(|s| s.as_str()),
        record.trying.map(|s| s.as_str()),
        would_run.as_str(),
        cfg.governor_path(would_run).display()
    );
    Ok(())
}
