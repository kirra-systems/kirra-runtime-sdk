//! WP-24 slice 2 (MGA G-15) — bind the parko model-integrity allow-list to a
//! **signed** Uptane `targets` manifest, reusing the WP-13 targets-role machinery.
//!
//! A "model manifest" is nothing new: it is a [`targets`](crate::uptane::TargetsMetadata)
//! metadata whose entries ARE the authorized ML model artifacts (SHA-256 digest +
//! version). A node VERIFIES it through the existing full chain
//! ([`uptane::verify_update`](crate::uptane::verify_update) —
//! timestamp→snapshot→targets: role separation, freshness, the rollback floor,
//! and no mix-and-match), then DERIVES the authorized model-digest set from the
//! verified `targets`. Feeding that set to parko as `KIRRA_MODEL_ALLOWLIST` makes
//! the parko `ModelAllowList` enforce a **signed** policy instead of a hand-set env
//! string — closing the "the allow-list is an operator ASSERTION, not a signed
//! FACT" gap the #G16 allow-list left open.
//!
//! **Type-enforced provenance.** These helpers take a
//! [`VerifiedUpdate`](crate::uptane::VerifiedUpdate), whose fields are private and
//! whose ONLY producer is the fail-closed `verify_update`. So the type itself
//! proves the manifest was cryptographically verified — an external caller cannot
//! forge one to derive an allow-list.
//!
//! **Deny-by-default is a STRICT-mode composition, not automatic.** A node that
//! wants "no signed manifest ⇒ run nothing" MUST also set
//! `KIRRA_MODEL_ALLOWLIST_STRICT` (parko's `ModelAllowList` strict flag): with
//! strict ON, an empty/absent `KIRRA_MODEL_ALLOWLIST` denies EVERY model
//! (deny-by-default), so only digests from a verified manifest ever load. WITHOUT
//! strict, an EMPTY allow-list means enforcement is OFF (models are accepted) —
//! so a node relying on fail-closed behaviour must enable strict. This module
//! never decides that policy; it only projects the signed digests.
//!
//! Node wiring (the thin remaining step): after `verify_update`, call
//! [`model_allowlist_env_value`](crate::model_targets::model_allowlist_env_value)
//! and set `KIRRA_MODEL_ALLOWLIST` (plus `KIRRA_MODEL_ALLOWLIST_STRICT=1`) for the
//! co-located parko process. Wiring parko's backend `load_model` to the
//! `ModelLineage` rollback and feeding the OOD monitor live per-tick confidences
//! in `run_pipeline_tick` are the recorded parko-side follow-ups (hardware / ROS2
//! gated).

use crate::uptane::{TargetEntry, VerifiedUpdate};

/// The authorized model digests from a VERIFIED signed targets manifest, verbatim
/// (manifest order, original case). For a policy value use
/// [`model_allowlist_env_value`], which canonicalizes to lowercase to match
/// parko's case-insensitive allow-list parse.
#[must_use]
pub fn authorized_model_digests(verified: &VerifiedUpdate) -> Vec<&str> {
    verified
        .targets()
        .targets
        .iter()
        .map(|t| t.digest_hex.as_str())
        .collect()
}

/// The `KIRRA_MODEL_ALLOWLIST` env value parko's `ModelAllowList::parse` consumes,
/// DERIVED from the signed targets: the authorized digests, **lowercased** (parko
/// parses digests case-insensitively, so canonicalizing here keeps the env policy
/// and the [`is_model_authorized`] lookups consistent), comma-separated. A node
/// sets this for its co-located parko process AFTER `verify_update`, so the model
/// allow-list is a signed fact, not an operator assertion. Empty when the manifest
/// authorizes no targets (→ under parko STRICT mode, deny every model).
#[must_use]
pub fn model_allowlist_env_value(verified: &VerifiedUpdate) -> String {
    verified
        .targets()
        .targets
        .iter()
        .map(|t| t.digest_hex.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(",")
}

/// The authorizing entry (digest + length + version) for `model_digest`, if the
/// verified manifest authorizes it — the signed lineage record for a model the
/// node is about to load. Case-INSENSITIVE (Uptane digests are lowercase hex, but
/// this matches parko's case-insensitive allow-list parse so authorization is
/// consistent regardless of the caller's digest case).
#[must_use]
pub fn authorized_model_entry<'a>(
    verified: &'a VerifiedUpdate,
    model_digest: &str,
) -> Option<&'a TargetEntry> {
    verified
        .targets()
        .targets
        .iter()
        .find(|t| t.digest_hex.eq_ignore_ascii_case(model_digest))
}

/// Is `model_digest` authorized by the verified signed manifest? Case-insensitive
/// (see [`authorized_model_entry`]).
#[must_use]
pub fn is_model_authorized(verified: &VerifiedUpdate, model_digest: &str) -> bool {
    authorized_model_entry(verified, model_digest).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uptane::{
        sign_snapshot, sign_targets, sign_timestamp, verify_update, RootMetadata, SnapshotMetadata,
        TargetsMetadata, TimestampMetadata, TrustedVersions,
    };
    use ed25519_dalek::SigningKey;

    const D1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const D2: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const UNLISTED: &str = "9999999999999999999999999999999999999999999999999999999999999999";
    const EXP: u64 = 10_000;
    const NOW: u64 = 1_000;

    fn model(digest: &str, version: &str) -> TargetEntry {
        TargetEntry {
            digest_hex: digest.to_string(),
            length_bytes: 1024,
            version: version.to_string(),
        }
    }

    /// Mint a consistent role keyset + a signed `targets`/`snapshot`/`timestamp`
    /// set carrying `models`, and return the `VerifiedUpdate` from the REAL
    /// `verify_update`. `VerifiedUpdate` has no public constructor (sealed to prove
    /// provenance), so every projection test obtains one only through verification
    /// — the boundary is real, not mocked.
    fn verified_with(models: Vec<TargetEntry>) -> VerifiedUpdate {
        let root_sk = SigningKey::from_bytes(&[1u8; 32]);
        let targets_sk = SigningKey::from_bytes(&[2u8; 32]);
        let snapshot_sk = SigningKey::from_bytes(&[3u8; 32]);
        let timestamp_sk = SigningKey::from_bytes(&[4u8; 32]);
        let root = RootMetadata {
            version: 1,
            expires_at_ms: EXP,
            root_key: root_sk.verifying_key().to_bytes(),
            targets_key: targets_sk.verifying_key().to_bytes(),
            snapshot_key: snapshot_sk.verifying_key().to_bytes(),
            timestamp_key: timestamp_sk.verifying_key().to_bytes(),
        };
        let targets = TargetsMetadata {
            version: 5,
            expires_at_ms: EXP,
            targets: models,
        };
        let snapshot = SnapshotMetadata {
            version: 5,
            expires_at_ms: EXP,
            targets_version: 5,
        };
        let timestamp = TimestampMetadata {
            version: 5,
            expires_at_ms: EXP,
            snapshot_version: 5,
        };
        verify_update(
            &root,
            TrustedVersions::default(),
            NOW,
            &timestamp,
            &sign_timestamp(&timestamp, &timestamp_sk),
            &snapshot,
            &sign_snapshot(&snapshot, &snapshot_sk),
            &targets,
            &sign_targets(&targets, &targets_sk),
        )
        .expect("a consistent signed manifest verifies")
    }

    #[test]
    fn allowlist_is_the_authorized_digest_set_in_order() {
        let v = verified_with(vec![model(D1, "v1"), model(D2, "v2")]);
        assert_eq!(authorized_model_digests(&v), vec![D1, D2]);
        assert_eq!(model_allowlist_env_value(&v), format!("{D1},{D2}"));
    }

    #[test]
    fn authorization_lookup_is_digest_scoped() {
        let v = verified_with(vec![model(D1, "v1")]);
        assert!(is_model_authorized(&v, D1));
        assert!(!is_model_authorized(&v, UNLISTED));
        assert_eq!(
            authorized_model_entry(&v, D1).map(|e| e.version.as_str()),
            Some("v1")
        );
        assert!(authorized_model_entry(&v, UNLISTED).is_none());
    }

    #[test]
    fn authorization_and_env_value_are_case_insensitive_consistently() {
        // A caller querying with an UPPERCASE digest must get the SAME answer parko
        // would (parko lowercases both sides). The env value is canonicalized to
        // lowercase so the env policy matches the lookup.
        let v = verified_with(vec![model(D1, "v1")]);
        assert!(
            is_model_authorized(&v, &D1.to_ascii_uppercase()),
            "uppercase query matches"
        );
        assert_eq!(
            model_allowlist_env_value(&v),
            D1,
            "env value is lowercase-canonical"
        );
    }

    #[test]
    fn an_empty_manifest_yields_an_empty_allowlist() {
        // Deny-by-default composition (STRICT mode): no authorized model → empty
        // KIRRA_MODEL_ALLOWLIST → under parko strict mode, deny every model.
        let v = verified_with(vec![]);
        assert_eq!(model_allowlist_env_value(&v), "");
        assert!(!is_model_authorized(&v, D1));
    }

    /// The headline: a SIGNED manifest, verified through the real Uptane chain,
    /// drives the allow-list — and a manifest tampered AFTER signing is refused, so
    /// no allow-list is ever derived from unsigned bytes.
    #[test]
    fn a_signed_manifest_drives_the_allowlist_and_tampering_is_refused() {
        let root_sk = SigningKey::from_bytes(&[1u8; 32]);
        let targets_sk = SigningKey::from_bytes(&[2u8; 32]);
        let snapshot_sk = SigningKey::from_bytes(&[3u8; 32]);
        let timestamp_sk = SigningKey::from_bytes(&[4u8; 32]);
        let root = RootMetadata {
            version: 1,
            expires_at_ms: EXP,
            root_key: root_sk.verifying_key().to_bytes(),
            targets_key: targets_sk.verifying_key().to_bytes(),
            snapshot_key: snapshot_sk.verifying_key().to_bytes(),
            timestamp_key: timestamp_sk.verifying_key().to_bytes(),
        };
        let targets = TargetsMetadata {
            version: 7,
            expires_at_ms: EXP,
            targets: vec![model(D1, "v1"), model(D2, "v2")],
        };
        let snapshot = SnapshotMetadata {
            version: 7,
            expires_at_ms: EXP,
            targets_version: 7,
        };
        let timestamp = TimestampMetadata {
            version: 7,
            expires_at_ms: EXP,
            snapshot_version: 7,
        };
        let ts_sig = sign_timestamp(&timestamp, &timestamp_sk);
        let sn_sig = sign_snapshot(&snapshot, &snapshot_sk);
        let tg_sig = sign_targets(&targets, &targets_sk);

        // Verified → the signed allow-list is the manifest's digests.
        let verified = verify_update(
            &root,
            TrustedVersions::default(),
            NOW,
            &timestamp,
            &ts_sig,
            &snapshot,
            &sn_sig,
            &targets,
            &tg_sig,
        )
        .expect("a consistent signed manifest verifies");
        assert_eq!(model_allowlist_env_value(&verified), format!("{D1},{D2}"));

        // Tamper: swap a model digest AFTER signing → the targets signature no
        // longer matches → verify_update refuses → no VerifiedUpdate, no allow-list.
        let mut forged = targets.clone();
        forged.targets[0].digest_hex = UNLISTED.to_string();
        let refused = verify_update(
            &root,
            TrustedVersions::default(),
            NOW,
            &timestamp,
            &ts_sig,
            &snapshot,
            &sn_sig,
            &forged,
            &tg_sig,
        );
        assert!(
            refused.is_err(),
            "a manifest tampered after signing must be refused"
        );
    }
}
