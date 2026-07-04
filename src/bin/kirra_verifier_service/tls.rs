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

/// Max time a single client may take to complete the TLS handshake before its
/// connection task is dropped — bounds a slow/never-completing handshake
/// (slowloris-style resource exhaustion) so it cannot pin a task indefinitely.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// On shutdown, in-flight connection tasks are awaited for at most this long before
/// any stragglers are aborted — a bounded graceful drain (mirrors the plaintext
/// path's `with_graceful_shutdown`, which also drains then stops).
const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// PEM certificate-chain path. Set together with [`ENV_TLS_KEY_PATH`] to enable TLS.
pub const ENV_TLS_CERT_PATH: &str = "KIRRA_TLS_CERT_PATH";
/// PEM private-key path. Set together with [`ENV_TLS_CERT_PATH`] to enable TLS.
pub const ENV_TLS_KEY_PATH: &str = "KIRRA_TLS_KEY_PATH";
/// PEM client-CA path (Track 1.2 mTLS). When set (server TLS must ALSO be on), the
/// verifier requires + CA-verifies a client certificate; the verified leaf's SHA-256
/// fingerprint is then mapped to a principal (`cert_principals`). Unset → no client auth.
pub const ENV_TLS_CLIENT_CA_PATH: &str = "KIRRA_TLS_CLIENT_CA_PATH";

/// The SHA-256 hex of a CA-verified client certificate's leaf DER, injected into
/// request extensions by [`serve_tls`] so the auth layer can resolve an mTLS
/// principal. Present ONLY when mTLS is enabled and the client presented a cert.
#[derive(Clone, Debug)]
pub struct ClientCertFingerprint(pub String);

/// The resolved serve mode — the fail-closed decision over the TLS env vars.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TlsResolution {
    /// Neither cert nor key set — serve plaintext (default, byte-identical to before).
    Plaintext,
    /// Cert + key set — terminate TLS. `client_ca_path` set → also require + verify
    /// client certs (mTLS).
    Tls {
        cert_path: PathBuf,
        key_path: PathBuf,
        client_ca_path: Option<PathBuf>,
    },
}

/// Fail-closed resolution over the (cert, key, client-CA) paths. Pure — reads no
/// env — so the config truth table is tested without `set_var` (INVARIANT #13).
///
/// Empty/whitespace values are treated as unset. Rules:
/// - neither cert nor key → plaintext (a client-CA without server TLS is an error);
/// - exactly one of cert/key → error (a half-configured TLS deployment must abort);
/// - both cert/key → TLS, with optional mTLS when `client_ca` is set.
pub fn resolve_tls(
    cert_path: Option<String>,
    key_path: Option<String>,
    client_ca: Option<String>,
) -> Result<TlsResolution, String> {
    let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let (cert, key, ca) = (clean(cert_path), clean(key_path), clean(client_ca));
    match (cert, key) {
        (None, None) => {
            if ca.is_some() {
                return Err(format!(
                    "{ENV_TLS_CLIENT_CA_PATH} is set but server TLS ({ENV_TLS_CERT_PATH}/\
                     {ENV_TLS_KEY_PATH}) is not — mTLS requires in-process TLS; refusing to start"
                ));
            }
            Ok(TlsResolution::Plaintext)
        }
        (Some(c), Some(k)) => Ok(TlsResolution::Tls {
            cert_path: PathBuf::from(c),
            key_path: PathBuf::from(k),
            client_ca_path: ca.map(PathBuf::from),
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
        std::env::var(ENV_TLS_CLIENT_CA_PATH).ok(),
    )
}

/// Build a rustls [`ServerConfig`] from a PEM cert-chain + private-key file, pinned
/// to the `ring` provider. When `client_ca_path` is `Some`, client certificates are
/// REQUIRED and CA-verified (mTLS) via rustls's audited [`WebPkiClientVerifier`].
/// Fail-closed: any read/parse/empty/rejected-key/bad-CA error is an `Err` (the
/// caller aborts startup before binding).
pub fn load_server_config(
    cert_path: &Path,
    key_path: &Path,
    client_ca_path: Option<&Path>,
) -> Result<Arc<ServerConfig>, String> {
    use tokio_rustls::rustls::server::WebPkiClientVerifier;
    use tokio_rustls::rustls::RootCertStore;

    let certs = load_cert_chain(cert_path)?;
    let key = load_private_key(key_path)?;

    // Explicit ring provider — no dependence on a process-global default provider,
    // and aws-lc-rs is never referenced.
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let builder = ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("rustls provider/protocol setup failed: {e}"))?;

    // Client auth: mTLS (CA-verified client certs) when a client-CA is configured,
    // otherwise none. rustls does the cryptographic verification (chain + proof of
    // possession); we pin the verified leaf's fingerprint to a principal downstream.
    let builder = match client_ca_path {
        Some(ca) => {
            let mut roots = RootCertStore::empty();
            for cert in load_cert_chain(ca)? {
                roots
                    .add(cert)
                    .map_err(|e| format!("client CA {} rejected: {e}", ca.display()))?;
            }
            let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
                .build()
                .map_err(|e| format!("client-cert verifier build failed: {e}"))?;
            builder.with_client_cert_verifier(verifier)
        }
        None => builder.with_no_client_auth(),
    };

    let config = builder
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

/// SHA-256 hex of a CA-verified client's leaf certificate DER, or `None` if the
/// peer presented no certificate (plain TLS, or mTLS not configured). The cert was
/// already cryptographically verified by rustls at the handshake — this only
/// derives the stable fingerprint used to pin it to a principal.
fn peer_cert_fingerprint(tls_stream: &tokio_rustls::server::TlsStream<tokio::net::TcpStream>) -> Option<String> {
    use sha2::{Digest, Sha256};
    let (_io, conn) = tls_stream.get_ref();
    let leaf = conn.peer_certificates().and_then(|chain| chain.first())?;
    let mut hasher = Sha256::new();
    hasher.update(leaf.as_ref());
    Some(hex::encode(hasher.finalize()))
}

/// Serve `app` over TLS on `listener`, terminating with `config`.
///
/// Each accepted TCP connection is moved to its OWN task before the TLS handshake,
/// so a slow/stalled handshake cannot head-of-line-block the accept loop (a DoS
/// concern for a safety service); the handshake itself is bounded by
/// [`HANDSHAKE_TIMEOUT`] so a client that never completes it cannot pin a task.
///
/// Connection tasks are tracked in a [`JoinSet`](tokio::task::JoinSet). When
/// `shutdown` fires the accept loop stops and in-flight connections are drained for
/// up to [`DRAIN_GRACE`]; any still-running past that are aborted (bounded
/// shutdown) — the same drain-then-stop shape as the plaintext path's
/// `with_graceful_shutdown`.
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
    let mut conns: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            // Reap finished connection tasks so the set does not grow unbounded over
            // a long uptime. Disabled while empty so it never busy-loops on `None`.
            Some(_) = conns.join_next(), if !conns.is_empty() => {},
            accepted = listener.accept() => {
                let (stream, _peer) = match accepted {
                    Ok(pair) => pair,
                    // A transient accept error must not kill the listener.
                    Err(e) => { tracing::warn!(error = %e, "tls: accept failed"); continue; }
                };
                let acceptor = acceptor.clone();
                let app = app.clone();
                conns.spawn(async move {
                    // Bound the handshake — a client that connects and stalls (or never
                    // speaks TLS) is dropped after HANDSHAKE_TIMEOUT, not held forever.
                    let tls_stream = match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                        Ok(Ok(s)) => s,
                        // A failed handshake (bad client, scan, plaintext probe) drops
                        // this connection only.
                        Ok(Err(e)) => { tracing::debug!(error = %e, "tls: handshake failed"); return; }
                        Err(_) => { tracing::debug!("tls: handshake timed out"); return; }
                    };
                    // mTLS: the rustls verifier has ALREADY CA-verified any presented
                    // client cert. Fingerprint its leaf (SHA-256 of the DER) so the auth
                    // layer can map it to a principal. `None` when no client cert (plain TLS).
                    let client_fp = peer_cert_fingerprint(&tls_stream);
                    let io = TokioIo::new(tls_stream);
                    let service = hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
                        if let Some(fp) = client_fp.clone() {
                            req.extensions_mut().insert(ClientCertFingerprint(fp));
                        }
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

    // Graceful, BOUNDED drain: stop accepting (drop the listener) and await in-flight
    // connection tasks up to DRAIN_GRACE. Dropping `conns` afterward aborts any that
    // outlast the grace window, so shutdown always terminates.
    drop(listener);
    let _ = tokio::time::timeout(DRAIN_GRACE, async {
        while conns.join_next().await.is_some() {}
    })
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- resolve_tls fail-closed truth table (pure, no env) ------------------

    #[test]
    fn neither_set_is_plaintext() {
        assert_eq!(resolve_tls(None, None, None), Ok(TlsResolution::Plaintext));
        // empty/whitespace counts as unset
        assert_eq!(
            resolve_tls(Some("  ".into()), Some("".into()), None),
            Ok(TlsResolution::Plaintext)
        );
    }

    #[test]
    fn both_set_is_tls() {
        assert_eq!(
            resolve_tls(Some("/c.pem".into()), Some("/k.pem".into()), None),
            Ok(TlsResolution::Tls {
                cert_path: PathBuf::from("/c.pem"),
                key_path: PathBuf::from("/k.pem"),
                client_ca_path: None,
            })
        );
    }

    #[test]
    fn client_ca_enables_mtls_when_server_tls_on() {
        assert_eq!(
            resolve_tls(Some("/c.pem".into()), Some("/k.pem".into()), Some("/ca.pem".into())),
            Ok(TlsResolution::Tls {
                cert_path: PathBuf::from("/c.pem"),
                key_path: PathBuf::from("/k.pem"),
                client_ca_path: Some(PathBuf::from("/ca.pem")),
            })
        );
    }

    #[test]
    fn client_ca_without_server_tls_is_fail_closed_error() {
        // mTLS requires in-process TLS to be enabled first.
        assert!(resolve_tls(None, None, Some("/ca.pem".into())).is_err());
    }

    #[test]
    fn exactly_one_set_is_fail_closed_error() {
        assert!(resolve_tls(Some("/c.pem".into()), None, None).is_err());
        assert!(resolve_tls(None, Some("/k.pem".into()), None).is_err());
        // whitespace-only key counts as unset → still a partial-config error
        assert!(resolve_tls(Some("/c.pem".into()), Some("   ".into()), None).is_err());
    }

    // --- the throwaway TEST-ONLY self-signed fixtures ------------------------
    // Base64-wrapped so no raw PEM `PRIVATE KEY` block lives in the tree. Decoded
    // to temp files at test time and fed through the SAME production loader, so the
    // real file path is exercised end-to-end. NEVER a production identity: a
    // self-signed cert whose only trust anchor is the test client below.

    const CERT_B64: &str = include_str!("../../../tests/fixtures/tls/test_server_cert.pem.b64");
    const KEY_B64: &str = include_str!("../../../tests/fixtures/tls/test_server_key.pem.b64");
    // mTLS fixtures: a client CA, and a client identity (cert signed by that CA + key).
    const CLIENT_CA_B64: &str = include_str!("../../../tests/fixtures/tls/test_client_ca_cert.pem.b64");
    const CLIENT_CERT_B64: &str = include_str!("../../../tests/fixtures/tls/test_client_cert.pem.b64");
    const CLIENT_KEY_B64: &str = include_str!("../../../tests/fixtures/tls/test_client_key.pem.b64");

    fn decode_b64(b64: &str) -> Vec<u8> {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.decode(b64.trim()).expect("decode fixture")
    }

    fn write_b64(dir: &Path, name: &str, b64: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, decode_b64(b64)).unwrap();
        path
    }

    fn write_fixture_pems(dir: &Path) -> (PathBuf, PathBuf) {
        (write_b64(dir, "cert.pem", CERT_B64), write_b64(dir, "key.pem", KEY_B64))
    }

    /// SHA-256 hex of the first cert in a PEM blob — the same value `serve_tls`
    /// derives from the peer leaf DER.
    fn pem_leaf_fingerprint(pem: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut rd = std::io::BufReader::new(pem);
        let leaf = rustls_pemfile::certs(&mut rd).next().unwrap().unwrap();
        let mut h = Sha256::new();
        h.update(leaf.as_ref());
        hex::encode(h.finalize())
    }

    #[test]
    fn valid_pems_load_a_server_config() {
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = write_fixture_pems(dir.path());
        assert!(load_server_config(&cert, &key, None).is_ok());
    }

    #[test]
    fn missing_or_garbage_files_are_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = write_fixture_pems(dir.path());
        // absent cert
        assert!(load_server_config(&dir.path().join("nope.pem"), &key, None).is_err());
        // garbage (no PEM cert block)
        let junk = dir.path().join("junk.pem");
        std::fs::write(&junk, b"not a pem").unwrap();
        assert!(load_server_config(&junk, &key, None).is_err());
        // cert file present but used as the key → no private key found
        assert!(load_server_config(&cert, &cert, None).is_err());
    }

    // --- live TLS handshake: real server + real client, real bytes -----------

    #[tokio::test]
    async fn live_handshake_terminates_tls_and_serves_requests() {
        use axum::routing::get;

        let dir = tempfile::tempdir().unwrap();
        let (cert_path, key_path) = write_fixture_pems(dir.path());
        let config = load_server_config(&cert_path, &key_path, None).expect("server config");

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

        // Signal shutdown (breaks the accept loop), then abort rather than await:
        // reqwest keeps the connection pooled, so awaiting would block the full
        // DRAIN_GRACE for the idle keep-alive conn. The handshake + round-trip above
        // is what this test asserts.
        let _ = tx.send(());
        server.abort();
    }

    // --- live mTLS: CA-verified client cert → injected fingerprint -----------

    #[tokio::test]
    async fn live_mtls_handshake_injects_client_cert_fingerprint() {
        use axum::routing::get;
        use axum::Extension;

        let dir = tempfile::tempdir().unwrap();
        let (server_cert, server_key) = write_fixture_pems(dir.path());
        let ca_path = write_b64(dir.path(), "client_ca.pem", CLIENT_CA_B64);
        let config = load_server_config(&server_cert, &server_key, Some(&ca_path))
            .expect("mTLS server config");

        // Handler echoes the fingerprint serve_tls injected for the verified client.
        let app = axum::Router::new().route(
            "/whoami",
            get(|Extension(fp): Extension<ClientCertFingerprint>| async move { fp.0 }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            serve_tls(listener, app, config, async move { let _ = rx.await; }).await.unwrap();
        });

        // Client trusts the server cert AND presents its CA-signed identity (cert+key).
        let server_cert_pem = std::fs::read(&server_cert).unwrap();
        let client_cert = decode_b64(CLIENT_CERT_B64);
        let client_key = decode_b64(CLIENT_KEY_B64);
        let mut identity_pem = client_cert.clone();
        identity_pem.push(b'\n');
        identity_pem.extend_from_slice(&client_key);
        let identity = reqwest::Identity::from_pem(&identity_pem).expect("client identity");
        let client = reqwest::Client::builder()
            .add_root_certificate(reqwest::Certificate::from_pem(&server_cert_pem).unwrap())
            .identity(identity)
            .build()
            .unwrap();

        let url = format!("https://127.0.0.1:{}/whoami", addr.port());
        let resp = client.get(&url).send().await.expect("mTLS request");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let seen_fp = resp.text().await.unwrap();

        // The server must have derived the client leaf's SHA-256 fingerprint.
        assert_eq!(seen_fp, pem_leaf_fingerprint(&client_cert),
            "server must inject the verified client's leaf fingerprint");

        let _ = tx.send(());
        server.abort();
    }

    #[tokio::test]
    async fn mtls_rejects_a_client_with_no_certificate() {
        use axum::routing::get;

        let dir = tempfile::tempdir().unwrap();
        let (server_cert, server_key) = write_fixture_pems(dir.path());
        let ca_path = write_b64(dir.path(), "client_ca.pem", CLIENT_CA_B64);
        let config = load_server_config(&server_cert, &server_key, Some(&ca_path)).unwrap();

        let app = axum::Router::new().route("/health", get(|| async { "ok" }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            serve_tls(listener, app, config, async move { let _ = rx.await; }).await.unwrap();
        });

        // Same trusted server cert, but NO client identity presented.
        let server_cert_pem = std::fs::read(&server_cert).unwrap();
        let client = reqwest::Client::builder()
            .add_root_certificate(reqwest::Certificate::from_pem(&server_cert_pem).unwrap())
            .build()
            .unwrap();

        let url = format!("https://127.0.0.1:{}/health", addr.port());
        let result = client.get(&url).send().await;
        assert!(result.is_err(), "mTLS server must reject a client presenting no certificate");

        let _ = tx.send(());
        server.abort();
    }
}
