# Per-Principal Admin Tokens + RBAC + Attribution (#G7, slices 1–3)

**Status:** LIVE. Gap **G7 — key/identity lifecycle**
(`INDUSTRY_BENCHMARK_GAP_ANALYSIS.md`). Slice 1 = per-principal tokens
(rotation / revocation); slice 2 = coarse method-based **RBAC** (`readonly`
scope); slice 3 = **audit-chain attribution** (successful admin mutations recorded
in the signed hash chain, naming the principal). Finer per-endpoint capabilities,
per-domain-event attribution, TPM key-binding, and TLS/mTLS are tracked
remainders (§4).

## 1. The gap

The admin+actuator surface was gated by a **single shared bearer token**
(`KIRRA_ADMIN_TOKEN`). Its compromise exposes the whole surface, it cannot be
rotated or revoked per-holder, and every mutation is attributed to the same
anonymous credential. (`INDUSTRY_BENCHMARK_GAP_ANALYSIS.md` G7.)

## 2. What this slice adds

An operator can issue a **named token per principal** — rotate or revoke one
without disturbing the others, and attribute each admin mutation to a named
identity — **without sharing** the root `KIRRA_ADMIN_TOKEN`.

- `parko`-free, pure primitives in `src/security.rs`:
  - `PrincipalRegistry` — the `(principal_id, role, token)` set, `from_env()` / `parse()`.
  - `authorize_admin(provided, configured_admin, registry) -> Option<AdminPrincipal>`
    — the single fail-closed **authentication** decision.
  - `admin_rbac_allows(role, method) -> bool` — the pure **RBAC** decision the
    middleware applies after authentication (slice 2).
  - `AdminPrincipal::{Root, Named{id, role}}` — the resolved identity + scope,
    attached to the request extensions and logged for attribution.
  - `Role::{Admin, ReadOnly}` — the RBAC scope.

### Env vars

| Var | Meaning |
|---|---|
| `KIRRA_ADMIN_TOKEN` | **Unchanged.** The root admin token (always `Admin` role). Absent/empty → **HTTP 503** for every admin route (INVARIANT #1/#6). |
| `KIRRA_PRINCIPAL_TOKENS` | Optional. `principal_id[:role]=token` entries, separated by `,` `;` or newlines. Only the FIRST `=` splits the id/role part from the token (tokens may contain `=`); the id part's FIRST `:` splits id from role. `role` ∈ {`admin`, `readonly`} (case-insensitive; omitted → `admin`). Whitespace-trimmed; an entry with no `=`, an empty id, an empty token, OR an **explicit-but-unrecognized role** is **ignored** (never a credential — a typo'd role can't silently become `admin`). |

### RBAC (slice 2)

Coarse and **method-based**, so it is enforced in the single admin middleware
with no router changes:

| Role | May do | Denied |
|---|---|---|
| `Admin` (and the root token) | everything | — |
| `ReadOnly` | nullipotent methods only (`GET`/`HEAD`/`OPTIONS`) — the admin reads: audit-verify, fabric state/telemetry, subsystem lists | **every mutation (all POST admin routes) AND the actuator** → `403 Forbidden` |

A `readonly` token is therefore a safe least-privilege **monitoring / audit**
credential that can never register a node, export a backup, rotate a key, or
command an actuator.

### Attribution audit (slice 3)

After a **successful** admin **mutation** (a 2xx response on a non-safe method),
`require_admin_token` appends an `ADMIN_ACTION` event to the **signed, hash-chained
audit ledger** (`save_posture_event_chained` → `append_audit_event_tx`) with
`{ principal, role, method, path }`. So the tamper-evident record names **who
changed what, and when** — the accountability the single shared token could never
provide.

- Only successful mutations are recorded (`should_record_admin_action`): reads
  (`GET`) and failed requests (non-2xx) are not, so the ledger names who actually
  CHANGED state, not every authorized touch.
- The write is **best-effort** and off the request's critical section: a failure
  is logged and increments no request error — it never fails an already-completed
  mutation (mirrors the `action_filter` audit path).
- The `ADMIN_ACTION` row is a distinct, correlated attribution row; embedding the
  actor into each domain event (the node-registration row itself) is a §4 refinement.

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

5. **INVARIANT #2 (RBAC too).** A token that resolves to multiple **distinct
   (id, role)** pairs is ambiguous — attribution OR privilege would be
   non-deterministic — so it **denies** (never silently picks a scope).

## 4. Tracked remainders (rest of G7)

1. **Finer per-endpoint capabilities.** Slice 2's RBAC is coarse (read vs
   mutate). A capability model (e.g. distinguishing node-registration from
   backup-export from actuator) is a follow-up — `AdminPrincipal` carries the
   identity + role to scope on.
2. **Per-domain-event attribution.** Slice 3 records a correlated `ADMIN_ACTION`
   row per successful mutation; embedding the actor INTO each domain event's own
   audit row (so the node-registration row itself names the principal) is a
   finer follow-up.
3. **TPM-bind the governor release-token signing key** (`tpm.rs` exists at the
   fleet layer) — remove the in-process signing key.
4. **TLS / mTLS on the verifier** — the bind is currently plaintext with
   header-based identity.

## 5. Test traceability

| Property | Test (`security::g7_principal_token_tests`) |
|---|---|
| Parse forms; malformed dropped | `parse_keeps_wellformed_drops_malformed` |
| Role parse/scope; unrecognized role dropped | `parse_roles_and_scope` |
| Empty provided never matches | `resolve_empty_provided_never_matches` |
| Rotation resolves; distinct id **or role** collision denies | `same_id_rotation_resolves_but_distinct_id_collision_denies` |
| Root token → `Root` (Admin) | `authorize_root_token_is_root_principal` |
| Principal token → `Named{id, role}`; unknown denies | `authorize_principal_token_is_named_and_attributed` |
| **Principal denied when root token absent/empty (no fail-open)** | `principal_token_denied_when_root_token_absent_or_empty` |
| **RBAC: `ReadOnly` allowed only on safe methods** | `admin_rbac_allows_read_only_only_on_safe_methods` |
| **Attribution: only successful mutations recorded** | `g7_admin_action_attribution_tests::admin_action_recorded_only_on_successful_mutation` (binary) |
