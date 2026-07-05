//! The Zenoh pub/sub edge of the fleet lane. Thin: it carries bytes and, on
//! ingest, **verifies before surfacing** (ADR-0007 Clause 1) — the carrier never
//! hands a caller an unverified payload. All trust/codec logic lives in the crate
//! root ([`crate`]); this module is transport only.

use zenoh::Session;

use kirra_fleet_types::federation_reconciliation::FederatedTrustReportV2;
use kirra_fleet_types::store::FleetTrustStore;

use crate::{
    accept_report, encode_report, ingest_clearance_grant, key_clearance_grant, key_posture,
    key_trust_report, FleetPosture, PostureSummary, RejectReason, RejectionCounter,
    SignedClearanceGrant,
};

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

/// Fleet-side subscriber for a node's signed trust reports. `recv_report`
/// **verifies the signature before returning** — an unsigned / bad-sig / malformed
/// payload is rejected and counted, never surfaced.
pub struct FleetSubscriber {
    subscriber:
        zenoh::pubsub::Subscriber<zenoh::handlers::FifoChannelHandler<zenoh::sample::Sample>>,
}

impl FleetSubscriber {
    /// Declare a subscriber on `kirra/v1/fleet/{node_id}/trust-report`.
    pub async fn declare(session: &Session, node_id: &str) -> Result<Self, String> {
        let subscriber = session
            .declare_subscriber(key_trust_report(node_id))
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self { subscriber })
    }

    /// Receive the next payload, **verify it against `public_key_b64`**, and return
    /// the verified report. Rejections increment `counter`.
    pub async fn recv_report(
        &self,
        public_key_b64: &str,
        counter: &RejectionCounter,
    ) -> Result<FederatedTrustReportV2, RejectReason> {
        let sample = self
            .subscriber
            .recv_async()
            .await
            .map_err(|e| RejectReason::Decode(format!("recv: {e}")))?;
        let bytes = sample.payload().to_bytes();
        accept_report(&bytes, public_key_b64, counter)
    }
}

/// Vehicle-side subscriber for DOWN-lane clearance grants. `recv_and_ingest`
/// verifies the signature then writes the grant through the EXISTING Phase-A store
/// path (a `PENDING` row Phase-B consumes) — never a second release path.
pub struct GrantIngest {
    subscriber:
        zenoh::pubsub::Subscriber<zenoh::handlers::FifoChannelHandler<zenoh::sample::Sample>>,
}

impl GrantIngest {
    /// Declare a subscriber on `kirra/v1/ops/{node_id}/clearance-grant`.
    pub async fn declare(session: &Session, node_id: &str) -> Result<Self, String> {
        let subscriber = session
            .declare_subscriber(key_clearance_grant(node_id))
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self { subscriber })
    }

    /// Receive the next grant, verify it against `public_key_b64`, and on success
    /// write it to `store` via the Phase-A path. Returns the store rowid.
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

    /// A deterministic in-process peer config: explicit TCP endpoint, multicast
    /// scouting OFF (so the two sessions connect ONLY to each other — no router,
    /// no network discovery).
    fn peer_config(listen: Option<&str>, connect: Option<&str>) -> zenoh::Config {
        let mut c = zenoh::Config::default();
        c.insert_json5("mode", "\"peer\"").unwrap();
        c.insert_json5("scouting/multicast/enabled", "false")
            .unwrap();
        c.insert_json5("scouting/gossip/enabled", "false").unwrap();
        // ALWAYS set listen explicitly — `[]` on the connect-only side — so zenoh
        // never falls back to its default `tcp/[::]:0` (IPv6) listener, which the
        // sandbox does not support. The connector dials out; it need not listen.
        let listen_json = match listen {
            Some(l) => format!("[\"{l}\"]"),
            None => "[]".to_string(),
        };
        c.insert_json5("listen/endpoints", &listen_json).unwrap();
        if let Some(cn) = connect {
            c.insert_json5("connect/endpoints", &format!("[\"{cn}\"]"))
                .unwrap();
        }
        c
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
        let ep = format!("tcp/127.0.0.1:{}", free_port());

        // Subscriber session listens; publisher session connects to it.
        let sub_session = zenoh::open(peer_config(Some(&ep), None)).await.unwrap();
        let subscriber = FleetSubscriber::declare(&sub_session, "robot-01")
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
            subscriber.recv_report(&pk, &counter),
        )
        .await
        .expect("recv timed out — the carrier did not deliver")
        .expect("verified report");

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
        let ep = format!("tcp/127.0.0.1:{}", free_port());

        let sub_session = zenoh::open(peer_config(Some(&ep), None)).await.unwrap();
        let subscriber = FleetSubscriber::declare(&sub_session, "robot-02")
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
            subscriber.recv_report(&pk, &counter),
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
        let ep = format!("tcp/127.0.0.1:{}", free_port());

        // Vehicle side declares the grant-ingest subscriber + owns the store.
        let veh_session = zenoh::open(peer_config(Some(&ep), None)).await.unwrap();
        let ingest = GrantIngest::declare(&veh_session, "robot-03")
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
}
