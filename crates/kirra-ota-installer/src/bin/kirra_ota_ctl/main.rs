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
    adoption_report_payload, artifact_sha256_hex, decide_pull, derive_model_allowlist,
    parse_metadata_bundle, plan_commit, plan_rollback, plan_run, plan_stage, uptane_pull_gate,
    verify_staged_artifact, AssignmentView, BootRecord, FileBootController, HealthGate, PullAction,
    Slot, UptaneTrustStore,
};

mod enroll;
mod probe;
mod pull;
mod report;
mod slots;
mod status;
mod uptane;

use enroll::cmd_enroll;
use probe::cmd_probe;
use pull::cmd_pull;
use report::cmd_report;
use slots::{cmd_commit, cmd_rollback, cmd_run, cmd_stage};
use status::cmd_status;
use uptane::{cmd_model_allowlist, cmd_trust_provision};

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
        "enroll" => cmd_enroll(rest),
        "trust-provision" => cmd_trust_provision(rest),
        "model-allowlist" => cmd_model_allowlist(rest),
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

// ---------------------------------------------------------------------------
// EP-06 (M1) — signed manifest → node model allow-list.
// ---------------------------------------------------------------------------

/// Atomic temp-fsync-rename write (the boot-record/trust-store discipline): a
/// reader — or a power loss — never observes a torn env file.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
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
         \x20 kirra-ota-ctl enroll --verifier <url>    register this node as measured-boot (AK + PCR16 + require-quote)\n\
         \x20 kirra-ota-ctl trust-provision --root F   anchor this node's Uptane trust (once)\n\
         \x20 kirra-ota-ctl model-allowlist ...        verified signed manifest -> KIRRA_MODEL_ALLOWLIST env file\n\
         \x20 kirra-ota-ctl status                     show the boot record\n\
         \n\
         probe flags: --cmd '<sh>' (exit 0 = healthy; required) --window-secs N (30)\n\
         \x20            --interval-secs S (2) --successes K (3) --unit NAME (kirra-governor)\n\
         \x20            --no-restart\n\
         pull flags:  --verifier <url> --node-id <id> --cohorts a,b --artifact-base <url>\n\
         \x20            [--release-pubkey <key>] [--trust-store <path>]\n\
         \x20            (each also from KIRRA_VERIFIER_URL/KIRRA_NODE_ID/KIRRA_NODE_COHORTS/KIRRA_OTA_ARTIFACT_BASE\n\
         \x20            /KIRRA_OTA_RELEASE_PUBKEY/KIRRA_OTA_TRUST_STORE; an anchored trust store\n\
         \x20            makes the pull enforce the full Uptane metadata set, fail-closed)\n\
         report flags: --verifier <url> --node-id <id> [--token T --client-id C]\n\
         \x20            [--campaign-id X --artifact-version V] [--ak-key <pkcs8.pem>]\n\
         \x20            (--ak-key signs the report → unforgeable; also KIRRA_OTA_AK_KEY)\n\
         enroll flags: --verifier <url> --node-id <id> --pcr16 <hex> [--token T --client-id C]\n\
         \x20            (--ak-key <pkcs8.pem> | --ak-pub <spki.pem>) [--site S --firmware-version V]\n\
         \x20            [--no-require-quote]  (also KIRRA_OTA_AK_KEY/KIRRA_OTA_AK_PUB/KIRRA_OTA_PCR16)\n\
         trust-provision flags: --root <signed-root.json> [--trust-store <path>]\n\
         model-allowlist flags: --metadata <bundle.json> | --metadata-url <url> --out <env-file>\n\
         \x20            [--trust-store <path>]\n\
         \x20            (also KIRRA_OTA_MODEL_METADATA/KIRRA_OTA_MODEL_METADATA_URL\n\
         \x20            /KIRRA_OTA_MODEL_ENV_FILE/KIRRA_OTA_TRUST_STORE)\n\
         \n\
         ENV: KIRRA_OTA_SLOT_A KIRRA_OTA_SLOT_B KIRRA_OTA_BOOT_RECORD KIRRA_OTA_GOVERNOR_BIN KIRRA_OTA_UNIT"
    );
}

/// WP-12 — load the governor artifact-release PUBLIC key: a file containing
/// either 32 raw bytes or 64 hex chars. `None` path → legacy mode (no key).
/// A path that is SET but unreadable/malformed is a hard error — a node told
/// to enforce signatures must never silently fall back to hash-only.
fn load_release_pubkey(path: Option<&str>) -> Result<Option<ed25519_dalek::VerifyingKey>, String> {
    let Some(path) = path else { return Ok(None) };
    let raw = std::fs::read(path).map_err(|e| format!("read release pubkey {path}: {e}"))?;
    let bytes: [u8; 32] = if raw.len() == 32 {
        raw.as_slice().try_into().expect("length checked")
    } else {
        let text = String::from_utf8_lossy(&raw);
        let text = text.trim();
        let mut buf = [0u8; 32];
        if text.len() != 64 {
            return Err(format!(
                "release pubkey {path} must be 32 raw bytes or 64 hex chars"
            ));
        }
        for i in 0..32 {
            buf[i] = u8::from_str_radix(&text[2 * i..2 * i + 2], 16)
                .map_err(|e| format!("release pubkey {path} hex: {e}"))?;
        }
        buf
    };
    ed25519_dalek::VerifyingKey::from_bytes(&bytes)
        .map(Some)
        .map_err(|e| format!("release pubkey {path} invalid: {e}"))
}

/// WP-12 — thin adapter over the shared verify seam with a ctl-friendly error.
fn kirra_ota_installer_release_verify(
    digest: &str,
    signature_b64: &str,
    vk: &ed25519_dalek::VerifyingKey,
) -> Result<(), String> {
    kirra_release_token::artifact_release::verify_artifact_release(digest, signature_b64, vk)
        .map_err(|e| format!("artifact release signature REFUSED ({e:?}) — not staging"))
}
