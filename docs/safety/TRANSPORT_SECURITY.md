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
- `request_transport_is_secure(require, header, headers)` — the **pure**, unit-
  tested decision.
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

When enforcement is ON, admit ONLY if the forwarded-proto header is present,
readable, and its **original-client** value — the FIRST entry of a possibly
comma-listed `client,proxy,…` chain (standard `X-Forwarded-Proto` semantics) — is
`https` (case-insensitive). Every other case denies:

| Header | Verdict |
|---|---|
| `https` / `HTTPS` / ` https ` | **admit** |
| `https, http` (client leg https) | **admit** |
| absent | **deny** (403) |
| `http` / `""` / unreadable | **deny** (403) |
| `http, https` (client leg plaintext) | **deny** (403) |

## 4. Assumption of use & the in-process-TLS alternative

**AOU-TRANSPORT-TLS-001 (load-bearing):** the trusted proxy/mesh MUST set —
overwriting any client-supplied value — the forwarded-proto header. A
directly-reachable (un-proxied) verifier would let a client spoof it, so this
enforcement is sound ONLY behind a trusted proxy — the same assumption that backs
`KIRRA_TRUSTED_INGRESS_MODE` / `x-kirra-client-id`.

**Tracked follow-up — in-process TLS termination.** Terminating TLS on the verifier
itself (e.g. `axum-server` + rustls, `KIRRA_TLS_CERT_PATH`/`KIRRA_TLS_KEY_PATH`,
fail-closed on partial config) removes the AoU for deployments without a mesh, and
**mTLS client-certificate identity** (mapping a client cert to a principal, tying
into the RBAC of `PRINCIPAL_TOKENS.md`) is the natural next step after that. Both
are deferred as a deliberate serve-path change (new dependency + rustls crypto-
provider selection + a live-handshake test).

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

**Env robustness (Copilot #805):** `KIRRA_REQUIRE_SECURE_TRANSPORT` is parsed
case-insensitively and trimmed (`TRUE`/`True`/` true ` all enable — a security
toggle must not silently stay off), and `KIRRA_FORWARDED_PROTO_HEADER` is
trimmed + lowercased (trailing whitespace would otherwise be an invalid header name
that denies every request).
