//! `kirra-ota-ctl` — health probe → commit-or-rollback (de-monolith split of kirra_ota_ctl.rs).
//!
//! Behaviour unchanged. Shared plumbing (Cfg, http/*, now_ms, write_atomic,
//! exec_governor, …) stays in the bin root and is visible to this submodule.

use crate::*;

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
        let cmd = cmd.ok_or_else(|| {
            "probe requires --cmd '<health command>' (exit 0 = healthy)".to_string()
        })?;
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
pub(crate) fn cmd_probe(args: &[String]) -> Result<(), String> {
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
    let mut ctrl = FileBootController::open(&cfg.record, Slot::A)
        .map_err(|e| format!("open boot record: {e}"))?;
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
            eprintln!(
                "warning: {e}; boot record is reverted, so the next restart runs the good slot"
            );
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
