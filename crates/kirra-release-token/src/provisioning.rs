//! Fail-closed provisioning of the governor release-signing key (ADR-0031 Clause E).
//!
//! [`issue_release_token`](crate::issue_release_token) SIGNS with a `SigningKey`
//! the caller supplies — the crate never decides *where that key comes from*. This
//! module is that ONE decision point, and it is **fail-closed**: an absent or
//! misconfigured source **refuses** to produce a key (so the governor simply cannot
//! issue releases) rather than fabricating or randomly generating one.
//!
//! Refusing is the safe outcome because the actuator (and the verifier's key
//! registry, ADR-0008) admit only a **pinned** governor verifying key: a key minted
//! on the fly would verify against *nothing*, silently breaking the trust chain —
//! every release would then be denied downstream anyway, but late and opaquely. We
//! fail loud and early instead.
//!
//! ## Sources ([`SigningKeySource`])
//!
//! - **`File`** — a file holding exactly the 32-byte Ed25519 **seed**. On Unix the
//!   file must not be group/other-accessible (a leaky key file is refused, SSH
//!   private-key hygiene); every intermediate buffer is zeroized.
//! - **`DevFixed`** — the fixed, well-known deterministic key (`[7u8; 32]`, the SAME
//!   key the `kirra-l3-e2e` harness and the release-flow tests use). It is **NEVER a
//!   production identity** and is admitted ONLY under an explicit `allow_dev` opt-in
//!   (`KIRRA_GOVERNOR_SIGNING_KEY_ALLOW_DEV=1`); otherwise it is refused.
//! - **`TpmUnseal`** — ADR-0031 Clause E, Phase-II. **Deferred**: unsealing the seed
//!   from a TPM needs tss2 libs + hardware (a named external dependency). The wiring
//!   exists so a deployment can *name* the source, but it is always refused today —
//!   it never silently degrades to a weaker source.
//!
//! ## Testability (INVARIANT #13)
//!
//! The parse + provision logic is **pure** ([`parse_source`], [`provision_signing_key`]
//! take their inputs by argument, read no env), so the truth table is exercised
//! without `std::env::set_var`. The thin [`source_from_env`] / [`provision_from_env`]
//! wrappers are the only env readers and are deliberately not `set_var`-tested.

use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;
use zeroize::Zeroize;

/// The fixed, well-known dev/test seed — the SAME key `kirra-l3-e2e`'s
/// `governor_key()` and the release-flow tests use. NEVER a production identity;
/// admitted only under an explicit `allow_dev` opt-in.
pub const DEV_FIXED_SEED: [u8; 32] = [7u8; 32];

/// Env var naming the key source (`file:/path`, `dev-fixed`, `tpm:<handle>`).
/// Unset/empty ⇒ refuse (fail-closed).
pub const ENV_SIGNING_KEY_SOURCE: &str = "KIRRA_GOVERNOR_SIGNING_KEY_SOURCE";
/// Env var that must be truthy (`1`/`true`) to admit the `dev-fixed` source.
pub const ENV_ALLOW_DEV_KEY: &str = "KIRRA_GOVERNOR_SIGNING_KEY_ALLOW_DEV";

/// Where the governor's Ed25519 signing seed is provisioned from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SigningKeySource {
    /// A file holding exactly 32 raw bytes — the Ed25519 seed.
    File(PathBuf),
    /// The fixed well-known dev/test key. Admitted ONLY with `allow_dev`.
    DevFixed,
    /// Unseal the seed from a TPM (ADR-0031 Clause E, Phase-II) — DEFERRED. Carries
    /// the operator's handle spec; always refused until tss2 + hardware land.
    TpmUnseal(String),
}

/// Why provisioning refused. Every variant is a fail-closed denial — there is no
/// success-by-default path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProvisionError {
    /// `KIRRA_GOVERNOR_SIGNING_KEY_SOURCE` unset or empty.
    SourceUnset,
    /// The source spec was not a recognized form.
    UnknownSource(String),
    /// `dev-fixed` requested without the explicit `allow_dev` opt-in.
    DevKeyNotAllowed,
    /// The key file could not be read (missing / unreadable / metadata failed).
    FileUnreadable { path: PathBuf, detail: String },
    /// The key file is not exactly 32 bytes (an Ed25519 seed).
    SeedLength { path: PathBuf, got: usize },
    /// The key file is group/other-accessible (Unix) — a leaky key is refused.
    InsecurePermissions { path: PathBuf, mode: u32 },
    /// TPM unseal is deferred (Phase-II) — no tss2 backing yet.
    TpmUnsealUnsupported,
}

impl std::fmt::Display for ProvisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceUnset => write!(
                f,
                "{ENV_SIGNING_KEY_SOURCE} is unset/empty — refusing to provision a governor signing key (fail-closed)"
            ),
            Self::UnknownSource(s) => write!(
                f,
                "unrecognized governor signing-key source {s:?} (expected file:<path>, dev-fixed, or tpm:<handle>)"
            ),
            Self::DevKeyNotAllowed => write!(
                f,
                "dev-fixed governor key requested but {ENV_ALLOW_DEV_KEY} is not set — refusing a non-production key"
            ),
            Self::FileUnreadable { path, detail } => {
                write!(f, "governor key file {} is unreadable: {detail}", path.display())
            }
            Self::SeedLength { path, got } => write!(
                f,
                "governor key file {} must be exactly 32 bytes (Ed25519 seed), got {got}",
                path.display()
            ),
            Self::InsecurePermissions { path, mode } => write!(
                f,
                "governor key file {} is group/other-accessible (mode {mode:#o}) — refusing a leaky key",
                path.display()
            ),
            Self::TpmUnsealUnsupported => write!(
                f,
                "TPM-unseal governor key source is deferred (ADR-0031 Clause E, Phase-II) — no tss2 backing yet"
            ),
        }
    }
}

impl std::error::Error for ProvisionError {}

/// Parse a source spec — `file:<path>`, `dev-fixed`, or `tpm:<handle>`.
///
/// Pure: reads no env and touches no file, so the truth table is testable without
/// `set_var` (INVARIANT #13). An empty/whitespace spec is [`ProvisionError::SourceUnset`].
pub fn parse_source(spec: &str) -> Result<SigningKeySource, ProvisionError> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(ProvisionError::SourceUnset);
    }
    if let Some(path) = spec.strip_prefix("file:") {
        let path = path.trim();
        if path.is_empty() {
            return Err(ProvisionError::UnknownSource(spec.to_string()));
        }
        return Ok(SigningKeySource::File(PathBuf::from(path)));
    }
    if spec.eq_ignore_ascii_case("dev-fixed") {
        return Ok(SigningKeySource::DevFixed);
    }
    if let Some(handle) = spec.strip_prefix("tpm:") {
        return Ok(SigningKeySource::TpmUnseal(handle.trim().to_string()));
    }
    Err(ProvisionError::UnknownSource(spec.to_string()))
}

/// Provision the governor signing key from `source`, fail-closed.
///
/// `allow_dev` must be `true` to admit [`SigningKeySource::DevFixed`]. No source
/// generates a random key: a misconfigured source REFUSES rather than minting an
/// unpinnable identity.
pub fn provision_signing_key(
    source: &SigningKeySource,
    allow_dev: bool,
) -> Result<SigningKey, ProvisionError> {
    match source {
        SigningKeySource::File(path) => load_seed_file(path),
        SigningKeySource::DevFixed => {
            if allow_dev {
                Ok(SigningKey::from_bytes(&DEV_FIXED_SEED))
            } else {
                Err(ProvisionError::DevKeyNotAllowed)
            }
        }
        SigningKeySource::TpmUnseal(_) => Err(ProvisionError::TpmUnsealUnsupported),
    }
}

/// Load a 32-byte Ed25519 seed from `path`, permission-checked and zeroizing every
/// intermediate buffer.
fn load_seed_file(path: &Path) -> Result<SigningKey, ProvisionError> {
    // Unix: refuse a key file any group/other principal can read (mode & 0o077).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path).map_err(|e| ProvisionError::FileUnreadable {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })?;
        let mode = meta.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(ProvisionError::InsecurePermissions {
                path: path.to_path_buf(),
                mode: mode & 0o7777,
            });
        }
    }
    let mut bytes = std::fs::read(path).map_err(|e| ProvisionError::FileUnreadable {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;
    if bytes.len() != 32 {
        let got = bytes.len();
        bytes.zeroize(); // whatever the file held may be secret regardless of length
        return Err(ProvisionError::SeedLength { path: path.to_path_buf(), got });
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&bytes);
    bytes.zeroize();
    // `from_bytes` copies the seed into the key's own (zeroize-on-drop) storage.
    let key = SigningKey::from_bytes(&seed);
    seed.zeroize();
    Ok(key)
}

/// Resolve the source from the environment — the production entry point. Fail-closed:
/// `KIRRA_GOVERNOR_SIGNING_KEY_SOURCE` unset/empty ⇒ [`ProvisionError::SourceUnset`].
pub fn source_from_env() -> Result<SigningKeySource, ProvisionError> {
    match std::env::var(ENV_SIGNING_KEY_SOURCE) {
        Ok(spec) => parse_source(&spec),
        Err(_) => Err(ProvisionError::SourceUnset),
    }
}

/// `true` iff the dev-key opt-in flag is set truthy (`1`/`true`, case-insensitive,
/// trimmed).
#[must_use]
pub fn dev_key_allowed_from_env() -> bool {
    std::env::var(ENV_ALLOW_DEV_KEY)
        .map(|v| {
            let v = v.trim();
            v.eq_ignore_ascii_case("1") || v.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

/// The one-call production entry point: resolve source + dev-allow flag from the
/// environment and provision, fail-closed.
pub fn provision_from_env() -> Result<SigningKey, ProvisionError> {
    let source = source_from_env()?;
    provision_signing_key(&source, dev_key_allowed_from_env())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_source truth table (pure, no env / no I/O) --------------------

    #[test]
    fn parses_file_source() {
        assert_eq!(
            parse_source("file:/etc/kirra/gov.key"),
            Ok(SigningKeySource::File(PathBuf::from("/etc/kirra/gov.key")))
        );
        // surrounding + inner whitespace trimmed
        assert_eq!(
            parse_source("  file: /etc/kirra/gov.key "),
            Ok(SigningKeySource::File(PathBuf::from("/etc/kirra/gov.key")))
        );
    }

    #[test]
    fn parses_dev_fixed_case_insensitively() {
        for s in ["dev-fixed", "DEV-FIXED", " Dev-Fixed "] {
            assert_eq!(parse_source(s), Ok(SigningKeySource::DevFixed), "{s:?}");
        }
    }

    #[test]
    fn parses_tpm_source() {
        assert_eq!(
            parse_source("tpm:0x81010001"),
            Ok(SigningKeySource::TpmUnseal("0x81010001".to_string()))
        );
    }

    #[test]
    fn empty_spec_is_source_unset() {
        assert_eq!(parse_source(""), Err(ProvisionError::SourceUnset));
        assert_eq!(parse_source("   "), Err(ProvisionError::SourceUnset));
    }

    #[test]
    fn unknown_and_empty_file_paths_are_rejected() {
        assert!(matches!(parse_source("garbage"), Err(ProvisionError::UnknownSource(_))));
        // `file:` with no path is not a valid file source.
        assert!(matches!(parse_source("file:"), Err(ProvisionError::UnknownSource(_))));
        assert!(matches!(parse_source("file:   "), Err(ProvisionError::UnknownSource(_))));
    }

    // --- provision_signing_key: dev + tpm (no I/O) ---------------------------

    #[test]
    fn dev_fixed_requires_the_allow_opt_in() {
        assert_eq!(
            provision_signing_key(&SigningKeySource::DevFixed, false),
            Err(ProvisionError::DevKeyNotAllowed)
        );
    }

    #[test]
    fn dev_fixed_key_matches_the_l3_harness_key() {
        // The provisioned dev key must be exactly the fixed [7u8;32] the l3-e2e
        // harness and release-flow tests pin — routing through the seam is a no-op
        // on the key material, only adding the allow-dev gate.
        let key = provision_signing_key(&SigningKeySource::DevFixed, true)
            .expect("dev-fixed + allow_dev is infallible");
        let expected = SigningKey::from_bytes(&[7u8; 32]);
        assert_eq!(key.verifying_key(), expected.verifying_key());
    }

    #[test]
    fn tpm_source_is_deferred_and_refused() {
        assert_eq!(
            provision_signing_key(&SigningKeySource::TpmUnseal("0x81010001".into()), true),
            Err(ProvisionError::TpmUnsealUnsupported)
        );
    }

    // --- file source (Unix: permission check + zeroization) ------------------

    #[cfg(unix)]
    mod file_source {
        use super::*;
        use crate::{issue_release_token, verify_release};
        use kirra_contract_channel::GovernorContractView;
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        // A per-test temp path under the system temp dir — no tempfile dev-dep, no
        // env mutation. The tag keeps parallel tests from colliding.
        fn temp_path(tag: &str) -> PathBuf {
            let mut p = std::env::temp_dir();
            p.push(format!("kirra_gov_key_{}_{}.seed", std::process::id(), tag));
            p
        }

        fn write_seed(path: &Path, seed: &[u8], mode: u32) {
            fs::write(path, seed).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        }

        #[test]
        fn secure_32_byte_file_provisions_a_working_signer() {
            let path = temp_path("ok");
            write_seed(&path, &[3u8; 32], 0o600);

            let key = provision_signing_key(&SigningKeySource::File(path.clone()), false)
                .expect("a 0600 32-byte seed file must provision");
            // The provisioned key must actually sign a token that verifies.
            let view = GovernorContractView::new_command(2, 1, 100, 10_000, b"go").unwrap();
            let token = issue_release_token(&view, &key);
            assert_eq!(verify_release(&token, &view, &key.verifying_key()), Ok(()));
            // And it is exactly the [3u8;32] seed.
            assert_eq!(key.verifying_key(), SigningKey::from_bytes(&[3u8; 32]).verifying_key());

            let _ = fs::remove_file(&path);
        }

        #[test]
        fn group_or_other_readable_file_is_refused() {
            let path = temp_path("leaky");
            write_seed(&path, &[3u8; 32], 0o644);

            assert!(matches!(
                provision_signing_key(&SigningKeySource::File(path.clone()), false),
                Err(ProvisionError::InsecurePermissions { .. })
            ));

            let _ = fs::remove_file(&path);
        }

        #[test]
        fn wrong_length_file_is_refused() {
            let path = temp_path("shortlen");
            write_seed(&path, &[3u8; 16], 0o600);

            assert!(matches!(
                provision_signing_key(&SigningKeySource::File(path.clone()), false),
                Err(ProvisionError::SeedLength { got: 16, .. })
            ));

            let _ = fs::remove_file(&path);
        }

        #[test]
        fn missing_file_is_refused() {
            let path = temp_path("absent");
            let _ = fs::remove_file(&path); // ensure absent
            assert!(matches!(
                provision_signing_key(&SigningKeySource::File(path), false),
                Err(ProvisionError::FileUnreadable { .. })
            ));
        }
    }
}
