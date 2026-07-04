# Per-Principal API Tokens, Scoped RBAC & Attribution (#G7 / WS-1)

**Status:** LIVE (unified). Gap **G7 — key/identity lifecycle**
(`INDUSTRY_BENCHMARK_GAP_ANALYSIS.md`), workstream **WS-1**
(`docs/roadmap/PRODUCT_EXECUTION_PLAN.md`). TPM-bound signing-key rotation and
in-process TLS/mTLS are the tracked remainders (§5).

> **History / unification.** WS-1 was implemented twice in parallel: an
> env-configured registry (`KIRRA_PRINCIPAL_TOKENS`, PRs #802/#803) and a
> DB-backed scoped-authz engine (the advisor branch). The **unification kept the
> DB-backed engine** — one token system, one RBAC model — and **removed** the env
> registry (see CHANGELOG). The #804 attribution middleware and the #805
> transport-security gate carried over and compose with it. Migration: mint a
> principal per env entry; `readonly` maps to the `auditor` role.

## 1. The gap

The admin+actuator surface was gated by a **single shared bearer token**
(`KIRRA_ADMIN_TOKEN`). Its compromise exposes the whole surface, it cannot be
rotated or revoked per-holder, and every mutation is attributed to the same
anonymous credential.

## 2. The unified model

One fail-closed authorization engine (`src/authz.rs`) gates every protected route
group by **scope**, satisfied by EITHER the break-glass root `KIRRA_ADMIN_TOKEN`
(all scopes) OR a **DB-backed API principal** whose role holds the scope:

| Role | Scope(s) | Surface |
|---|---|---|
| `admin` | every scope | full mutation surface (`admin_routes`) + everything below |
| `integrator` | `integration:evaluate` | identity-gated evaluations (action-filter / industrial / federation-submit / posture-stream) |
| `operator` | `actuator:command` | `POST /actuator/motion/command` |
| `auditor` | `audit:read` | read-only audit verify / causal-verify / export (`auditor_routes`, carved out of the admin group) |

- **Lifecycle:** mint / list / revoke via the **admin-scoped**
  `POST/GET /system/principals` and `POST /system/principals/{id}/revoke`.
  The server generates a 256-bit token from the OS CSPRNG and returns the
  plaintext **exactly once**; the store holds only its **SHA-256** (lookup is by
  hash — plaintext never persisted). Re-minting a principal rotates its token
  and clears any revocation.
- **Pure decision core:** `authz::authorize_request(scope, admin, bearer,
  principal)` reads no env and no store (INVARIANT #13); the middleware
  (`authorize_scope`, `src/bin/kirra_verifier_service/auth.rs`) lifts env + the
  hashed store lookup in, and fail-closes to "no principal" on any store error.
- **Attribution (slice 3, #804):** on Allow the resolved identity
  (`AuthenticatedPrincipal { label, role }` — the principal id, or `root` for
  the break-glass token) is attached to the request extensions; after a
  **successful admin mutation** the `record_admin_action_audit` middleware
  appends an `ADMIN_ACTION` event to the signed hash-chained ledger naming who
  changed what. Reads and failures are not recorded; the actuator route and the
  self-auditing evaluations are deliberately excluded.
- **Transport security (#805):** `KIRRA_REQUIRE_SECURE_TRANSPORT` layers
  `require_secure_transport` OUTERMOST on every gated group (admin, auditor,
  actuator, identity-gated, attestation) — a credential or nonce is never
  processed off a leg not asserted as TLS by the trusted proxy/mesh
  (`docs/safety/TRANSPORT_SECURITY.md`, AOU-TRANSPORT-TLS-001).

## 3. Fail-closed & invariant preservation

1. **INVARIANT #1/#6 verbatim.** `KIRRA_ADMIN_TOKEN` absent/empty → **503** for
   every gated route, unconditionally — a per-principal token never authorizes
   without a configured root (an API principal only ADDS least privilege on top).
   `require_admin_token` is preserved by name as the `SCOPE_ADMIN`
   specialization.
2. **INVARIANT #2.** The root token is compared via `admin_token_ok` /
   `constant_time_compare`; principal tokens are resolved by **SHA-256 hash
   lookup** — no `==` ever touches a raw secret.
3. **Fail-closed everywhere:** unknown token → 401; revoked → 401; authenticated
   but under-scoped → 403 (distinct, so "wrong token" ≠ "insufficient
   privilege"); corrupt stored role → 401; store error → treated as no
   principal (401); denials are logged, not chained (an unauthenticated caller
   must not append to the audit ledger — denial-flood DoS).
4. **INVARIANT #13.** The decision truth table (`authz::tests`), the store↔authz
   composition (`tests/authz_rbac.rs`), and the router ordering
   (`auth::g7_transport_security_router_tests`) are all tested without
   `set_var`.
5. **Back-compat:** an admin-token-only deployment (no principals minted) is
   byte-compatible with the pre-WS-1 behavior.

## 4. Route authorization matrix

See `CLAUDE.md` → "Route Authorization Matrix" (updated for scopes) — the
matrix there is the maintained copy.

## 5. Tracked remainders (rest of G7 / WS-1)

1. **TPM-bind the governor release-token signing key** — the fail-closed
   key-provisioning seam LANDED (file / dev-fixed sources,
   `docs/safety/GOVERNOR_KEY_PROVISIONING.md`); the **TPM-unseal source** remains
   the hardware-gated follow-up (tss2 libs + hardware).
2. **In-process TLS termination + mTLS client-cert → principal identity** —
   ✅ **LANDED** (Track 1.2, `TRANSPORT_SECURITY.md` §4). Opt-in server-side TLS
   (`KIRRA_TLS_CERT_PATH`/`KEY_PATH`) plus mTLS (`KIRRA_TLS_CLIENT_CA_PATH`): a
   CA-verified client cert's SHA-256 fingerprint pins to a `cert_principals`
   principal, feeding the same `ResolvedPrincipal` as the bearer path (no-bearer
   only). Admin registry `POST/GET /system/cert-principals` + `.../{id}/revoke`.
3. **Promoting scoped-principal ALLOW decisions into the audit chain**
   (rate-limited) — currently allows are traced, mutations are chained.

## 6. Test traceability

| Property | Test |
|---|---|
| RBAC matrix (roles × scopes, least privilege) | `authz::tests` (`admin_holds_every_scope`, `non_admin_roles_are_least_privilege`, role parse round-trip) |
| Decision truth table (503/401/403/Allow; revoked; corrupt role) | `authz::tests` |
| Store↔authz composition (mint → resolve-by-hash → authorize) | `tests/authz_rbac.rs` |
| Principal mint/revoke/rotate persistence | `verifier_store::principals` tests |
| **No fail-open without root token** | `authz::tests` (Unconfigured precedence) + `tests/authz_rbac.rs` |
| Attribution: only successful mutations recorded | `auth::g7_admin_action_attribution_tests` (binary) |
| Transport gate wired, outermost, off-by-default; attestation + auditor groups gated | `auth::g7_transport_security_router_tests` (binary) |
| Principal routes classify as WriteState (posture gate) | `gateway::policy::tests::test_classifies_api_principal_writes` |
| mTLS cert principal: register/rotate/revoke/resolve-by-fingerprint | `verifier_store::cert_principals` tests |
| mTLS identity: cert principal authorizes WITHOUT a bearer (no-bearer path, 503/403/401 fail-closed) | `authz::tests` (`cert_principal_*`) |
| mTLS transport: CA-verified handshake injects the leaf fingerprint; no-cert client rejected | `tls::tests` (`live_mtls_handshake_injects_client_cert_fingerprint`, `mtls_rejects_a_client_with_no_certificate`) |
| Cert-principal routes classify as WriteState (posture gate) | `gateway::policy::tests::test_classifies_cert_principal_writes` |
