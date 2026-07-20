# Transport-Security Enforcement (#G7)

**Status:** LIVE. The **mesh-mTLS** half of gap **G7**'s TLS track — the roadmap's
"TLS on the verifier **OR mandated mesh with enforcement check**"
(`INDUSTRY_BENCHMARK_GAP_ANALYSIS.md` G7, roadmap §11.11). In-process TLS
termination on the verifier itself is the parallel option, tracked in §4.

## 1. The gap

The verifier's HTTP listener is a **plaintext bind**. In a service-mesh deployment
that is by design — the verifier binds over loopback to a trusted sidecar that
performs (m)TLS with peers — but nothing *enforced* that a request actually arrived
over TLS. A misconfigured mesh (or a directly-reachable pod) could serve admin and
actuator traffic in cleartext undetected.

## 2. What this adds

A **fail-closed transport-security gate**: when enabled, a request that the trusted
proxy/mesh does not assert arrived over TLS is **rejected with 403 before
authentication** — a bearer token or attestation nonce is never processed off a
plaintext leg.

- `TransportSecurityConfig` (`src/verifier.rs`) — `require_secure_transport` +
  `forwarded_proto_header`, from env.
- `request_transport_is_secure(require, connection_is_tls, header, headers)` — the
  **pure**, unit-tested decision. `connection_is_tls` is the authoritative tier-1
  signal (see §3): a server-side, client-unspoofable request extension
  (`ServerTerminatedTls`) injected only on the post-handshake in-process-TLS serve
  path.
- `require_secure_transport` middleware — layered **OUTERMOST** on the sensitive
  route groups (admin, the WS-1 auditor read group, actuator, identity-gated,
  **and attestation** — the challenge/verify nonce flow, even though it is
  otherwise unauthenticated), so it runs before auth.

### Env vars

| Var | Default | Meaning |
|---|---|---|
| `KIRRA_REQUIRE_SECURE_TRANSPORT` | `false` | `1`/`true` → enforce. Off → the gate is a **no-op** (byte-identical to before). |
| `KIRRA_FORWARDED_PROTO_HEADER` | `x-forwarded-proto` | The header the trusted proxy sets to the client's protocol. |

## 3. Decision & fail-closed rules

When enforcement is ON the decision is **connection-first, two-tier**:

- **Tier 1 — real connection (unspoofable).** If the request arrived over the
  verifier's own in-process TLS terminator (`ServerTerminatedTls` extension present),
  **admit** — the transport is TLS by construction, independent of any header. A
  client cannot set this: request extensions are server-side typed values injected
  only on the post-handshake serve path.
- **Tier 2 — forwarded-proto header (proxy assertion).** Only when tier 1 does not
  apply (no in-process TLS): admit ONLY if the forwarded-proto header is present,
  readable, and its **original-client** value — the FIRST entry of a possibly
  comma-listed `client,proxy,…` chain (standard `X-Forwarded-Proto` semantics) — is
  `https` (case-insensitive). Every other case denies.

| Signal | Verdict |
|---|---|
| in-process-TLS connection (any/no header) | **admit** (tier 1) |
| header `https` / `HTTPS` / ` https ` | **admit** (tier 2) |
| header `https, http` (client leg https) | **admit** (tier 2) |
| no TLS connection, header absent | **deny** (403) |
| no TLS connection, header `http` / `""` / unreadable | **deny** (403) |
| no TLS connection, header `http, https` (client leg plaintext) | **deny** (403) |

## 4. Assumption of use & in-process TLS

**AOU-TRANSPORT-PROXY-001 (load-bearing for the tier-2 header fallback only):** when
the gate falls through to the forwarded-proto header — i.e. the verifier is NOT
terminating TLS in-process — the trusted proxy/mesh MUST set, overwriting any
client-supplied value, the forwarded-proto header. A directly-reachable (un-proxied)
verifier would let a client spoof it, so the header-based enforcement is sound ONLY
behind a trusted proxy — the same assumption that backs `KIRRA_TRUSTED_INGRESS_MODE`
/ `x-kirra-client-id`. **In-process TLS (below) removes this AoU** for deployments
that enable it: the tier-1 `ServerTerminatedTls` connection signal is authoritative
and the header is never consulted, because the transport is then TLS by construction
rather than by a proxy's assertion. **Startup WARNs (AOU-TRANSPORT-PROXY-001)** when
`KIRRA_REQUIRE_SECURE_TRANSPORT` is ON but in-process TLS is OFF — the configuration
that leaves the gate depending on this AoU — so an operator running un-proxied sees
that the spoofable header is the only barrier.

**In-process TLS termination — LIVE (opt-in, server-side; WS-1 Track 1.2).**
Terminating TLS on the verifier itself is **opt-in and default-OFF**: with neither
env var set the serve path is byte-identical plaintext (`axum::serve`), so ADR-0006
Clause 3's mesh-first default is unchanged — this only ADDS TLS as an option.

| Var | Default | Meaning |
|---|---|---|
| `KIRRA_TLS_CERT_PATH` | — | PEM certificate-chain path. |
| `KIRRA_TLS_KEY_PATH` | — | PEM private-key path. |

- Set **both** → the verifier terminates TLS in-process (`src/bin/kirra_verifier_service/tls.rs`).
- Set **exactly one** → **fail-closed startup abort** before bind: a half-configured
  TLS listener must never silently fall back to plaintext.
- Invalid / missing / empty cert or key → abort before bind (validated before the
  port is claimed or systemd is told READY).
- rustls is pinned to the **`ring`** provider (explicit `builder_with_provider`), so
  no process-global provider is required and `aws-lc-rs` never enters the build; the
  accept loop hands each connection its own handshake task (no accept-loop
  head-of-line blocking — a DoS concern for a safety service). A live-handshake test
  (`tls::tests`) exercises a real client TLS handshake + HTTP round-trip against the
  terminator through the same production config-loader.

**mTLS client-certificate identity — LIVE (opt-in; WS-1 Track 1.2).** With server
TLS on, set `KIRRA_TLS_CLIENT_CA_PATH` (PEM) to REQUIRE + verify client certificates:

| Var | Default | Meaning |
|---|---|---|
| `KIRRA_TLS_CLIENT_CA_PATH` | — | PEM client-CA. Set (server TLS must also be on) → client certs required and CA-verified. Set without server TLS → fail-closed startup abort. |

- rustls's **audited `WebPkiClientVerifier`** does the cryptographic verification
  (chain to the configured CA + proof of possession) — no hand-rolled cert
  verification in the safety path. `ring` provider, as with the server side.
- The verified leaf's **SHA-256 fingerprint** is injected into request extensions
  (`ClientCertFingerprint`). When a request carries **no bearer token**, the auth
  layer resolves that fingerprint to a **cert principal** (`cert_principals` table),
  feeding the SAME `ResolvedPrincipal` the bearer path produces — one RBAC model
  (`PRINCIPAL_TOKENS.md`). A presented bearer is the explicit credential and is NOT
  silently rescued by a cert.
- **Pin, don't trust-the-CA-alone:** CA verification proves authenticity; the
  fingerprint pin (`POST /system/cert-principals`, admin-scoped) authorizes the
  SPECIFIC cert to a role. An unpinned (even CA-valid) cert resolves no principal →
  fail-closed 401. The server never sees the client's private key.
- Live tests (`tls::tests`): a CA-verified client cert handshake injects the correct
  fingerprint; a client presenting no cert is rejected at the handshake.

**Cert LIFECYCLE — expiry, renewal, revocation (WP-15 / MGA G-19).** A pinned cert
is not a permanent on/off switch — it is a lifecycle credential with a valid window:

- **Expiry.** `POST /system/cert-principals` accepts an optional `not_after_ms` (the
  cert's X.509 notAfter, computed offline alongside the fingerprint; it must be in the
  future). It is persisted on `cert_principals.not_after_ms` (nullable — an omitted /
  legacy value = no expiry tracked, never ages out). At resolution the auth layer
  fail-closes a cert at/past its `not_after_ms` (inclusive bound) exactly as it does a
  revoked one → **401**, with a distinct WARN so a lapse is not a mystery. Proven end
  to end in `tests/cert_lifecycle.rs` (valid before → Allow; at/after notAfter → 401).
- **Renewal (no restart).** Renewing is re-pinning the SAME principal with the renewed
  leaf's fingerprint + a later `not_after_ms` (`POST /system/cert-principals` rotates
  in place and clears any revocation). The very next resolution honors it — no process
  restart, and the old (lapsed) leaf no longer resolves
  (`renewal_restores_authorization_without_a_restart`).
- **Revocation** is honored on the next resolution (a fresh handshake) via
  `POST /system/cert-principals/{id}/revoke` — unchanged, and independent of expiry
  (`revocation_is_honored_on_the_next_resolution`).
- **Observability.** A background monitor (`cert_expiry_monitor`, hourly) censuses the
  registry and WARN-logs + hash-chain-audits (`CertPrincipalExpiryWarning`) when a cert
  has lapsed or is within the 14-day renewal window — "warn before it bites." The same
  census rides `/metrics` as the `kirra_cert_principals{state="active|revoked|expired|
  expiring_soon|no_expiry"}` gauge family (posture-exempt, so it survives LockedOut).
- **CRL:** a file-based CRL at the TLS verifier callback is the recorded follow-up; the
  explicit-revocation + expiry gates above are the live lifecycle-enforcement path
  today (a revoked/expired pin stops authorizing at the next handshake regardless).
- `GET /system/cert-principals` surfaces `not_after_ms`, `expired`, and `valid` per
  principal (evaluated at request time).

## 5. Test traceability

| Property | Test (`verifier::transport_security_tests`) |
|---|---|
| Disabled → admit all (backward-compatible) | `disabled_admits_everything_backward_compatible` |
| Enabled → requires `https` (case/trim) | `enabled_requires_https_assertion` |
| Enabled → absent/plaintext/empty deny (fail-closed) | `enabled_rejects_insecure_or_absent_fail_closed` |
| Proxy-chain: original client leg governs | `enabled_uses_original_client_protocol_from_a_proxy_chain` |
| Custom header name respected | `custom_header_name_is_respected` |
| **Router-level: gate wired, OUTERMOST (403 before auth), off by default** | `g7_transport_security_router_tests::secure_transport_gate_is_wired_outermost_and_fail_closed` (binary) |
| **Attestation nonce flow gated** | `g7_transport_security_router_tests::attestation_challenge_is_gated_by_secure_transport` (binary) |
| **In-process TLS: partial config is fail-closed** | `tls::tests::exactly_one_set_is_fail_closed_error` (binary) |
| **In-process TLS: invalid/missing cert-key fail-closed** | `tls::tests::missing_or_garbage_files_are_fail_closed` (binary) |
| **In-process TLS: live handshake terminates + serves** | `tls::tests::live_handshake_terminates_tls_and_serves_requests` (binary) |

**Env robustness (Copilot #805):** `KIRRA_REQUIRE_SECURE_TRANSPORT` is parsed
case-insensitively and trimmed (`TRUE`/`True`/` true ` all enable — a security
toggle must not silently stay off), and `KIRRA_FORWARDED_PROTO_HEADER` is
trimmed + lowercased (trailing whitespace would otherwise be an invalid header name
that denies every request).
