// src/gateway/policy.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationalCommand {
    /// Safe reads: telemetry, metrics, health probes. Allowed in all postures.
    ReadTelemetry,
    /// Actuator writes and velocity commands. Denied when LockedOut.
    WriteState,
    /// Actuator motion command on the ONE route gated by an inner kinematic
    /// safety envelope (`/actuator/motion/command`). Allowed under Nominal AND
    /// Degraded — under Degraded the inner `enforce_degraded_decel_to_stop` gate
    /// (decel-to-stop-and-HOLD over the MRC 5.0 m/s envelope) authors the safe
    /// verdict, so the OUTER posture gate defers to it instead of returning a
    /// blanket 503 (Option A / ADR-0011). Denied under LockedOut and on a
    /// stale/cold cache (fail-closed), exactly like any other state write — and
    /// it is still treated as a state mutation by the HA epoch fence.
    ActuatorMotion,
    /// Firmware, reboot, config mutations. Denied unless Nominal.
    SystemMutation,
    /// Unrecognised HTTP method. Denied in ALL postures (fail-closed).
    Unknown,
}

/// Classifies an HTTP request into an OperationalCommand based solely on method
/// and path prefix. No state access — pure function, always total.
// SAFETY: SG7 SG9 | REQ: doer-agnostic-classification,unknown-path-denied-allowlist | TEST: sg7_doer_agnostic_verdict_byte_identical_across_ingress_paths,test_safety_goal_sg_006_unknown_command_denial,test_unrecognized_write_path_is_unknown,test_known_write_paths_preserved
// (No `source` field in the signature: the classifier is path/method-only,
//  so teleop vs planner ingress produces identical OperationalCommands —
//  SG7. Unknown HTTP method maps to OperationalCommand::Unknown which
//  feeds SG9 fail-closed at should_route_command.)
//
// #69 / SG-006: write-method (POST/PUT) requests are classified ONLY against
// an explicit known-path allowlist. An UNRECOGNISED write path falls through
// to `Unknown` (denied in ALL postures, incl. Nominal) — NOT to `WriteState`
// (which Nominal permits). The pre-#69 catch-all returned `WriteState` for any
// unlisted path, so a future `POST /admin/...` would have silently passed the
// posture gate. A new write route must now be added to `is_write_state_path`
// or `is_system_mutation_path` below to be routable at all — fail-closed by
// construction.
pub fn classify_http_command(method: &str, path: &str) -> OperationalCommand {
    // Match against the path only — strip any query string / fragment so the
    // exact-match allowlist below is robust regardless of whether the caller
    // pre-trimmed the URI. (`enforce_posture_routing` passes `uri().path()`,
    // which is already query-free; this keeps the pure function correct for
    // any caller.)
    let path = path.split(['?', '#']).next().unwrap_or(path);

    match method {
        "GET" | "HEAD" => OperationalCommand::ReadTelemetry,

        "DELETE" => OperationalCommand::SystemMutation,

        "POST" | "PUT" => {
            if path == "/actuator/motion/command" {
                // The ONLY actuator route mounted with the inner
                // `enforce_actuator_safety_envelope` decel-to-stop gate (see
                // `build_app`). Classed distinctly so the outer posture gate can
                // DEFER the Degraded verdict to that inner kinematic gate
                // (Option A / ADR-0011) instead of a blanket 503 — while every
                // other WriteState path stays fail-closed under Degraded. Exact
                // match (not a `/actuator` prefix) so a future actuator route
                // without an inner gate cannot inherit the relaxation: it falls
                // to the `is_write_state_path` arm and stays fail-closed.
                OperationalCommand::ActuatorMotion
            } else if is_write_state_path(path) {
                OperationalCommand::WriteState
            } else if is_system_mutation_path(path) {
                OperationalCommand::SystemMutation
            } else {
                // #69 / SG-006 fail-closed: an unrecognised write path is
                // NOT a state mutation we know how to gate — refuse it in
                // every posture rather than defaulting to WriteState.
                OperationalCommand::Unknown
            }
        }

        _ => OperationalCommand::Unknown,
    }
}

/// Allowlist of known state-writing (`WriteState`) POST/PUT paths.
///
/// Actuator / velocity commands plus the control-plane state-write endpoints
/// actually registered on the verifier router (see
/// `src/bin/kirra_verifier_service.rs`). Path-parameter routes
/// (`/attestation/challenge/{node_id}`, `/fabric/command/{asset_id}`) are
/// matched by their fixed prefix; every other entry is an exact match so an
/// unlisted sibling path cannot ride in on a loose prefix.
fn is_write_state_path(path: &str) -> bool {
    // Actuator + velocity commands (physical state writes).
    if path.starts_with("/actuator") || path == "/cmd_vel" || path.starts_with("/cmd_vel/") {
        return true;
    }
    // Path-parameterised control-plane writes (prefix is fixed; the trailing
    // segment is the {node_id} / {asset_id} / {principal_id} parameter).
    if path.starts_with("/attestation/challenge/")
        || path.starts_with("/fabric/command/")
        // WS-1 (#G7) — `/system/principals/{principal_id}/revoke`.
        || path.starts_with("/system/principals/")
        // WS-1 (#G7) Track 1.2 — `/system/cert-principals/{principal_id}/revoke`.
        || path.starts_with("/system/cert-principals/")
    {
        return true;
    }
    // Exact control-plane state-write endpoints.
    matches!(
        path,
        "/action_filter/evaluate"
            | "/attestation/identity/register"
            | "/attestation/register"
            | "/attestation/verify"
            | "/fabric/assets/register"
            | "/federation/controllers/register"
            | "/federation/reports/submit"
            | "/fleet/assets/register"
            | "/fleet/dependencies"
            | "/fleet/diagnostics/report"
            | "/industrial/evaluate"
            | "/industrial/canopen/evaluate"
            | "/industrial/dnp3/evaluate"
            | "/industrial/ethernet-ip/evaluate"
            | "/system/backup/export"
            | "/system/audit/rotate-signing-key"
            // WS-1 (#G7) — mint an API principal (POST). The parameterised revoke
            // is matched by the `/system/principals/` prefix above.
            | "/system/principals"
            // WS-1 (#G7) Track 1.2 — register a cert principal (POST). The revoke is
            // matched by the `/system/cert-principals/` prefix above.
            | "/system/cert-principals"
    )
}

/// Allowlist of known infrastructure-mutation (`SystemMutation`) POST/PUT paths
/// (firmware, reboot, config). `SystemMutation` and `WriteState` gate
/// identically today (both Nominal-only); the split is retained for intent and
/// future divergence.
fn is_system_mutation_path(path: &str) -> bool {
    path.starts_with("/firmware")
        || path == "/reboot"
        || path.starts_with("/reboot/")
        || path.starts_with("/config")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classifies_read_telemetry() {
        assert_eq!(
            classify_http_command("GET", "/telemetry/status"),
            OperationalCommand::ReadTelemetry
        );
        assert_eq!(
            classify_http_command("GET", "/metrics"),
            OperationalCommand::ReadTelemetry
        );
        assert_eq!(
            classify_http_command("GET", "/health/live"),
            OperationalCommand::ReadTelemetry
        );
    }

    #[test]
    fn test_classifies_cmd_vel_as_write_state() {
        assert_eq!(
            classify_http_command("POST", "/cmd_vel"),
            OperationalCommand::WriteState
        );
    }

    #[test]
    fn test_classifies_actuator_as_write_state() {
        assert_eq!(
            classify_http_command("POST", "/actuator/servo"),
            OperationalCommand::WriteState
        );
        assert_eq!(
            classify_http_command("PUT", "/actuator/valve"),
            OperationalCommand::WriteState
        );
    }

    /// Option A / ADR-0011: the ONE actuator route mounted behind the inner
    /// `enforce_actuator_safety_envelope` decel gate classifies as the distinct
    /// `ActuatorMotion`, so the outer posture gate defers its Degraded verdict to
    /// that inner gate instead of a blanket 503. Match is EXACT — a sibling
    /// `/actuator/*` path without an inner gate must stay `WriteState`
    /// (fail-closed under Degraded), so the relaxation cannot leak to a route
    /// that has no kinematic envelope behind it.
    #[test]
    fn test_actuator_motion_command_classifies_as_actuator_motion() {
        assert_eq!(
            classify_http_command("POST", "/actuator/motion/command"),
            OperationalCommand::ActuatorMotion,
            "the inner-gated actuator route must classify as ActuatorMotion (Option A)"
        );
        // Query string must not defeat the exact match.
        assert_eq!(
            classify_http_command("POST", "/actuator/motion/command?dry_run=1"),
            OperationalCommand::ActuatorMotion
        );
        // Sibling actuator paths (NO inner gate mounted) stay fail-closed WriteState.
        assert_eq!(
            classify_http_command("POST", "/actuator/motion/command/extra"),
            OperationalCommand::WriteState,
            "a non-exact actuator sub-path must NOT inherit the ActuatorMotion relaxation"
        );
        assert_eq!(
            classify_http_command("POST", "/actuator/servo"),
            OperationalCommand::WriteState
        );
    }

    #[test]
    fn test_classifies_api_principal_writes() {
        // WS-1 (#G7): the principal mint (exact) and the parameterised revoke must
        // classify as WriteState — NOT Unknown (which the outer posture gate denies
        // in every posture, incl. Nominal — that would brick the routes).
        assert_eq!(
            classify_http_command("POST", "/system/principals"),
            OperationalCommand::WriteState
        );
        assert_eq!(
            classify_http_command("POST", "/system/principals/svc-a/revoke"),
            OperationalCommand::WriteState
        );
        // The GET list is a read.
        assert_eq!(
            classify_http_command("GET", "/system/principals"),
            OperationalCommand::ReadTelemetry
        );
    }

    #[test]
    fn test_classifies_cert_principal_writes() {
        // WS-1 (#G7) Track 1.2: mTLS cert-principal register (exact) + parameterised
        // revoke are WriteState; the list GET is a read. The `/system/principals`
        // classifier must NOT accidentally swallow `/system/cert-principals`.
        assert_eq!(
            classify_http_command("POST", "/system/cert-principals"),
            OperationalCommand::WriteState
        );
        assert_eq!(
            classify_http_command("POST", "/system/cert-principals/svc-a/revoke"),
            OperationalCommand::WriteState
        );
        assert_eq!(
            classify_http_command("GET", "/system/cert-principals"),
            OperationalCommand::ReadTelemetry
        );
    }

    #[test]
    fn test_classifies_system_mutations() {
        assert_eq!(
            classify_http_command("POST", "/firmware/update"),
            OperationalCommand::SystemMutation
        );
        assert_eq!(
            classify_http_command("POST", "/reboot"),
            OperationalCommand::SystemMutation
        );
        assert_eq!(
            classify_http_command("PUT", "/config/network"),
            OperationalCommand::SystemMutation
        );
        assert_eq!(
            classify_http_command("DELETE", "/anything"),
            OperationalCommand::SystemMutation
        );
    }

    #[test]
    fn test_unknown_method_classifies_as_unknown() {
        // Unknown HTTP methods map to OperationalCommand::Unknown, which is
        // denied in ALL posture states including Nominal — closing the implicit
        // fallback bypass identified in the v1 gateway policy specification.
        assert_eq!(
            classify_http_command("PATCH", "/unknown"),
            OperationalCommand::Unknown
        );
        assert_eq!(
            classify_http_command("FROBNI", "/x"),
            OperationalCommand::Unknown
        );
    }

    // -------------------------------------------------------------------------
    // MC/DC pair-completion tests (S3 / #115 — KIRRA-OCCY-MCDC-001).
    //
    // The POST/PUT OR-chain at l.29 has three alternates
    //   (a) path.starts_with("/actuator")
    //   (b) path == "/cmd_vel"
    //   (c) path.starts_with("/cmd_vel/")
    // and the SystemMutation OR-chain at l.31–34 has four alternates with the
    // same shape. The existing tests cover (a), (b), and "/firmware",
    // "/reboot" exact, "/config" prefix. The two undemonstrated independent
    // effects are (c) — a sub-path of /cmd_vel/ — and the "/reboot/" prefix.
    // -------------------------------------------------------------------------

    /// MC/DC: independent-effect of `path.starts_with("/cmd_vel/")`
    /// (l.29 third OR clause). All prior clauses are false; this one
    /// decides the verdict.
    #[test]
    fn test_cmd_vel_sub_path_classifies_as_write_state() {
        assert_eq!(
            classify_http_command("POST", "/cmd_vel/replay"),
            OperationalCommand::WriteState,
            "/cmd_vel/* sub-path must classify as WriteState (third OR clause)"
        );
        assert_eq!(
            classify_http_command("PUT", "/cmd_vel/buffer"),
            OperationalCommand::WriteState
        );
    }

    /// MC/DC: independent-effect of `path.starts_with("/reboot/")`
    /// (l.33 third OR clause in the SystemMutation chain). /firmware
    /// prefix false, exact /reboot false, prefix /reboot/ decides.
    #[test]
    fn test_reboot_sub_path_classifies_as_system_mutation() {
        assert_eq!(
            classify_http_command("POST", "/reboot/now"),
            OperationalCommand::SystemMutation,
            "/reboot/* sub-path must classify as SystemMutation (third OR clause)"
        );
        assert_eq!(
            classify_http_command("POST", "/reboot/scheduled/15s"),
            OperationalCommand::SystemMutation
        );
    }

    // -------------------------------------------------------------------------
    // #69 / SG-006 — unrecognised write paths must NOT bypass the Unknown
    // deny-all gate. Pre-#69 the POST/PUT catch-all returned `WriteState`
    // (Nominal-permitted) for any unlisted path; now an unrecognised write
    // path is `Unknown` (denied in ALL postures).
    // -------------------------------------------------------------------------

    /// GAP CLOSED: a write to a path that is NOT on the allowlist classifies
    /// as `Unknown`. This is the exact hole #69 describes — a future
    /// `POST /admin/...` (or any unlisted write route) must be refused, not
    /// silently treated as `WriteState`.
    /// Verifies REQ: unknown-path-denied-allowlist (#69 / SG-006).
    #[test]
    fn test_unrecognized_write_path_is_unknown() {
        for (m, p) in [
            ("POST", "/admin/shutdown"),
            ("POST", "/admin/users/create"),
            ("PUT", "/admin/policy"),
            ("POST", "/totally/unknown/route"),
            ("PUT", "/fleet/secret-backdoor"),
            // A loose prefix sibling of a known path must NOT ride in:
            // "/attestation/register" is allowlisted, "/attestation/wipe" is not.
            ("POST", "/attestation/wipe"),
            ("POST", "/federation"),
            ("POST", "/control/arm"),
        ] {
            assert_eq!(
                classify_http_command(m, p),
                OperationalCommand::Unknown,
                "unrecognised write path {m} {p} must classify as Unknown (fail-closed, #69/SG-006)"
            );
        }
    }

    /// LEGITIMATE STILL WORKS: every real write endpoint registered on the
    /// verifier router keeps its prior (non-Unknown) classification, so the
    /// posture gate does not start 503-ing valid traffic. If a route is added
    /// to the router without updating the allowlist this test is the canary —
    /// add the path here AND to `is_write_state_path`/`is_system_mutation_path`.
    #[test]
    fn test_known_write_paths_preserved() {
        // Control-plane / actuator WriteState endpoints.
        // NOTE: `/actuator/motion/command` is intentionally absent here — it
        // classifies as `ActuatorMotion` (Option A / ADR-0011), asserted
        // separately in `test_actuator_motion_command_classifies_as_actuator_motion`.
        for p in [
            "/cmd_vel",
            "/action_filter/evaluate",
            "/attestation/register",
            "/attestation/verify",
            "/attestation/identity/register",
            "/attestation/challenge/node-abc", // path-param prefix
            "/fabric/assets/register",
            "/fabric/command/asset-7", // path-param prefix
            "/federation/controllers/register",
            "/federation/reports/submit",
            "/fleet/assets/register",
            "/fleet/dependencies",
            "/fleet/diagnostics/report",
            "/industrial/evaluate",
            "/industrial/canopen/evaluate",
            "/industrial/dnp3/evaluate",
            "/industrial/ethernet-ip/evaluate",
            "/system/backup/export",
            "/system/audit/rotate-signing-key",
        ] {
            assert_eq!(
                classify_http_command("POST", p),
                OperationalCommand::WriteState,
                "known write endpoint {p} must remain WriteState (no regression)"
            );
        }
        // Query string must not defeat the exact-match allowlist.
        assert_eq!(
            classify_http_command("POST", "/federation/reports/submit?dry_run=1"),
            OperationalCommand::WriteState,
            "query string must be stripped before allowlist matching"
        );
    }

    /// FAIL-CLOSED outcome end-to-end: an unrecognised write path classified as
    /// `Unknown` is denied by `should_route_command` in EVERY posture —
    /// including Nominal, where `WriteState` would have been permitted. This is
    /// the safety consequence the classification change buys.
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
            });
            assert!(
                !should_route_command(&cache, now, cmd),
                "Unknown write path must be denied under {posture:?} (fail-closed, no silent proceed)"
            );
        }
    }
}
