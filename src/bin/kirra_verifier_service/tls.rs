// src/bin/kirra_verifier_service/tls.rs
// WS-1 Track 1.2 — opt-in in-process TLS termination for the verifier serve path.
//
// Default OFF: with neither KIRRA_TLS_CERT_PATH nor KIRRA_TLS_KEY_PATH set, the
// serve path stays byte-identical plaintext (`axum::serve`), so ADR-0006 Clause 3's
// mesh-first default is unchanged — this only ADDS TLS as an option for a mesh-less
// deployment (see docs/safety/TRANSPORT_SECURITY.md §4). Setting BOTH terminates
// TLS in-process; setting exactly ONE is a fail-closed startup abort — a
// half-configured TLS listener must NEVER silently fall back to plaintext.
//
// rustls is pinned to the `ring` crypto provider (explicit
// `builder_with_provider`), so no ambient/global provider is required and
// `aws-lc-rs` never enters the build.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

/// PEM certificate-chain path. Set together with [`ENV_TLS_KEY_PATH`] to enable TLS.
pub const ENV_TLS_CERT_PATH: &str = "KIRRA_TLS_CERT_PATH";
/// PEM private-key path. Set together with [`ENV_TLS_CERT_PATH`] to enable TLS.
pub const ENV_TLS_KEY_PATH: &str = "KIRRA_TLS_KEY_PATH";

/// The resolved serve mode — the fail-closed decision over the two env vars.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TlsResolution {
    /// Neither var set — serve plaintext (default, byte-identical to before).
    Plaintext,
    /// Both set — terminate TLS from these PEM files.
    Tls { cert_path: PathBuf, key_path: PathBuf },
}

/// Fail-closed resolution over the (cert, key) path pair. Pure — reads no env — so
/// the partial-config truth table is tested without `set_var` (INVARIANT #13).
///
/// Empty/whitespace values are treated as unset. Exactly one of the two present is
/// an error: a half-configured TLS deployment must abort, never serve plaintext.
pub fn resolve_tls(
    cert_path: Option<String>,
    key_path: Option<String>,
) -> Result<TlsResolution, String> {
    let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    match (clean(cert_path), clean(key_path)) {
        (None, None) => Ok(TlsResolution::Plaintext),
        (Some(c), Some(k)) => Ok(TlsResolution::Tls {
            cert_path: PathBuf::from(c),
            key_path: PathBuf::from(k),
        }),
        (Some(_), None) => Err(format!(
            "{ENV_TLS_CERT_PATH} is set but {ENV_TLS_KEY_PATH} is not — refusing to start \
             (fail-closed; a half-configured TLS listener must not fall back to plaintext)"
        )),
        (None, Some(_)) => Err(format!(
            "{ENV_TLS_KEY_PATH} is set but {ENV_TLS_CERT_PATH} is not — refusing to start (fail-closed)"
        )),
    }
}

/// Resolve the serve mode from the environment (the production entry point).
pub fn resolve_tls_from_env() -> Result<TlsResolution, String> {
    resolve_tls(
        std::env::var(ENV_TLS_CERT_PATH).ok(),
        std::env::var(ENV_TLS_KEY_PATH).ok(),
    )
}

/// Build a rustls [`ServerConfig`] from a PEM cert-chain + private-key file, pinned
/// to the `ring` provider. Fail-closed: any read/parse/empty/rejected-key error is
/// an `Err` (the caller aborts startup before binding).
pub fn load_server_config(cert_path: &Path, key_path: &Path) -> Result<Arc<ServerConfig>, String> {
    let certs = load_cert_chain(cert_path)?;
    let key = load_private_key(key_path)?;

    // Explicit ring provider — no dependence on a process-global default provider,
    // and aws-lc-rs is never referenced.
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("rustls provider/protocol setup failed: {e}"))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("rustls rejected the server certificate/key: {e}"))?;
    Ok(Arc::new(config))
}

fn load_cert_chain(path: &Path) -> Result<Vec<CertificateDer<'static>>, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("{ENV_TLS_CERT_PATH} {}: {e}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let certs: Result<Vec<_>, _> = rustls_pemfile::certs(&mut reader).collect();
    let certs =
        certs.map_err(|e| format!("failed to parse certificates from {}: {e}", path.display()))?;
    if certs.is_empty() {
        return Err(format!("no PEM certificates found in {}", path.display()));
    }
    Ok(certs)
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("{ENV_TLS_KEY_PATH} {}: {e}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| format!("failed to parse a private key from {}: {e}", path.display()))?
        .ok_or_else(|| format!("no PEM private key found in {}", path.display()))
}

/// Serve `app` over TLS on `listener`, terminating with `config`.
///
/// Each accepted TCP connection is moved to its OWN task before the TLS handshake,
/// so a slow/stalled handshake cannot head-of-line-block the accept loop (a DoS
/// concern for a safety service). `shutdown` stops the accept loop; in-flight
/// connections drain on their own tasks.
pub async fn serve_tls(
    listener: TcpListener,
    app: axum::Router,
    config: Arc<ServerConfig>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto::Builder;
    use tower::ServiceExt; // oneshot

    let acceptor = TlsAcceptor::from(config);
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let (stream, _peer) = match accepted {
                    Ok(pair) => pair,
                    // A transient accept error must not kill the listener.
                    Err(e) => { tracing::warn!(error = %e, "tls: accept failed"); continue; }
                };
                let acceptor = acceptor.clone();
                let app = app.clone();
                tokio::spawn(async move {
                    let tls_stream = match acceptor.accept(stream).await {
                        Ok(s) => s,
                        // A failed handshake (bad client, scan, plaintext probe) drops
                        // this connection only.
                        Err(e) => { tracing::debug!(error = %e, "tls: handshake failed"); return; }
                    };
                    let io = TokioIo::new(tls_stream);
                    let service = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                        app.clone().oneshot(req)
                    });
                    if let Err(e) = Builder::new(TokioExecutor::new())
                        .serve_connection_with_upgrades(io, service)
                        .await
                    {
                        tracing::debug!(error = %e, "tls: connection error");
                    }
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- resolve_tls fail-closed truth table (pure, no env) ------------------

    #[test]
    fn neither_set_is_plaintext() {
        assert_eq!(resolve_tls(None, None), Ok(TlsResolution::Plaintext));
        // empty/whitespace counts as unset
        assert_eq!(
            resolve_tls(Some("  ".into()), Some("".into())),
            Ok(TlsResolution::Plaintext)
        );
    }

    #[test]
    fn both_set_is_tls() {
        assert_eq!(
            resolve_tls(Some("/c.pem".into()), Some("/k.pem".into())),
            Ok(TlsResolution::Tls {
                cert_path: PathBuf::from("/c.pem"),
                key_path: PathBuf::from("/k.pem"),
            })
        );
    }

    #[test]
    fn exactly_one_set_is_fail_closed_error() {
        assert!(resolve_tls(Some("/c.pem".into()), None).is_err());
        assert!(resolve_tls(None, Some("/k.pem".into())).is_err());
        // whitespace-only key counts as unset → still a partial-config error
        assert!(resolve_tls(Some("/c.pem".into()), Some("   ".into())).is_err());
    }

    // --- the throwaway TEST-ONLY self-signed fixtures ------------------------
    // Base64-wrapped so no raw PEM `PRIVATE KEY` block lives in the tree. Decoded
    // to temp files at test time and fed through the SAME production loader, so the
    // real file path is exercised end-to-end. NEVER a production identity: a
    // self-signed cert whose only trust anchor is the test client below.

    const CERT_B64: &str = include_str!("../../../tests/fixtures/tls/test_server_cert.pem.b64");
    const KEY_B64: &str = include_str!("../../../tests/fixtures/tls/test_server_key.pem.b64");

    fn write_fixture_pems(dir: &Path) -> (PathBuf, PathBuf) {
        use base64::Engine;
        let eng = base64::engine::general_purpose::STANDARD;
        let cert = eng.decode(CERT_B64.trim()).expect("decode test cert");
        let key = eng.decode(KEY_B64.trim()).expect("decode test key");
        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");
        std::fs::write(&cert_path, cert).unwrap();
        std::fs::write(&key_path, key).unwrap();
        (cert_path, key_path)
    }

    #[test]
    fn valid_pems_load_a_server_config() {
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = write_fixture_pems(dir.path());
        assert!(load_server_config(&cert, &key).is_ok());
    }

    #[test]
    fn missing_or_garbage_files_are_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = write_fixture_pems(dir.path());
        // absent cert
        assert!(load_server_config(&dir.path().join("nope.pem"), &key).is_err());
        // garbage (no PEM cert block)
        let junk = dir.path().join("junk.pem");
        std::fs::write(&junk, b"not a pem").unwrap();
        assert!(load_server_config(&junk, &key).is_err());
        // cert file present but used as the key → no private key found
        assert!(load_server_config(&cert, &cert).is_err());
    }

    // --- live TLS handshake: real server + real client, real bytes -----------

    #[tokio::test]
    async fn live_handshake_terminates_tls_and_serves_requests() {
        use axum::routing::get;

        let dir = tempfile::tempdir().unwrap();
        let (cert_path, key_path) = write_fixture_pems(dir.path());
        let config = load_server_config(&cert_path, &key_path).expect("server config");

        // A minimal router — this test proves the TLS terminator carries HTTP, not
        // the full app surface.
        let app = axum::Router::new().route("/health", get(|| async { "ok" }));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Drive the server until we send on `tx`.
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let shutdown = async move {
                let _ = rx.await;
            };
            serve_tls(listener, app, config, shutdown).await.unwrap();
        });

        // Client trusts ONLY the self-signed test cert (added as a root); a real TLS
        // handshake + HTTP round-trip over 127.0.0.1.
        let cert_pem = std::fs::read(&cert_path).unwrap();
        let client = reqwest::Client::builder()
            .add_root_certificate(reqwest::Certificate::from_pem(&cert_pem).unwrap())
            .build()
            .unwrap();

        let url = format!("https://127.0.0.1:{}/health", addr.port());
        let resp = client.get(&url).send().await.expect("https request");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "ok");

        let _ = tx.send(());
        let _ = server.await;
    }
}
