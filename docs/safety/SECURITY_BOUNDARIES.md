# Kirra Safety Kernel — Security Boundaries

**Status:** Evidence / design-decision record.
**Cross-refs:** `REQUIREMENTS_TRACEABILITY.md` (SG-015), `src/bin/kirra_verifier_service.rs`
(router construction), `src/security.rs` (`admin_token_ok`, `constant_time_compare`),
`src/gateway/policy_layer.rs` (`enforce_posture_routing`, `is_posture_exempt`),
CLAUDE.md "Route Authorization Matrix".

---

## SG-015 — Admin-Token Mutation Gate: Coverage and the Attestation-Handshake Carve-Out

**Claim (SG-015, ASIL B).** Privileged mutation routes are fail-closed behind
`require_admin_token`: the request is denied (503 when `KIRRA_ADMIN_TOKEN` is absent/empty,
401 on mismatch) unless a token is presented that matches the configured value under
`constant_time_compare` (never `==`). The check is the pure predicate
`security::admin_token_ok`, unit-tested in `src/security.rs` mod `sg_015_admin_token_tests`.

> **WS-1 (#G7) update — scoped RBAC layered on top, admin root preserved.** The
> gate is now the unified `authorize_scope` predicate (`src/authz.rs`,
> `authorize_request`). `require_admin_token` is PRESERVED by name and role as the
> `SCOPE_ADMIN` specialization (INVARIANT #1/#6 verbatim: absent/empty root →
> 503; the admin token is compared constant-time). It is only ever a STRICT
> SUPERSET: a per-principal API token (`api_principals` table; role ∈
> {`admin`,`integrator`,`auditor`,`operator`}) may ADDITIONALLY authorize a route
> whose scope its role holds. Absent/empty `KIRRA_ADMIN_TOKEN` still fail-closes
> the WHOLE surface to 503 regardless of principals — principals never substitute
> for the root. Decision truth-table: `authz::tests`; store↔authz composition:
> `tests/authz_rbac.rs`; router wiring fail-closed: `posture_gate_real_router_tests
> ::ws1_scope_gated_routes_fail_closed_on_real_router`.

### Gated mutation routes

Each route group terminates in a scope layer (WS-1: `middleware::from_fn_with_state
(svc, require_<scope>)`; the admin group keeps `require_admin_token`). Every scope
is satisfied by the break-glass admin token (Admin holds all scopes), so an
admin-token-only deployment is unchanged:

- **identity/industrial group** (`identity_gated_routes`, `SCOPE_INTEGRATION_EVALUATE`
  via `require_integration_scope`; additionally gated by `require_client_identity`):
  `/federation/reports/submit`, `/action_filter/evaluate`, `/industrial/evaluate`,
  `/industrial/ethernet-ip/evaluate`, `/industrial/canopen/evaluate`,
  `/industrial/dnp3/evaluate` (and the `/system/posture/stream` SSE). Admin token or
  an `integrator`-role principal.
- **admin group** (`admin_routes`, `SCOPE_ADMIN` via `require_admin_token`):
  `/attestation/register`, `/fleet/dependencies`, `/fleet/diagnostics/report`,
  `/fleet/assets/register`, `/system/backup/export`,
  `/system/audit/rotate-signing-key`, `/federation/controllers/register`,
  `/attestation/identity/register`, `/fabric/assets/register`,
  `/fabric/command/{asset_id}`, and the WS-1 API-principal registry
  (`POST/GET /system/principals`, `POST /system/principals/{principal_id}/revoke`).
  (The same layer additionally gates the admin GET reads — `/fabric/assets`,
  `/fabric/state`, `/fabric/telemetry[/{asset_id}]`, `/fabric/causal-log[/{entry_id}]`,
  `/fleet/av-subsystems` — so they are admin-only too.) Admin token or an
  `admin`-role principal.
- **auditor group** (`auditor_routes`, `SCOPE_AUDIT_READ` via `require_audit_scope`) —
  WS-1 carve-out of the read-only audit surface for a least-privilege auditor:
  `/system/audit/verify`, `/system/audit/causal/verify`, `/system/audit/export`.
  Admin token or an `auditor`-role principal (NO mutation rights). The full-state
  `/system/backup/export` dump and the `rotate-signing-key` mutation deliberately
  stay in the admin group.
- **actuator group** (`actuator_routes`, `SCOPE_ACTUATOR_COMMAND` via
  `require_actuator_scope`; auth outermost, then the inner safety envelope):
  `/actuator/motion/command`. Admin token or an `operator`-role principal — the
  envelope and the posture gate independently bound WHAT command is accepted.

### Deliberate carve-out — the attestation handshake (NOT a bypass)

Two POST routes are intentionally **outside** the admin-token gate (`attestation_routes`
is merged with no `require_admin_token` layer):

- `POST /attestation/challenge/{node_id}` (`issue_challenge`)
- `POST /attestation/verify` (`verify_attestation`)

These are the trust-establishment handshake. Admin-gating them would be **circular**: a node
cannot hold an admin token before it has been attested, so requiring one would make
attestation unreachable for every not-yet-trusted node. (This matches the CLAUDE.md route
matrix, which lists both as "Unauthenticated — challenge-response provides its own guarantee".)

#### Why the carve-out is safe (compensating controls)

1. **Authentication is the attestation protocol itself** — the nonce challenge plus the
   signature / PCR-quote verification performed by `verify_attestation` — not the admin
   token. A forged or invalid quote establishes no trust; the verify handler validates the
   quote before any trust state changes (per CRITICAL SECURITY INVARIANT #3,
   `verify_attestation` must cryptographically verify a per-node Ed25519 proof and
   fail-closed — it must never mock trust).
2. **No privileged fleet mutation.** `challenge` issues a nonce; `verify` validates a quote
   and, only on success, records an attested identity. Both failure paths grant nothing.
3. **Bounded blast radius** — limited to identity establishment for a single `node_id`. All
   fleet / actuator / system state mutations remain behind the admin gate above.

### Assessor note

SG-015's "all mutation route handlers call `require_admin_token`" (TR-015a) should be read as
**all *privileged* mutation routes**, with the two attestation-handshake endpoints as an
explicit, justified exception authenticated by the attestation protocol rather than the admin
token. This entry exists so the router's two un-gated POSTs read as a recorded design
decision, not an oversight. **If a future change moves trust-affecting logic into these
handlers, this carve-out must be re-evaluated.**

### Verification (this branch)

Router wiring confirmed in `src/bin/kirra_verifier_service.rs`: `identity_gated_routes`,
`admin_routes`, `auditor_routes`, and `actuator_routes` each terminate in a
`.layer(middleware::from_fn_with_state(svc, require_<scope>))` scope layer (the admin
group's is `require_admin_token`, the `SCOPE_ADMIN` specialization); `attestation_routes`
(the two handshake POSTs) is constructed and merged with **no** such layer. The root
predicate `security::admin_token_ok` still uses `constant_time_compare` (never `==`) and is
unit-tested in `src/security.rs` mod `sg_015_admin_token_tests`; the RBAC layer above it is
the pure `authz::authorize_request` (`src/authz.rs`), unit-tested in `authz::tests` and
composed against a real store in `tests/authz_rbac.rs`. Per-principal tokens are stored ONLY
as their SHA-256 (`api_principals.token_sha256`, UNIQUE), resolved by hash — never plaintext.

---

## Posture-Routing Gate — the Exemption Registry (#306)

**Claim.** The outermost router layer `enforce_posture_routing`
(`src/gateway/policy_layer.rs`) gates every inbound request by **fleet posture**:
each request is classified into an `OperationalCommand` (`classify_http_command`)
and checked by `should_route_command` against a **fail-closed** snapshot of the
posture cache (a poisoned / cold `None` cache blocks). A denied request returns
**HTTP 503** — posture denial is a transient SERVER-STATE condition
(`LockedOut` / `Degraded` / cold-or-stale cache), retryable once posture recovers,
matching the `require_admin_token` 503 shape rather than a per-client 403. A small
**allowlist of posture-EXEMPT paths** (`is_posture_exempt`) bypasses this gate so
the service stays reachable regardless of posture.

> **The exemption is from *fleet-posture routing* only — NOT from authentication.**
> Admin-token and supervisor-key gates still apply to any exempt path that carries
> them.

### Tier 1 — Liveness / observability

`/health`, `/health/live`, `/ready`, `/metrics`.

A literal "gate everything" deadlocks cold start: the posture cache is initially
`None`, which `should_route_command` blocks unconditionally, and external liveness
probes could never confirm the process is alive. This tier is liveness + metrics
only; readiness MAY still reflect posture inside its own handler.

### Tier 2 — The operator-console plane

`/console` and everything under `/console/` (`#103` SG6 / Phase A, PR #305).

The console is the **observe-and-recover plane**, and it **MUST be reachable
during `LockedOut`** — that is exactly when an operator needs to SEE a locked-out
fleet and record a supervisor clearance grant. A posture-gated console would lock
the operator out of the recovery affordance precisely when it is most needed.

#### Console-plane invariant (stated explicitly)

**Everything under `/console` is read-only (QM) EXCEPT the one supervisor-key-gated
mutation, `POST /console/clearance-grants`.** That mutation is:

- authenticated **in the handler** by the supervisor key — an out-of-band operator
  action, **not** a fleet command — not by fleet posture; and
- **record-only**: it records + signs a clearance grant (delivery to the node is
  Phase B); it never mutates posture.

So the `/console` exemption removes these routes from *posture* routing while the
single console mutation retains its own (supervisor-key) authentication.

### Tier 3 — Documented public read-only observability (Bug 2)

`GET`/`HEAD` on the documented "public read-only" observability endpoints:
`/fleet/posture`, `/fleet/posture/{node_id}`, `/fleet/history/{node_id}`,
`/fleet/flapping/{node_id}`, `/attestation/status/{node_id}`, and
`/federation/reports/{asset_id}`.

Before Bug 2 these were posture-GATED, so they returned **503 under `LockedOut`
and under a cold/stale posture cache** — removing fleet observability at the exact
moment an operator or external monitor most needs to distinguish "fleet
`LockedOut`" from "service down". A `GET` **cannot reach an actuator** (the gate
exists to block COMMANDS, not reads), and `/metrics` — already Tier 1 — already
exposes fleet posture during `LockedOut`, so exempting these JSON reads leaks
nothing new and makes behaviour consistent with the documented contract.

**Method-scoped — GET/HEAD only.** The exemption is guarded by request method so a
sibling WRITE sharing a prefix stays fully posture-gated: notably
`POST /federation/reports/submit` (identity-gated) is NOT exempt even though
`GET /federation/reports/{asset_id}` is. **Deliberately NOT exempt** (still gated):
`/fleet/campaigns/assignment/{node_id}` (it drives a node's install decision, not
observability — denial under `LockedOut` is intended), the `/fabric/*` reads (not
in the documented public-read-only set), and every admin/auditor read.

This tier only widens *read* reachability; it never un-gates a mutation. The
directional pin (`console_exemption_set_is_pinned`) asserts each Tier-3 path is
exempt for `GET`/`HEAD`, NOT exempt for a write method, and that the
`/federation/reports/submit` write stays gated.

### Why this matters — the regression it guards

Losing the `/console` exemption locks the operator out of the recovery affordance
exactly when the fleet is `LockedOut` — the worst failure this gate can have. The
set is therefore pinned in BOTH directions by `console_exemption_set_is_pinned`
(`policy_layer.rs`): the test fails if a new path silently **gains** exemption
(un-gates a real fleet path) **or** if the `/console` plane silently **loses** it.

### Assessor note

The posture gate and the admin gate (SG-015 above) are **independent, composed**
controls: a request under `/console` is exempt from *posture* routing yet, if it
is the supervisor mutation, still authenticated in-handler. "Posture-exempt" must
never be read as "unauthenticated." **If a future change adds a non-`/console`
fleet-mutating route to `is_posture_exempt`, or moves a privileged mutation under a
posture-exempt prefix without its own auth, this entry must be re-evaluated.**

### Verification (this branch)

`is_posture_exempt(method, path)` (`src/gateway/policy_layer.rs`) exempts, for ALL
methods, `/health | /health/live | /ready | /metrics` plus `path == "/console"` and
`path.starts_with("/console/")`; and, for `GET`/`HEAD` ONLY, the Tier-3
observability reads (`/fleet/posture`, `/fleet/posture/`, `/fleet/history/`,
`/fleet/flapping/`, `/attestation/status/`, `/federation/reports/`).
`enforce_posture_routing` returns early for those and 503-gates everything else
against the fail-closed posture snapshot. The exemption set is unit-pinned in both
directions (and, for Tier 3, per-method) by `console_exemption_set_is_pinned`
(`src/gateway/policy_layer.rs`).
