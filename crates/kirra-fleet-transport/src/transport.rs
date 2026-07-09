//! The Zenoh pub/sub edge of the fleet lane. Thin: it carries bytes and, on
//! ingest, **verifies before surfacing** (ADR-0007 Clause 1) — the carrier never
//! hands a caller an unverified payload. All trust/codec logic lives in the crate
//! root ([`crate`]); this module is transport only.

use std::sync::Mutex;

use zenoh::Session;

use kirra_fleet_types::federation_reconciliation::FederatedTrustReportV2;
use kirra_fleet_types::store::FleetTrustStore;

use crate::ingress_limit::IngressRateLimiter;
use crate::{
    accept_report, encode_report, ingest_clearance_grant, key_clearance_grant, key_posture,
    key_trust_report, FleetPosture, PostureSummary, RejectReason, RejectionCounter,
    SignedClearanceGrant,
};

/// Opt-in TLS material for an encrypted fleet link (ADR-0007 §"Transport
/// confidentiality"). Every field is a filesystem path to a PEM file; an absent
/// field is simply not wired.
///
/// TLS here is **confidentiality + link authentication**, *not* the trust root —
/// payload trust is still the Ed25519 signature verified at ingest (ADR-0007
/// Clause 1). Enabling TLS does not change what a caller must verify; it only
/// keeps the carrier from exposing the plaintext report/grant stream on the wire.
#[derive(Clone, Debug, Default)]
pub struct FleetTlsConfig {
    /// Trust anchor the CONNECT side verifies the server against (PEM path).
    pub root_ca_certificate: Option<String>,
    /// Server (LISTEN side) certificate chain (PEM path).
    pub listen_certificate: Option<String>,
    /// Server (LISTEN side) private key (PEM path).
    pub listen_private_key: Option<String>,
    /// Client (CONNECT side) certificate — only needed under mutual TLS.
    pub connect_certificate: Option<String>,
    /// Client (CONNECT side) private key — only needed under mutual TLS.
    pub connect_private_key: Option<String>,
    /// Require the client to present a cert too (mutual TLS). `None` → Zenoh
    /// default (off). mTLS is link-level peer auth; it does not replace the
    /// per-payload Ed25519 check.
    pub enable_mtls: Option<bool>,
    /// Verify the server name/SAN on connect. `None` → Zenoh default (on). Set
    /// `Some(false)` only for a bare-IP endpoint whose cert carries no matching
    /// SAN (e.g. an ad-hoc test peer).
    pub verify_name_on_connect: Option<bool>,
}

fn insert_cfg(c: &mut zenoh::Config, key: &str, json5: &str) -> Result<(), String> {
    c.insert_json5(key, json5)
        .map_err(|e| format!("zenoh config `{key}`: {e:?}"))
}

/// Build a deterministic peer-session [`zenoh::Config`] for the fleet lane — the
/// **production config seam**.
///
/// `listen` / `connect` are bare `host:port` (no scheme); the scheme is derived
/// from `tls`: `tcp/…` when `tls` is `None` (plaintext, byte-identical to the
/// prior behaviour), `tls/…` when `Some`. Multicast + gossip scouting are OFF so
/// the peers connect only via the explicit endpoints (no router, no discovery) —
/// the same deterministic topology the in-process tests rely on.
///
/// # Errors
/// Returns a message if any Zenoh config insertion fails (malformed key/value).
pub fn fleet_peer_config(
    listen: Option<&str>,
    connect: Option<&str>,
    tls: Option<&FleetTlsConfig>,
) -> Result<zenoh::Config, String> {
    let scheme = if tls.is_some() { "tls" } else { "tcp" };
    let mut c = zenoh::Config::default();
    insert_cfg(&mut c, "mode", "\"peer\"")?;
    insert_cfg(&mut c, "scouting/multicast/enabled", "false")?;
    insert_cfg(&mut c, "scouting/gossip/enabled", "false")?;

    // ALWAYS set listen explicitly — `[]` on the connect-only side — so Zenoh
    // never falls back to its default `tcp/[::]:0` (IPv6) listener. Build the
    // endpoint array via serde so any character needing JSON escaping in the
    // caller-supplied `host:port` is escaped correctly (never a raw `format!`).
    let listen_endpoints: Vec<String> = listen
        .map(|l| vec![format!("{scheme}/{l}")])
        .unwrap_or_default();
    let listen_json = serde_json::to_string(&listen_endpoints).map_err(|e| e.to_string())?;
    insert_cfg(&mut c, "listen/endpoints", &listen_json)?;
    if let Some(cn) = connect {
        let connect_json =
            serde_json::to_string(&[format!("{scheme}/{cn}")]).map_err(|e| e.to_string())?;
        insert_cfg(&mut c, "connect/endpoints", &connect_json)?;
    }

    if let Some(t) = tls {
        // PEM paths → JSON5 string values (serde escapes the path safely).
        for (key, val) in [
            ("root_ca_certificate", &t.root_ca_certificate),
            ("listen_certificate", &t.listen_certificate),
            ("listen_private_key", &t.listen_private_key),
            ("connect_certificate", &t.connect_certificate),
            ("connect_private_key", &t.connect_private_key),
        ] {
            if let Some(path) = val {
                let json = serde_json::to_string(path).map_err(|e| e.to_string())?;
                insert_cfg(&mut c, &format!("transport/link/tls/{key}"), &json)?;
            }
        }
        if let Some(m) = t.enable_mtls {
            insert_cfg(&mut c, "transport/link/tls/enable_mtls", bool_json5(m))?;
        }
        if let Some(v) = t.verify_name_on_connect {
            insert_cfg(
                &mut c,
                "transport/link/tls/verify_name_on_connect",
                bool_json5(v),
            )?;
        }
    }
    Ok(c)
}

fn bool_json5(b: bool) -> &'static str {
    if b {
        "true"
    } else {
        "false"
    }
}

/// Vehicle-side publisher (vehicle → ops/cloud). Publishes signed trust reports +
/// posture summaries on the versioned `kirra/v1/fleet/{node_id}/…` keys.
pub struct FleetPublisher {
    session: Session,
}

impl FleetPublisher {
    #[must_use]
    pub fn new(session: Session) -> Self {
        Self { session }
    }

    /// Publish a signed [`FederatedTrustReportV2`] for its asset.
    pub async fn publish_report(&self, report: &FederatedTrustReportV2) -> Result<(), String> {
        let bytes = encode_report(report).map_err(|e| format!("{e:?}"))?;
        self.session
            .put(key_trust_report(&report.asset_id), bytes)
            .await
            .map_err(|e| e.to_string())
    }

    /// Publish a posture summary (advisory telemetry; see [`PostureSummary`]).
    pub async fn publish_posture(
        &self,
        node_id: &str,
        posture: FleetPosture,
        now_ms: u64,
    ) -> Result<(), String> {
        let summary = PostureSummary {
            node_id: node_id.to_string(),
            posture,
            generated_at_ms: now_ms,
        };
        let bytes = serde_json::to_vec(&summary).map_err(|e| e.to_string())?;
        self.session
            .put(key_posture(node_id), bytes)
            .await
            .map_err(|e| e.to_string())
    }

    /// Ops/cloud-side: publish a SIGNED clearance grant DOWN to a vehicle. (In a
    /// real deployment this runs on the ops controller; co-located here for the
    /// spike.) The signature is the trust root — the vehicle verifies before use.
    pub async fn publish_clearance_grant(
        &self,
        grant: &SignedClearanceGrant,
    ) -> Result<(), String> {
        let bytes = serde_json::to_vec(grant).map_err(|e| e.to_string())?;
        self.session
            .put(key_clearance_grant(&grant.node_id), bytes)
            .await
            .map_err(|e| e.to_string())
    }

    /// Access the underlying session (e.g. to close it).
    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }
}

/// Default ingest rate limits (WS-4 transport hardening). Trust reports and
/// clearance grants are low-rate control-plane traffic (heartbeat-cadence, not
/// telemetry), so the steady-state allowances are deliberately far above any
/// legitimate rate while still bounding a signature-verify flood to a trickle:
/// a source is allowed a 20-message burst refilling at 10/s; the global
/// backstop admits a 200-message burst refilling at 100/s across all sources;
/// at most 1024 sources are tracked before unknown sources fall through to the
/// global bucket alone (the memory bound). Tune per deployment via
/// [`FleetSubscriber::declare_with_limiter`] / [`GrantIngest::declare_with_limiter`].
pub const INGRESS_PER_SOURCE_BURST: u32 = 20;
pub const INGRESS_PER_SOURCE_REFILL_PER_SEC: f64 = 10.0;
pub const INGRESS_GLOBAL_BURST: u32 = 200;
pub const INGRESS_GLOBAL_REFILL_PER_SEC: f64 = 100.0;
pub const INGRESS_MAX_TRACKED_SOURCES: usize = 1024;

fn default_ingress_limiter(now_ms: u64) -> IngressRateLimiter {
    IngressRateLimiter::new(
        INGRESS_GLOBAL_BURST,
        INGRESS_GLOBAL_REFILL_PER_SEC,
        INGRESS_PER_SOURCE_BURST,
        INGRESS_PER_SOURCE_REFILL_PER_SEC,
        INGRESS_MAX_TRACKED_SOURCES,
        now_ms,
    )
}

/// The rate-limit bucketing source for a sample: the node-id segment of the
/// fleet key expression (`kirra/v1/fleet/{node}/trust-report`,
/// `kirra/v1/ops/{node}/clearance-grant` → segment 3). A key that does not
/// have that shape buckets under its whole expression — never a panic, and a
/// malformed key cannot escape bucketing. The id is untrusted; spoofing many
/// ids only degrades to the global backstop (see `ingress_limit`).
fn bucket_source_from_key(key_expr: &str) -> &str {
    key_expr
        .split('/')
        .nth(3)
        .filter(|s| !s.is_empty())
        .unwrap_or(key_expr)
}

/// Gate one ingest through the shared limiter BEFORE any decode/verify work.
/// Denial counts and returns [`RejectReason::RateLimited`] — fail-closed drop.
/// A poisoned limiter lock is recovered (`into_inner`): the limiter is a DoS
/// shield, and losing it to a one-off panic elsewhere must not wedge ingest —
/// matching the store-handle poison policy.
fn gate_ingest(
    limiter: &Mutex<IngressRateLimiter>,
    source: &str,
    now_ms: u64,
    counter: &RejectionCounter,
) -> Result<(), RejectReason> {
    let allowed = limiter
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .allow(source, now_ms);
    if allowed {
        Ok(())
    } else {
        counter.record(&RejectReason::RateLimited);
        Err(RejectReason::RateLimited)
    }
}

/// Fleet-side subscriber for a node's signed trust reports. `recv_report`
/// **rate-limits, then verifies the signature before returning** — a flood is
/// dropped cheaply as [`RejectReason::RateLimited`] before the Ed25519 verify
/// (WS-4: a signature-verify DoS cannot ride the carrier), and an unsigned /
/// bad-sig / malformed payload is rejected and counted, never surfaced.
pub struct FleetSubscriber {
    subscriber:
        zenoh::pubsub::Subscriber<zenoh::handlers::FifoChannelHandler<zenoh::sample::Sample>>,
    limiter: Mutex<IngressRateLimiter>,
}

impl FleetSubscriber {
    /// Declare a subscriber on `kirra/v1/fleet/{node_id}/trust-report` with the
    /// default ingest limits. `now_ms` seeds the limiter clock (the limiter is
    /// pure/clock-injected; every `recv_report` call supplies the current time).
    pub async fn declare(session: &Session, node_id: &str, now_ms: u64) -> Result<Self, String> {
        Self::declare_with_limiter(session, node_id, default_ingress_limiter(now_ms)).await
    }

    /// As [`declare`](Self::declare), with deployment-tuned ingest limits.
    pub async fn declare_with_limiter(
        session: &Session,
        node_id: &str,
        limiter: IngressRateLimiter,
    ) -> Result<Self, String> {
        let subscriber = session
            .declare_subscriber(key_trust_report(node_id))
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self {
            subscriber,
            limiter: Mutex::new(limiter),
        })
    }

    /// Receive the next payload, gate it through the ingest rate limiter, then
    /// **verify it against `public_key_b64`** and return the verified report.
    /// Rejections (including [`RejectReason::RateLimited`], which is decided
    /// BEFORE the expensive signature verify) increment `counter`.
    pub async fn recv_report(
        &self,
        public_key_b64: &str,
        counter: &RejectionCounter,
        now_ms: u64,
    ) -> Result<FederatedTrustReportV2, RejectReason> {
        let sample = self
            .subscriber
            .recv_async()
            .await
            .map_err(|e| RejectReason::Decode(format!("recv: {e}")))?;
        gate_ingest(
            &self.limiter,
            bucket_source_from_key(sample.key_expr().as_str()),
            now_ms,
            counter,
        )?;
        let bytes = sample.payload().to_bytes();
        accept_report(&bytes, public_key_b64, counter)
    }
}

/// Vehicle-side subscriber for DOWN-lane clearance grants. `recv_and_ingest`
/// rate-limits, verifies the signature, then writes the grant through the
/// EXISTING Phase-A store path (a `PENDING` row Phase-B consumes) — never a
/// second release path.
pub struct GrantIngest {
    subscriber:
        zenoh::pubsub::Subscriber<zenoh::handlers::FifoChannelHandler<zenoh::sample::Sample>>,
    limiter: Mutex<IngressRateLimiter>,
}

impl GrantIngest {
    /// Declare a subscriber on `kirra/v1/ops/{node_id}/clearance-grant` with the
    /// default ingest limits (`now_ms` seeds the limiter clock).
    pub async fn declare(session: &Session, node_id: &str, now_ms: u64) -> Result<Self, String> {
        Self::declare_with_limiter(session, node_id, default_ingress_limiter(now_ms)).await
    }

    /// As [`declare`](Self::declare), with deployment-tuned ingest limits.
    pub async fn declare_with_limiter(
        session: &Session,
        node_id: &str,
        limiter: IngressRateLimiter,
    ) -> Result<Self, String> {
        let subscriber = session
            .declare_subscriber(key_clearance_grant(node_id))
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self {
            subscriber,
            limiter: Mutex::new(limiter),
        })
    }

    /// Receive the next grant, gate it through the ingest rate limiter (a flood
    /// drops as [`RejectReason::RateLimited`] before any decode/verify), verify
    /// it against `public_key_b64`, and on success write it to `store` via the
    /// Phase-A path. Returns the store rowid.
    pub async fn recv_and_ingest<S: FleetTrustStore>(
        &self,
        store: &mut S,
        public_key_b64: &str,
        counter: &RejectionCounter,
        now_ms: u64,
    ) -> Result<i64, RejectReason> {
        let sample = self
            .subscriber
            .recv_async()
            .await
            .map_err(|e| RejectReason::Decode(format!("recv: {e}")))?;
        gate_ingest(
            &self.limiter,
            bucket_source_from_key(sample.key_expr().as_str()),
            now_ms,
            counter,
        )?;
        let bytes = sample.payload().to_bytes();
        let grant: SignedClearanceGrant = serde_json::from_slice(&bytes).map_err(|e| {
            let r = RejectReason::Decode(e.to_string());
            counter.record(&r);
            r
        })?;
        ingest_clearance_grant(store, &grant, public_key_b64, counter, now_ms)
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    // R2: reference `FleetTrustStore` impl for the in-process round-trip tests
    // (DEV-dependency only).
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    use ed25519_dalek::{Signer, SigningKey};
    use kirra_fleet_types::federation_reconciliation::canonical_federation_payload_v2;
    use kirra_verifier::verifier_store::VerifierStore;

    use crate::{key_trust_report, sign_clearance_grant};

    fn keypair() -> (SigningKey, String) {
        let mut seed = [0u8; 32];
        rand::Rng::fill(&mut rand::thread_rng(), &mut seed);
        let sk = SigningKey::from_bytes(&seed);
        let pk = B64.encode(sk.verifying_key().to_bytes());
        (sk, pk)
    }

    fn signed_report(sk: &SigningKey, asset: &str) -> FederatedTrustReportV2 {
        let mut r = FederatedTrustReportV2 {
            source_controller_id: "controller-A".into(),
            asset_id: asset.into(),
            posture: FleetPosture::Nominal,
            issued_at_ms: 1_000,
            expires_at_ms: 6_000,
            nonce_hex: "deadbeef".into(),
            signature_b64: String::new(),
            source_generation: Some(3),
        };
        let sig = sk.sign(canonical_federation_payload_v2(&r).as_bytes());
        r.signature_b64 = B64.encode(sig.to_bytes());
        r
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// A deterministic in-process plaintext peer config — thin wrapper over the
    /// production seam [`fleet_peer_config`] with no TLS. `ep` is a bare
    /// `host:port`; the `tcp/` scheme is supplied by the seam.
    fn peer_config(listen: Option<&str>, connect: Option<&str>) -> zenoh::Config {
        fleet_peer_config(listen, connect, None).unwrap()
    }

    // A minimal ed25519 test PKI, used ONLY by the in-process TLS round-trip test
    // below: a CA (`fleet_test_ca`, CA:TRUE) that signs a server leaf
    // (`fleet_test_server_cert`, CA:FALSE, SAN IP:127.0.0.1 / localhost). The client
    // trusts the CA; the server presents the leaf. Base64-wrapped (matching the
    // verifier's `tls.rs` convention) so no raw PEM `PRIVATE KEY` block lives in the
    // tree; decoded to temp files at test time and fed through the SAME config seam.
    // Localhost test material — NEVER a deployment credential; the trust root remains
    // the Ed25519 payload signature (ADR-0007 Clause 1), which TLS does not replace.
    const TEST_CA_B64: &str = include_str!("../tests/fixtures/tls/fleet_test_ca.pem.b64");
    const TEST_SERVER_CERT_B64: &str =
        include_str!("../tests/fixtures/tls/fleet_test_server_cert.pem.b64");
    const TEST_SERVER_KEY_B64: &str =
        include_str!("../tests/fixtures/tls/fleet_test_server_key.pem.b64");

    fn write_b64_pem(dir: &std::path::Path, name: &str, b64: &str) -> String {
        let path = dir.join(name);
        let pem = B64.decode(b64.trim()).expect("decode tls fixture");
        std::fs::write(&path, pem).unwrap();
        path.to_str().unwrap().to_string()
    }

    async fn settle() {
        // Allow the TCP session + subscription state to propagate between peers.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }

    /// REPORT ROUND-TRIP over two in-process Zenoh peer sessions: publish a signed
    /// report, receive it on the other session, and verify it lands intact and
    /// signature-valid.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn report_round_trip_over_two_peer_sessions_verifies() {
        let (sk, pk) = keypair();
        let ep = format!("127.0.0.1:{}", free_port());

        // Subscriber session listens; publisher session connects to it.
        let sub_session = zenoh::open(peer_config(Some(&ep), None)).await.unwrap();
        let subscriber = FleetSubscriber::declare(&sub_session, "robot-01", 1_000)
            .await
            .unwrap();

        let pub_session = zenoh::open(peer_config(None, Some(&ep))).await.unwrap();
        let publisher = FleetPublisher::new(pub_session);
        settle().await;

        let report = signed_report(&sk, "robot-01");
        publisher.publish_report(&report).await.unwrap();

        let counter = RejectionCounter::new();
        let got = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            subscriber.recv_report(&pk, &counter, 1_000),
        )
        .await
        .expect("recv timed out — the carrier did not deliver")
        .expect("verified report");

        assert_eq!(got, report);
        assert_eq!(counter.snapshot().accepted, 1);
        assert_eq!(counter.total_rejected(), 0);
    }

    /// REPORT ROUND-TRIP over an **encrypted** (`tls/…`) link: the same publish →
    /// verify path as the plaintext test, but the two peer sessions negotiate TLS
    /// (server presents the test cert; client verifies it against the same cert as
    /// its root CA, with SAN name-verification ON against the `127.0.0.1` endpoint).
    /// Proves the opt-in TLS seam ([`fleet_peer_config`] + [`FleetTlsConfig`])
    /// actually establishes an encrypted carrier that still delivers + verifies.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn report_round_trip_over_tls_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let ca = write_b64_pem(dir.path(), "ca.pem", TEST_CA_B64);
        let cert = write_b64_pem(dir.path(), "server.pem", TEST_SERVER_CERT_B64);
        let key = write_b64_pem(dir.path(), "server-key.pem", TEST_SERVER_KEY_B64);

        // Server: present the leaf cert+key. Client: trust the CA and verify the
        // server name (the leaf carries an IP:127.0.0.1 SAN).
        let server_tls = FleetTlsConfig {
            listen_certificate: Some(cert.clone()),
            listen_private_key: Some(key.clone()),
            ..Default::default()
        };
        let client_tls = FleetTlsConfig {
            root_ca_certificate: Some(ca.clone()),
            verify_name_on_connect: Some(true),
            ..Default::default()
        };

        let (sk, pk) = keypair();
        let ep = format!("127.0.0.1:{}", free_port());

        let sub_session =
            zenoh::open(fleet_peer_config(Some(&ep), None, Some(&server_tls)).unwrap())
                .await
                .unwrap();
        let subscriber = FleetSubscriber::declare(&sub_session, "robot-tls", 1_000)
            .await
            .unwrap();

        let pub_session =
            zenoh::open(fleet_peer_config(None, Some(&ep), Some(&client_tls)).unwrap())
                .await
                .unwrap();
        let publisher = FleetPublisher::new(pub_session);
        settle().await;

        let report = signed_report(&sk, "robot-tls");
        publisher.publish_report(&report).await.unwrap();

        let counter = RejectionCounter::new();
        let got = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            subscriber.recv_report(&pk, &counter, 1_000),
        )
        .await
        .expect("recv timed out — the TLS carrier did not deliver")
        .expect("verified report over TLS");

        assert_eq!(got, report);
        assert_eq!(counter.snapshot().accepted, 1);
        assert_eq!(counter.total_rejected(), 0);
    }

    /// TAMPER over the wire: a byte flipped in the published payload is rejected at
    /// the subscriber (bad signature) and counted — the carrier cannot launder a
    /// tampered payload into an accepted one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tampered_payload_over_the_wire_is_rejected_and_counted() {
        let (sk, pk) = keypair();
        let ep = format!("127.0.0.1:{}", free_port());

        let sub_session = zenoh::open(peer_config(Some(&ep), None)).await.unwrap();
        let subscriber = FleetSubscriber::declare(&sub_session, "robot-02", 1_000)
            .await
            .unwrap();
        let pub_session = zenoh::open(peer_config(None, Some(&ep))).await.unwrap();
        settle().await;

        // Publish raw tampered bytes directly on the key (a hostile/garbled carrier).
        let report = signed_report(&sk, "robot-02");
        let mut bytes = encode_report(&report).unwrap();
        let pos = bytes.windows(8).position(|w| w == b"robot-02").unwrap();
        bytes[pos] ^= 0x01;
        pub_session
            .put(key_trust_report("robot-02"), bytes)
            .await
            .unwrap();

        let counter = RejectionCounter::new();
        let err = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            subscriber.recv_report(&pk, &counter, 1_000),
        )
        .await
        .expect("recv timed out")
        .unwrap_err();
        assert_eq!(err, RejectReason::BadSignature);
        assert_eq!(counter.snapshot().bad_signature, 1);
        assert_eq!(counter.snapshot().accepted, 0);
    }

    /// GRANT RELAY over Zenoh → the EXISTING store → the EXISTING Phase-B pickup.
    /// The composition proof end-to-end across the real carrier: a signed grant
    /// published down-lane lands a PENDING row that `take_pending_clearance_grant`
    /// then consumes exactly once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn grant_relay_over_the_wire_lands_a_pending_row_phase_b_consumes() {
        let (sk, pk) = keypair();
        let ep = format!("127.0.0.1:{}", free_port());

        // Vehicle side declares the grant-ingest subscriber + owns the store.
        let veh_session = zenoh::open(peer_config(Some(&ep), None)).await.unwrap();
        let ingest = GrantIngest::declare(&veh_session, "robot-03", 1_000)
            .await
            .unwrap();
        let mut store = VerifierStore::new(":memory:").unwrap();

        // Ops side connects + publishes the signed grant.
        let ops_session = zenoh::open(peer_config(None, Some(&ep))).await.unwrap();
        let ops = FleetPublisher::new(ops_session);
        settle().await;

        let grant = sign_clearance_grant(&sk, "robot-03", "alice", 1_000);
        ops.publish_clearance_grant(&grant).await.unwrap();

        let counter = RejectionCounter::new();
        let rowid = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ingest.recv_and_ingest(&mut store, &pk, &counter, 1_001),
        )
        .await
        .expect("recv timed out")
        .expect("verified grant ingested");
        assert!(rowid > 0);
        assert_eq!(counter.snapshot().accepted, 1);

        // The EXISTING Phase-B one-shot pickup consumes exactly that row.
        let picked = store
            .take_pending_clearance_grant("robot-03", 1_500)
            .unwrap()
            .expect("relayed grant is the pending row Phase-B picks up");
        assert_eq!(picked.operator_id, "alice");
        assert!(store
            .take_pending_clearance_grant("robot-03", 1_600)
            .unwrap()
            .is_none());
    }

    /// Bucketing source extraction: node-id segment for both fleet key shapes;
    /// a malformed key buckets under its whole expression (never a panic).
    #[test]
    fn bucket_source_extracts_the_node_segment() {
        assert_eq!(
            bucket_source_from_key("kirra/v1/fleet/robot-9/trust-report"),
            "robot-9"
        );
        assert_eq!(
            bucket_source_from_key("kirra/v1/ops/robot-9/clearance-grant"),
            "robot-9"
        );
        assert_eq!(bucket_source_from_key("weird"), "weird");
        assert_eq!(bucket_source_from_key("a/b/c//d"), "a/b/c//d");
    }

    /// FLOOD over the wire is dropped by the ingest rate limiter BEFORE the
    /// Ed25519 verify (WS-4: a signature-verify DoS cannot ride the carrier).
    /// The limiter admits a burst of 2 from one source with zero refill; three
    /// messages arrive at the same `now_ms`, the third carrying a DELIBERATELY
    /// BAD signature. If the limiter is wired, the third rejects as
    /// `RateLimited`; if it were not, the verify would run and the reject would
    /// be `BadSignature` — so the observed reason proves the verify was never
    /// reached. (Multi-source spoofing → the global backstop is covered by the
    /// `ingress_limit` unit tests; this subscriber is keyed to one node.)
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn flood_is_rate_limited_before_the_signature_verify() {
        let (sk, pk) = keypair();
        let ep = format!("127.0.0.1:{}", free_port());

        let sub_session = zenoh::open(peer_config(Some(&ep), None)).await.unwrap();
        let limiter = crate::ingress_limit::IngressRateLimiter::new(
            100, 0.0, // global: ample burst, no refill needed at one instant
            2, 0.0, // per-source: admit exactly 2 at now_ms, then dry
            16, 1_000,
        );
        let subscriber =
            FleetSubscriber::declare_with_limiter(&sub_session, "robot-flood", limiter)
                .await
                .unwrap();
        let pub_session = zenoh::open(peer_config(None, Some(&ep))).await.unwrap();
        let publisher = FleetPublisher::new(pub_session);
        settle().await;

        // Two well-signed reports, then a bad-signature third (the flood excess).
        publisher
            .publish_report(&signed_report(&sk, "robot-flood"))
            .await
            .unwrap();
        publisher
            .publish_report(&signed_report(&sk, "robot-flood"))
            .await
            .unwrap();
        let mut forged = signed_report(&sk, "robot-flood");
        forged.signature_b64 = B64.encode([0u8; 64]);
        publisher.publish_report(&forged).await.unwrap();

        let counter = RejectionCounter::new();
        let recv = |c| {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                subscriber.recv_report(&pk, c, 1_000),
            )
        };
        recv(&counter)
            .await
            .expect("recv 1")
            .expect("first report admitted + verified");
        recv(&counter)
            .await
            .expect("recv 2")
            .expect("second report admitted + verified");
        let third = recv(&counter).await.expect("recv 3");
        assert_eq!(
            third.unwrap_err(),
            RejectReason::RateLimited,
            "flood excess must be dropped by the limiter BEFORE the verify \
             (BadSignature here would mean the limiter is not wired)"
        );

        let snap = counter.snapshot();
        assert_eq!(snap.accepted, 2);
        assert_eq!(snap.rate_limited, 1);
        assert_eq!(
            snap.bad_signature, 0,
            "the forged payload was never verified"
        );
    }
}
