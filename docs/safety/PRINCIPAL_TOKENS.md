# Per-Principal Admin Tokens (#G7, slice 1)

**Status:** LIVE. First slice of gap **G7 — key/identity lifecycle**
(`INDUSTRY_BENCHMARK_GAP_ANALYSIS.md`). RBAC scoping, TPM key-binding, and
TLS/mTLS are tracked remainders (§4).

## 1. The gap

The admin+actuator surface was gated by a **single shared bearer token**
(`KIRRA_ADMIN_TOKEN`). Its compromise exposes the whole surface, it cannot be
rotated or revoked per-holder, and every mutation is attributed to the same
anonymous credential. (`INDUSTRY_BENCHMARK_GAP_ANALYSIS.md` G7.)

## 2. What this slice adds

An operator can issue a **named token per principal** — rotate or revoke one
without disturbing the others, and attribute each admin mutation to a named
identity — **without sharing** the root `KIRRA_ADMIN_TOKEN`.

- `parko`-free, pure primitive in `src/security.rs`:
  - `PrincipalRegistry` — the `(principal_id, token)` set, `from_env()` / `parse()`.
  - `authorize_admin(provided, configured_admin, registry) -> Option<AdminPrincipal>`
    — the single fail-closed decision the `require_admin_token` middleware gates on.
  - `AdminPrincipal::{Root, Named(id)}` — the resolved identity, attached to the
    request extensions and logged for attribution.

### Env vars

| Var | Meaning |
|---|---|
| `KIRRA_ADMIN_TOKEN` | **Unchanged.** The root admin token. Absent/empty → **HTTP 503** for every admin route (INVARIANT #1/#6). |
| `KIRRA_PRINCIPAL_TOKENS` | Optional. `principal_id=token` entries, separated by `,` `;` or newlines. Only the FIRST `=` splits id from token (tokens may contain `=`). Whitespace-trimmed; an entry with no `=`, an empty id, or an empty token is **ignored** (never a credential). |

## 3. Fail-closed & invariant preservation

The extension is **purely additive** and **cannot fail open**:

1. **INVARIANT #1/#6 unchanged.** `require_admin_token` still returns **503**
   when `KIRRA_ADMIN_TOKEN` is absent/empty, as the FIRST check — before the
   registry is ever consulted. A per-principal token therefore **never**
   authorizes without a configured root token
   (`principal_token_denied_when_root_token_absent_or_empty`).
2. **INVARIANT #2.** Every token comparison — root and per-principal — goes
   through `constant_time_compare`. `PrincipalRegistry::resolve` compares against
   **every** entry with no early-out, so a match does not leak its position by
   timing. A token that matches the same id twice (overlapping-window rotation)
   resolves to that id; a token that matches **multiple distinct** ids is an
   ambiguous misconfiguration and **denies** (fail-closed — never a
   non-deterministic audit identity).
3. **Root path unchanged.** With `KIRRA_PRINCIPAL_TOKENS` unset the registry is
   empty and behavior is byte-identical to before (root-token-only).
4. **INVARIANT #13.** The decision logic is pure and unit-tested without touching
   process env (env is read only by the thin `from_env` wrapper), exactly like
   the sibling `admin_token_ok` (SG-015).

## 4. Tracked remainders (rest of G7)

1. **RBAC scoping.** v1 grants every principal the SAME capability as the root
   token (admin-equivalent). Per-route capability scoping (read-only vs mutate vs
   actuator) is the next slice — `AdminPrincipal` already carries the identity to
   scope on.
2. **Audit-chain attribution.** The principal is attached to the request
   extension and logged; recording it on each SHA-256-chained audit row is a
   follow-up.
3. **TPM-bind the governor release-token signing key** (`tpm.rs` exists at the
   fleet layer) — remove the in-process signing key.
4. **TLS / mTLS on the verifier** — the bind is currently plaintext with
   header-based identity.

## 5. Test traceability

| Property | Test (`security::g7_principal_token_tests`) |
|---|---|
| Parse forms; malformed dropped | `parse_keeps_wellformed_drops_malformed` |
| Empty provided never matches | `resolve_empty_provided_never_matches` |
| Root token → `Root` | `authorize_root_token_is_root_principal` |
| Principal token → `Named(id)`; unknown denies | `authorize_principal_token_is_named_and_attributed` |
| **Principal denied when root token absent/empty (no fail-open)** | `principal_token_denied_when_root_token_absent_or_empty` |
