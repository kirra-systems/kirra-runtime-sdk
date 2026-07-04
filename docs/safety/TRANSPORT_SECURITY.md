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

## 4. Assumption of use & in-process TLS

**AOU-TRANSPORT-TLS-001 (load-bearing for the mesh-mTLS gate above):** the trusted
proxy/mesh MUST set — overwriting any client-supplied value — the forwarded-proto
header. A directly-reachable (un-proxied) verifier would let a client spoof it, so
the header-based enforcement is sound ONLY behind a trusted proxy — the same
assumption that backs `KIRRA_TRUSTED_INGRESS_MODE` / `x-kirra-client-id`. **In-process
TLS (below) removes this AoU** for deployments that enable it, because the transport
is then TLS by construction rather than by a proxy's assertion.

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

**Tracked follow-up — mTLS client-certificate identity.** Mapping a verified client
certificate to a principal (feeding the same `ResolvedPrincipal` the bearer path
produces, tying into `PRINCIPAL_TOKENS.md` RBAC) is the natural next slice; deferred
so the server-side serve-path change lands and bakes first.

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
