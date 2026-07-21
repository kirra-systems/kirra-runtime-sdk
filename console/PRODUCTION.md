# Kirra Console — Production Deployment & Operations

The hardening record and runbook for running the console as a customer-facing
operations surface. Companion to `REVIEW.md` (design/UX pass). Everything here
is implemented and verified unless explicitly listed under **Known gaps**.

## What this application is — and is not

The console is a **read-only observation surface** over the Kirra verifier.
By construction it cannot actuate, register, or clear anything: the server-side
proxy forwards **GET only**, against a regex **allow-list** of verifier read
paths. Write paths (operator clearance grants, node registration) live in the
verifier's own operator console (`static/console.html`) and the Vite dashboard,
deliberately. Keep that split until per-operator sessions exist (see gaps).

## Runtime configuration (all server-side; nothing reaches the bundle)

| Variable | Effect |
|---|---|
| `KIRRA_API_URL` | Verifier base URL. **Unset → demo mode** (bundled simulated fleet, clearly labeled at nav, page, and shell level). |
| `KIRRA_ADMIN_TOKEN` | Bearer for admin-gated reads. Injected by the proxy per-request; never sent to the browser. |
| `KIRRA_CLIENT_ID` | `x-kirra-client-id` for identity-gated reads. |

Set in `console/.env.local` (dev) or the process environment (prod).

## Serving

```bash
npm ci && npm run build && npm start        # or:
node .next/standalone/server.js             # output: 'standalone' bundle
```

`output: 'standalone'` produces a self-contained server for container images.
Containerize per the repository's supply-chain policy (digest-pinned base
images, cosign signing — see `.github/workflows/docker.yml` for the pattern).

## Hardening implemented (verify with `npm run smoke` + `curl -I`)

- **Proxy** (`app/api/kirra/[...path]/route.ts`):
  - 10 s upstream timeout on every read (504 to the client); the SSE stream
    path is exempt (long-lived) but aborts on client disconnect
  - per-IP sliding-window rate limit (240 req/min, bounded memory) → 429
  - query-string length cap (414); GET-only; path allow-list (403)
  - structured JSON request logs on stdout (`src: kirra-proxy`, request id,
    path, status, duration, upstream status/error) — ship with any log agent
  - `x-request-id` echoed on every response for support correlation;
    upstream error detail is logged server-side, never returned to clients
- **Headers** (`next.config.mjs`, applied to every response including the API):
  `X-Frame-Options: DENY`, `nosniff`, `Referrer-Policy`, `Permissions-Policy`,
  CSP (`frame-ancestors 'none'; object-src 'none'; base-uri 'self'`),
  `poweredByHeader: false`
- **Failure containment**: route-segment error boundary (`app/error.tsx`) and
  root boundary (`app/global-error.tsx`) — a screen crash never blanks the
  console, and the recovery copy states that the UI is not the fleet
- **Data-layer resilience** (pre-existing, kept): every hook aborts in-flight
  requests on unmount, distinguishes demo/abort/failure, settles to labeled
  demo data, and keeps re-polling — network interruptions self-heal
- **Determinism**: demo data uses a seeded PRNG and fixed epoch; all times
  render in pinned UTC (`lib/format.ts`) — no hydration drift, no ambiguous
  local timestamps for distributed operators
- **Supply chain**: `package-lock.json` committed; `npm ci` in CI; fonts
  self-hosted (no build-time network); CI actions SHA-pinned (repo M-6 gate)

## CI gate (`.github/workflows/ci.yml` → `console` job)

`npm ci` → typecheck → lint → production build → headless smoke
(`scripts/smoke.mjs`): all 18 routes must return 200 and render with **zero
page errors** (catches hydration regressions), the shell must compose, and the
quick-nav keyboard flow must navigate. Locally:
`npm run smoke` (set `KIRRA_SMOKE_CHROMIUM` to reuse a preinstalled browser).

## Security posture — read before exposing this to a network

1. **The console has no user authentication of its own.** Anyone who can reach
   it gets read access to whatever the configured token can read. Deploy behind
   an authenticating reverse proxy / SSO (oauth2-proxy, IAP, mTLS mesh). This
   is a deliberate scope line, not an oversight: adding a half-measure login to
   a read-only surface would imply session security that doesn't exist yet.
2. The rate limiter is per-instance and in-memory. Multi-replica deployments
   should rate-limit at the edge as well.
3. The CSP does not lock `script-src` (Next inline bootstrap requires nonce
   plumbing). All scripts are same-origin and no third-party requests are made
   at runtime; full nonce-based CSP is a tracked gap.
4. The token grants **read** scopes only in spirit — but it is the *admin*
   token today. When the verifier's per-principal tokens (WS-1 `auditor` role)
   are provisioned, use one: the proxy needs no change.

## Known gaps before enterprise deployment (ranked)

1. **Per-operator sessions + RBAC** — requires the verifier's operator
   registry (#314); until then, SSO-in-front is the supported model.
2. **Auditor-role token instead of admin token** — supported by the verifier
   today; a deployment choice, documented above.
3. **SSE adoption** — the stream is proxied but unconsumed; polling remains
   the transport (5–30 s). Roadmap item #1 in REVIEW.md.
4. **Nonce-based CSP**, error-reporter integration (the boundaries and
   structured logs are the hook points), OpenTelemetry spans on the proxy.
5. **Test depth** — the smoke gate covers rendering and navigation; component
   and contract tests (wire types vs verifier serde) are the next layer.
6. **Preview screens in customer deployments** — the 12 simulated screens are
   labeled at three levels; a build-time flag to hide them entirely is a
   product decision awaiting real customer telemetry to replace them.
