# kirra-fleet-transport — fleet-lane (QM) Zenoh transport spike (#296)

The **fleet lane**: vehicle ↔ ops/cloud transport for cellular / distributed
fleets. The decision lives in [`docs/adr/0007-fleet-transport-zenoh.md`](../../docs/adr/0007-fleet-transport-zenoh.md);
this crate is the spike.

## The three clauses (ADR-0007), in one paragraph

Zenoh is an **untrusted carrier** — trust derives from **Ed25519 payload
signatures** (federation reports via the SDK's
`verify_federated_report_signature_v2`; grants via this crate's
`verify_clearance_grant`), **never** from transport identity, topic name, or
Zenoh's own auth, and every ingest **verifies before use** with unsigned /
bad-signature / malformed payloads rejected and **counted** (`RejectionCounter`)
[Clause 1]. It is **strictly QM**: this crate is a **leaf** consumer that depends
on the SDK, and **nothing under `src/gateway/` or any safety path may depend on
it** — ADR-0006 Clause 2's boundary asymmetry is the parent rule [Clause 2]. The
down-lane grant **terminates at the vehicle's verifier store**: a verified grant
is written through the **existing** Phase-A path (`save_clearance_grant_chained`,
a `PENDING` row) and Phase-B's one-shot pickup + two-checkpoint delivery proceed
**unchanged** — remote transport composes with the clearance design, it never
creates a second release path [Clause 3].

## Layout

- **`lib.rs`** — the transport-free **trust + codec core**: the namespace key
  expressions, `accept_report` (decode → **verify-first**), the signed-grant
  envelope (`sign_clearance_grant` / `verify_clearance_grant`), `ingest_clearance_grant`
  (verify → existing store path), and `RejectionCounter`. Unit-tested with no
  Zenoh session.
- **`transport.rs`** — the thin Zenoh edge: `FleetPublisher`, `FleetSubscriber`
  (`recv_report` verifies before surfacing), `GrantIngest` (`recv_and_ingest`).
  Tested with two **in-process peer sessions** (no router, localhost TCP, multicast
  off).

## Transport confidentiality (opt-in TLS)

TLS is **available and opt-in** (`transport_tls`). It is **confidentiality + link
authentication**, *not* the trust root — the trust root is the Ed25519 payload
signature verified at ingest (Clause 1), so a plaintext deployment still verifies
every payload; TLS only stops the carrier from exposing the report/grant stream on
the wire.

The production config seam is `transport::fleet_peer_config(listen, connect, tls)`
+ [`FleetTlsConfig`](src/transport.rs): pass `None` for `tls` (the default) and the
session is plaintext `tcp/...`, **byte-identical** to the prior behaviour; pass
`Some(&FleetTlsConfig{..})` (cert/key/CA PEM paths) and the endpoints become
`tls/...` and the link is encrypted + cert-verified. `report_round_trip_over_tls_verifies`
drives a real two-session encrypted round-trip end-to-end (CA-signed server leaf,
SAN name-verification on).

### The toolchain wall (cleared)

Zenoh's TLS stack pulls a `time` / x509 chain that used to hit an `E0119`
conflicting-impl break on rustc 1.94.1 — the anticipated MSRV/toolchain wall. It is
**now cleared** by bumping `time` 0.3.48 → 0.3.53 in the workspace lockfile. Zenoh's
TLS uses the **`ring`** rustls provider (not `aws-lc-rs`), so it does not conflict
with the verifier's ring-only rustls invariant. If a future transitive dep ever
breaks the resolve, the `zenoh-pinned-deps-1-75` lockdown is the escape hatch.

The Zenoh tests require a **multi-threaded** tokio runtime
(`#[tokio::test(flavor = "multi_thread", …)]`) — Zenoh's runtime rejects the
current-thread scheduler.

## What this spike is NOT

Router / cellular / NAT deployment (ops/router territory), QoS / `AdvancedPublisher`
delivery guarantees (named future — `zenoh-ext`), and the multi-tenant fleet side
with a per-controller key registry (#314). See the ADR's *Honest limits*.
