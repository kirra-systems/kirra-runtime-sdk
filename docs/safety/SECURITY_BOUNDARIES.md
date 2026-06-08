# Kirra Safety Kernel — Security Boundaries

**Status:** Evidence / design-decision record.
**Cross-refs:** `REQUIREMENTS_TRACEABILITY.md` (SG-015), `src/bin/kirra_verifier_service.rs`
(router construction), `src/security.rs` (`admin_token_ok`, `constant_time_compare`),
CLAUDE.md "Route Authorization Matrix".

---

## SG-015 — Admin-Token Mutation Gate: Coverage and the Attestation-Handshake Carve-Out

**Claim (SG-015, ASIL B).** Privileged mutation routes are fail-closed behind
`require_admin_token`: the request is denied (503 when `KIRRA_ADMIN_TOKEN` is absent/empty,
401 on mismatch) unless a token is presented that matches the configured value under
`constant_time_compare` (never `==`). The check is the pure predicate
`security::admin_token_ok`, unit-tested in `src/security.rs` mod `sg_015_admin_token_tests`.

### Gated mutation routes

Each of the following route groups applies `middleware::from_fn(require_admin_token)`
(verified on `cert/rtm-gap-closure`, `src/bin/kirra_verifier_service.rs`):

- **identity/industrial group** (`identity_gated_routes`, layered at the `.layer(...require_admin_token)`
  after the route list; additionally gated by `require_client_identity`):
  `/federation/reports/submit`, `/action_filter/evaluate`, `/industrial/evaluate`,
  `/industrial/ethernet-ip/evaluate`, `/industrial/canopen/evaluate`,
  `/industrial/dnp3/evaluate` (and the `/system/posture/stream` SSE).
- **admin group** (`admin_routes`): `/attestation/register`, `/fleet/dependencies`,
  `/fleet/diagnostics/report`, `/fleet/assets/register`, `/system/backup/export`,
  `/system/audit/rotate-signing-key`, `/federation/controllers/register`,
  `/attestation/identity/register`, `/fabric/assets/register`,
  `/fabric/command/{asset_id}`. (The same layer additionally gates the admin GET
  reads — `/system/audit/verify`, `/system/audit/export`, `/fabric/assets`,
  `/fabric/state`, `/fabric/telemetry[/{asset_id}]`, `/fabric/causal-log[/{entry_id}]`
  — so they are admin-only too, beyond the mutation set this claim is about.)
- **actuator group** (`actuator_routes`): `/actuator/motion/command`.

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
`admin_routes`, and `actuator_routes` each terminate in
`.layer(middleware::from_fn(require_admin_token))`; `attestation_routes` (the two handshake
POSTs) is constructed and merged with **no** such layer. The gate predicate
`security::admin_token_ok` uses `constant_time_compare` (never `==`) and is unit-tested in
`src/security.rs` mod `sg_015_admin_token_tests` (absent/empty configured → deny;
absent/mismatched provided → deny; exact token → allow).
