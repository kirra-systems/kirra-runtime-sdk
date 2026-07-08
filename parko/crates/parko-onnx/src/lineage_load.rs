// crates/parko-onnx/src/lineage_load.rs
//
// EP-04 (M1) — lineage-supervised model loading: the live consumer of
// `parko_core::model_lineage`.
//
// The #G16 integrity gate answers "is THIS artifact allowed?"; the lineage layer
// answers "what should the node RUN instead?" when the answer is no. Because an
// `ort::Session` is committed from the model path at CONSTRUCTION time, the
// rollback seam lives at construction: `LineageLoader::resolve` decides — from
// the integrity verdict + the lineage state — WHICH artifact path to build the
// session from, and `OrtBackend::new_with_lineage` builds it.
//
// Fail-closed table (composing `verify_model_file` × `ModelLineage::admit`):
//
// | integrity verdict          | lineage state    | resolved load                  |
// |----------------------------|------------------|--------------------------------|
// | Ok, verified=true          | —                | requested path (and it becomes |
// |                            |                  | the rollback anchor + ledger)  |
// | Ok, verified=false (off)   | —                | requested path (NOT anchored)  |
// | Err(IntegrityRejected)     | last-good known  | last-good path — RE-VERIFIED   |
// |                            |                  | against its recorded digest    |
// |                            |                  | (tampered fallback ⇒ deny)     |
// | Err(IntegrityRejected)     | no last-good     | deny (no model runs)           |
// | Err(other: unreadable/IO)  | any              | deny (cannot be trusted)       |
//
// The resolve step is PURE of ORT (integrity + lineage + a digest→path ledger),
// so the whole decision table is unit-tested in the default build — no
// onnxruntime dylib needed. Only `new_with_lineage`'s session build requires the
// runtime (exercised in the ORT CI lane like every other session test).

use std::collections::HashMap;

use parko_core::backend::BackendError;
use parko_core::model_integrity::{verify_model_file, ModelAllowList};
use parko_core::model_lineage::{LineageDecision, ModelLineage};

/// The lineage + the digest→path ledger a node keeps beside its model directory.
/// `ModelLineage` anchors *digests* (the trust identity); the ledger remembers
/// where each verified digest's bytes live so a rollback can actually re-load.
#[derive(Debug, Default)]
pub struct LineageLoader {
    lineage: ModelLineage,
    /// digest (lowercase SHA-256 hex) → filesystem path of the artifact that
    /// carried it when it verified clean. Only `verified == true` commits are
    /// recorded — an unverified artifact must never become a rollback target
    /// (the same rule `ModelLineage` enforces for its anchor).
    paths_by_digest: HashMap<String, String>,
}

/// A resolved load: the artifact path the backend should build its session
/// from, plus the lineage decision that selected it (for logging/telemetry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLoad {
    pub path: String,
    pub decision: LineageDecision,
}

impl LineageLoader {
    /// A fresh loader (no known-good yet: the FIRST load must verify clean or
    /// enforcement must be off — a first-load rejection is a deny-by-default).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed with a known-good artifact (e.g. the factory model), so even the
    /// very first rejection has a rollback target.
    #[must_use]
    pub fn with_last_good(digest: impl Into<String>, path: impl Into<String>) -> Self {
        let digest = digest.into();
        let mut paths_by_digest = HashMap::new();
        paths_by_digest.insert(digest.clone(), path.into());
        Self { lineage: ModelLineage::with_last_good(digest), paths_by_digest }
    }

    /// Decide which artifact to load for `requested_path` under `allow` — the
    /// full fail-closed table from the module header. Pure of ORT; the injected
    /// allow-list keeps this testable without env mutation (INVARIANT #13).
    pub fn resolve(
        &mut self,
        requested_path: &str,
        allow: &ModelAllowList,
    ) -> Result<ResolvedLoad, BackendError> {
        let verdict = verify_model_file(requested_path, allow);
        let decision = self.lineage.admit(verdict);
        match &decision {
            LineageDecision::Commit { digest, verified } => {
                if *verified {
                    // Remember where this verified digest's bytes live so a
                    // LATER rejection can roll back to them.
                    self.paths_by_digest.insert(digest.clone(), requested_path.to_string());
                }
                Ok(ResolvedLoad { path: requested_path.to_string(), decision })
            }
            LineageDecision::Rollback { to, rejected } => {
                let Some(fallback_path) = self.paths_by_digest.get(to).cloned() else {
                    // Anchor digest with no ledger entry (e.g. seeded lineage
                    // without a path) — nothing loadable ⇒ fail closed.
                    return Err(BackendError::IntegrityRejected {
                        path: requested_path.to_string(),
                        sha256: rejected.clone(),
                    });
                };
                // RE-VERIFY the fallback bytes still carry the anchored digest —
                // the last-good file could itself have been tampered since it was
                // recorded. A drifted fallback is a deny, never a silent load.
                match verify_model_file(&fallback_path, allow) {
                    Ok(v) if v.sha256_hex == *to => {
                        tracing::warn!(
                            rejected_path = requested_path,
                            rejected_sha256 = %rejected,
                            fallback_path = %fallback_path,
                            fallback_sha256 = %to,
                            "model lineage ROLLBACK: rejected artifact; loading last-good"
                        );
                        Ok(ResolvedLoad { path: fallback_path, decision })
                    }
                    _ => Err(BackendError::IntegrityRejected {
                        path: fallback_path,
                        sha256: to.clone(),
                    }),
                }
            }
            LineageDecision::Deny { reason } => Err(BackendError::InitializationError(format!(
                "model lineage DENY (fail-closed, no model runs): {reason}"
            ))),
        }
    }

    /// The current last-good digest, if any (observability).
    #[must_use]
    pub fn last_good(&self) -> Option<&str> {
        self.lineage.last_good()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parko_core::model_integrity::sha256_file;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Unique temp path per call (parallel tests; no env mutation, no RNG) — the
    // same pattern as parko-core's model_integrity tests.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    fn write_temp(content: &[u8]) -> std::path::PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("parko_ep04_{}_{n}.onnx", std::process::id()));
        std::fs::File::create(&p).unwrap().write_all(content).unwrap();
        p
    }
    fn digest_of(p: &std::path::Path) -> String {
        sha256_file(p).unwrap()
    }

    #[test]
    fn clean_load_commits_and_records_the_rollback_target() {
        let good = write_temp(b"model-v1");
        let allow = ModelAllowList::from_parts([digest_of(&good)], false);
        let mut loader = LineageLoader::new();
        let r = loader.resolve(good.to_str().unwrap(), &allow).unwrap();
        assert_eq!(r.path, good.to_str().unwrap());
        assert!(matches!(r.decision, LineageDecision::Commit { verified: true, .. }));
        assert_eq!(loader.last_good().unwrap(), digest_of(&good));
        std::fs::remove_file(&good).ok();
    }

    #[test]
    fn a_rejected_artifact_rolls_back_to_the_last_good_path() {
        let good = write_temp(b"model-v1");
        let evil = write_temp(b"model-v2-EVIL");
        let allow = ModelAllowList::from_parts([digest_of(&good)], false);
        let mut loader = LineageLoader::new();
        loader.resolve(good.to_str().unwrap(), &allow).unwrap();

        let r = loader.resolve(evil.to_str().unwrap(), &allow).unwrap();
        assert_eq!(r.path, good.to_str().unwrap(), "rollback resolves to the last-good bytes");
        assert!(matches!(r.decision, LineageDecision::Rollback { .. }));
        std::fs::remove_file(&good).ok();
        std::fs::remove_file(&evil).ok();
    }

    #[test]
    fn a_tampered_rollback_target_is_denied_not_silently_loaded() {
        let good = write_temp(b"model-v1");
        let evil = write_temp(b"model-v2-EVIL");
        let allow = ModelAllowList::from_parts([digest_of(&good)], false);
        let mut loader = LineageLoader::new();
        loader.resolve(good.to_str().unwrap(), &allow).unwrap();
        // Tamper the LAST-GOOD file after it was recorded.
        std::fs::File::create(&good).unwrap().write_all(b"tampered-behind-our-back").unwrap();

        let err = loader.resolve(evil.to_str().unwrap(), &allow).unwrap_err();
        assert!(
            matches!(err, BackendError::IntegrityRejected { .. }),
            "a drifted fallback must fail closed, got {err:?}"
        );
        std::fs::remove_file(&good).ok();
        std::fs::remove_file(&evil).ok();
    }

    #[test]
    fn first_load_rejection_with_no_known_good_denies() {
        let evil = write_temp(b"unlisted");
        let allow = ModelAllowList::from_parts([format!("{:0>64}", "a")], true);
        let mut loader = LineageLoader::new();
        let err = loader.resolve(evil.to_str().unwrap(), &allow).unwrap_err();
        assert!(matches!(err, BackendError::InitializationError(_)), "deny-by-default: {err:?}");
        std::fs::remove_file(&evil).ok();
    }

    #[test]
    fn enforcement_off_commits_but_never_becomes_a_rollback_target() {
        let m = write_temp(b"anything");
        let off = ModelAllowList::from_parts(Vec::<String>::new(), false);
        let mut loader = LineageLoader::new();
        let r = loader.resolve(m.to_str().unwrap(), &off).unwrap();
        assert!(matches!(r.decision, LineageDecision::Commit { verified: false, .. }));
        assert!(loader.last_good().is_none(), "unverified never anchors");

        // Now strict enforcement rejects a substituted file: with no verified
        // anchor there is nothing to roll back to → deny.
        let strict = ModelAllowList::from_parts(Vec::<String>::new(), true);
        let evil = write_temp(b"evil");
        let err = loader.resolve(evil.to_str().unwrap(), &strict).unwrap_err();
        assert!(matches!(err, BackendError::InitializationError(_)));
        std::fs::remove_file(&m).ok();
        std::fs::remove_file(&evil).ok();
    }

    #[test]
    fn seeded_last_good_without_a_ledger_path_fails_closed() {
        // A lineage seeded with a digest but whose ledger path is MISSING (only
        // possible via a future constructor misuse) must deny, not panic. Here we
        // seed properly then remove the file so re-verification fails.
        let factory = write_temp(b"factory-model");
        let factory_digest = digest_of(&factory);
        let allow = ModelAllowList::from_parts([factory_digest.clone()], false);
        let mut loader =
            LineageLoader::with_last_good(factory_digest, factory.to_str().unwrap());
        std::fs::remove_file(&factory).ok(); // the fallback bytes vanish

        let evil = write_temp(b"evil");
        let err = loader.resolve(evil.to_str().unwrap(), &allow).unwrap_err();
        assert!(matches!(err, BackendError::Io(_) | BackendError::IntegrityRejected { .. }));
        std::fs::remove_file(&evil).ok();
    }
}
