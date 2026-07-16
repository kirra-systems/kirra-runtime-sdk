//! `kirra-ota-ctl` — measured-boot enrollment (de-monolith split of kirra_ota_ctl.rs).
//!
//! Behaviour unchanged. Shared plumbing (Cfg, http/*, now_ms, write_atomic,
//! exec_governor, …) stays in the bin root and is visible to this submodule.

use crate::*;

struct EnrollOpts {
    verifier: String,
    node_id: String,
    /// Admin bearer token — `/attestation/register` is admin-scoped.
    token: Option<String>,
    client_id: Option<String>,
    /// PKCS#8 Ed25519 PRIVATE key PEM — the public half is DERIVED and enrolled.
    ak_key: Option<PathBuf>,
    /// OR a public SubjectPublicKeyInfo PEM supplied directly (mutually sufficient).
    ak_pub: Option<PathBuf>,
    /// Expected PCR16 measured-boot VALUE, hex (the verifier bridges to the quote's
    /// pcrDigest = SHA256(value)). Read from the node's TPM offline (`tpm2_pcrread
    /// sha256:16`) or a swtpm; supplied here so enrollment is sandbox-testable.
    pcr16: String,
    site: Option<String>,
    firmware_version: Option<String>,
    /// Whether to require a TPM quote. DEFAULT true (enroll IS the measured-boot
    /// path); `--no-require-quote` opts a TPM-less node out. Sent EXPLICITLY so the
    /// enrollment is deterministic regardless of the verifier's fleet-default gate.
    require_quote: bool,
}

impl EnrollOpts {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut verifier = std::env::var("KIRRA_VERIFIER_URL").ok();
        let mut node_id = std::env::var("KIRRA_NODE_ID").ok();
        // Register is admin-scoped: prefer an explicit admin token, else the API token.
        let mut token = std::env::var("KIRRA_ADMIN_TOKEN")
            .ok()
            .or_else(|| std::env::var("KIRRA_API_TOKEN").ok());
        let mut client_id = std::env::var("KIRRA_CLIENT_ID").ok();
        let mut ak_key = std::env::var("KIRRA_OTA_AK_KEY").ok().map(PathBuf::from);
        let mut ak_pub = std::env::var("KIRRA_OTA_AK_PUB").ok().map(PathBuf::from);
        let mut pcr16 = std::env::var("KIRRA_OTA_PCR16").ok();
        let mut site = std::env::var("KIRRA_NODE_SITE").ok();
        let mut firmware_version = std::env::var("KIRRA_NODE_FIRMWARE").ok();
        let mut require_quote = true;

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
                "--ak-key" => ak_key = Some(PathBuf::from(next("--ak-key")?)),
                "--ak-pub" => ak_pub = Some(PathBuf::from(next("--ak-pub")?)),
                "--pcr16" => pcr16 = Some(next("--pcr16")?),
                "--site" => site = Some(next("--site")?),
                "--firmware-version" => firmware_version = Some(next("--firmware-version")?),
                "--no-require-quote" => require_quote = false,
                other => return Err(format!("unknown enroll flag {other:?}")),
            }
        }
        Ok(EnrollOpts {
            verifier: verifier.ok_or("enroll requires --verifier <url> (or KIRRA_VERIFIER_URL)")?,
            node_id: node_id.ok_or("enroll requires --node-id <id> (or KIRRA_NODE_ID)")?,
            token,
            client_id,
            ak_key,
            ak_pub,
            pcr16: pcr16.ok_or("enroll requires --pcr16 <hex> (or KIRRA_OTA_PCR16)")?,
            site,
            firmware_version,
            require_quote,
        })
    }
}

/// Normalize + validate an expected-PCR16 hex value: exactly 64 hex chars (a
/// SHA-256 PCR value, 32 bytes). The verifier's quote parser enforces the SHA-256
/// PCR bank (`sha256:16`), so any other length is an expectation a real quote could
/// never satisfy (Copilot #861) — reject it here rather than enroll a dead node.
fn validate_pcr16_hex(v: &str) -> Result<String, String> {
    let v = v.trim().to_ascii_lowercase();
    if v.len() != 64 || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "--pcr16 must be a SHA-256 PCR16 value: exactly 64 hex chars (sha256:16); got {} chars",
            v.len()
        ));
    }
    Ok(v)
}

/// Wrap a raw 32-byte Ed25519 public key as an SPKI PEM (RFC 8410) — the exact
/// form the verifier's `parse_ed25519_public_pem` decodes (12-byte prefix + key).
fn ed25519_spki_pem(pubkey: &[u8; 32]) -> String {
    use base64::{engine::general_purpose::STANDARD as b64e, Engine as _};
    const ED25519_SPKI_PREFIX: [u8; 12] = [
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut der = ED25519_SPKI_PREFIX.to_vec();
    der.extend_from_slice(pubkey);
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
        b64e.encode(&der)
    )
}

/// Derive the AK PUBLIC-key SPKI PEM from a PKCS#8 Ed25519 PRIVATE key PEM. The
/// private key stays on the node; only the public half is enrolled.
fn ak_public_pem_from_pkcs8(key_path: &Path) -> Result<String, String> {
    use ed25519_dalek::pkcs8::DecodePrivateKey as _;
    use ed25519_dalek::SigningKey;
    let pem = std::fs::read_to_string(key_path)
        .map_err(|e| format!("read AK key {}: {e}", key_path.display()))?;
    let sk = SigningKey::from_pkcs8_pem(&pem)
        .map_err(|e| format!("parse AK key (expect a PKCS#8 Ed25519 PEM): {e}"))?;
    Ok(ed25519_spki_pem(&sk.verifying_key().to_bytes()))
}

/// Build the `/attestation/register` JSON body for a measured-boot enrollment.
/// Pure (no IO), so the exact wire shape is unit-tested. `require_tpm_quote` is
/// emitted EXPLICITLY so the enrollment is deterministic regardless of the
/// verifier's `KIRRA_ATTEST_REQUIRE_QUOTE_DEFAULT` gate.
fn enroll_body(
    node_id: &str,
    ak_public_pem: &str,
    pcr16_value_hex: &str,
    require_tpm_quote: bool,
    site: Option<&str>,
    firmware_version: Option<&str>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "node_id": node_id,
        "ak_public_pem": ak_public_pem,
        "expected_pcr16_digest_hex": pcr16_value_hex,
        "require_tpm_quote": require_tpm_quote,
    });
    if let Some(s) = site {
        body["site"] = serde_json::Value::String(s.to_string());
    }
    if let Some(v) = firmware_version {
        body["firmware_version"] = serde_json::Value::String(v.to_string());
    }
    body
}

/// `enroll` — register THIS node with the verifier as a measured-boot node in one
/// audited call: AK public key + expected PCR16 + `require_tpm_quote`. Unlike
/// `report` (best-effort observability), enrollment is provisioning: a non-201 is
/// a HARD failure (the node is not enrolled), so the operator sees it and retries.
pub(crate) fn cmd_enroll(args: &[String]) -> Result<(), String> {
    let opts = EnrollOpts::parse(args)?;

    // Resolve the AK PUBLIC PEM: a supplied SPKI PEM, else derived from the PKCS#8
    // private key. EXACTLY ONE source is required — both set is rejected (Copilot
    // #861: silently preferring one could enroll a different key than intended).
    let ak_public_pem = match (&opts.ak_pub, &opts.ak_key) {
        (Some(_), Some(_)) => {
            return Err(
                "give exactly one of --ak-pub <spki.pem> / --ak-key <pkcs8.pem>, \
                        not both (they may name different keys)"
                    .to_string(),
            )
        }
        (Some(pub_path), None) => std::fs::read_to_string(pub_path)
            .map_err(|e| format!("read AK public PEM {}: {e}", pub_path.display()))?,
        (None, Some(key_path)) => ak_public_pem_from_pkcs8(key_path)?,
        (None, None) => {
            return Err(
                "enroll requires --ak-pub <spki.pem> or --ak-key <pkcs8.pem> \
                        (or KIRRA_OTA_AK_PUB / KIRRA_OTA_AK_KEY)"
                    .to_string(),
            )
        }
    };
    if !ak_public_pem.contains("BEGIN PUBLIC KEY") {
        return Err("the AK public key must be a SubjectPublicKeyInfo PEM \
                    (-----BEGIN PUBLIC KEY-----)"
            .to_string());
    }
    let pcr16 = validate_pcr16_hex(&opts.pcr16)?;

    let body = enroll_body(
        &opts.node_id,
        &ak_public_pem,
        &pcr16,
        opts.require_quote,
        opts.site.as_deref(),
        opts.firmware_version.as_deref(),
    )
    .to_string();

    let url = format!(
        "{}/attestation/register",
        opts.verifier.trim_end_matches('/')
    );
    let (code, resp) = http_post_json(
        &url,
        &body,
        opts.token.as_deref(),
        opts.client_id.as_deref(),
    )?;
    if code == 201 {
        println!(
            "enrolled: node {} (require_tpm_quote={}) — /attestation/verify now demands a TPM quote",
            opts.node_id, opts.require_quote
        );
        Ok(())
    } else {
        // Provisioning is not best-effort: surface a non-201 as a hard failure.
        Err(format!(
            "enroll failed — verifier returned HTTP {code}: {resp}"
        ))
    }
}

#[cfg(test)]
mod enroll_tests {
    use super::*;
    use ed25519_dalek::pkcs8::EncodePrivateKey as _;
    use ed25519_dalek::SigningKey;

    /// `enroll_body` emits the exact `/attestation/register` wire shape, with
    /// `require_tpm_quote` ALWAYS explicit (so it doesn't depend on the verifier's
    /// fleet-default gate), and optional labels only when supplied.
    #[test]
    fn enroll_body_is_the_register_wire_shape() {
        let body = enroll_body("edge-7", "PEM", "abab", true, Some("dock-3"), None);
        assert_eq!(body["node_id"], "edge-7");
        assert_eq!(body["ak_public_pem"], "PEM");
        assert_eq!(body["expected_pcr16_digest_hex"], "abab");
        assert_eq!(body["require_tpm_quote"], true, "always sent explicitly");
        assert_eq!(body["site"], "dock-3");
        assert!(
            body.get("firmware_version").is_none(),
            "omitted label is absent, not null"
        );

        // The opt-out path is faithfully carried too.
        let out = enroll_body("n", "P", "cd", false, None, None);
        assert_eq!(out["require_tpm_quote"], false);
        assert!(out.get("site").is_none());
    }

    #[test]
    fn pcr16_hex_requires_64_sha256_chars() {
        // A real SHA-256 PCR16 value (exactly 64 hex chars) is accepted, trimmed + lowered.
        assert_eq!(
            validate_pcr16_hex(&format!("  {}  ", "AB".repeat(32))).unwrap(),
            "ab".repeat(32)
        );
        assert!(validate_pcr16_hex("").is_err(), "empty refused");
        assert!(
            validate_pcr16_hex("abab").is_err(),
            "short (non-64) refused"
        );
        assert!(
            validate_pcr16_hex(&"ab".repeat(31)).is_err(),
            "62 chars refused"
        );
        assert!(
            validate_pcr16_hex(&"ab".repeat(33)).is_err(),
            "66 chars refused"
        );
        assert!(
            validate_pcr16_hex(&format!("xy{}", "ab".repeat(31))).is_err(),
            "non-hex refused"
        );
    }

    /// The AK public PEM derived from a PKCS#8 private key round-trips to the same
    /// 32-byte key the SPKI wrapper embeds — i.e. the verifier's decoder will see
    /// the node's real public key.
    #[test]
    fn ak_public_pem_derives_the_matching_spki() {
        let sk = SigningKey::from_bytes(&[3u8; 32]);
        let pkcs8 = sk
            .to_pkcs8_pem(ed25519_dalek::pkcs8::spki::der::pem::LineEnding::LF)
            .unwrap();
        let dir = std::env::temp_dir();
        let path = dir.join(format!("kirra_ak_{}.pem", std::process::id()));
        std::fs::write(&path, pkcs8.as_bytes()).unwrap();

        let pem = ak_public_pem_from_pkcs8(&path).unwrap();
        assert!(pem.contains("BEGIN PUBLIC KEY"));
        // It equals the direct SPKI wrap of the verifying key's raw bytes.
        assert_eq!(pem, ed25519_spki_pem(&sk.verifying_key().to_bytes()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn enroll_opts_parse_defaults_require_quote_true_and_reads_flags() {
        let args: Vec<String> = [
            "--verifier",
            "https://v:8090",
            "--node-id",
            "edge-7",
            "--pcr16",
            "abab",
            "--ak-pub",
            "/k/pub.pem",
            "--token",
            "adm",
            "--site",
            "dock-3",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let o = EnrollOpts::parse(&args).unwrap();
        assert_eq!(o.verifier, "https://v:8090");
        assert_eq!(o.node_id, "edge-7");
        assert_eq!(o.pcr16, "abab");
        assert_eq!(o.ak_pub.as_deref(), Some(Path::new("/k/pub.pem")));
        assert_eq!(o.token.as_deref(), Some("adm"));
        assert_eq!(o.site.as_deref(), Some("dock-3"));
        assert!(o.require_quote, "enroll defaults to require_tpm_quote=true");

        // --no-require-quote opts a TPM-less node out.
        let mut args2 = args.clone();
        args2.push("--no-require-quote".to_string());
        assert!(!EnrollOpts::parse(&args2).unwrap().require_quote);
    }

    #[test]
    fn cmd_enroll_rejects_both_ak_sources() {
        // Both --ak-pub and --ak-key set → hard error BEFORE any network/file IO
        // (the ak-source match returns early), so this runs offline.
        let pcr = "ab".repeat(32);
        let args: Vec<String> = vec![
            "--verifier".into(),
            "https://v".into(),
            "--node-id".into(),
            "n".into(),
            "--pcr16".into(),
            pcr,
            "--ak-pub".into(),
            "/a.pem".into(),
            "--ak-key".into(),
            "/b.pem".into(),
        ];
        let err = cmd_enroll(&args).expect_err("both AK sources must be rejected");
        assert!(err.contains("exactly one"), "got: {err}");
    }

    #[test]
    fn enroll_opts_parse_requires_verifier_node_and_pcr16() {
        // Missing --pcr16 (and no env) → error.
        let args: Vec<String> = ["--verifier", "u", "--node-id", "n"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(EnrollOpts::parse(&args).is_err(), "pcr16 is required");
        assert!(
            EnrollOpts::parse(&["--pcr16".into(), "ab".into()]).is_err(),
            "verifier+node required"
        );
    }
}
