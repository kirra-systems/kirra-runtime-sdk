//! `kirra-ota-ctl` — adoption report (de-monolith split of kirra_ota_ctl.rs).
//!
//! Behaviour unchanged. Shared plumbing (Cfg, http/*, now_ms, write_atomic,
//! exec_governor, …) stays in the bin root and is visible to this submodule.

use crate::*;

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
            verifier: verifier.ok_or("report requires --verifier <url> (or KIRRA_VERIFIER_URL)")?,
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
pub(crate) fn cmd_report(args: &[String]) -> Result<(), String> {
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
    let digest =
        artifact_sha256_hex(&active_path).map_err(|e| format!("hash active slot artifact: {e}"))?;

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
    let (code, resp) = http_post_json(
        &url,
        &body,
        opts.token.as_deref(),
        opts.client_id.as_deref(),
    )?;
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

// ===========================================================================
// WP-16 (MGA G-8) — measured-boot enrollment. `enroll` registers THIS node with
// the verifier as a hardware-attesting node in one audited call: its AK public
// key + expected PCR16 value + `require_tpm_quote=true`. After enrollment the
// node's `/attestation/verify` demands a genuine TPM quote (a self-reported PCR16
// alone no longer suffices). The private AK never leaves the node — only the
// public half is sent.
// ===========================================================================
