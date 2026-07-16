//! `kirra-ota-ctl` — Uptane trust provisioning + model allow-list (de-monolith split of kirra_ota_ctl.rs).
//!
//! Behaviour unchanged. Shared plumbing (Cfg, http/*, now_ms, write_atomic,
//! exec_governor, …) stays in the bin root and is visible to this submodule.

use crate::*;

/// Default durable-trust-store path: a sibling of the boot record (the WP-14
/// `UptaneTrustStore` convention).
pub(crate) const DEFAULT_TRUST_STORE: &str = "/var/lib/kirra/uptane-trust.json";

/// `trust-provision` — anchor this node's Uptane trust ONCE from a self-signed
/// root bundle. Refuses if a trust state already exists (a node is anchored
/// once; rotations flow through the metadata channel, not re-provisioning).
pub(crate) fn cmd_trust_provision(args: &[String]) -> Result<(), String> {
    let mut root_path: Option<PathBuf> = None;
    let mut store_path =
        std::env::var("KIRRA_OTA_TRUST_STORE").unwrap_or_else(|_| DEFAULT_TRUST_STORE.to_string());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut next = |flag: &str| -> Result<String, String> {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match a.as_str() {
            "--root" => root_path = Some(PathBuf::from(next("--root")?)),
            "--trust-store" => store_path = next("--trust-store")?,
            other => return Err(format!("unknown trust-provision flag {other:?}")),
        }
    }
    let root_path = root_path.ok_or("trust-provision requires --root <signed-root.json>")?;
    let text = std::fs::read_to_string(&root_path)
        .map_err(|e| format!("read signed root {}: {e}", root_path.display()))?;
    let anchor: kirra_release_token::uptane::SignedRoot =
        serde_json::from_str(&text).map_err(|e| format!("parse signed root JSON: {e}"))?;
    if let Some(parent) = Path::new(&store_path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let store = UptaneTrustStore::new(&store_path);
    let state = store
        .provision(&anchor)
        .map_err(|e| format!("provision trust anchor: {e}"))?;
    println!(
        "trust anchored: root v{} persisted to {store_path}",
        state.root.meta.version
    );
    Ok(())
}

/// `model-allowlist` — the EP-06 node step: verify a presented Uptane metadata
/// bundle (timestamp→snapshot→targets) against this node's durable trust state
/// and ATOMICALLY write the `KIRRA_MODEL_ALLOWLIST(_STRICT)` env file the
/// co-located parko process launches under (systemd `EnvironmentFile=`).
///
/// Fail-closed: ANY refusal (no trust state, bad signature, expiry, rollback,
/// tampered targets) exits non-zero and leaves the previous env file
/// UNTOUCHED — a node never runs under an allow-list derived from unverified
/// bytes. The bundle arrives either as a file drop (`--metadata`) or FETCHED
/// over HTTP (`--metadata-url`, the EP-13 client replacing the file-drop
/// stub) — either way the carrier is untrusted and verification happens here.
pub(crate) fn cmd_model_allowlist(args: &[String]) -> Result<(), String> {
    let mut metadata = std::env::var("KIRRA_OTA_MODEL_METADATA")
        .ok()
        .map(PathBuf::from);
    let mut metadata_url = std::env::var("KIRRA_OTA_MODEL_METADATA_URL").ok();
    let mut out = std::env::var("KIRRA_OTA_MODEL_ENV_FILE")
        .ok()
        .map(PathBuf::from);
    let mut store_path =
        std::env::var("KIRRA_OTA_TRUST_STORE").unwrap_or_else(|_| DEFAULT_TRUST_STORE.to_string());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut next = |flag: &str| -> Result<String, String> {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match a.as_str() {
            "--metadata" => metadata = Some(PathBuf::from(next("--metadata")?)),
            "--metadata-url" => metadata_url = Some(next("--metadata-url")?),
            "--out" => out = Some(PathBuf::from(next("--out")?)),
            "--trust-store" => store_path = next("--trust-store")?,
            other => return Err(format!("unknown model-allowlist flag {other:?}")),
        }
    }
    let out =
        out.ok_or("model-allowlist requires --out <env-file> (or KIRRA_OTA_MODEL_ENV_FILE)")?;

    let text = match (metadata, metadata_url) {
        (Some(_), Some(_)) => {
            return Err(
                "--metadata and --metadata-url are mutually exclusive — pick one source".into(),
            )
        }
        (Some(path), None) => std::fs::read_to_string(&path)
            .map_err(|e| format!("read metadata bundle {}: {e}", path.display()))?,
        (None, Some(url)) => {
            let (code, body) = http_get(&url)?;
            if code != 200 {
                // Fail-closed: a fetch failure emits NOTHING (the previous env
                // file survives untouched); the next poll retries.
                return Err(format!("metadata fetch {url} returned HTTP {code}"));
            }
            body
        }
        (None, None) => {
            return Err(
                "model-allowlist requires --metadata <bundle.json> or --metadata-url <url> \
                 (or KIRRA_OTA_MODEL_METADATA / KIRRA_OTA_MODEL_METADATA_URL)"
                    .into(),
            )
        }
    };
    let bundle = parse_metadata_bundle(&text).map_err(|e| e.to_string())?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("system clock before epoch: {e}"))?
        .as_millis() as u64;

    let store = UptaneTrustStore::new(&store_path);
    let (env_text, verified) =
        derive_model_allowlist(&store, &bundle, now_ms).map_err(|e| e.to_string())?;

    write_atomic(&out, env_text.as_bytes())
        .map_err(|e| format!("write env file {}: {e}", out.display()))?;
    println!(
        "model allow-list written: {} ({} artifact(s), targets v{}, STRICT) — \
         restart the parko unit to adopt it",
        out.display(),
        verified.targets().targets.len(),
        verified.targets().version
    );
    Ok(())
}

#[cfg(test)]
mod model_allowlist_cli_tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use kirra_release_token::uptane::{
        author_initial_root, sign_snapshot, sign_targets, sign_timestamp, RootMetadata,
        SnapshotMetadata, TargetEntry, TargetsMetadata, TimestampMetadata,
    };

    // Far-future expiry so the CLI's real wall clock passes freshness.
    const EXP: u64 = u64::MAX;
    const D1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const D2: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    struct Repo {
        root_sk: SigningKey,
        targets_sk: SigningKey,
        snapshot_sk: SigningKey,
        timestamp_sk: SigningKey,
    }
    impl Repo {
        fn new() -> Self {
            Self {
                root_sk: SigningKey::from_bytes(&[1u8; 32]),
                targets_sk: SigningKey::from_bytes(&[2u8; 32]),
                snapshot_sk: SigningKey::from_bytes(&[3u8; 32]),
                timestamp_sk: SigningKey::from_bytes(&[4u8; 32]),
            }
        }
        fn root(&self) -> RootMetadata {
            RootMetadata {
                version: 1,
                expires_at_ms: EXP,
                root_key: self.root_sk.verifying_key().to_bytes(),
                targets_key: self.targets_sk.verifying_key().to_bytes(),
                snapshot_key: self.snapshot_sk.verifying_key().to_bytes(),
                timestamp_key: self.timestamp_sk.verifying_key().to_bytes(),
            }
        }
        fn bundle_json(&self, version: u64, models: Vec<TargetEntry>) -> String {
            let targets = TargetsMetadata {
                version,
                expires_at_ms: EXP,
                targets: models,
            };
            let snapshot = SnapshotMetadata {
                version,
                expires_at_ms: EXP,
                targets_version: version,
            };
            let timestamp = TimestampMetadata {
                version,
                expires_at_ms: EXP,
                snapshot_version: version,
            };
            serde_json::json!({
                "timestamp": timestamp,
                "timestamp_sig_b64": sign_timestamp(&timestamp, &self.timestamp_sk),
                "snapshot": snapshot,
                "snapshot_sig_b64": sign_snapshot(&snapshot, &self.snapshot_sk),
                "targets": targets,
                "targets_sig_b64": sign_targets(&targets, &self.targets_sk),
            })
            .to_string()
        }
    }

    fn model(digest: &str) -> TargetEntry {
        TargetEntry {
            digest_hex: digest.into(),
            length_bytes: 64,
            version: "m-v1".into(),
        }
    }

    /// A per-test workspace dir: trust store + bundle + out file live together
    /// and are removed at the end (parallel-safe, no env mutation).
    fn workspace(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kirra_ma_cli_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn provision(dir: &Path, repo: &Repo) -> String {
        let anchor = author_initial_root(repo.root(), &repo.root_sk);
        let root_file = dir.join("signed-root.json");
        std::fs::write(&root_file, serde_json::to_string(&anchor).unwrap()).unwrap();
        let store = dir.join("uptane-trust.json").to_string_lossy().into_owned();
        cmd_trust_provision(&[
            "--root".into(),
            root_file.to_string_lossy().into_owned(),
            "--trust-store".into(),
            store.clone(),
        ])
        .expect("provision anchors");
        store
    }

    fn args_for(dir: &Path, store: &str, bundle: &str) -> Vec<String> {
        let bundle_file = dir.join("bundle.json");
        std::fs::write(&bundle_file, bundle).unwrap();
        vec![
            "--metadata".into(),
            bundle_file.to_string_lossy().into_owned(),
            "--out".into(),
            dir.join("model-allowlist.env")
                .to_string_lossy()
                .into_owned(),
            "--trust-store".into(),
            store.to_string(),
        ]
    }

    /// EP-06 DoD: a signed set → the emitted env file matches the manifest's
    /// digests with STRICT pinned.
    #[test]
    fn signed_set_emits_matching_env_file() {
        let dir = workspace("signed");
        let repo = Repo::new();
        let store = provision(&dir, &repo);
        let args = args_for(
            &dir,
            &store,
            &repo.bundle_json(5, vec![model(D1), model(D2)]),
        );
        cmd_model_allowlist(&args).expect("derive + write");
        let text = std::fs::read_to_string(dir.join("model-allowlist.env")).unwrap();
        assert!(
            text.contains(&format!("KIRRA_MODEL_ALLOWLIST={D1},{D2}\n")),
            "{text}"
        );
        assert!(text.contains("KIRRA_MODEL_ALLOWLIST_STRICT=1\n"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// EP-06 DoD: tampered targets → non-zero (Err) and NO file emitted.
    #[test]
    fn tampered_targets_emit_nothing() {
        let dir = workspace("tamper");
        let repo = Repo::new();
        let store = provision(&dir, &repo);
        // Tamper after signing: swap the digest inside the serialized bundle.
        let bundle = repo.bundle_json(5, vec![model(D1)]).replace(D1, D2);
        let args = args_for(&dir, &store, &bundle);
        let err = cmd_model_allowlist(&args).unwrap_err();
        assert!(err.contains("uptane verification refused"), "{err}");
        assert!(
            !dir.join("model-allowlist.env").exists(),
            "a refused manifest must leave no env file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// EP-06 DoD: an EMPTY signed manifest emits the strict-deny config
    /// (empty allow-list + STRICT=1 → parko denies every model).
    #[test]
    fn empty_manifest_emits_strict_deny_config() {
        let dir = workspace("empty");
        let repo = Repo::new();
        let store = provision(&dir, &repo);
        let args = args_for(&dir, &store, &repo.bundle_json(2, vec![]));
        cmd_model_allowlist(&args).expect("empty manifest still derives");
        let text = std::fs::read_to_string(dir.join("model-allowlist.env")).unwrap();
        assert!(text.contains("KIRRA_MODEL_ALLOWLIST=\n"), "{text}");
        assert!(text.contains("KIRRA_MODEL_ALLOWLIST_STRICT=1\n"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A replay of already-adopted metadata is refused (the floor advanced
    /// durably on the first derive) — and the previously-emitted file survives.
    #[test]
    fn replayed_metadata_is_refused_and_the_prior_file_survives() {
        let dir = workspace("replay");
        let repo = Repo::new();
        let store = provision(&dir, &repo);
        let args = args_for(&dir, &store, &repo.bundle_json(5, vec![model(D1)]));
        cmd_model_allowlist(&args).expect("first derive");
        let before = std::fs::read_to_string(dir.join("model-allowlist.env")).unwrap();
        let err = cmd_model_allowlist(&args).unwrap_err();
        assert!(err.contains("uptane verification refused"), "{err}");
        let after = std::fs::read_to_string(dir.join("model-allowlist.env")).unwrap();
        assert_eq!(
            before, after,
            "a refused replay must not touch the emitted file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A second provision refuses (a node is anchored once).
    #[test]
    fn trust_provision_is_once() {
        let dir = workspace("anchor_once");
        let repo = Repo::new();
        let store = provision(&dir, &repo);
        let anchor = author_initial_root(repo.root(), &repo.root_sk);
        let root_file = dir.join("signed-root.json");
        std::fs::write(&root_file, serde_json::to_string(&anchor).unwrap()).unwrap();
        let err = cmd_trust_provision(&[
            "--root".into(),
            root_file.to_string_lossy().into_owned(),
            "--trust-store".into(),
            store,
        ])
        .unwrap_err();
        assert!(err.contains("provision"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
