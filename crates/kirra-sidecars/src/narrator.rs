//! The Mick-adjacent NARRATOR — the read-only consumer of the #893 verdict
//! narration side-channel.
//!
//! The verifier latches every actuator-envelope verdict (action + deny code +
//! operator sentence) and serves the most recent one on
//! `GET /system/verdicts/last` in the AUDITOR tier (`SCOPE_AUDIT_READ`,
//! `src/authz.rs`). This module fetches and relays it so a Mick-adjacent
//! surface can say, in a sentence, why the governor last refused.
//!
//! 🔴 **Auditor tier, never the admin token.** The narrator authenticates
//! with a dedicated `auditor`-role principal token
//! (`KIRRA_MICK_AUDITOR_TOKEN`); it deliberately never reads
//! `KIRRA_ADMIN_TOKEN`, so a compromised narrator holds a read-only,
//! revocable credential with zero mutation rights. Half-configuring the pair
//! (URL without token or vice versa) is a startup ABORT — the fail-closed
//! half-config convention (`KIRRA_TLS_*`).

use std::time::Duration;

/// Verifier base URL, e.g. `http://127.0.0.1:8090`.
pub const VERIFIER_URL_ENV: &str = "KIRRA_VERIFIER_URL";
/// The auditor-role principal token (minted via `POST /system/principals`
/// with role `auditor`). NEVER the admin token.
pub const AUDITOR_TOKEN_ENV: &str = "KIRRA_MICK_AUDITOR_TOKEN";

/// The narration endpoint path (auditor tier).
pub const LAST_VERDICT_PATH: &str = "/system/verdicts/last";

/// HTTP timeout for the relay fetch — narration is telemetry; a slow
/// verifier must not wedge the sidecar's serve loop.
const FETCH_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NarratorConfig {
    pub verifier_url: String,
    pub auditor_token: String,
}

impl NarratorConfig {
    /// Resolve from the environment. `None` → narration not configured (the
    /// endpoint answers 503). `Some(Err)` → HALF-configured — the binary
    /// aborts startup rather than running with a silently dead narrator.
    pub fn from_env() -> Option<Result<Self, String>> {
        Self::resolve(
            std::env::var(VERIFIER_URL_ENV).ok(),
            std::env::var(AUDITOR_TOKEN_ENV).ok(),
        )
    }

    /// Pure resolution (testable without env mutation — INV-13).
    pub fn resolve(url: Option<String>, token: Option<String>) -> Option<Result<Self, String>> {
        let url = url.filter(|s| !s.is_empty());
        let token = token.filter(|s| !s.is_empty());
        match (url, token) {
            (None, None) => None,
            (Some(u), Some(t)) => Some(Ok(Self {
                verifier_url: u,
                auditor_token: t,
            })),
            _ => Some(Err(format!(
                "narrator half-configured: set BOTH {VERIFIER_URL_ENV} and \
                 {AUDITOR_TOKEN_ENV} (an auditor-role principal token — never \
                 the admin token), or neither."
            ))),
        }
    }

    /// The request this narrator makes: `(url, bearer)`. Pure — the test pins
    /// that the bearer is the AUDITOR token and the path is the #893 sidecar.
    #[must_use]
    pub fn request_parts(&self) -> (String, String) {
        (
            format!(
                "{}{}",
                self.verifier_url.trim_end_matches('/'),
                LAST_VERDICT_PATH
            ),
            format!("Bearer {}", self.auditor_token),
        )
    }
}

/// Fetch the last verdict from the verifier and relay its JSON body.
/// Failures are narrated, not panicked — narration must never take the
/// sidecar down.
pub fn fetch_last_verdict(cfg: &NarratorConfig) -> Result<serde_json::Value, String> {
    let (url, bearer) = cfg.request_parts();
    let client = reqwest::blocking::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| format!("narrator client build: {e}"))?;
    let resp = client
        .get(&url)
        .header("Authorization", bearer)
        .send()
        .map_err(|e| format!("verifier unreachable: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("verifier answered {status} (auditor token valid?)"));
    }
    resp.json::<serde_json::Value>()
        .map_err(|e| format!("narration body undecodable: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn request_parts_use_the_auditor_token_and_the_893_path() {
        let cfg = NarratorConfig {
            verifier_url: "http://127.0.0.1:8090/".into(),
            auditor_token: "aud-principal-token".into(),
        };
        let (url, bearer) = cfg.request_parts();
        assert_eq!(url, "http://127.0.0.1:8090/system/verdicts/last");
        assert_eq!(bearer, "Bearer aud-principal-token");
    }

    /// Live wire pin: the narrator's actual HTTP request carries the auditor
    /// bearer to the #893 path — proven against a real socket, not a mock of
    /// our own client.
    #[test]
    fn fetch_sends_the_auditor_bearer_to_the_last_verdict_path() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = r#"{"last":{"action":"DenyBreach","deny_code":"X","explanation":"why"}}"#;
            let _ = s.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            );
            req
        });
        let cfg = NarratorConfig {
            verifier_url: format!("http://{addr}"),
            auditor_token: "auditor-tok".into(),
        };
        let v = fetch_last_verdict(&cfg).expect("relay succeeds");
        assert_eq!(v["last"]["explanation"], "why");
        let seen = handle.join().unwrap();
        assert!(
            seen.starts_with("GET /system/verdicts/last HTTP/1.1"),
            "{seen}"
        );
        assert!(
            seen.to_ascii_lowercase()
                .contains("authorization: bearer auditor-tok"),
            "the AUDITOR token authenticates the read: {seen}"
        );
    }

    #[test]
    fn half_configuration_fails_closed_and_neither_means_unconfigured() {
        assert!(NarratorConfig::resolve(None, None).is_none());
        assert!(NarratorConfig::resolve(Some("u".into()), Some("t".into()))
            .unwrap()
            .is_ok());
        // Half-configured (either half, including empty strings) → startup error.
        assert!(NarratorConfig::resolve(Some("u".into()), None)
            .unwrap()
            .is_err());
        assert!(NarratorConfig::resolve(None, Some("t".into()))
            .unwrap()
            .is_err());
        assert!(
            NarratorConfig::resolve(Some("u".into()), Some(String::new()))
                .unwrap()
                .is_err()
        );
    }
}
