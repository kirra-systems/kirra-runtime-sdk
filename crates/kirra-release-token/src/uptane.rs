//! WP-13 (MGA G-7) — the Uptane four-role metadata model.
//!
//! WP-12 gave the OTA path ONE governor release signature: it proves a
//! specific artifact was released, but a single key means (a) compromising
//! that key forges any artifact, with no recovery short of re-flashing every
//! node's pinned key; (b) no rollback/freeze-attack protection — nothing binds
//! "this is the *newest* authorized artifact". Uptane (the automotive OTA
//! standard) closes both by separating trust into four roles, each with its
//! own key, and a chain of monotonically-versioned, expiring metadata:
//!
//!   - **root**      — the root of trust. Pins the verifying key of every role
//!                     (including its own). Rotating any role's key is a new,
//!                     higher-version root signed by BOTH the old and new root
//!                     keys — so key compromise is RECOVERABLE without touching
//!                     the device, and an attacker with one role key cannot
//!                     re-delegate the others.
//!   - **targets**   — signs the artifact facts (digest, length, version).
//!   - **snapshot**  — pins the exact `targets` metadata version, so an
//!                     attacker cannot mix-and-match an old targets file with a
//!                     current one (consistency).
//!   - **timestamp** — pins the exact `snapshot` version + a near-term expiry,
//!                     so a stale (frozen) metadata set is refused (freshness).
//!
//! This module is the PURE core: metadata types, canonical signing payloads
//! (explicit length-prefixed byte images — the crate's house style, no
//! canonical-JSON dependency), the role-key-separated verification pipeline,
//! and the rollback/freeze/expiry checks. It has NO I/O and NO storage — the
//! durable metadata store, the campaign-engine wiring, and the on-device
//! client are the recorded follow-up (see `docs/ota/UPTANE_ROLES.md`).
//!
//! `#![forbid(unsafe_code)]` (crate-level); Ed25519 `verify_strict` throughout.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::b64;

/// A role in the Uptane trust hierarchy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Root,
    Targets,
    Snapshot,
    Timestamp,
}

impl Role {
    /// The per-role domain tag mixed into the signing payload, so a signature
    /// made for one role can never be replayed as another's (role separation
    /// at the crypto layer, not just the key layer).
    fn domain(self) -> &'static [u8] {
        match self {
            Role::Root => b"KIRRA-UPTANE-ROOT-V1",
            Role::Targets => b"KIRRA-UPTANE-TARGETS-V1",
            Role::Snapshot => b"KIRRA-UPTANE-SNAPSHOT-V1",
            Role::Timestamp => b"KIRRA-UPTANE-TIMESTAMP-V1",
        }
    }
}

/// Why an Uptane verification step was refused. Every variant is fail-closed:
/// "do not trust / do not install this metadata or artifact".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UptaneError {
    /// A role signature did not verify against the key root pins for it.
    SignatureInvalid(Role),
    /// The metadata is expired (`now_ms >= expires_at_ms`) — freeze/staleness.
    Expired(Role),
    /// The metadata version is <= the last trusted version for this role — a
    /// rollback attempt (an attacker replaying older, once-valid metadata).
    RollbackAttempt(Role),
    /// The chain does not agree: `timestamp.snapshot_version` != the presented
    /// snapshot's version, or `snapshot.targets_version` != the presented
    /// targets' version (a mix-and-match / inconsistent metadata set).
    ChainMismatch,
    /// A root rotation was not signed by BOTH the outgoing and incoming root
    /// keys, or did not strictly increase the root version.
    InvalidRootRotation,
}

// ---------------------------------------------------------------------------
// Metadata bodies + their canonical signing images.
// ---------------------------------------------------------------------------

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_u64(out, b.len() as u64);
    out.extend_from_slice(b);
}

/// The `root` metadata: the versioned, expiring set of role verifying keys.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootMetadata {
    pub version: u64,
    pub expires_at_ms: u64,
    pub root_key: [u8; 32],
    pub targets_key: [u8; 32],
    pub snapshot_key: [u8; 32],
    pub timestamp_key: [u8; 32],
}

impl RootMetadata {
    fn signing_image(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(160);
        out.extend_from_slice(Role::Root.domain());
        put_u64(&mut out, self.version);
        put_u64(&mut out, self.expires_at_ms);
        put_bytes(&mut out, &self.root_key);
        put_bytes(&mut out, &self.targets_key);
        put_bytes(&mut out, &self.snapshot_key);
        put_bytes(&mut out, &self.timestamp_key);
        out
    }
    fn key_for(&self, role: Role) -> Result<VerifyingKey, UptaneError> {
        let raw = match role {
            Role::Root => &self.root_key,
            Role::Targets => &self.targets_key,
            Role::Snapshot => &self.snapshot_key,
            Role::Timestamp => &self.timestamp_key,
        };
        VerifyingKey::from_bytes(raw).map_err(|_| UptaneError::SignatureInvalid(role))
    }
}

/// One artifact entry in the `targets` metadata.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetEntry {
    /// SHA-256 hex (64 lowercase) of the governor artifact.
    pub digest_hex: String,
    pub length_bytes: u64,
    pub version: String,
}

/// The `targets` metadata: the authorized artifacts.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetsMetadata {
    pub version: u64,
    pub expires_at_ms: u64,
    pub targets: Vec<TargetEntry>,
}

impl TargetsMetadata {
    fn signing_image(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(Role::Targets.domain());
        put_u64(&mut out, self.version);
        put_u64(&mut out, self.expires_at_ms);
        put_u64(&mut out, self.targets.len() as u64);
        for t in &self.targets {
            put_bytes(&mut out, t.digest_hex.as_bytes());
            put_u64(&mut out, t.length_bytes);
            put_bytes(&mut out, t.version.as_bytes());
        }
        out
    }
    /// The entry for `digest_hex`, if this targets set authorizes it.
    #[must_use]
    pub fn find(&self, digest_hex: &str) -> Option<&TargetEntry> {
        self.targets.iter().find(|t| t.digest_hex == digest_hex)
    }
}

/// The `snapshot` metadata: pins the `targets` version (consistency).
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotMetadata {
    pub version: u64,
    pub expires_at_ms: u64,
    pub targets_version: u64,
}

impl SnapshotMetadata {
    fn signing_image(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(48);
        out.extend_from_slice(Role::Snapshot.domain());
        put_u64(&mut out, self.version);
        put_u64(&mut out, self.expires_at_ms);
        put_u64(&mut out, self.targets_version);
        out
    }
}

/// The `timestamp` metadata: pins the `snapshot` version (freshness).
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimestampMetadata {
    pub version: u64,
    pub expires_at_ms: u64,
    pub snapshot_version: u64,
}

impl TimestampMetadata {
    fn signing_image(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(48);
        out.extend_from_slice(Role::Timestamp.domain());
        put_u64(&mut out, self.version);
        put_u64(&mut out, self.expires_at_ms);
        put_u64(&mut out, self.snapshot_version);
        out
    }
}

/// A role's last-trusted metadata versions on a node — the rollback floor. A
/// verification never accepts a version at or below the corresponding field.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrustedVersions {
    pub targets: u64,
    pub snapshot: u64,
    pub timestamp: u64,
}

// ---------------------------------------------------------------------------
// Signing + the verification pipeline.
// ---------------------------------------------------------------------------

fn sign_image(image: &[u8], key: &SigningKey) -> String {
    b64::encode(&key.sign(image).to_bytes())
}

fn verify_image(
    image: &[u8],
    sig_b64: &str,
    vk: &VerifyingKey,
    role: Role,
) -> Result<(), UptaneError> {
    let bytes = b64::decode(sig_b64).ok_or(UptaneError::SignatureInvalid(role))?;
    let arr: [u8; 64] = bytes.try_into().map_err(|_| UptaneError::SignatureInvalid(role))?;
    vk.verify_strict(image, &Signature::from_bytes(&arr))
        .map_err(|_| UptaneError::SignatureInvalid(role))
}

/// Sign each metadata type (repository side) → base64 signature.
pub fn sign_root(m: &RootMetadata, root_key: &SigningKey) -> String {
    sign_image(&m.signing_image(), root_key)
}
pub fn sign_targets(m: &TargetsMetadata, targets_key: &SigningKey) -> String {
    sign_image(&m.signing_image(), targets_key)
}
pub fn sign_snapshot(m: &SnapshotMetadata, snapshot_key: &SigningKey) -> String {
    sign_image(&m.signing_image(), snapshot_key)
}
pub fn sign_timestamp(m: &TimestampMetadata, timestamp_key: &SigningKey) -> String {
    sign_image(&m.signing_image(), timestamp_key)
}

/// Verify a ROOT ROTATION: the incoming root must (a) have a strictly greater
/// version, (b) be signed by the OUTGOING root key (proving the current root
/// authorizes the change — key-compromise recovery flows through here), AND
/// (c) be signed by the INCOMING root key (proving possession of the new key).
/// Returns the new trusted root on success.
///
/// `current` is the node's already-trusted root; `new_meta` + the two
/// signatures are the presented rotation.
pub fn verify_root_rotation(
    current: &RootMetadata,
    new_meta: &RootMetadata,
    sig_by_old_root_b64: &str,
    sig_by_new_root_b64: &str,
) -> Result<RootMetadata, UptaneError> {
    if new_meta.version <= current.version {
        return Err(UptaneError::InvalidRootRotation);
    }
    let image = new_meta.signing_image();
    // (b) outgoing root authorizes the change.
    let old_vk = current.key_for(Role::Root)?;
    verify_image(&image, sig_by_old_root_b64, &old_vk, Role::Root)
        .map_err(|_| UptaneError::InvalidRootRotation)?;
    // (c) incoming root proves possession.
    let new_vk = new_meta.key_for(Role::Root)?;
    verify_image(&image, sig_by_new_root_b64, &new_vk, Role::Root)
        .map_err(|_| UptaneError::InvalidRootRotation)?;
    Ok(new_meta.clone())
}

/// Verify a root that is SELF-SIGNED (the initial trust anchor a node is
/// provisioned with, or a re-fetch of the same root). Signature by the root
/// key it pins; no version/rotation check (that is `verify_root_rotation`).
pub fn verify_root_self(meta: &RootMetadata, sig_b64: &str) -> Result<(), UptaneError> {
    let vk = meta.key_for(Role::Root)?;
    verify_image(&meta.signing_image(), sig_b64, &vk, Role::Root)
}

// ---------------------------------------------------------------------------
// WP-14 (MGA G-7) — key-rotation OPERATIONS: the operator/repository side that
// MINTS a rotation, the wire/storage bundle, and the node-side application.
// ---------------------------------------------------------------------------

/// A root metadata bundled with the signature(s) that authorize it — the
/// wire/storage form a node persists and a repository serves.
///
/// - the INITIAL anchor is self-signed: `sig_by_current_root == sig_by_new_root`
///   (both are the root key signing itself), authored by [`author_initial_root`];
/// - a ROTATION carries two DISTINCT signatures — the outgoing root
///   (authorizes) and the incoming root (proves possession) — authored by
///   [`author_root_rotation`].
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedRoot {
    pub meta: RootMetadata,
    /// Signature by the CURRENT (outgoing) root key over `meta`'s image.
    pub sig_by_current_root_b64: String,
    /// Signature by the NEW (incoming) root key over `meta`'s image. Equal to
    /// `sig_by_current_root_b64` for a self-signed anchor.
    pub sig_by_new_root_b64: String,
}

/// Author the INITIAL trust anchor: a self-signed root at the given version.
/// The node is provisioned with the result (or its `meta.root_key`) as its
/// trust root; [`verify_root_self`] checks it.
#[must_use]
pub fn author_initial_root(meta: RootMetadata, root_key: &SigningKey) -> SignedRoot {
    let sig = sign_root(&meta, root_key);
    SignedRoot { meta, sig_by_current_root_b64: sig.clone(), sig_by_new_root_b64: sig }
}

/// Author a ROOT ROTATION that produces `new_meta` from the currently-trusted
/// root, signed by BOTH the outgoing and incoming root keys. `new_meta.version`
/// MUST exceed the current root's (asserted at [`apply_root_rotation`] /
/// [`verify_root_rotation`] time; this only signs). When the ROOT key itself is
/// unchanged, pass the same key twice — the two signatures are then identical
/// but the bundle is still a valid rotation of the OTHER role keys.
///
/// This is the compromise-recovery mint: to revoke a leaked `targets` (or
/// `snapshot` / `timestamp`) key, author a higher-version root that lists a
/// fresh key for that role; nodes that adopt it will refuse anything signed by
/// the old key (see the revocation test).
#[must_use]
pub fn author_root_rotation(
    new_meta: RootMetadata,
    outgoing_root_key: &SigningKey,
    incoming_root_key: &SigningKey,
) -> SignedRoot {
    let image = new_meta.signing_image();
    SignedRoot {
        sig_by_current_root_b64: sign_image(&image, outgoing_root_key),
        sig_by_new_root_b64: sign_image(&image, incoming_root_key),
        meta: new_meta,
    }
}

/// Node side: apply a presented [`SignedRoot`] rotation against the currently
/// trusted root, returning the new trusted root iff it verifies (strictly
/// higher version + both signatures). Ergonomic wrapper over
/// [`verify_root_rotation`] that threads the bundle's two signatures.
pub fn apply_root_rotation(
    current: &RootMetadata,
    presented: &SignedRoot,
) -> Result<RootMetadata, UptaneError> {
    verify_root_rotation(
        current,
        &presented.meta,
        &presented.sig_by_current_root_b64,
        &presented.sig_by_new_root_b64,
    )
}

/// A verified metadata update: the caller may now trust `targets` and should
/// persist `new_versions` as its rollback floor.
///
/// Fields are PRIVATE and there is no public constructor — a `VerifiedUpdate` can
/// ONLY be produced by [`verify_update`]. This is load-bearing for the WP-24
/// model-manifest binding (`crate::model_targets`): the projection helpers take a
/// `VerifiedUpdate`, so the type itself proves the manifest was cryptographically
/// verified — an external caller cannot forge one to derive an allow-list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedUpdate {
    targets: TargetsMetadata,
    new_versions: TrustedVersions,
}

impl VerifiedUpdate {
    /// The verified `targets` metadata (the authorized artifacts).
    #[must_use]
    pub fn targets(&self) -> &TargetsMetadata {
        &self.targets
    }

    /// The new trusted version floor to persist after adopting this update.
    #[must_use]
    pub fn new_versions(&self) -> TrustedVersions {
        self.new_versions
    }
}

/// The full timestamp → snapshot → targets verification, as a node performs it
/// each poll. Given a TRUSTED `root` (already anchored / rotation-verified),
/// the node's `trusted` version floor, and the current time, verify the three
/// presented (metadata, signature) pairs:
///
///  1. each signature verifies against the key `root` pins for that role
///     (role separation — a targets-key holder cannot forge timestamp);
///  2. none is expired (`now_ms < expires_at_ms`) — freshness/freeze;
///  3. each version is STRICTLY greater than the trusted floor — rollback;
///  4. the chain agrees: `timestamp.snapshot_version == snapshot.version` and
///     `snapshot.targets_version == targets.version` — no mix-and-match.
///
/// Order matters: timestamp first (cheapest freshness gate), then snapshot,
/// then targets — each step only reached if the prior pinned it.
#[allow(clippy::too_many_arguments)]
pub fn verify_update(
    root: &RootMetadata,
    trusted: TrustedVersions,
    now_ms: u64,
    timestamp: &TimestampMetadata,
    timestamp_sig_b64: &str,
    snapshot: &SnapshotMetadata,
    snapshot_sig_b64: &str,
    targets: &TargetsMetadata,
    targets_sig_b64: &str,
) -> Result<VerifiedUpdate, UptaneError> {
    // 1+2+3 for timestamp.
    verify_image(
        &timestamp.signing_image(),
        timestamp_sig_b64,
        &root.key_for(Role::Timestamp)?,
        Role::Timestamp,
    )?;
    if now_ms >= timestamp.expires_at_ms {
        return Err(UptaneError::Expired(Role::Timestamp));
    }
    if timestamp.version <= trusted.timestamp {
        return Err(UptaneError::RollbackAttempt(Role::Timestamp));
    }

    // snapshot, pinned by timestamp.
    verify_image(
        &snapshot.signing_image(),
        snapshot_sig_b64,
        &root.key_for(Role::Snapshot)?,
        Role::Snapshot,
    )?;
    if now_ms >= snapshot.expires_at_ms {
        return Err(UptaneError::Expired(Role::Snapshot));
    }
    if snapshot.version <= trusted.snapshot {
        return Err(UptaneError::RollbackAttempt(Role::Snapshot));
    }
    if timestamp.snapshot_version != snapshot.version {
        return Err(UptaneError::ChainMismatch);
    }

    // targets, pinned by snapshot.
    verify_image(
        &targets.signing_image(),
        targets_sig_b64,
        &root.key_for(Role::Targets)?,
        Role::Targets,
    )?;
    if now_ms >= targets.expires_at_ms {
        return Err(UptaneError::Expired(Role::Targets));
    }
    if targets.version <= trusted.targets {
        return Err(UptaneError::RollbackAttempt(Role::Targets));
    }
    if snapshot.targets_version != targets.version {
        return Err(UptaneError::ChainMismatch);
    }

    Ok(VerifiedUpdate {
        targets: targets.clone(),
        new_versions: TrustedVersions {
            targets: targets.version,
            snapshot: snapshot.version,
            timestamp: timestamp.version,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // A full role keyset + a repository that mints a consistent metadata set.
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
        fn root(&self, version: u64, expires: u64) -> RootMetadata {
            RootMetadata {
                version,
                expires_at_ms: expires,
                root_key: self.root_sk.verifying_key().to_bytes(),
                targets_key: self.targets_sk.verifying_key().to_bytes(),
                snapshot_key: self.snapshot_sk.verifying_key().to_bytes(),
                timestamp_key: self.timestamp_sk.verifying_key().to_bytes(),
            }
        }
    }

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const EXP: u64 = 10_000;
    const NOW: u64 = 1_000;

    // Build a consistent (tv, sv, tsv) set at the given versions.
    fn metaset(
        r: &Repo,
        tgt_v: u64,
        snap_v: u64,
        ts_v: u64,
    ) -> (TargetsMetadata, String, SnapshotMetadata, String, TimestampMetadata, String) {
        let targets = TargetsMetadata {
            version: tgt_v,
            expires_at_ms: EXP,
            targets: vec![TargetEntry {
                digest_hex: DIGEST.into(),
                length_bytes: 42,
                version: "v2".into(),
            }],
        };
        let snapshot =
            SnapshotMetadata { version: snap_v, expires_at_ms: EXP, targets_version: tgt_v };
        let timestamp =
            TimestampMetadata { version: ts_v, expires_at_ms: EXP, snapshot_version: snap_v };
        (
            targets.clone(),
            sign_targets(&targets, &r.targets_sk),
            snapshot,
            sign_snapshot(&snapshot, &r.snapshot_sk),
            timestamp,
            sign_timestamp(&timestamp, &r.timestamp_sk),
        )
    }

    #[test]
    fn happy_path_verifies_and_returns_the_targets() {
        let r = Repo::new();
        let root = r.root(1, EXP);
        let (tgt, ts_sig, snap, sn_sig, tsm, tm_sig) = metaset(&r, 5, 5, 5);
        let out = verify_update(
            &root,
            TrustedVersions::default(),
            NOW,
            &tsm,
            &tm_sig,
            &snap,
            &sn_sig,
            &tgt,
            &ts_sig,
        )
        .expect("consistent, fresh, forward metadata verifies");
        assert_eq!(out.targets.find(DIGEST).unwrap().length_bytes, 42);
        assert_eq!(out.new_versions.targets, 5);
    }

    /// ROLE SEPARATION: a targets-key holder cannot forge the timestamp role —
    /// the timestamp signature is checked against root's timestamp_key.
    #[test]
    fn targets_key_cannot_forge_timestamp() {
        let r = Repo::new();
        let root = r.root(1, EXP);
        let (tgt, ts_sig, snap, sn_sig, tsm, _real_tm_sig) = metaset(&r, 5, 5, 5);
        // Sign the timestamp with the TARGETS key (an attacker who stole it).
        let forged_tm_sig = sign_image(&tsm.signing_image(), &r.targets_sk);
        assert_eq!(
            verify_update(
                &root, TrustedVersions::default(), NOW,
                &tsm, &forged_tm_sig, &snap, &sn_sig, &tgt, &ts_sig,
            ),
            Err(UptaneError::SignatureInvalid(Role::Timestamp))
        );
    }

    /// ROLLBACK: metadata at or below the trusted floor is refused per role.
    #[test]
    fn rollback_below_the_trusted_floor_is_refused() {
        let r = Repo::new();
        let root = r.root(1, EXP);
        let (tgt, ts_sig, snap, sn_sig, tsm, tm_sig) = metaset(&r, 5, 5, 5);
        let trusted = TrustedVersions { targets: 5, snapshot: 5, timestamp: 5 };
        // A replay of version 5 when 5 is already trusted → timestamp rollback.
        assert_eq!(
            verify_update(
                &root, trusted, NOW,
                &tsm, &tm_sig, &snap, &sn_sig, &tgt, &ts_sig,
            ),
            Err(UptaneError::RollbackAttempt(Role::Timestamp))
        );
    }

    /// FREEZE / EXPIRY: an expired timestamp is refused even if everything else
    /// is valid (a frozen metadata set cannot be served forever).
    #[test]
    fn expired_timestamp_is_refused() {
        let r = Repo::new();
        let root = r.root(1, EXP);
        let (tgt, ts_sig, snap, sn_sig, tsm, tm_sig) = metaset(&r, 5, 5, 5);
        assert_eq!(
            verify_update(
                &root, TrustedVersions::default(), EXP + 1, // now past expiry
                &tsm, &tm_sig, &snap, &sn_sig, &tgt, &ts_sig,
            ),
            Err(UptaneError::Expired(Role::Timestamp))
        );
    }

    /// MIX-AND-MATCH: a timestamp pinning a DIFFERENT snapshot version than the
    /// one presented is a chain mismatch (consistency).
    #[test]
    fn snapshot_version_mismatch_is_a_chain_error() {
        let r = Repo::new();
        let root = r.root(1, EXP);
        // timestamp says snapshot_version=6, but the presented snapshot is v5.
        let (tgt, ts_sig, snap, sn_sig, _tsm, _tm_sig) = metaset(&r, 5, 5, 5);
        let bad_ts =
            TimestampMetadata { version: 7, expires_at_ms: EXP, snapshot_version: 6 };
        let bad_ts_sig = sign_timestamp(&bad_ts, &r.timestamp_sk);
        assert_eq!(
            verify_update(
                &root, TrustedVersions::default(), NOW,
                &bad_ts, &bad_ts_sig, &snap, &sn_sig, &tgt, &ts_sig,
            ),
            Err(UptaneError::ChainMismatch)
        );
    }

    #[test]
    fn targets_version_mismatch_is_a_chain_error() {
        let r = Repo::new();
        let root = r.root(1, EXP);
        // snapshot pins targets_version=9 but the presented targets is v5.
        let (tgt, ts_sig, _snap, _sn_sig, _tsm, _tm_sig) = metaset(&r, 5, 5, 5);
        let bad_snap =
            SnapshotMetadata { version: 5, expires_at_ms: EXP, targets_version: 9 };
        let bad_sn_sig = sign_snapshot(&bad_snap, &r.snapshot_sk);
        let tsm =
            TimestampMetadata { version: 5, expires_at_ms: EXP, snapshot_version: 5 };
        let tm_sig = sign_timestamp(&tsm, &r.timestamp_sk);
        assert_eq!(
            verify_update(
                &root, TrustedVersions::default(), NOW,
                &tsm, &tm_sig, &bad_snap, &bad_sn_sig, &tgt, &ts_sig,
            ),
            Err(UptaneError::ChainMismatch)
        );
    }

    /// KEY ROTATION: the current root can authorize a new root+targets key
    /// (compromise recovery). The rotation is signed by BOTH roots and must
    /// increase the version.
    #[test]
    fn root_rotation_happy_path() {
        let r = Repo::new();
        let current = r.root(1, EXP);
        // Rotate the targets key to a fresh one; new root at v2.
        let new_targets_sk = SigningKey::from_bytes(&[9u8; 32]);
        let mut new_root = r.root(2, EXP);
        new_root.targets_key = new_targets_sk.verifying_key().to_bytes();
        let img = new_root.signing_image();
        let by_old = b64::encode(&r.root_sk.sign(&img).to_bytes());
        let by_new = b64::encode(&r.root_sk.sign(&img).to_bytes()); // same root key rotates targets
        let rotated = verify_root_rotation(&current, &new_root, &by_old, &by_new).unwrap();
        assert_eq!(rotated.targets_key, new_targets_sk.verifying_key().to_bytes());
    }

    /// A rotation NOT signed by the outgoing root is refused (an attacker with
    /// only the new key cannot install a new root).
    #[test]
    fn root_rotation_without_old_signature_is_refused() {
        let r = Repo::new();
        let current = r.root(1, EXP);
        let attacker_root_sk = SigningKey::from_bytes(&[99u8; 32]);
        let mut new_root = r.root(2, EXP);
        new_root.root_key = attacker_root_sk.verifying_key().to_bytes();
        let img = new_root.signing_image();
        // Signed only by the attacker's new key, not the current root.
        let by_attacker = b64::encode(&attacker_root_sk.sign(&img).to_bytes());
        assert_eq!(
            verify_root_rotation(&current, &new_root, &by_attacker, &by_attacker),
            Err(UptaneError::InvalidRootRotation)
        );
    }

    /// A rotation that does not increase the version is refused (no downgrade).
    #[test]
    fn root_rotation_downgrade_is_refused() {
        let r = Repo::new();
        let current = r.root(3, EXP);
        let new_root = r.root(3, EXP); // same version
        let img = new_root.signing_image();
        let sig = b64::encode(&r.root_sk.sign(&img).to_bytes());
        assert_eq!(
            verify_root_rotation(&current, &new_root, &sig, &sig),
            Err(UptaneError::InvalidRootRotation)
        );
    }

    #[test]
    fn self_signed_root_verifies_and_a_forged_one_does_not() {
        let r = Repo::new();
        let root = r.root(1, EXP);
        let sig = sign_root(&root, &r.root_sk);
        assert!(verify_root_self(&root, &sig).is_ok());
        let forged = sign_image(&root.signing_image(), &r.targets_sk);
        assert_eq!(
            verify_root_self(&root, &forged),
            Err(UptaneError::SignatureInvalid(Role::Root))
        );
    }

    /// Per-role domain separation: the four signing images never collide even
    /// at identical version/expiry, so a signature for one role is invalid for
    /// another.
    #[test]
    fn role_domains_are_separated() {
        let snap = SnapshotMetadata { version: 1, expires_at_ms: EXP, targets_version: 1 };
        let tsm = TimestampMetadata { version: 1, expires_at_ms: EXP, snapshot_version: 1 };
        assert_ne!(snap.signing_image(), tsm.signing_image());
    }

    // ---- WP-14: rotation OPERATIONS (author + apply + revocation) ----------

    /// The authored initial anchor is self-signed and verifies; `apply_root_rotation`
    /// then adopts an authored higher-version rotation.
    #[test]
    fn author_and_apply_round_trip() {
        let r = Repo::new();
        let anchor = author_initial_root(r.root(1, EXP), &r.root_sk);
        assert!(verify_root_self(&anchor.meta, &anchor.sig_by_current_root_b64).is_ok());

        // Rotate the targets key (root key unchanged) at v2.
        let new_targets_sk = SigningKey::from_bytes(&[9u8; 32]);
        let mut m2 = r.root(2, EXP);
        m2.targets_key = new_targets_sk.verifying_key().to_bytes();
        let rotation = author_root_rotation(m2, &r.root_sk, &r.root_sk);
        let adopted = apply_root_rotation(&anchor.meta, &rotation).expect("valid rotation adopts");
        assert_eq!(adopted.version, 2);
        assert_eq!(adopted.targets_key, new_targets_sk.verifying_key().to_bytes());
    }

    /// REVOCATION — the payoff of rotation. After the node adopts a root that
    /// rotates the `targets` key, metadata signed by the OLD targets key is
    /// refused, while metadata signed by the NEW key verifies. A leaked key is
    /// dead the moment the node adopts the new root — no device re-flash.
    #[test]
    fn rotating_the_targets_key_revokes_the_old_one() {
        let r = Repo::new();
        let anchor = r.root(1, EXP);

        // A metadata set the OLD targets key signs — valid under the old root.
        let (tgt, ts_sig, snap, sn_sig, tsm, tm_sig) = metaset(&r, 5, 5, 5);
        assert!(verify_update(
            &anchor, TrustedVersions::default(), NOW,
            &tsm, &tm_sig, &snap, &sn_sig, &tgt, &ts_sig,
        )
        .is_ok());

        // Rotate the targets key to a fresh one; node adopts the new root.
        let new_targets_sk = SigningKey::from_bytes(&[9u8; 32]);
        let mut m2 = r.root(2, EXP);
        m2.targets_key = new_targets_sk.verifying_key().to_bytes();
        let rotation = author_root_rotation(m2, &r.root_sk, &r.root_sk);
        let adopted = apply_root_rotation(&anchor, &rotation).expect("adopt");

        // The SAME metadata (still signed by the OLD targets key) is now REFUSED
        // under the new root — the old key is revoked.
        assert_eq!(
            verify_update(
                &adopted, TrustedVersions::default(), NOW,
                &tsm, &tm_sig, &snap, &sn_sig, &tgt, &ts_sig,
            ),
            Err(UptaneError::SignatureInvalid(Role::Targets))
        );

        // Re-signed by the NEW targets key, it verifies under the new root.
        let new_tgt_sig = sign_targets(&tgt, &new_targets_sk);
        assert!(verify_update(
            &adopted, TrustedVersions::default(), NOW,
            &tsm, &tm_sig, &snap, &sn_sig, &tgt, &new_tgt_sig,
        )
        .is_ok());
    }

    /// An authored rotation that does not raise the version is refused on apply
    /// (no silent downgrade), and a rotation authored by an ATTACKER root key
    /// (not the trusted outgoing one) is refused.
    #[test]
    fn apply_rejects_downgrade_and_untrusted_author() {
        let r = Repo::new();
        let current = r.root(3, EXP);

        let same_ver = author_root_rotation(r.root(3, EXP), &r.root_sk, &r.root_sk);
        assert_eq!(
            apply_root_rotation(&current, &same_ver),
            Err(UptaneError::InvalidRootRotation)
        );

        let attacker = SigningKey::from_bytes(&[99u8; 32]);
        let mut m4 = r.root(4, EXP);
        m4.root_key = attacker.verifying_key().to_bytes();
        // Signed only by the attacker's key on both sides — the outgoing
        // signature is NOT the trusted current root's.
        let forged = author_root_rotation(m4, &attacker, &attacker);
        assert_eq!(
            apply_root_rotation(&current, &forged),
            Err(UptaneError::InvalidRootRotation)
        );
    }
}
