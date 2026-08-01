//! Sidecar capability advertisement — "healthy" is not "compatible".
//!
//! During R2 bring-up the installed `/opt/kirra/taj_service` answered
//! `GET /health` with `{"status":"ok"}` while returning the LEGACY perception
//! shape: no `frame_id`, so no `profile_digest` and no `evidence_digest`. The
//! diagnostics read the 200 and reported Taj healthy. It was healthy — and
//! incompatible with the checked-out consumer, which needs the evidence-bound
//! V2 chain. The operator lost time to a service that was working perfectly at
//! answering the wrong question.
//!
//! A liveness probe cannot detect a stale binary, because a stale binary is
//! alive. So every separately-installed Kirra artifact advertises WHAT IT CAN
//! DO, not merely that it is up:
//!
//! ```text
//! GET /capabilities → {"service":"taj","contract":2,"capabilities":["perception.v2",…],
//!                      "pkg_version":"…","git_describe":"…"}
//! ```
//!
//! `contract` is the load-bearing field: a single integer the consumer and the
//! doctor compare against what they require. It changes ONLY when the wire
//! contract changes, so it is meaningful across rebuilds in a way a version
//! string is not.
//!
//! Deliberately NOT `strings` on the binary — the task called that out and it
//! is right: it reads bytes that were never a promise, and it cannot
//! distinguish "this symbol exists" from "this code path is reachable".

use serde::{Deserialize, Serialize};

/// The perception wire contract this build speaks.
///
/// * `1` — legacy: `{"healthy","speed_cap_mps","left",…}`, no `frame_id`.
/// * `2` — evidence-bound: adds `frame_id{profile_digest,evidence_digest}`,
///   which is what binds a perception frame to the profile that produced it.
///
/// BUMP ONLY on a wire-contract change. A version bump that does not change
/// what a peer must understand must NOT touch this, or it becomes noise and
/// gets ignored — which is how the legacy binary survived in the first place.
pub const TAJ_CONTRACT: u32 = 2;

/// The minimum Taj contract a current consumer/doctor can work with.
pub const TAJ_MIN_SUPPORTED_CONTRACT: u32 = 2;

/// Capability tokens. Additive and never renamed: a peer matches on presence,
/// so removing one is a breaking change and reusing one for a different meaning
/// is worse.
pub const CAP_PERCEPTION_V2: &str = "perception.v2";
pub const CAP_FRAME_ID: &str = "frame_id";
pub const CAP_EVIDENCE_DIGEST: &str = "evidence_digest";

/// What a service advertises about itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub service: String,
    pub contract: u32,
    pub capabilities: Vec<String>,
    pub pkg_version: String,
}

impl Capabilities {
    /// This build's Taj advertisement.
    #[must_use]
    pub fn taj() -> Self {
        Self {
            service: "taj".to_string(),
            contract: TAJ_CONTRACT,
            capabilities: vec![
                CAP_PERCEPTION_V2.to_string(),
                CAP_FRAME_ID.to_string(),
                CAP_EVIDENCE_DIGEST.to_string(),
            ],
            pkg_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    #[must_use]
    pub fn has(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }
}

/// What a probe concluded about an installed peer. Three states, deliberately
/// distinct — collapsing the middle one is the bug this module exists for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Compatibility {
    /// Nothing answered — not running, wrong port, crashed.
    Unavailable,
    /// Alive and speaking a contract we support.
    Current { contract: u32 },
    /// Alive, healthy, and TOO OLD. The live failure.
    Legacy { contract: u32, required: u32 },
}

impl Compatibility {
    #[must_use]
    pub fn is_usable(&self) -> bool {
        matches!(self, Self::Current { .. })
    }

    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Unavailable => "service unavailable (nothing answered)".into(),
            Self::Current { contract } => format!("healthy and contract v{contract} — current"),
            Self::Legacy { contract, required } => format!(
                "healthy but LEGACY: speaks contract v{contract}, this build needs \
                 v{required} — rebuild and reinstall the binary"
            ),
        }
    }
}

/// Classify a peer from its `/capabilities` reply, if it gave one.
///
/// `None` means the endpoint was absent (404) or unparseable — which is itself
/// evidence of a pre-capability build, so it classifies as `Legacy` at contract
/// 1 rather than `Unavailable`. That distinction matters: "did not answer at
/// all" and "answered, but is old" call for different operator actions.
#[must_use]
pub fn classify(reply: Option<&str>, reachable: bool, required: u32) -> Compatibility {
    if !reachable {
        return Compatibility::Unavailable;
    }
    let contract = reply
        .and_then(|r| serde_json::from_str::<Capabilities>(r).ok())
        .map_or(1, |c| c.contract); // reachable but no/!parseable caps ⇒ pre-capability build
    if contract >= required {
        Compatibility::Current { contract }
    } else {
        Compatibility::Legacy { contract, required }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_build_advertises_the_v2_perception_contract() {
        let c = Capabilities::taj();
        assert_eq!(c.contract, TAJ_CONTRACT);
        assert!(c.has(CAP_FRAME_ID) && c.has(CAP_EVIDENCE_DIGEST) && c.has(CAP_PERCEPTION_V2));
        assert!(c.to_json().contains("\"contract\":2"));
    }

    #[test]
    fn a_healthy_legacy_taj_is_reported_incompatible_not_current() {
        // THE live failure: /health said ok, the perception reply had no
        // frame_id, and diagnostics called it healthy. A pre-capability build
        // has no /capabilities either — reachable, unparseable, contract 1.
        let v = classify(None, true, TAJ_MIN_SUPPORTED_CONTRACT);
        assert_eq!(
            v,
            Compatibility::Legacy {
                contract: 1,
                required: 2
            }
        );
        assert!(!v.is_usable(), "a legacy peer must not read as usable");
        assert!(v.summary().contains("LEGACY"), "{}", v.summary());
        assert!(v.summary().contains("rebuild"), "must name the action");
    }

    #[test]
    fn a_current_taj_is_usable() {
        let v = classify(
            Some(&Capabilities::taj().to_json()),
            true,
            TAJ_MIN_SUPPORTED_CONTRACT,
        );
        assert_eq!(v, Compatibility::Current { contract: 2 });
        assert!(v.is_usable());
    }

    #[test]
    fn unreachable_is_its_own_state_not_legacy() {
        let v = classify(None, false, TAJ_MIN_SUPPORTED_CONTRACT);
        assert_eq!(v, Compatibility::Unavailable);
        assert!(!v.is_usable());
        assert!(v.summary().contains("unavailable"));
    }

    #[test]
    fn a_future_contract_is_usable_not_a_failure() {
        // Forward compatibility: a NEWER peer still satisfies our minimum.
        let newer = r#"{"service":"taj","contract":9,"capabilities":[],"pkg_version":"x"}"#;
        assert!(classify(Some(newer), true, TAJ_MIN_SUPPORTED_CONTRACT).is_usable());
    }

    #[test]
    fn garbage_from_a_reachable_peer_is_legacy_not_a_crash() {
        for junk in ["", "not json", "{}", "{\"contract\":\"two\"}"] {
            let v = classify(Some(junk), true, TAJ_MIN_SUPPORTED_CONTRACT);
            assert!(!v.is_usable(), "junk {junk:?} must not read as current");
        }
    }

    #[test]
    fn the_minimum_never_exceeds_what_this_build_advertises() {
        // Both sides are consts, so evaluate at compile time (and keep newer
        // clippy's assertions_on_constants happy without weakening the check).
        const {
            assert!(
                TAJ_MIN_SUPPORTED_CONTRACT <= TAJ_CONTRACT,
                "this build could not satisfy its own requirement"
            );
        }
    }
}
