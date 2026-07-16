//! `kirra-ota-ctl` — campaign assignment pull + download (de-monolith split of kirra_ota_ctl.rs).
//!
//! Behaviour unchanged. Shared plumbing (Cfg, http/*, now_ms, write_atomic,
//! exec_governor, …) stays in the bin root and is visible to this submodule.

use crate::slots::stage_verified;
use crate::uptane::DEFAULT_TRUST_STORE;
use crate::*;

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
    /// WP-12 — path to the governor artifact-release PUBLIC key (32-byte raw
    /// Ed25519, hex or raw file). When set, staging REQUIRES a valid release
    /// signature on the assignment (fail-closed); unset = legacy hash-only
    /// mode (pre-provisioning), with a loud warning.
    release_pubkey: Option<String>,
    /// EP-13 — durable Uptane trust-store path. If the file EXISTS the node is
    /// anchored: the assignment must carry a metadata set that verifies against
    /// the persisted root/floor and authorizes the assigned digest (fail-closed);
    /// no file = legacy mode, warned loudly like the release key.
    trust_store: String,
}

impl PullOpts {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut verifier = std::env::var("KIRRA_VERIFIER_URL").ok();
        let mut node_id = std::env::var("KIRRA_NODE_ID").ok();
        let mut cohorts = std::env::var("KIRRA_NODE_COHORTS").ok().unwrap_or_default();
        let mut artifact_base = std::env::var("KIRRA_OTA_ARTIFACT_BASE").ok();
        let mut release_pubkey = std::env::var("KIRRA_OTA_RELEASE_PUBKEY").ok();
        let mut trust_store = std::env::var("KIRRA_OTA_TRUST_STORE")
            .unwrap_or_else(|_| DEFAULT_TRUST_STORE.to_string());

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
                "--release-pubkey" => release_pubkey = Some(next("--release-pubkey")?),
                "--trust-store" => trust_store = next("--trust-store")?,
                other => return Err(format!("unknown pull flag {other:?}")),
            }
        }
        let cohorts: Vec<String> = cohorts
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Ok(PullOpts {
            verifier: verifier.ok_or("pull requires --verifier <url> (or KIRRA_VERIFIER_URL)")?,
            node_id: node_id.ok_or("pull requires --node-id <id> (or KIRRA_NODE_ID)")?,
            cohorts,
            artifact_base: artifact_base
                .ok_or("pull requires --artifact-base <url> (or KIRRA_OTA_ARTIFACT_BASE)")?,
            release_pubkey,
            trust_store,
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
pub(crate) fn cmd_pull(args: &[String]) -> Result<(), String> {
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
            signature_b64,
            version,
            campaign_id,
        } => {
            println!(
                "assigned {} (version {}, campaign {}) differs from running — staging",
                digest,
                version.as_deref().unwrap_or("?"),
                campaign_id.as_deref().unwrap_or("?")
            );
            // EP-13: with a provisioned Uptane trust anchor the assignment MUST
            // carry a metadata set that verifies end-to-end at the node (root-
            // keyed role signatures, freshness, chain agreement, rollback
            // floors) AND whose targets authorize exactly this digest — all
            // BEFORE the download. The verifier is an untrusted carrier here.
            // No anchor = legacy mode, warned loudly (same posture as WP-12).
            let trust_store = UptaneTrustStore::new(&opts.trust_store);
            let trust_state = if Path::new(&opts.trust_store).exists() {
                // An anchored node whose trust state fails to LOAD must refuse
                // the pull outright — never degrade to a legacy pull.
                Some(trust_store.load().map_err(|e| {
                    format!(
                        "uptane trust state at {} is unreadable — refusing to stage \
                     (fail-closed): {e}",
                        opts.trust_store
                    )
                })?)
            } else {
                None
            };
            let uptane_floor = uptane_pull_gate(
                trust_state.as_ref(),
                assignment.uptane_metadata.as_ref(),
                &digest,
                now_ms(),
            )
            .map_err(|e| e.to_string())?;
            match &uptane_floor {
                Some(_) => println!("uptane metadata set verified; digest is authorized"),
                None => eprintln!(
                    "WARNING: no uptane trust anchor provisioned \
                     (`trust-provision` / --trust-store / KIRRA_OTA_TRUST_STORE) \
                     — the assignment's metadata set is NOT enforced (legacy \
                     mode); anchor the node to enforce Uptane end-to-end"
                ),
            }
            // WP-12: with a provisioned release key the assignment MUST carry
            // a valid release signature over the digest — verified BEFORE the
            // download (a forged assignment costs nothing). No key = legacy
            // hash-only mode, warned loudly so provisioning debt is visible.
            match load_release_pubkey(opts.release_pubkey.as_deref())? {
                Some(vk) => {
                    let sig = signature_b64.as_deref().ok_or(
                        "release key provisioned but the assignment carries no \
                         artifact signature — refusing to stage (fail-closed)",
                    )?;
                    kirra_ota_installer_release_verify(&digest, sig, &vk)?;
                    println!("artifact release signature verified");
                }
                None => eprintln!(
                    "WARNING: no release public key provisioned \
                     (--release-pubkey / KIRRA_OTA_RELEASE_PUBKEY) — staging on \
                     digest alone (legacy mode); provision the key to enforce \
                     release signatures"
                ),
            }
            // 4. Download the content-addressed artifact to a temp file.
            let tmp = cfg.record.with_file_name("kirra-ota-pull.tmp");
            let art_url = format!("{}/{}", opts.artifact_base.trim_end_matches('/'), digest);
            let dl = http_download(&art_url, &tmp);
            // 5. Verify + stage (stage_verified re-verifies both source and copy).
            let result = dl.and_then(|()| stage_verified(&cfg, &tmp, &digest));
            std::fs::remove_file(&tmp).ok();
            let target = result?;
            // EP-13: only AFTER the artifact itself verified and staged does
            // the node advance its durable rollback floor — a failed download
            // or stage never burns the floor (the same set can be retried).
            if let Some(floor) = uptane_floor {
                trust_store.record_versions(floor).map_err(|e| {
                    format!("staged, but persisting the uptane version floor failed: {e}")
                })?;
                println!(
                    "uptane rollback floor advanced (targets v{}, snapshot v{}, timestamp v{})",
                    floor.targets, floor.snapshot, floor.timestamp
                );
            }
            println!(
                "staged assigned artifact into slot {} ({}); `systemctl restart` then `probe`",
                target.as_str(),
                cfg.governor_path(target).display()
            );
            Ok(())
        }
    }
}
