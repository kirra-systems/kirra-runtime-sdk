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
//! pull --verifier <url> ...  poll the verifier for this node's assigned artifact,
//!                            download + verify + stage it if it changed
//! report --verifier <url>    report the ACTIVE slot's digest to the verifier's fleet
//!                            adoption summary (run after a commit)
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
    adoption_report_payload, artifact_sha256_hex, decide_pull, plan_commit, plan_rollback,
    plan_run, plan_stage, verify_staged_artifact, AssignmentView, BootRecord, FileBootController,
    HealthGate, PullAction, Slot,
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
        "pull" => cmd_pull(rest),
        "report" => cmd_report(rest),
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
fn stage_verified(cfg: &Cfg, artifact: &Path, digest: &str) -> Result<Slot, String> {
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

/// Options for `pull` (the fleet-driven install agent).
struct PullOpts {
    /// Verifier base URL, e.g. `http://verifier:8090`.
    verifier: String,
    /// This node's id (the campaign rollout bucket is salted by it).
    node_id: String,
    /// Cohort labels the node belongs to (its deployment ring).
    cohorts: Vec<String>,
    /// Content-addressed artifact store base URL; the artifact is fetched from
    /// `{artifact_base}/{digest}`.
    artifact_base: String,
}

impl PullOpts {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut verifier = std::env::var("KIRRA_VERIFIER_URL").ok();
        let mut node_id = std::env::var("KIRRA_NODE_ID").ok();
        let mut cohorts = std::env::var("KIRRA_NODE_COHORTS").ok().unwrap_or_default();
        let mut artifact_base = std::env::var("KIRRA_OTA_ARTIFACT_BASE").ok();

        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut next = |flag: &str| -> Result<String, String> {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{flag} needs a value"))
            };
            match a.as_str() {
                "--verifier" => verifier = Some(next("--verifier")?),
                "--node-id" => node_id = Some(next("--node-id")?),
                "--cohorts" => cohorts = next("--cohorts")?,
                "--artifact-base" => artifact_base = Some(next("--artifact-base")?),
                other => return Err(format!("unknown pull flag {other:?}")),
            }
        }
        let cohorts: Vec<String> = cohorts
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Ok(PullOpts {
            verifier: verifier
                .ok_or("pull requires --verifier <url> (or KIRRA_VERIFIER_URL)")?,
            node_id: node_id.ok_or("pull requires --node-id <id> (or KIRRA_NODE_ID)")?,
            cohorts,
            artifact_base: artifact_base
                .ok_or("pull requires --artifact-base <url> (or KIRRA_OTA_ARTIFACT_BASE)")?,
        })
    }
}

/// `pull` — the fleet-driven install agent. Polls the verifier for THIS node's
/// campaign assignment, and if it names a signed artifact digest different from what
/// the node is running, downloads it from the content-addressed store, verifies its
/// SHA-256, and stages it into the inactive slot (a `systemctl restart` then
/// trial-boots it; `probe` gates commit/rollback). Closes the loop from a campaign
/// declared in the verifier (#827–#829) to an installed artifact on the node.
///
/// FAIL-CLOSED / idempotent: a non-200 assignment (e.g. the fleet is LockedOut, so
/// the posture gate denies the read) is treated as "no update this cycle" and exits
/// 0; an in-flight trial is never disturbed; an already-applied digest is a no-op;
/// and the download is SHA-256-verified against the assigned digest before it can
/// ever be staged (a mismatch/short download never arms a slot).
fn cmd_pull(args: &[String]) -> Result<(), String> {
    let opts = PullOpts::parse(args)?;
    let cfg = Cfg::from_env();

    // 1. Fetch this node's assignment.
    let url = format!(
        "{}/fleet/campaigns/assignment/{}?cohorts={}",
        opts.verifier.trim_end_matches('/'),
        opts.node_id,
        opts.cohorts.join(",")
    );
    let (code, body) = http_get(&url)?;
    if code != 200 {
        // 403 under LockedOut / 5xx / etc. — transient. A periodic poll retries; no
        // artifact is ever adopted while the fleet is locked out (by design).
        println!("verifier returned HTTP {code} for assignment; no update this cycle");
        return Ok(());
    }
    let assignment: AssignmentView = serde_json::from_str(&body)
        .map_err(|e| format!("parse assignment JSON: {e}; body was: {body}"))?;

    // 2. Current on-disk state: is a cycle in flight, and what digest is running?
    let record = if cfg.record.exists() {
        FileBootController::open(&cfg.record, Slot::A)
            .and_then(|c| c.record())
            .map_err(|e| format!("read boot record: {e}"))?
    } else {
        BootRecord {
            active: Slot::A,
            try_boot: None,
            trying: None,
        }
    };
    let in_flight = record.try_boot.is_some() || record.trying.is_some();
    let active_path = cfg.governor_path(record.active);
    let active_digest = if active_path.exists() {
        Some(
            artifact_sha256_hex(&active_path)
                .map_err(|e| format!("hash active slot artifact: {e}"))?,
        )
    } else {
        None
    };

    // 3. Reconcile (pure).
    match decide_pull(&assignment, active_digest.as_deref(), in_flight) {
        PullAction::UpToDate => {
            println!(
                "up to date (rolled={}, active_digest={}); nothing to stage",
                assignment.rolled,
                active_digest.as_deref().unwrap_or("none")
            );
            Ok(())
        }
        PullAction::Stage {
            digest,
            version,
            campaign_id,
        } => {
            println!(
                "assigned {} (version {}, campaign {}) differs from running — staging",
                digest,
                version.as_deref().unwrap_or("?"),
                campaign_id.as_deref().unwrap_or("?")
            );
            // 4. Download the content-addressed artifact to a temp file.
            let tmp = cfg.record.with_file_name("kirra-ota-pull.tmp");
            let art_url = format!("{}/{}", opts.artifact_base.trim_end_matches('/'), digest);
            let dl = http_download(&art_url, &tmp);
            // 5. Verify + stage (stage_verified re-verifies both source and copy).
            let result = dl.and_then(|()| stage_verified(&cfg, &tmp, &digest));
            std::fs::remove_file(&tmp).ok();
            let target = result?;
            println!(
                "staged assigned artifact into slot {} ({}); `systemctl restart` then `probe`",
                target.as_str(),
                cfg.governor_path(target).display()
            );
            Ok(())
        }
    }
}

/// HTTP GET via `curl`; returns `(status_code, body)`. Errors ONLY on transport
/// failure (curl could not run/connect) — an HTTP status >= 400 is returned as the
/// code so the caller decides what it means (a LockedOut 403 is not an agent error).
fn http_get(url: &str) -> Result<(u16, String), String> {
    let out = std::process::Command::new("curl")
        .args(["-sS", "--max-time", "20", "-w", "\n%{http_code}", url])
        .output()
        .map_err(|e| format!("run curl (is it installed?): {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "curl GET {url} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // `-w "\n%{http_code}"` appends a newline + the status after the body.
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text
        .rsplit_once('\n')
        .ok_or("curl output missing status line")?;
    let code: u16 = code
        .trim()
        .parse()
        .map_err(|_| format!("curl returned a non-numeric status: {code:?}"))?;
    Ok((code, body.to_string()))
}

/// Download `url` to `dest` via `curl -f` (fails on HTTP >= 400, so a missing
/// artifact is an error, never a silent empty file).
fn http_download(url: &str, dest: &Path) -> Result<(), String> {
    let status = std::process::Command::new("curl")
        .args(["-fsS", "--max-time", "120", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|e| format!("run curl (is it installed?): {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("curl download of {url} failed ({status})"))
    }
}

/// Options for `report` (the fleet adoption report).
struct ReportOpts {
    verifier: String,
    node_id: String,
    /// Bearer API token (the report route is identity-gated).
    token: Option<String>,
    /// The `x-kirra-client-id` identity header value.
    client_id: Option<String>,
    campaign_id: Option<String>,
    artifact_version: Option<String>,
    /// Optional PKCS#8 PEM of the node's Ed25519 attestation key — when set, the
    /// report is SIGNED (unforgeable attribution).
    ak_key: Option<PathBuf>,
}

impl ReportOpts {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut verifier = std::env::var("KIRRA_VERIFIER_URL").ok();
        let mut node_id = std::env::var("KIRRA_NODE_ID").ok();
        let mut token = std::env::var("KIRRA_API_TOKEN").ok();
        let mut client_id = std::env::var("KIRRA_CLIENT_ID").ok();
        let mut campaign_id = None;
        let mut artifact_version = None;
        let mut ak_key = std::env::var("KIRRA_OTA_AK_KEY").ok().map(PathBuf::from);

        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut next = |flag: &str| -> Result<String, String> {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{flag} needs a value"))
            };
            match a.as_str() {
                "--verifier" => verifier = Some(next("--verifier")?),
                "--node-id" => node_id = Some(next("--node-id")?),
                "--token" => token = Some(next("--token")?),
                "--client-id" => client_id = Some(next("--client-id")?),
                "--campaign-id" => campaign_id = Some(next("--campaign-id")?),
                "--artifact-version" => artifact_version = Some(next("--artifact-version")?),
                "--ak-key" => ak_key = Some(PathBuf::from(next("--ak-key")?)),
                other => return Err(format!("unknown report flag {other:?}")),
            }
        }
        Ok(ReportOpts {
            verifier: verifier
                .ok_or("report requires --verifier <url> (or KIRRA_VERIFIER_URL)")?,
            node_id: node_id.ok_or("report requires --node-id <id> (or KIRRA_NODE_ID)")?,
            token,
            client_id,
            campaign_id,
            artifact_version,
            ak_key,
        })
    }
}

/// `report` — tell the verifier which governor digest this node is now RUNNING, so
/// the fleet rollout summary's adoption count reflects it. Hashes the ACTIVE slot's
/// governor binary and POSTs it to `/fleet/campaigns/report` (identity-gated: sends a
/// Bearer token + `x-kirra-client-id`). Run after a commit (e.g. from the probe unit
/// on a successful commit, or a periodic timer). Best-effort observability: a non-200
/// is a warning, not a hard failure — the report retries next cycle.
fn cmd_report(args: &[String]) -> Result<(), String> {
    let opts = ReportOpts::parse(args)?;
    let cfg = Cfg::from_env();

    // Hash the active slot's governor — the digest the node is actually running.
    let record = if cfg.record.exists() {
        FileBootController::open(&cfg.record, Slot::A)
            .and_then(|c| c.record())
            .map_err(|e| format!("read boot record: {e}"))?
    } else {
        BootRecord {
            active: Slot::A,
            try_boot: None,
            trying: None,
        }
    };
    let active_path = cfg.governor_path(record.active);
    if !active_path.exists() {
        return Err(format!(
            "active slot {} has no governor at {}; nothing to report",
            record.active.as_str(),
            active_path.display()
        ));
    }
    let digest = artifact_sha256_hex(&active_path)
        .map_err(|e| format!("hash active slot artifact: {e}"))?;

    let mut body = serde_json::json!({
        "node_id": opts.node_id,
        "applied_digest": digest,
    });
    if let Some(c) = &opts.campaign_id {
        body["campaign_id"] = serde_json::Value::String(c.clone());
    }
    if let Some(v) = &opts.artifact_version {
        body["artifact_version"] = serde_json::Value::String(v.clone());
    }
    // Optional attestation signature → unforgeable attribution. Sign the SAME payload
    // the verifier reconstructs (node_id, digest, reported_at_ms) with the node's AK.
    if let Some(key_path) = &opts.ak_key {
        let ts = now_ms();
        let sig_b64 = sign_report(key_path, &opts.node_id, &digest, ts)?;
        body["signature"] = serde_json::Value::String(sig_b64);
        body["reported_at_ms"] = serde_json::json!(ts);
    }
    let body = body.to_string();

    let url = format!(
        "{}/fleet/campaigns/report",
        opts.verifier.trim_end_matches('/')
    );
    let (code, resp) = http_post_json(&url, &body, opts.token.as_deref(), opts.client_id.as_deref())?;
    if code == 200 {
        println!(
            "reported: node {} running digest {} (campaign {})",
            opts.node_id,
            digest,
            opts.campaign_id.as_deref().unwrap_or("?")
        );
    } else {
        // BEST-EFFORT: a non-200 (LockedOut 403, transient 5xx, auth hiccup) is a
        // warning, NOT a process failure — exit 0 so a systemd oneshot/timer doesn't
        // mark the unit failed / trigger backoff; the next cycle retries. A genuine
        // agent error (curl transport, bad AK key, unreadable slot) still returned
        // `Err` above.
        eprintln!("kirra-ota-ctl: report not recorded — verifier returned HTTP {code}: {resp} (will retry next cycle)");
    }
    Ok(())
}

/// HTTP POST JSON via `curl`; returns `(status_code, body)`. Sends the identity
/// credential (Bearer token + `x-kirra-client-id`) when provided.
fn http_post_json(
    url: &str,
    body: &str,
    token: Option<&str>,
    client_id: Option<&str>,
) -> Result<(u16, String), String> {
    let mut cmd = std::process::Command::new("curl");
    cmd.args([
        "-sS",
        "--max-time",
        "20",
        "-X",
        "POST",
        "-H",
        "content-type: application/json",
    ]);
    if let Some(t) = token {
        cmd.arg("-H").arg(format!("authorization: Bearer {t}"));
    }
    if let Some(c) = client_id {
        cmd.arg("-H").arg(format!("x-kirra-client-id: {c}"));
    }
    cmd.args(["-d", body, "-w", "\n%{http_code}", url]);
    let out = cmd.output().map_err(|e| format!("run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "curl POST {url} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (resp, code) = text
        .rsplit_once('\n')
        .ok_or("curl output missing status line")?;
    let code: u16 = code
        .trim()
        .parse()
        .map_err(|_| format!("curl returned a non-numeric status: {code:?}"))?;
    Ok((code, resp.to_string()))
}

/// Milliseconds since the Unix epoch (the report timestamp the signature covers).
fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Sign an adoption report with the node's Ed25519 attestation key (PKCS#8 PEM),
/// over the SAME payload the verifier reconstructs. Returns the base64 signature.
fn sign_report(key_path: &Path, node_id: &str, digest: &str, ts: u64) -> Result<String, String> {
    use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
    use ed25519_dalek::pkcs8::DecodePrivateKey as _;
    use ed25519_dalek::{Signer as _, SigningKey};
    let pem = std::fs::read_to_string(key_path)
        .map_err(|e| format!("read AK key {}: {e}", key_path.display()))?;
    let sk = SigningKey::from_pkcs8_pem(&pem)
        .map_err(|e| format!("parse AK key (expect a PKCS#8 Ed25519 PEM): {e}"))?;
    let payload = adoption_report_payload(node_id, digest, ts);
    Ok(b64e.encode(sk.sign(&payload).to_bytes()))
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
         \x20 kirra-ota-ctl pull --verifier <url> ...  poll + install this node's assigned artifact\n\
         \x20 kirra-ota-ctl report --verifier <url>    report the active slot's digest to the fleet summary\n\
         \x20 kirra-ota-ctl status                     show the boot record\n\
         \n\
         probe flags: --cmd '<sh>' (exit 0 = healthy; required) --window-secs N (30)\n\
         \x20            --interval-secs S (2) --successes K (3) --unit NAME (kirra-governor)\n\
         \x20            --no-restart\n\
         pull flags:  --verifier <url> --node-id <id> --cohorts a,b --artifact-base <url>\n\
         \x20            (each also from KIRRA_VERIFIER_URL/KIRRA_NODE_ID/KIRRA_NODE_COHORTS/KIRRA_OTA_ARTIFACT_BASE)\n\
         report flags: --verifier <url> --node-id <id> [--token T --client-id C]\n\
         \x20            [--campaign-id X --artifact-version V] [--ak-key <pkcs8.pem>]\n\
         \x20            (--ak-key signs the report → unforgeable; also KIRRA_OTA_AK_KEY)\n\
         \n\
         ENV: KIRRA_OTA_SLOT_A KIRRA_OTA_SLOT_B KIRRA_OTA_BOOT_RECORD KIRRA_OTA_GOVERNOR_BIN KIRRA_OTA_UNIT"
    );
}
