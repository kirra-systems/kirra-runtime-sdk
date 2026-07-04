//! **Model-integrity allow-list (#G16).**
//!
//! The gap: a backend loads whatever model file it is pointed at. parko-tensorrt
//! *computes* a SHA-256 of its cached engine but only logs it; parko-onnx computes
//! nothing. So a substituted or corrupted ML artifact loads and runs undetected —
//! a fail-OPEN in an otherwise fail-closed stack.
//!
//! This module is the shared, backend-agnostic primitive: hash the model file and
//! compare it to an operator-configured allow-list before the model is allowed to
//! run. The hashing/parsing/verification core ([`sha256_file`], [`ModelAllowList::parse`],
//! [`verify_model_file`]) is pure — no logging and no retained global state — so it
//! is fully unit-testable; [`ModelAllowList::from_env`] is the small
//! environment-reading wrapper backends use to obtain the policy. Backends call
//! [`verify_model_file`] from their `load_model` and surface the
//! [`BackendError::IntegrityRejected`] verdict.
//!
//! **Fail-closed policy.**
//!
//! | `KIRRA_MODEL_ALLOWLIST` | `KIRRA_MODEL_ALLOWLIST_STRICT` | model digest | result |
//! |---|---|---|---|
//! | has entries | any | in the list | `Ok { verified: true }` |
//! | has entries | any | NOT in the list | **`Err(IntegrityRejected)`** |
//! | empty / unset | `1`/`true`/`yes`/`on` | (nothing allowed) | **`Err(IntegrityRejected)`** — high-assurance: no model may load without an explicit entry |
//! | empty / unset | off | — | `Ok { verified: false }` — enforcement OFF; the digest is still computed for audit, acceptance is byte-identical to today (a warn is the caller's job) |
//! | (enforcing or not) | — | file unreadable | **`Err(Io)`** — a model that cannot be hashed cannot be proven, and cannot be loaded anyway |
//!
//! Enforcement is therefore OPT-IN (configuring an allow-list turns it on),
//! matching the repo's other env-gated safety gates — absent config never rejects
//! a previously-accepted model, but a configured operator gets hard substitution
//! detection, and `STRICT` gives the deny-by-default posture.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::backend::BackendError;

/// Env var naming the allowed model digests: comma/space-separated lowercase
/// SHA-256 hex strings. Each entry may be a bare `<64-hex>` digest or a
/// `name=<64-hex>` pair (only the digest is compared). Non-hex / wrong-length
/// entries are ignored (they simply never match — safe).
pub const MODEL_ALLOWLIST_ENV: &str = "KIRRA_MODEL_ALLOWLIST";

/// Env var enabling strict (deny-by-default) mode: `1`/`true` → a model whose
/// digest is not explicitly allow-listed is rejected even when the allow-list is
/// empty. Off by default so existing deployments/tests are unaffected.
pub const MODEL_ALLOWLIST_STRICT_ENV: &str = "KIRRA_MODEL_ALLOWLIST_STRICT";

/// Streaming read buffer — bounded memory regardless of model size.
const HASH_BUF_BYTES: usize = 64 * 1024;

/// An operator's model-integrity policy: the set of allowed SHA-256 digests plus
/// the strict flag. Construct from the environment ([`ModelAllowList::from_env`])
/// or explicitly ([`ModelAllowList::from_parts`], used by tests).
#[derive(Debug, Clone, Default)]
pub struct ModelAllowList {
    allowed: BTreeSet<String>,
    strict: bool,
}

/// The outcome of a successful integrity check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedModel {
    /// Lowercase hex SHA-256 of the model file.
    pub sha256_hex: String,
    /// `true` iff enforcement was on and the digest matched the allow-list.
    /// `false` means enforcement was OFF (the digest is informational only).
    pub verified: bool,
}

impl ModelAllowList {
    /// Read the policy from the process environment.
    pub fn from_env() -> Self {
        let allow = std::env::var(MODEL_ALLOWLIST_ENV).ok();
        let strict = std::env::var(MODEL_ALLOWLIST_STRICT_ENV).ok();
        Self::parse(allow.as_deref(), strict.as_deref())
    }

    /// Pure parser (the testable core of [`from_env`]). Splits on commas and
    /// whitespace, lowercases, strips an optional `name=` prefix, and keeps only
    /// well-formed 64-char hex digests.
    pub fn parse(allowlist: Option<&str>, strict: Option<&str>) -> Self {
        let mut allowed = BTreeSet::new();
        if let Some(list) = allowlist {
            for tok in list.split([',', ' ', '\t', '\n', ';']) {
                let tok = tok.trim();
                if tok.is_empty() {
                    continue;
                }
                // Accept `name=digest`; keep only the digest half.
                let digest = tok.rsplit('=').next().unwrap_or(tok).trim().to_ascii_lowercase();
                if is_sha256_hex(&digest) {
                    allowed.insert(digest);
                }
            }
        }
        let strict = matches!(strict.map(|s| s.trim().to_ascii_lowercase()).as_deref(), Some("1" | "true" | "yes" | "on"));
        Self { allowed, strict }
    }

    /// Explicit constructor for tests / programmatic policies.
    pub fn from_parts<I, S>(digests: I, strict: bool) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let allowed = digests
            .into_iter()
            .filter_map(|d| {
                let d = d.as_ref().trim().to_ascii_lowercase();
                is_sha256_hex(&d).then_some(d)
            })
            .collect();
        Self { allowed, strict }
    }

    /// Is integrity enforcement active? True when any digest is allow-listed OR
    /// strict mode is on. When false, [`verify_model_file`] computes the digest
    /// for audit but does not reject.
    #[must_use]
    pub fn is_enforcing(&self) -> bool {
        !self.allowed.is_empty() || self.strict
    }

    fn contains(&self, digest_hex: &str) -> bool {
        self.allowed.contains(&digest_hex.to_ascii_lowercase())
    }
}

/// Is `s` a well-formed lowercase-or-mixed 64-char SHA-256 hex digest?
fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Stream a file through SHA-256, returning the lowercase hex digest. Bounded
/// memory ([`HASH_BUF_BYTES`]). An unreadable file is a fail-closed `Io` error.
pub fn sha256_file(path: &Path) -> Result<String, BackendError> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| BackendError::Io(format!("cannot open model '{}': {e}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_BUF_BYTES];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| BackendError::Io(format!("cannot read model '{}': {e}", path.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// **The #G16 primitive.** Hash the model at `path` and check it against `allow`.
///
/// Fail-closed per the module table: a digest not in a non-empty (or strict)
/// allow-list is [`BackendError::IntegrityRejected`]; an unreadable file is
/// [`BackendError::Io`]; enforcement-off returns `Ok { verified: false }` with the
/// computed digest (the caller may log it). Never runs the model on rejection.
pub fn verify_model_file(path: &str, allow: &ModelAllowList) -> Result<VerifiedModel, BackendError> {
    let sha256_hex = sha256_file(Path::new(path))?;
    if allow.is_enforcing() {
        if allow.contains(&sha256_hex) {
            Ok(VerifiedModel { sha256_hex, verified: true })
        } else {
            Err(BackendError::IntegrityRejected { path: path.to_string(), sha256: sha256_hex })
        }
    } else {
        // Enforcement OFF: compute-and-accept (byte-identical to prior behaviour);
        // the digest is returned so the backend can log it for audit.
        Ok(VerifiedModel { sha256_hex, verified: false })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Unique temp path per call (tests run in parallel; no env mutation, no RNG).
    static SEQ: AtomicU64 = AtomicU64::new(0);
    fn write_temp(content: &[u8]) -> std::path::PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("parko_g16_{}_{n}.bin", std::process::id()));
        std::fs::File::create(&p).unwrap().write_all(content).unwrap();
        p
    }

    #[test]
    fn digest_is_stable_and_matches_known_vector() {
        // SHA-256("abc") — the canonical NIST test vector.
        let p = write_temp(b"abc");
        let d = sha256_file(&p).unwrap();
        assert_eq!(d, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn matching_digest_is_verified() {
        let p = write_temp(b"model-bytes-v1");
        let digest = sha256_file(&p).unwrap();
        let allow = ModelAllowList::from_parts([digest.clone()], false);
        assert!(allow.is_enforcing());
        let v = verify_model_file(p.to_str().unwrap(), &allow).unwrap();
        assert_eq!(v.sha256_hex, digest);
        assert!(v.verified);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn substituted_model_is_rejected_fail_closed() {
        // Allow-list pins v1's digest; then the file is SUBSTITUTED with v2.
        let p = write_temp(b"model-bytes-v1");
        let v1 = sha256_file(&p).unwrap();
        let allow = ModelAllowList::from_parts([v1], false);
        std::fs::File::create(&p).unwrap().write_all(b"model-bytes-v2-EVIL").unwrap();
        let err = verify_model_file(p.to_str().unwrap(), &allow).unwrap_err();
        assert!(
            matches!(err, BackendError::IntegrityRejected { .. }),
            "a substituted model must be rejected, got {err:?}"
        );
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn enforcement_off_accepts_but_reports_unverified() {
        let p = write_temp(b"anything");
        let allow = ModelAllowList::from_parts(Vec::<String>::new(), false); // empty, not strict
        assert!(!allow.is_enforcing());
        let v = verify_model_file(p.to_str().unwrap(), &allow).unwrap();
        assert!(!v.verified, "enforcement off → unverified");
        assert_eq!(v.sha256_hex, sha256_file(&p).unwrap(), "digest still computed for audit");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn strict_mode_rejects_when_allowlist_empty() {
        // Deny-by-default: strict + no entries → nothing may load.
        let p = write_temp(b"unlisted-model");
        let allow = ModelAllowList::from_parts(Vec::<String>::new(), true);
        assert!(allow.is_enforcing(), "strict is enforcing even with an empty list");
        let err = verify_model_file(p.to_str().unwrap(), &allow).unwrap_err();
        assert!(matches!(err, BackendError::IntegrityRejected { .. }));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn unreadable_file_fails_closed_even_when_enforcement_off() {
        let allow = ModelAllowList::from_parts(Vec::<String>::new(), false);
        let err = verify_model_file("/no/such/parko/model.onnx", &allow).unwrap_err();
        assert!(matches!(err, BackendError::Io(_)), "a model that cannot be hashed cannot be proven");
    }

    #[test]
    fn parse_reads_env_forms() {
        let d1 = "a".repeat(64);
        let d2 = "b".repeat(64);
        // mixed separators, a name= form, an UPPERCASE digest, and junk that must be dropped.
        let list = format!("{d1} , primary={}  ; not-a-digest ,", d2.to_ascii_uppercase());
        let allow = ModelAllowList::parse(Some(&list), Some("true"));
        assert!(allow.is_enforcing());
        assert!(allow.strict);
        assert!(allow.contains(&d1));
        assert!(allow.contains(&d2), "name= form and case are normalized");
        assert!(!allow.contains(&"c".repeat(64)));
        // junk token was not admitted as a digest
        assert_eq!(allow.allowed.len(), 2);
    }

    #[test]
    fn parse_unset_is_not_enforcing() {
        let allow = ModelAllowList::parse(None, None);
        assert!(!allow.is_enforcing());
        // strict parsing is exact: only 1/true/yes/on enable it.
        assert!(!ModelAllowList::parse(None, Some("0")).strict);
        assert!(!ModelAllowList::parse(None, Some("")).strict);
        assert!(ModelAllowList::parse(None, Some("ON")).strict);
    }

    #[test]
    fn is_sha256_hex_validates() {
        assert!(is_sha256_hex(&"0".repeat(64)));
        // exactly 64 mixed-case hex chars ("aAbBcCdDeEfF" is 12; ×6 = 72; take 64).
        let mixed: String = "aAbBcCdDeEfF".repeat(6).chars().take(64).collect();
        assert_eq!(mixed.len(), 64);
        assert!(is_sha256_hex(&mixed));
        assert!(!is_sha256_hex(&"0".repeat(63)), "wrong length");
        assert!(!is_sha256_hex(&"g".repeat(64)), "non-hex");
    }
}
