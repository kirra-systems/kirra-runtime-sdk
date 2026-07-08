//! **Model lineage — rollback-to-last-good (WP-24 / G-15 software half, part a).**
//!
//! The [`model_integrity`](crate::model_integrity) allow-list already detects a
//! substituted or corrupted model and returns a fail-closed
//! [`BackendError::IntegrityRejected`]. That answers "is THIS artifact allowed?"
//! but not "what should the node RUN instead?" — a rejection at load time still
//! leaves the doer without a model. This module adds the lineage layer: it tracks
//! the last artifact that verified clean (the **last-good** anchor) and turns a
//! load verdict into an actionable [`LineageDecision`] — commit the new model,
//! roll back to the last-good one, or (deny-by-default) refuse when there is no
//! known-good artifact to fall back to.
//!
//! Fail-closed policy, layered on the integrity verdict:
//!
//! | integrity verdict | last-good known? | decision |
//! |---|---|---|
//! | `Ok { verified: true }`  | — | **`Commit`** (and this digest BECOMES the last-good) |
//! | `Ok { verified: false }` (enforcement OFF) | — | **`Commit`** (accepted, but NOT anchored — only a *verified* model becomes a rollback target) |
//! | `Err(IntegrityRejected)` | yes | **`Rollback { to: last_good }`** |
//! | `Err(IntegrityRejected)` | no  | **`Deny`** — deny-by-default: nothing proven-good to run |
//! | `Err(_)` (I/O / init) | any | **`Deny`** — a model that cannot even be hashed cannot be trusted |
//!
//! Pure and self-contained (a small state machine over the integrity verdict, no
//! I/O of its own), so the rollback logic is fully unit-testable; the backend
//! wiring that actually re-loads the last-good artifact on a `Rollback` is the
//! recorded follow-up, as is binding the allow-list to a signed (Uptane targets)
//! manifest rather than a raw digest set.

use crate::backend::BackendError;
use crate::model_integrity::VerifiedModel;

/// What the node should do with a model load, given the integrity verdict and the
/// lineage's last-good anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineageDecision {
    /// Run this model. `verified` mirrors the integrity verdict (a `false` here
    /// means enforcement was off — accepted but not anchored as last-good).
    Commit { digest: String, verified: bool },
    /// The presented model was rejected; fall back to the last-good digest.
    Rollback { to: String, rejected: String },
    /// The presented model was rejected and there is no known-good fallback —
    /// deny-by-default (fail closed; the doer gets no model, the checker holds).
    Deny { reason: String },
}

impl LineageDecision {
    /// Does this decision permit a model to run (either the new one or the
    /// rolled-back one)?
    #[must_use]
    pub fn admits_model(&self) -> bool {
        matches!(self, LineageDecision::Commit { .. } | LineageDecision::Rollback { .. })
    }
}

/// Tracks the last artifact that verified clean, so a later rejection can roll
/// back to it. Construct fresh (`default`) or seeded with a known-good digest
/// (`with_last_good`, e.g. the digest baked into a signed release manifest).
#[derive(Debug, Clone, Default)]
pub struct ModelLineage {
    last_good: Option<String>,
}

impl ModelLineage {
    /// A lineage seeded with a known-good digest (e.g. the factory/last-release
    /// model), so the very first rejection already has a rollback target.
    #[must_use]
    pub fn with_last_good(digest: impl Into<String>) -> Self {
        Self { last_good: Some(digest.into()) }
    }

    /// The current last-good digest, if any.
    #[must_use]
    pub fn last_good(&self) -> Option<&str> {
        self.last_good.as_deref()
    }

    /// Fold an integrity verdict into a lineage decision, advancing the last-good
    /// anchor when a model verifies clean. Consumes the verdict (`BackendError`
    /// is not `Clone`); returns an owned decision.
    pub fn admit(&mut self, verdict: Result<VerifiedModel, BackendError>) -> LineageDecision {
        match verdict {
            Ok(VerifiedModel { sha256_hex, verified: true }) => {
                // A clean, allow-listed model: run it AND make it the rollback anchor.
                self.last_good = Some(sha256_hex.clone());
                LineageDecision::Commit { digest: sha256_hex, verified: true }
            }
            Ok(VerifiedModel { sha256_hex, verified: false }) => {
                // Enforcement off: accept (byte-identical to today) but do NOT
                // anchor — an unverified model must never become a rollback target.
                LineageDecision::Commit { digest: sha256_hex, verified: false }
            }
            Err(BackendError::IntegrityRejected { sha256, .. }) => match &self.last_good {
                Some(good) => LineageDecision::Rollback { to: good.clone(), rejected: sha256 },
                None => LineageDecision::Deny {
                    reason: format!(
                        "model {sha256} rejected and no known-good artifact to roll back to"
                    ),
                },
            },
            Err(other) => LineageDecision::Deny {
                reason: format!("model load failed and cannot be trusted: {other}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verified(digest: &str) -> Result<VerifiedModel, BackendError> {
        Ok(VerifiedModel { sha256_hex: digest.into(), verified: true })
    }
    fn rejected(digest: &str) -> Result<VerifiedModel, BackendError> {
        Err(BackendError::IntegrityRejected { path: "m.onnx".into(), sha256: digest.into() })
    }

    #[test]
    fn a_verified_model_commits_and_becomes_last_good() {
        let mut lin = ModelLineage::default();
        assert_eq!(lin.last_good(), None);
        let d = lin.admit(verified("aa"));
        assert_eq!(d, LineageDecision::Commit { digest: "aa".into(), verified: true });
        assert_eq!(lin.last_good(), Some("aa"));
    }

    #[test]
    fn a_substituted_model_rolls_back_to_the_last_good() {
        let mut lin = ModelLineage::default();
        lin.admit(verified("good1"));
        let d = lin.admit(rejected("EVIL"));
        assert_eq!(d, LineageDecision::Rollback { to: "good1".into(), rejected: "EVIL".into() });
        assert!(d.admits_model(), "a rollback still runs a (good) model");
        // The rejection did NOT move the anchor.
        assert_eq!(lin.last_good(), Some("good1"));
    }

    #[test]
    fn rejection_with_no_known_good_denies_by_default() {
        let mut lin = ModelLineage::default();
        let d = lin.admit(rejected("EVIL"));
        assert!(matches!(d, LineageDecision::Deny { .. }), "no fallback ⇒ deny: {d:?}");
        assert!(!d.admits_model());
    }

    #[test]
    fn last_good_advances_across_a_sequence_of_clean_models() {
        let mut lin = ModelLineage::default();
        lin.admit(verified("v1"));
        lin.admit(verified("v2"));
        assert_eq!(lin.last_good(), Some("v2"));
        // A later rejection rolls back to the MOST RECENT good, not the first.
        let d = lin.admit(rejected("v3-bad"));
        assert_eq!(d, LineageDecision::Rollback { to: "v2".into(), rejected: "v3-bad".into() });
    }

    #[test]
    fn enforcement_off_commits_but_does_not_anchor() {
        let mut lin = ModelLineage::default();
        let d = lin.admit(Ok(VerifiedModel { sha256_hex: "x".into(), verified: false }));
        assert_eq!(d, LineageDecision::Commit { digest: "x".into(), verified: false });
        // Unverified must not become a rollback target.
        assert_eq!(lin.last_good(), None);
    }

    #[test]
    fn seeded_last_good_gives_the_first_rejection_a_target() {
        let mut lin = ModelLineage::with_last_good("factory");
        let d = lin.admit(rejected("bad"));
        assert_eq!(d, LineageDecision::Rollback { to: "factory".into(), rejected: "bad".into() });
    }

    #[test]
    fn an_unhashable_model_denies_regardless_of_last_good() {
        let mut lin = ModelLineage::with_last_good("factory");
        let d = lin.admit(Err(BackendError::Io("disk gone".into())));
        assert!(matches!(d, LineageDecision::Deny { .. }), "I/O failure ⇒ deny: {d:?}");
    }
}
