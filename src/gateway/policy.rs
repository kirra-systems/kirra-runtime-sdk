// src/gateway/policy.rs

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationalCommand {
    /// Safe reads: telemetry, metrics, health probes. Allowed in all postures.
    ReadTelemetry,
    /// Actuator writes and velocity commands. Denied when LockedOut.
    WriteState,
    /// Firmware, reboot, config mutations. Denied unless Nominal.
    SystemMutation,
    /// Unrecognised HTTP method. Denied in ALL postures (fail-closed).
    Unknown,
}

/// Classifies an HTTP request into an OperationalCommand based solely on method
/// and path prefix. No state access — pure function, always total.
pub fn classify_http_command(method: &str, path: &str) -> OperationalCommand {
    match method {
        "GET" | "HEAD" => OperationalCommand::ReadTelemetry,

        "DELETE" => OperationalCommand::SystemMutation,

        "POST" | "PUT" => {
            if path.starts_with("/actuator") || path == "/cmd_vel" || path.starts_with("/cmd_vel/") {
                OperationalCommand::WriteState
            } else if path.starts_with("/firmware")
                || path == "/reboot"
                || path.starts_with("/reboot/")
                || path.starts_with("/config")
            {
                OperationalCommand::SystemMutation
            } else {
                // All other POST/PUT: treat as WriteState (state mutation, not
                // infrastructure mutation). Attestation, federation, action-filter
                // endpoints all fall here and are further gated by auth middleware.
                OperationalCommand::WriteState
            }
        }

        _ => OperationalCommand::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classifies_read_telemetry() {
        assert_eq!(classify_http_command("GET", "/telemetry/status"), OperationalCommand::ReadTelemetry);
        assert_eq!(classify_http_command("GET", "/metrics"),          OperationalCommand::ReadTelemetry);
        assert_eq!(classify_http_command("GET", "/health/live"),      OperationalCommand::ReadTelemetry);
    }

    #[test]
    fn test_classifies_cmd_vel_as_write_state() {
        assert_eq!(classify_http_command("POST", "/cmd_vel"), OperationalCommand::WriteState);
    }

    #[test]
    fn test_classifies_actuator_as_write_state() {
        assert_eq!(classify_http_command("POST", "/actuator/servo"), OperationalCommand::WriteState);
        assert_eq!(classify_http_command("PUT",  "/actuator/valve"), OperationalCommand::WriteState);
    }

    #[test]
    fn test_classifies_system_mutations() {
        assert_eq!(classify_http_command("POST",   "/firmware/update"), OperationalCommand::SystemMutation);
        assert_eq!(classify_http_command("POST",   "/reboot"),          OperationalCommand::SystemMutation);
        assert_eq!(classify_http_command("PUT",    "/config/network"),  OperationalCommand::SystemMutation);
        assert_eq!(classify_http_command("DELETE", "/anything"),        OperationalCommand::SystemMutation);
    }

    #[test]
    fn test_unknown_method_classifies_as_unknown() {
        // Unknown HTTP methods map to OperationalCommand::Unknown, which is
        // denied in ALL posture states including Nominal — closing the implicit
        // fallback bypass identified in the v1 gateway policy specification.
        assert_eq!(classify_http_command("PATCH",  "/unknown"), OperationalCommand::Unknown);
        assert_eq!(classify_http_command("FROBNI", "/x"),       OperationalCommand::Unknown);
    }
}
