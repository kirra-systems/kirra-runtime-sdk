//! WP-14 (MGA G-7) — node-side durable Uptane trust state + key-rotation
//! application.
//!
//! WP-13 gave the pure Uptane verification core (`kirra_release_token::uptane`)
//! and WP-14 the authoring/rotation operations. A node must PERSIST what it
//! trusts — its adopted `root` metadata (the role keys) and its rollback
//! version floor — so a rotated (compromise-recovered) key set and an
//! advanced version floor survive a restart. Without persistence a reboot
//! would drop back to the provisioned anchor, re-trusting a revoked key.
//!
//! [`UptaneTrustStore`] is a small JSON file (a sibling of the OTA boot
//! record), written with the same atomic temp-fsync-rename discipline as the
//! boot record so a power loss mid-write never leaves torn trust state. Load
//! is FAIL-CLOSED: a missing or unparseable file yields an error, never a
//! silent "trust nothing / trust anything".

use std::path::{Path, PathBuf};

use kirra_release_token::uptane::{
    apply_root_rotation, verify_root_self, RootMetadata, SignedRoot, TrustedVersions, UptaneError,
    UptaneMetadataSet,
};

/// The durable trust state a node persists between OTA polls / restarts.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrustState {
    /// The currently-trusted, verified root (role keys + version). Carried as
    /// the [`SignedRoot`] bundle so a re-serve of the same root re-verifies.
    pub root: SignedRoot,
    /// The rollback floor advanced by each accepted metadata update.
    pub versions: TrustedVersions,
}

/// Why a trust-store operation failed. All fail-closed.
#[derive(Debug)]
pub enum TrustStoreError {
    /// I/O reading/writing the trust file, or the file is absent.
    Io(std::io::Error),
    /// The stored (or presented) root did not verify, or a rotation was
    /// rejected (bad signature / downgrade / untrusted author).
    Uptane(UptaneError),
}

impl std::fmt::Display for TrustStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrustStoreError::Io(e) => write!(f, "trust store io: {e}"),
            TrustStoreError::Uptane(e) => write!(f, "trust store uptane: {e:?}"),
        }
    }
}
impl std::error::Error for TrustStoreError {}
impl From<std::io::Error> for TrustStoreError {
    fn from(e: std::io::Error) -> Self {
        TrustStoreError::Io(e)
    }
}
impl From<UptaneError> for TrustStoreError {
    fn from(e: UptaneError) -> Self {
        TrustStoreError::Uptane(e)
    }
}

/// Why the EP-13 Uptane pull gate refused an assignment. All fail-closed —
/// any of these must abort the stage, never degrade to a legacy pull.
#[derive(Debug, PartialEq, Eq)]
pub enum UptaneGateError {
    /// The node is Uptane-anchored but the assignment carries no metadata set.
    /// An anchored node NEVER falls back to an unattested pull — a stripped
    /// metadata set (a freeze/downgrade-by-omission attack) must refuse.
    MetadataMissing,
    /// The presented metadata set failed Uptane verification (bad signature,
    /// rollback attempt, expired role, chain mismatch, ...).
    Verify(UptaneError),
    /// The metadata set verified, but its `targets` do not authorize the
    /// digest this assignment tells the node to pull — the campaign plane and
    /// the release plane disagree, so nothing is pulled.
    DigestNotAuthorized(String),
}

impl std::fmt::Display for UptaneGateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UptaneGateError::MetadataMissing => write!(
                f,
                "uptane trust anchored but the assignment carries no metadata set — refusing to stage (fail-closed)"
            ),
            UptaneGateError::Verify(e) => write!(f, "uptane metadata verification failed: {e:?}"),
            UptaneGateError::DigestNotAuthorized(d) => write!(
                f,
                "verified targets metadata does not authorize the assigned digest {d} — refusing to stage"
            ),
        }
    }
}
impl std::error::Error for UptaneGateError {}
impl From<UptaneError> for UptaneGateError {
    fn from(e: UptaneError) -> Self {
        UptaneGateError::Verify(e)
    }
}

/// EP-13 (MGA G-7) — the node-side Uptane gate on an OTA pull, run BEFORE the
/// artifact download. Pure over injected trust state and clock.
///
/// - **Unanchored** (`state == None`, no provisioned trust store): `Ok(None)` —
///   the legacy WP-12 signature path still applies; the caller warns loudly,
///   mirroring the release-key posture (present ⇒ enforced, absent ⇒ legacy).
/// - **Anchored**: the assignment MUST carry a metadata set, the set must pass
///   full `verify_update` (root-keyed signatures, freshness, chain agreement,
///   rollback floors), and the verified `targets` must authorize exactly the
///   digest the node was assigned — pull only by authorized digest. On success
///   returns the advanced version floor for the caller to persist AFTER the
///   staged artifact itself verifies.
pub fn uptane_pull_gate(
    state: Option<&TrustState>,
    set: Option<&UptaneMetadataSet>,
    assigned_digest: &str,
    now_ms: u64,
) -> Result<Option<TrustedVersions>, UptaneGateError> {
    let Some(state) = state else {
        return Ok(None);
    };
    let set = set.ok_or(UptaneGateError::MetadataMissing)?;
    let verified = set.verify(&state.root.meta, state.versions, now_ms)?;
    if verified.targets().find(assigned_digest).is_none() {
        return Err(UptaneGateError::DigestNotAuthorized(
            assigned_digest.to_string(),
        ));
    }
    Ok(Some(verified.new_versions()))
}

/// A file-backed [`TrustState`] store.
pub struct UptaneTrustStore {
    path: PathBuf,
}

impl UptaneTrustStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Provision the INITIAL trust anchor. The presented self-signed root must
    /// verify against its own root key; on success the state is persisted with
    /// a zeroed version floor. Refuses to overwrite an existing state (a node
    /// is anchored once).
    pub fn provision(&self, anchor: &SignedRoot) -> Result<TrustState, TrustStoreError> {
        if self.path.exists() {
            return Err(TrustStoreError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "trust state already provisioned",
            )));
        }
        verify_root_self(&anchor.meta, &anchor.sig_by_new_root_b64)?;
        let state = TrustState {
            root: anchor.clone(),
            versions: TrustedVersions::default(),
        };
        self.write(&state)?;
        Ok(state)
    }

    /// Load the persisted trust state. FAIL-CLOSED: a missing/unreadable/
    /// unparseable file is an error (the caller must not proceed to install
    /// against unknown trust), and the stored root is RE-VERIFIED on load so a
    /// tampered file is rejected.
    pub fn load(&self) -> Result<TrustState, TrustStoreError> {
        let text = std::fs::read_to_string(&self.path)?;
        let state: TrustState = serde_json::from_str(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // Re-verify the stored root against the key EMBEDDED in its own meta —
        // that key is the trust anchor for this root. Use `sig_by_new_root_b64`
        // (the incoming-root signature): for the self-signed anchor both bundle
        // signatures are equal, and for a root-KEY rotation `sig_by_current` is
        // by the OUTGOING key and would NOT verify against the new `meta.root_key`
        // (Copilot #856 — that path would brick trust on restart). A hostile edit
        // of any persisted role key breaks this signature → fail closed.
        verify_root_self(&state.root.meta, &state.root.sig_by_new_root_b64)?;
        Ok(state)
    }

    /// Apply a presented root ROTATION against the currently-trusted root and
    /// PERSIST the new trust state (rolling the version floor forward is the
    /// caller's job via [`record_versions`](Self::record_versions)). Fail-closed: a rejected rotation
    /// leaves the stored state untouched. Returns the adopted root.
    pub fn adopt_rotation(&self, presented: &SignedRoot) -> Result<RootMetadata, TrustStoreError> {
        let mut state = self.load()?;
        let adopted = apply_root_rotation(&state.root.meta, presented)?;
        state.root = presented.clone();
        self.write(&state)?;
        Ok(adopted)
    }

    /// Persist an advanced rollback floor (after a verified metadata update).
    /// Monotonic by construction — the caller passes the `new_versions` a
    /// successful `verify_update` returned, which are strictly above the old.
    pub fn record_versions(&self, versions: TrustedVersions) -> Result<(), TrustStoreError> {
        let mut state = self.load()?;
        state.versions = versions;
        self.write(&state)?;
        Ok(())
    }

    /// Atomic temp-fsync-rename write (same discipline as the boot record): a
    /// reader (or a power loss) never observes torn trust state.
    fn write(&self, state: &TrustState) -> Result<(), TrustStoreError> {
        use std::io::Write as _;
        let text = serde_json::to_string_pretty(state)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let mut tmp = self.path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        if let Some(parent) = self.path.parent() {
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all(); // best-effort parent-dir durability
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use kirra_release_token::uptane::{
        author_initial_root, author_root_rotation, sign_snapshot, sign_targets, sign_timestamp,
        verify_update, Role, SnapshotMetadata, TargetEntry, TargetsMetadata, TimestampMetadata,
    };

    const EXP: u64 = 10_000;
    const NOW: u64 = 1_000;
    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

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
        fn root(&self, version: u64) -> RootMetadata {
            RootMetadata {
                version,
                expires_at_ms: EXP,
                root_key: self.root_sk.verifying_key().to_bytes(),
                targets_key: self.targets_sk.verifying_key().to_bytes(),
                snapshot_key: self.snapshot_sk.verifying_key().to_bytes(),
                timestamp_key: self.timestamp_sk.verifying_key().to_bytes(),
            }
        }
    }

    fn tmp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("kirra_trust_{}_{}.json", std::process::id(), name))
    }

    #[test]
    fn provision_persists_and_reloads_the_anchor() {
        let r = Repo::new();
        let path = tmp_path("provision");
        let _ = std::fs::remove_file(&path);
        let store = UptaneTrustStore::new(&path);
        let anchor = author_initial_root(r.root(1), &r.root_sk);
        store.provision(&anchor).expect("provision");
        let loaded = store.load().expect("load");
        assert_eq!(loaded.root.meta.version, 1);
        assert_eq!(loaded.versions, TrustedVersions::default());
        // Second provision refuses (anchored once).
        assert!(store.provision(&anchor).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_fails_closed_on_missing_or_tampered_state() {
        let r = Repo::new();
        let path = tmp_path("tamper");
        let _ = std::fs::remove_file(&path);
        let store = UptaneTrustStore::new(&path);
        assert!(store.load().is_err(), "missing file fails closed");

        store
            .provision(&author_initial_root(r.root(1), &r.root_sk))
            .expect("provision");
        // Tamper: swap the persisted targets_key for an attacker's — the stored
        // self-signature no longer covers it, so load must reject.
        let mut state = store.load().expect("load ok before tamper");
        state.root.meta.targets_key = SigningKey::from_bytes(&[7u8; 32])
            .verifying_key()
            .to_bytes();
        // Write the tampered state directly (bypassing verification).
        std::fs::write(&path, serde_json::to_string(&state).unwrap()).unwrap();
        assert!(
            matches!(
                store.load(),
                Err(TrustStoreError::Uptane(UptaneError::SignatureInvalid(
                    Role::Root
                )))
            ),
            "a tampered persisted root must fail closed on load"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// END-TO-END WP-14: after the node adopts a rotation that replaces the
    /// targets key AND the new trust state is PERSISTED, a reload trusts only
    /// the new key — metadata signed by the revoked old key is refused across
    /// the restart boundary. Revocation survives persistence.
    #[test]
    fn adopted_rotation_revokes_the_old_key_across_a_reload() {
        let r = Repo::new();
        let path = tmp_path("rotate");
        let _ = std::fs::remove_file(&path);
        let store = UptaneTrustStore::new(&path);
        store
            .provision(&author_initial_root(r.root(1), &r.root_sk))
            .expect("provision");

        // A metadata set signed by the OLD targets key, valid under v1.
        let targets = TargetsMetadata {
            version: 5,
            expires_at_ms: EXP,
            targets: vec![TargetEntry {
                digest_hex: DIGEST.into(),
                length_bytes: 9,
                version: "v2".into(),
            }],
        };
        let snap = SnapshotMetadata {
            version: 5,
            expires_at_ms: EXP,
            targets_version: 5,
        };
        let tsm = TimestampMetadata {
            version: 5,
            expires_at_ms: EXP,
            snapshot_version: 5,
        };
        let (t_sig, s_sig, ts_sig) = (
            sign_targets(&targets, &r.targets_sk),
            sign_snapshot(&snap, &r.snapshot_sk),
            sign_timestamp(&tsm, &r.timestamp_sk),
        );
        let v1 = store.load().unwrap();
        assert!(verify_update(
            &v1.root.meta,
            v1.versions,
            NOW,
            &tsm,
            &ts_sig,
            &snap,
            &s_sig,
            &targets,
            &t_sig
        )
        .is_ok());

        // Rotate targets → a fresh key, adopt + persist.
        let new_targets_sk = SigningKey::from_bytes(&[9u8; 32]);
        let mut m2 = r.root(2);
        m2.targets_key = new_targets_sk.verifying_key().to_bytes();
        let rotation = author_root_rotation(m2, &r.root_sk, &r.root_sk);
        store.adopt_rotation(&rotation).expect("adopt");

        // RELOAD (restart boundary): the old-key metadata is now refused...
        let v2 = store.load().expect("reload");
        assert_eq!(
            verify_update(
                &v2.root.meta,
                v2.versions,
                NOW,
                &tsm,
                &ts_sig,
                &snap,
                &s_sig,
                &targets,
                &t_sig
            ),
            Err(UptaneError::SignatureInvalid(Role::Targets)),
            "the revoked old targets key must be refused after the persisted rotation"
        );
        // ...while the NEW key verifies.
        let new_t_sig = sign_targets(&targets, &new_targets_sk);
        assert!(verify_update(
            &v2.root.meta,
            v2.versions,
            NOW,
            &tsm,
            &ts_sig,
            &snap,
            &s_sig,
            &targets,
            &new_t_sig
        )
        .is_ok());
        let _ = std::fs::remove_file(&path);
    }

    /// ROOT-KEY rotation must survive a reload (Copilot #856 regression guard).
    /// This is the case that broke `load`: after adopting a root whose ROOT key
    /// changed, the persisted `sig_by_current` is by the OUTGOING key and does
    /// NOT verify against the new `meta.root_key` — a reload verifying with that
    /// signature would fail closed and brick trust. `load` verifies with
    /// `sig_by_new_root_b64`, so the adopted new root reloads cleanly, and a
    /// FURTHER rotation still chains from it (proving the new key is trusted).
    #[test]
    fn adopted_root_key_rotation_survives_a_reload() {
        let r = Repo::new();
        let path = tmp_path("rootrot");
        let _ = std::fs::remove_file(&path);
        let store = UptaneTrustStore::new(&path);
        store
            .provision(&author_initial_root(r.root(1), &r.root_sk))
            .expect("provision");

        // Rotate the ROOT key itself to a fresh one at v2 (signed by outgoing
        // old root + incoming new root), adopt + persist.
        let new_root_sk = SigningKey::from_bytes(&[42u8; 32]);
        let mut m2 = r.root(2);
        m2.root_key = new_root_sk.verifying_key().to_bytes();
        let rotation = author_root_rotation(m2, &r.root_sk, &new_root_sk);
        store
            .adopt_rotation(&rotation)
            .expect("adopt root-key rotation");

        // RELOAD must SUCCEED and trust the NEW root key.
        let reloaded = store.load().expect("root-key rotation must reload cleanly");
        assert_eq!(reloaded.root.meta.version, 2);
        assert_eq!(
            reloaded.root.meta.root_key,
            new_root_sk.verifying_key().to_bytes()
        );

        // A further rotation must chain from the NEW root: the old root key can
        // no longer authorize a rotation (it is not the trusted outgoing key).
        let mut m3 = r.root(3);
        m3.root_key = new_root_sk.verifying_key().to_bytes();
        m3.targets_key = SigningKey::from_bytes(&[77u8; 32])
            .verifying_key()
            .to_bytes();
        let by_stale_old = author_root_rotation(m3.clone(), &r.root_sk, &new_root_sk);
        assert!(
            store.adopt_rotation(&by_stale_old).is_err(),
            "a rotation authored by the stale OLD root key must be refused after the root-key rotation"
        );
        let by_new = author_root_rotation(m3, &new_root_sk, &new_root_sk);
        store
            .adopt_rotation(&by_new)
            .expect("the new root authorizes the next rotation");
        assert_eq!(store.load().unwrap().root.meta.version, 3);
        let _ = std::fs::remove_file(&path);
    }

    /// A full, correctly-signed metadata set for `Repo` at role version `v`,
    /// authorizing `DIGEST`.
    fn metadata_set(r: &Repo, v: u64) -> UptaneMetadataSet {
        let targets = TargetsMetadata {
            version: v,
            expires_at_ms: EXP,
            targets: vec![TargetEntry {
                digest_hex: DIGEST.into(),
                length_bytes: 9,
                version: "v2".into(),
            }],
        };
        let snap = SnapshotMetadata {
            version: v,
            expires_at_ms: EXP,
            targets_version: v,
        };
        let tsm = TimestampMetadata {
            version: v,
            expires_at_ms: EXP,
            snapshot_version: v,
        };
        UptaneMetadataSet {
            timestamp_sig_b64: sign_timestamp(&tsm, &r.timestamp_sk),
            timestamp: tsm,
            snapshot_sig_b64: sign_snapshot(&snap, &r.snapshot_sk),
            snapshot: snap,
            targets_sig_b64: sign_targets(&targets, &r.targets_sk),
            targets,
        }
    }

    fn anchored_state(r: &Repo) -> TrustState {
        TrustState {
            root: author_initial_root(r.root(1), &r.root_sk),
            versions: TrustedVersions::default(),
        }
    }

    // --- EP-13 pull gate ---------------------------------------------------

    #[test]
    fn pull_gate_unanchored_is_legacy_passthrough() {
        // No trust anchor → the gate defers to the legacy path (caller warns).
        let r = Repo::new();
        let set = metadata_set(&r, 3);
        assert_eq!(uptane_pull_gate(None, Some(&set), DIGEST, NOW), Ok(None));
        assert_eq!(uptane_pull_gate(None, None, DIGEST, NOW), Ok(None));
    }

    #[test]
    fn pull_gate_anchored_refuses_a_missing_metadata_set() {
        // Downgrade-by-omission: an anchored node must never accept an
        // assignment stripped of its metadata set.
        let r = Repo::new();
        let state = anchored_state(&r);
        assert_eq!(
            uptane_pull_gate(Some(&state), None, DIGEST, NOW),
            Err(UptaneGateError::MetadataMissing)
        );
    }

    #[test]
    fn pull_gate_verifies_and_returns_the_advanced_floor() {
        let r = Repo::new();
        let state = anchored_state(&r);
        let set = metadata_set(&r, 3);
        let floor = uptane_pull_gate(Some(&state), Some(&set), DIGEST, NOW)
            .expect("valid set must pass")
            .expect("anchored path returns a floor");
        assert_eq!(
            floor,
            TrustedVersions {
                targets: 3,
                snapshot: 3,
                timestamp: 3
            }
        );
    }

    #[test]
    fn pull_gate_rejects_a_rollback_attack() {
        // The node's floor has advanced to v5 (a prior accepted update); a
        // re-served OLDER set (v3) — the classic rollback attack — must refuse.
        let r = Repo::new();
        let mut state = anchored_state(&r);
        state.versions = TrustedVersions {
            targets: 5,
            snapshot: 5,
            timestamp: 5,
        };
        let stale = metadata_set(&r, 3);
        assert!(
            matches!(
                uptane_pull_gate(Some(&state), Some(&stale), DIGEST, NOW),
                Err(UptaneGateError::Verify(UptaneError::RollbackAttempt(_)))
            ),
            "a metadata set below the persisted floor is a rollback attack"
        );
    }

    #[test]
    fn pull_gate_rejects_an_unauthorized_digest() {
        // The set verifies, but the assignment tells the node to pull a digest
        // the targets metadata does not authorize — the campaign plane and the
        // release plane disagree, so nothing is pulled.
        let r = Repo::new();
        let state = anchored_state(&r);
        let set = metadata_set(&r, 3);
        let other = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        assert_eq!(
            uptane_pull_gate(Some(&state), Some(&set), other, NOW),
            Err(UptaneGateError::DigestNotAuthorized(other.to_string()))
        );
    }

    #[test]
    fn pull_gate_rejects_a_forged_signature() {
        // A set re-signed by an attacker's key (not the anchored role keys)
        // must refuse even though it is structurally valid.
        let r = Repo::new();
        let state = anchored_state(&r);
        let mut set = metadata_set(&r, 3);
        let attacker = SigningKey::from_bytes(&[66u8; 32]);
        set.targets_sig_b64 = sign_targets(&set.targets, &attacker);
        assert!(matches!(
            uptane_pull_gate(Some(&state), Some(&set), DIGEST, NOW),
            Err(UptaneGateError::Verify(UptaneError::SignatureInvalid(
                Role::Targets
            )))
        ));
    }

    #[test]
    fn record_versions_advances_the_floor_durably() {
        let r = Repo::new();
        let path = tmp_path("floor");
        let _ = std::fs::remove_file(&path);
        let store = UptaneTrustStore::new(&path);
        store
            .provision(&author_initial_root(r.root(1), &r.root_sk))
            .expect("provision");
        store
            .record_versions(TrustedVersions {
                targets: 5,
                snapshot: 5,
                timestamp: 5,
            })
            .expect("record");
        assert_eq!(
            store.load().unwrap().versions,
            TrustedVersions {
                targets: 5,
                snapshot: 5,
                timestamp: 5
            }
        );
        let _ = std::fs::remove_file(&path);
    }
}
