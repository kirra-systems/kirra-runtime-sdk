//! `kirra-ota-ctl` — the node-side app-level A/B governor launcher + install CLI
//! (WS-4 / Track 3, doer side). A thin IO shell over the pure transition planners
//! in `kirra_ota_installer` (`plan_run`/`plan_stage`/`plan_commit`/`plan_rollback`),
//! plus `verify_staged_artifact` and `FileBootController`. The safety logic is in
//! the library (unit-tested); this binary only reads env config, does filesystem
//! IO, and `exec`s the selected governor slot.
//!
//! Commands:
//! ```text
//! run                        systemd ExecStart: run the active (or one-shot trial)
//!                            slot's governor, exec-replacing this process
//! stage <artifact> <digest>  verify SHA-256, copy into the inactive slot, arm try_boot
//! commit                     make the in-progress trial slot the new active
//! rollback                   abandon any staged/trial state, stay on active
//! status                     print the boot record + which slot `run` would launch
//! ```
//!
//! Config (env, with defaults):
//! ```text
//! KIRRA_OTA_SLOT_A       (default /opt/kirra/slots/a)
//! KIRRA_OTA_SLOT_B       (default /opt/kirra/slots/b)
//! KIRRA_OTA_BOOT_RECORD  (default /var/lib/kirra/boot-record.json)
//! KIRRA_OTA_GOVERNOR_BIN (default kirra-governor)  — binary name within a slot dir
//! ```

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kirra_ota_installer::{
    plan_commit, plan_rollback, plan_run, plan_stage, verify_staged_artifact, FileBootController,
    Slot,
};

struct Cfg {
    slot_a: PathBuf,
    slot_b: PathBuf,
    record: PathBuf,
    governor_bin: String,
}

impl Cfg {
    fn from_env() -> Self {
        let env_or = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
        Cfg {
            slot_a: PathBuf::from(env_or("KIRRA_OTA_SLOT_A", "/opt/kirra/slots/a")),
            slot_b: PathBuf::from(env_or("KIRRA_OTA_SLOT_B", "/opt/kirra/slots/b")),
            record: PathBuf::from(env_or(
                "KIRRA_OTA_BOOT_RECORD",
                "/var/lib/kirra/boot-record.json",
            )),
            governor_bin: env_or("KIRRA_OTA_GOVERNOR_BIN", "kirra-governor"),
        }
    }
    fn slot_dir(&self, slot: Slot) -> &Path {
        match slot {
            Slot::A => &self.slot_a,
            Slot::B => &self.slot_b,
        }
    }
    fn governor_path(&self, slot: Slot) -> PathBuf {
        self.slot_dir(slot).join(&self.governor_bin)
    }
    fn controller(&self) -> std::io::Result<FileBootController> {
        // A brand-new node defaults to slot A active.
        if let Some(parent) = self.record.parent() {
            std::fs::create_dir_all(parent)?;
        }
        FileBootController::open(&self.record, Slot::A)
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let rest = &args.get(1..).unwrap_or(&[]);

    let result = match cmd {
        "run" => cmd_run(rest),
        "stage" => cmd_stage(rest),
        "commit" => cmd_commit(),
        "rollback" => cmd_rollback(),
        "status" => cmd_status(),
        "" | "-h" | "--help" | "help" => {
            print_usage();
            return ExitCode::from(if cmd.is_empty() { 2 } else { 0 });
        }
        other => Err(format!(
            "unknown command {other:?}; try `kirra-ota-ctl help`"
        )),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("kirra-ota-ctl: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `run` — the systemd `ExecStart`. Persists the one-shot transition FIRST, then
/// exec-replaces this process with the selected slot's governor (so systemd
/// supervises the governor PID directly). Never returns on success.
fn cmd_run(passthrough: &[String]) -> Result<(), String> {
    let cfg = Cfg::from_env();
    let mut ctrl = cfg
        .controller()
        .map_err(|e| format!("open boot record: {e}"))?;
    let record = ctrl
        .record()
        .map_err(|e| format!("read boot record: {e}"))?;

    let (slot, next) = plan_run(&record);
    // Persist the one-shot consume BEFORE handing off — a crash after exec must
    // see the already-consumed record so the next run auto-rolls-back.
    ctrl.write(&next)
        .map_err(|e| format!("persist boot record: {e}"))?;

    let bin = cfg.governor_path(slot);
    exec_governor(&bin, passthrough).map_err(|e| format!("exec {}: {e}", bin.display()))
}

/// `stage <artifact> <digest>` — verify the artifact's SHA-256 against the
/// campaign's signed digest, copy it into the INACTIVE slot, and arm `try_boot`.
fn cmd_stage(args: &[String]) -> Result<(), String> {
    let [artifact, digest] = args else {
        return Err("usage: kirra-ota-ctl stage <artifact-path> <sha256-hex>".into());
    };
    let cfg = Cfg::from_env();
    let mut ctrl = cfg
        .controller()
        .map_err(|e| format!("open boot record: {e}"))?;
    let record = ctrl
        .record()
        .map_err(|e| format!("read boot record: {e}"))?;
    let target = record.active.other();

    // FAIL-CLOSED: verify the source artifact BEFORE it is copied into a slot.
    verify_staged_artifact(Path::new(artifact), digest)
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

    ctrl.write(&plan_stage(&record))
        .map_err(|e| format!("persist boot record: {e}"))?;
    println!(
        "staged into slot {} ({}); `systemctl restart` to trial-boot it",
        target.as_str(),
        dest.display()
    );
    Ok(())
}

/// `commit` — make the in-progress trial slot the new active (health confirmed).
fn cmd_commit() -> Result<(), String> {
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
fn cmd_rollback() -> Result<(), String> {
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

/// `status` — print the record + which slot `run` would launch (no side effects).
fn cmd_status() -> Result<(), String> {
    let cfg = Cfg::from_env();
    let ctrl = cfg
        .controller()
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

#[cfg(unix)]
fn exec_governor(bin: &Path, passthrough: &[String]) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt as _;
    // `exec` replaces this process image; it only returns on error.
    Err(std::process::Command::new(bin).args(passthrough).exec())
}

#[cfg(not(unix))]
fn exec_governor(_bin: &Path, _passthrough: &[String]) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "`run` (exec) is a Unix-only launcher; on the Jetson target it is available",
    ))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn print_usage() {
    eprintln!(
        "kirra-ota-ctl — app-level A/B governor launcher + installer\n\
         \n\
         USAGE:\n\
         \x20 kirra-ota-ctl run                        run the active/trial slot (systemd ExecStart)\n\
         \x20 kirra-ota-ctl stage <artifact> <sha256>  verify + stage into the inactive slot\n\
         \x20 kirra-ota-ctl commit                     commit the trial slot as active\n\
         \x20 kirra-ota-ctl rollback                   abandon the staged/trial state\n\
         \x20 kirra-ota-ctl status                     show the boot record\n\
         \n\
         ENV: KIRRA_OTA_SLOT_A KIRRA_OTA_SLOT_B KIRRA_OTA_BOOT_RECORD KIRRA_OTA_GOVERNOR_BIN"
    );
}
