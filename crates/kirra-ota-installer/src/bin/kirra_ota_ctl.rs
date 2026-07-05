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
//! probe --cmd '<health>'     after a trial boot, auto-commit-or-rollback on health
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
use std::time::{Duration, Instant};

use kirra_ota_installer::{
    plan_commit, plan_rollback, plan_run, plan_stage, verify_staged_artifact, FileBootController,
    HealthGate, Slot,
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
        "probe" => cmd_probe(rest),
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

    // FAIL-CLOSED: refuse (before doing any work) if a stage/trial is already in
    // flight — re-arming would break the one-shot rollback guarantee.
    let staged_record =
        plan_stage(&record).map_err(|e| format!("{e}; commit or rollback first"))?;
    let target = record.active.other();

    // Verify the SOURCE artifact BEFORE it is copied into a slot.
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

    ctrl.write(&staged_record)
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

/// Options for `probe` (the automatic health gate).
struct ProbeOpts {
    /// Health command; exit status 0 = healthy. Run via `sh -c`.
    cmd: String,
    /// Max seconds to keep probing before giving up and rolling back.
    window_secs: u64,
    /// Seconds between samples (>= 1).
    interval_secs: u64,
    /// Consecutive healthy samples required to commit.
    successes: u32,
    /// systemd unit to restart on rollback (to switch the running process back to
    /// the good slot). Defaults to `KIRRA_OTA_UNIT` or `kirra-governor`.
    unit: String,
    /// Whether to `systemctl restart` on rollback (default true).
    restart: bool,
}

impl ProbeOpts {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut cmd: Option<String> = None;
        let mut window_secs = 30u64;
        let mut interval_secs = 2u64;
        let mut successes = 3u32;
        let mut unit =
            std::env::var("KIRRA_OTA_UNIT").unwrap_or_else(|_| "kirra-governor".to_string());
        let mut restart = true;

        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut next = |flag: &str| -> Result<String, String> {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{flag} needs a value"))
            };
            match a.as_str() {
                "--cmd" => cmd = Some(next("--cmd")?),
                "--window-secs" => {
                    window_secs = next("--window-secs")?
                        .parse()
                        .map_err(|_| "--window-secs must be a non-negative integer".to_string())?
                }
                "--interval-secs" => {
                    interval_secs = next("--interval-secs")?
                        .parse()
                        .map_err(|_| "--interval-secs must be a positive integer".to_string())?
                }
                "--successes" => {
                    successes = next("--successes")?
                        .parse()
                        .map_err(|_| "--successes must be a positive integer".to_string())?
                }
                "--unit" => unit = next("--unit")?,
                "--no-restart" => restart = false,
                other => return Err(format!("unknown probe flag {other:?}")),
            }
        }
        if interval_secs == 0 {
            return Err("--interval-secs must be >= 1".into());
        }
        let cmd = cmd
            .ok_or_else(|| "probe requires --cmd '<health command>' (exit 0 = healthy)".to_string())?;
        Ok(ProbeOpts {
            cmd,
            window_secs,
            interval_secs,
            successes,
            unit,
            restart,
        })
    }
}

/// `probe --cmd '<health command>'` — the automatic health gate. After a trial boot
/// (`trying` set), sample the health command until it passes `--successes` times in a
/// row (→ `commit`) or the `--window-secs` window expires without such a streak (→
/// `rollback` + restart the unit, switching the running process back to the good
/// slot). READ-ONLY / no-op when NOT in a trial, so it is safe to run on every start
/// (e.g. an `After=kirra-governor` oneshot). Fail-closed: a health command that can't
/// even launch counts as unhealthy, and a probe that reaches the window without a
/// healthy streak always rolls back — an ambiguous trial never commits.
fn cmd_probe(args: &[String]) -> Result<(), String> {
    let opts = ProbeOpts::parse(args)?;
    let cfg = Cfg::from_env();

    // Do NOT create the record or its dir: an uninitialized node is trivially "not in
    // a trial". Only a real trial gets probed.
    if !cfg.record.exists() {
        println!(
            "no boot record at {}; not in a trial, nothing to probe",
            cfg.record.display()
        );
        return Ok(());
    }
    let mut ctrl =
        FileBootController::open(&cfg.record, Slot::A).map_err(|e| format!("open boot record: {e}"))?;
    let record = ctrl
        .record()
        .map_err(|e| format!("read boot record: {e}"))?;
    let Some(trial) = record.trying else {
        println!("not in a trial (trying=None); nothing to probe");
        return Ok(());
    };

    println!(
        "probing trial slot {}: need {} consecutive healthy samples within {}s (every {}s): `{}`",
        trial.as_str(),
        opts.successes,
        opts.window_secs,
        opts.interval_secs,
        opts.cmd
    );

    let mut gate = HealthGate::new(opts.successes);
    let deadline = Instant::now() + Duration::from_secs(opts.window_secs);
    loop {
        let healthy = run_health_cmd(&opts.cmd);
        if let Some(true) = gate.observe(healthy) {
            // HEALTHY streak reached → commit the trial as the new active. The
            // already-running trial process simply becomes the committed one; no
            // restart needed on the happy path.
            let next = plan_commit(&record).map_err(|e| e.to_string())?;
            ctrl.write(&next)
                .map_err(|e| format!("persist boot record: {e}"))?;
            println!(
                "healthy ({}/{} consecutive) -> committed: active slot is now {}",
                gate.required_streak(),
                gate.required_streak(),
                next.active.as_str()
            );
            return Ok(());
        }
        println!(
            "  sample healthy={healthy} streak={}/{}",
            gate.streak(),
            gate.required_streak()
        );
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_secs(opts.interval_secs));
    }

    // Window expired without a healthy streak → automatic rollback to the known-good
    // active slot. Persist the reverted record FIRST (so any restart runs the good
    // slot), then restart the unit to switch the currently-running trial process out.
    let next = plan_rollback(&record);
    ctrl.write(&next)
        .map_err(|e| format!("persist boot record: {e}"))?;
    eprintln!(
        "unhealthy: no {}-sample healthy streak within {}s -> rolled back to slot {}",
        opts.successes,
        opts.window_secs,
        next.active.as_str()
    );
    if opts.restart {
        if let Err(e) = restart_unit(&opts.unit) {
            // The record is already reverted, so correctness holds regardless — a
            // failed restart just means the switch waits for the next natural one.
            eprintln!("warning: {e}; boot record is reverted, so the next restart runs the good slot");
        }
    } else {
        eprintln!(
            "(--no-restart) boot record reverted; restart {} to run the good slot",
            opts.unit
        );
    }
    Err(format!(
        "trial slot {} unhealthy — rolled back to {}",
        trial.as_str(),
        next.active.as_str()
    ))
}

/// Run the health command via `sh -c`; exit status 0 = healthy. A command that fails
/// to even launch is treated as UNHEALTHY (fail-closed).
fn run_health_cmd(cmd: &str) -> bool {
    match std::process::Command::new("sh").arg("-c").arg(cmd).status() {
        Ok(s) => s.success(),
        Err(_) => false,
    }
}

/// `systemctl restart <unit>` — switch the running process back to the good slot on
/// rollback.
fn restart_unit(unit: &str) -> Result<(), String> {
    let status = std::process::Command::new("systemctl")
        .arg("restart")
        .arg(unit)
        .status()
        .map_err(|e| format!("systemctl restart {unit}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("systemctl restart {unit} exited with {status}"))
    }
}

/// `status` — print the record + which slot `run` would launch. READ-ONLY: it does
/// NOT create the record or its directory (unlike the mutating commands), so
/// querying an uninitialized node has no side effects.
fn cmd_status() -> Result<(), String> {
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
         \x20 kirra-ota-ctl probe --cmd '<health>'     auto commit-or-rollback on a trial's health\n\
         \x20 kirra-ota-ctl status                     show the boot record\n\
         \n\
         probe flags: --cmd '<sh>' (exit 0 = healthy; required) --window-secs N (30)\n\
         \x20            --interval-secs S (2) --successes K (3) --unit NAME (kirra-governor)\n\
         \x20            --no-restart\n\
         \n\
         ENV: KIRRA_OTA_SLOT_A KIRRA_OTA_SLOT_B KIRRA_OTA_BOOT_RECORD KIRRA_OTA_GOVERNOR_BIN KIRRA_OTA_UNIT"
    );
}
