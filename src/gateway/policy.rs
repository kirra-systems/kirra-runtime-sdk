// src/gateway/policy.rs
//
// ADR-0035 Stage 0a: the command-classification types (`OperationalCommand` +
// `classify_http_command` + the private path allowlists) were extracted VERBATIM
// into the zero-dependency `kirra-policy-types` leaf crate. This module is now a
// re-export shim so every existing `crate::gateway::policy::{OperationalCommand,
// classify_http_command}` path (posture gate, industrial adapters, tests)
// resolves unchanged.
pub use kirra_policy_types::{classify_http_command, OperationalCommand};

#[cfg(test)]
mod tests {
    use super::*;

    /// FAIL-CLOSED outcome end-to-end: an unrecognised write path classified as
    /// `Unknown` is denied by `should_route_command` in EVERY posture —
    /// including Nominal, where `WriteState` would have been permitted. This is
    /// the safety consequence the classification change buys.
    ///
    /// This CROSS-LAYER test stays in the root crate (not `kirra-policy-types`)
    /// because it exercises `should_route_command` from `posture_cache`, which
    /// sits ABOVE the policy-types leaf in the ADR-0035 dependency DAG. The pure
    /// classifier tests live with the code in `kirra-policy-types`.
    /// Verifies REQ: unknown-path-denied-allowlist (#69 / SG-006) end-to-end.
    #[test]
    fn test_unrecognized_write_path_denied_in_all_postures() {
        use crate::posture_cache::{
            should_route_command, CachedFleetPosture, POSTURE_CACHE_TTL_MS,
        };
        use kirra_core::FleetPosture;

        let cmd = classify_http_command("POST", "/admin/shutdown");
        assert_eq!(cmd, OperationalCommand::Unknown);

        let now = 1_000_000u64;
        for posture in [
            FleetPosture::Nominal,
            FleetPosture::Degraded,
            FleetPosture::LockedOut,
        ] {
            let cache = Some(CachedFleetPosture {
                posture,
                generated_at_ms: now,
                ttl_ms: POSTURE_CACHE_TTL_MS,
                generation: 1,
                epoch: 0,
            });
            assert!(
                !should_route_command(&cache, now, cmd),
                "Unknown write path must be denied under {posture:?} (fail-closed, no silent proceed)"
            );
        }
    }
}
