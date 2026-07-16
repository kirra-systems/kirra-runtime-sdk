//! `kirra-ota-ctl` — run / stage / commit / rollback (A/B slot lifecycle) (de-monolith split of kirra_ota_ctl.rs).
//!
//! Behaviour unchanged. Shared plumbing (Cfg, http/*, now_ms, write_atomic,
//! exec_governor, …) stays in the bin root and is visible to this submodule.

use crate::*;

/// `run` — the systemd `ExecStart`. Persists the one-shot transition FIRST, then
/// exec-replaces this process with the selected slot's governor (so systemd
/// supervises the governor PID directly). Never returns on success.
pub(crate) fn cmd_run(passthrough: &[String]) -> Result<(), String> {
    let cfg = Cfg::from_env();
    let mut ctrl = cfg
        .controller()
        .map_err(|e| format!("open boot record: {e}"))?;
    let record = ctrl
        .record()
        .map_err(|e| format!("read boot record: {e}"))?;

    let (slot, next) = plan_run(&record);
    // Persist the one-shot consume BEFORE handing off — a crash after exec must
    // see the already-consumed record so the next run auto-rolls-back. Only WRITE
    // when the record actually changed: in steady state `plan_run` is a no-op, and
    // the write fsyncs the file + parent dir, so an unconditional write would wear
    // flash on every governor restart.
    if next != record {
        ctrl.write(&next)
            .map_err(|e| format!("persist boot record: {e}"))?;
    }

    let bin = cfg.governor_path(slot);
    exec_governor(&bin, passthrough).map_err(|e| format!("exec {}: {e}", bin.display()))
}

/// `stage <artifact> <digest>` — verify the artifact's SHA-256 against the
/// campaign's signed digest, copy it into the INACTIVE slot, and arm `try_boot`.
pub(crate) fn cmd_stage(args: &[String]) -> Result<(), String> {
    let [artifact, digest] = args else {
        return Err("usage: kirra-ota-ctl stage <artifact-path> <sha256-hex>".into());
    };
    let cfg = Cfg::from_env();
    let target = stage_verified(&cfg, Path::new(artifact), digest)?;
    println!(
        "staged into slot {} ({}); `systemctl restart` to trial-boot it",
        target.as_str(),
        cfg.governor_path(target).display()
    );
    Ok(())
}

/// Stage a verified artifact into the inactive slot and arm the one-shot `try_boot`.
/// FAIL-CLOSED: refuses if a stage/trial is already in flight; verifies the SHA-256
/// of BOTH the source and the landed copy against `digest`. Shared by `stage` and
/// `pull`. Returns the target slot.
pub(crate) fn stage_verified(cfg: &Cfg, artifact: &Path, digest: &str) -> Result<Slot, String> {
    let mut ctrl = cfg
        .controller()
        .map_err(|e| format!("open boot record: {e}"))?;
    let record = ctrl
        .record()
        .map_err(|e| format!("read boot record: {e}"))?;

    // FAIL-CLOSED: refuse (before doing any work) if a stage/trial is already in
    // flight — re-arming would break the one-shot rollback guarantee.
    let staged_record =
        plan_stage(&record).map_err(|e| format!("{e}; commit or rollback first"))?;
    let target = record.active.other();

    // Verify the SOURCE artifact BEFORE it is copied into a slot.
    verify_staged_artifact(artifact, digest)
        .map_err(|e| format!("artifact verification failed: {e}"))?;

    let dest = cfg.governor_path(target);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create slot dir: {e}"))?;
    }
    std::fs::copy(artifact, &dest).map_err(|e| format!("copy into slot: {e}"))?;
    set_executable(&dest).map_err(|e| format!("chmod +x: {e}"))?;
    // Re-verify the COPY landed intact (defense against a truncated/short write).
    verify_staged_artifact(&dest, digest)
        .map_err(|e| format!("staged copy verification failed: {e}"))?;

    ctrl.write(&staged_record)
        .map_err(|e| format!("persist boot record: {e}"))?;
    Ok(target)
}

/// `commit` — make the in-progress trial slot the new active (health confirmed).
pub(crate) fn cmd_commit() -> Result<(), String> {
    let cfg = Cfg::from_env();
    let mut ctrl = cfg
        .controller()
        .map_err(|e| format!("open boot record: {e}"))?;
    let record = ctrl
        .record()
        .map_err(|e| format!("read boot record: {e}"))?;
    let next = plan_commit(&record).map_err(|e| format!("{e}"))?;
    ctrl.write(&next)
        .map_err(|e| format!("persist boot record: {e}"))?;
    println!("committed: active slot is now {}", next.active.as_str());
    Ok(())
}

/// `rollback` — abandon any staged/trial state and stay on the current active slot.
pub(crate) fn cmd_rollback() -> Result<(), String> {
    let cfg = Cfg::from_env();
    let mut ctrl = cfg
        .controller()
        .map_err(|e| format!("open boot record: {e}"))?;
    let record = ctrl
        .record()
        .map_err(|e| format!("read boot record: {e}"))?;
    let next = plan_rollback(&record);
    ctrl.write(&next)
        .map_err(|e| format!("persist boot record: {e}"))?;
    println!("rolled back: active slot stays {}", next.active.as_str());
    Ok(())
}
