# ADR-0007: Fleet transport — Zenoh on the QM fleet lane; an untrusted carrier under Ed25519 payload trust; grants terminate at the verifier store

| Field | Value |
|---|---|
| Status | **Accepted (direction)** — spike landed; see *Conditions that reopen this decision* |
| Date | 2026-06-12 |
| Deciders | Project owner |
| Issues | #296 (this ADR + spike); #304 (the remote-grant variant it composes with); #314 (multi-tenant fleet side); the market driver `docs/MARKET_AUTONOMOUS_SERVICES.md` |
| Doc | `crates/kirra-fleet-transport/README.md` (the spike) |
| Builds on | **ADR-0006 Clause 2** (the frozen-layout boundary asymmetry — the parent rule that keeps a fleet transport OUT of the safety path) |

## Context

KIRRA's safety argument rests on an **independent safety channel** and a
**frozen-layout partition boundary** (ADR-0004 / ADR-0006): the governor's
authority never travels over a general-purpose network transport. Separately,
KIRRA has a **fleet lane** — vehicle ↔ ops/cloud — that carries QM traffic:
federated trust reports (`FederatedTrustReportV2`), fleet-posture dissemination,
and (the #304 deferral) **remote operator clearance grants**.

The autonomous-services market (`docs/MARKET_AUTONOMOUS_SERVICES.md`) makes this
lane strategic: sidewalk-courier and delivery-AV fleets are **cellular** and
**distributed**, with single operators and platforms spanning many vehicles. The
trust reports and posture they generate are only useful if they cross the
wide-area hop — and a remote grant must reach the vehicle. That hop needs a
transport. It must **never** become a safety-bearing channel.

The federation payloads already carry their own cryptographic trust: the v2 report
is Ed25519-signed over a **canonical payload** (`canonical_federation_payload_v2`),
verified by `verify_federated_report_signature_v2` — so the **trust is in the
payload, independent of whatever carries it.** That is the hinge this decision
turns on.

## Decision

Adopt **Zenoh** (`zenoh` + `zenoh-ext` **stable** APIs only, per upstream
guidance) as the fleet-lane transport for cellular/distributed fleets. Three
clauses, each load-bearing and **distinct**.

### Clause 1 — the transport is an UNTRUSTED CARRIER (the trust rule)

Trust derives from **Ed25519 payload signatures**, **never** from transport
identity, topic name, or Zenoh's own authentication:

- Trust reports — `verify_federated_report_signature_v2` over the canonical v2
  payload (SDK).
- Clearance grants — a signed envelope (`SignedClearanceGrant`) verified by
  `verify_clearance_grant` against the issuing controller's registered public key,
  reusing the **same** canonical-payload + Ed25519 pattern.

**Every ingest verifies BEFORE use.** The carrier never surfaces an unverified
payload to a caller; unsigned / bad-signature / malformed payloads are **rejected
and counted** (`RejectionCounter`, surfaced for ops). Zenoh-level TLS/auth is
**confidentiality defense-in-depth, not the trust root** — a deployment may enable
it, but the safety/trust argument does not depend on it. (Concretely: TLS is now
**available and opt-in** via `transport::fleet_peer_config` + `FleetTlsConfig`
(`tls/...` endpoints, cert/key/CA PEM paths); the default plaintext path still
**loses no trust** because every payload is Ed25519-verified at ingest regardless —
see *Honest limits*.)

### Clause 2 — strictly QM / fleet lane (the domain rule)

Zenoh is the **fleet lane only** (robot↔robot, robot↔cloud) and **never the safety
channel.** The crate `kirra-fleet-transport` is a **LEAF** consumer: it depends on
the SDK; **nothing under `src/gateway/`, the governor, the boundary channel, or any
safety path may ever depend on it.** The dependency edge points one way, downhill,
out of the safety domain.

This is **ADR-0006 Clause 2's asymmetry applied**: across the partition boundary
the transport is a frozen `#[repr(C)]` layout precisely so that discovery,
lifecycle, and version-compat machinery stay out of the TCB — a fleet transport
with all of that machinery belongs strictly on the QM side. ADR-0006 Clause 2 holds
even if Zenoh were dropped entirely; this ADR can never weaken it.

### Clause 3 — the remote grant TERMINATES AT THE VERIFIER STORE (the grant rule)

The down-lane grant composes with the operator-console clearance design; it does
**not** create a second release path:

1. The ops/cloud side publishes a **signed** `SignedClearanceGrant`.
2. The vehicle side's subscriber **verifies the signature**, then writes the grant
   through the **EXISTING Phase-A path** — `VerifierStore::save_clearance_grant_chained`
   — which lands a `PENDING-NODE-TRANSPORT` row and a signed audit event.
3. **Phase-B is unchanged**: `take_pending_clearance_grant` (the one-shot,
   exactly-once consume) + the loop's two-checkpoint re-validation at delivery
   proceed exactly as before.

**No new store schema. No new release semantics.** Remote transport is just another
way a `PENDING` row arrives; everything downstream is the audited, one-shot path
that already exists. (If the store had lacked a public write API for this, the
correct move would have been to STOP rather than add a route/schema — it did not;
`save_clearance_grant_chained` is public and is exactly that seam.)

## Namespace — versioned key expressions

`v1` is the wire-contract version; a breaking change bumps it. Up-lane and
down-lane are split by the `fleet/` vs `ops/` segment:

| Direction | Key expression | Payload |
|---|---|---|
| Up (vehicle → fleet) | `kirra/v1/fleet/{node_id}/trust-report` | signed `FederatedTrustReportV2` |
| Up (vehicle → fleet) | `kirra/v1/fleet/{node_id}/posture` | `PostureSummary` (advisory telemetry) |
| Down (ops → vehicle) | `kirra/v1/ops/{node_id}/clearance-grant` | signed `SignedClearanceGrant` |

## Wire codec

**JSON** (`serde_json`) on the fleet wire — **not** the bincode of
`kirra-wire-client`'s governor UDP hot path: the fleet lane is a debuggable
ops/cloud hop and trust is anchored in the Ed25519 signature over the *canonical*
payload (not the wire bytes), so JSON costs no trust and buys cross-tooling
debuggability.

## Considered and rejected

- **Zenoh as (or near) the safety channel** — rejected by Clause 2 / ADR-0006 C2:
  a general-purpose pub/sub transport in the safety partition imports its entire
  discovery/lifecycle/version surface into the TCB.
- **Trust from transport identity / topic ACLs / Zenoh auth** — rejected by
  Clause 1: that would make the carrier the trust root. Trust is the payload
  signature; the carrier is replaceable.
- **A second remote release path for grants** — rejected by Clause 3: the grant
  must land as a `PENDING` row in the existing store and be consumed by the
  existing one-shot Phase-B pickup; a parallel release path would be an
  unaudited bypass.
- **bincode on the fleet wire** (to match `kirra-wire-client`) — not chosen:
  the governor hot path values compactness; the fleet lane values debuggability,
  and the codec is trust-irrelevant here.

## Honest limits (named, not built)

- **Router / cellular / NAT** — real fleets need a Zenoh router and cellular/NAT
  traversal; that is **ops/router deployment territory** (a deployment doc), not
  this crate. The spike uses in-process localhost peer sessions.
- **QoS / delivery guarantees** — `zenoh-ext`'s `AdvancedPublisher` / reliability
  is the path to delivery guarantees over a lossy cellular link; a **named
  future**, not in the spike.
- **Multi-tenant fleet side** — a per-controller **public-key registry** (which
  key verifies which source) and per-operator identity at fleet scale is **#314**.
  The spike verifies against a supplied key; it does not yet manage a keyring.
- **TLS + the toolchain wall (MSRV) — now cleared, opt-in.** Zenoh's TLS features
  pull a `time` / x509 chain that used to hit an `E0119` conflicting-impl break on
  the bench toolchain (rustc 1.94.1) — the anticipated toolchain wall. Bumping
  `time` 0.3.48 → 0.3.53 in the workspace lockfile clears it, so `transport_tls` is
  now **enabled and opt-in**: `transport::fleet_peer_config(listen, connect, tls)`
  builds a plaintext `tcp/...` session when `tls` is `None` (byte-identical default)
  and an encrypted, cert-verified `tls/...` session when a `FleetTlsConfig`
  (cert/key/CA PEM paths, optional mTLS) is supplied. Per Clause 1 the plaintext
  path still costs no trust. Zenoh's TLS uses the **`ring`** rustls provider (not
  `aws-lc-rs`), so it does not conflict with the verifier's ring-only rustls
  invariant. The `zenoh-pinned-deps-1-75` dependency lockdown remains the escape
  hatch if a future transitive break appears. Zenoh's runtime additionally requires
  a multi-threaded scheduler (the tests use `flavor = "multi_thread"`).

## Conditions that reopen this decision

- A QoS/reliability requirement that `zenoh-ext` stable APIs cannot meet.
- A certification finding that the QM/safety split (Clause 2) is insufficiently
  enforced (e.g. a discovered dependency edge into a safety crate).
- A move of the wire-contract version (`v1` → `v2`) that changes payload trust.
