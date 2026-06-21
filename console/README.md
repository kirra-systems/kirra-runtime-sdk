# Kirra Mission Console

Enterprise robotics & AI-safety operations dashboard for the Kirra Systems runtime governor — fleet operations, safety-governor oversight, runtime health, telemetry, and compliance.

> Design language: Palantir Foundry x mission-control x autonomous-systems ops. Dark-first, graphite surfaces, restrained safety/amber/critical/ice accents. Built to be credible to operators and safety auditors while presenting at executive level.

## Stack

- **Next.js 15** (App Router) · **React 18** · **TypeScript**
- **TailwindCSS** with shadcn-style CSS-variable tokens
- **Recharts** (engineering-grade charts) · **Framer Motion** (subtle motion) · **lucide-react**

## Run

```bash
cd console
npm install
npm run dev   # http://localhost:3000
```

## Architecture

```
console/
  app/
    layout.tsx          # shell: top nav + sidebar grid
    page.tsx            # Fleet Overview
    [...slug]/page.tsx  # styled placeholder for not-yet-built modules
    api/kirra/          # server proxy to the verifier (token-injecting, CORS-free)
    globals.css         # design tokens + base
  components/
    shell/              # top-nav, sidebar
    ui/primitives.tsx   # Panel, Stat, Pill, Meter, StatusDot
    charts/charts.tsx   # TrendArea, Spark, ScoreRing (Recharts)
  lib/
    types.ts            # domain model (Robot, KPI, Posture, EventItem)
    mock.ts             # mock fleet + telemetry data
    api/                # live verifier client + hooks (types, client, hooks)
  tailwind.config.ts    # color tokens, fonts, shadows
```

## Design tokens

| Token | Use |
|-------|-----|
| `bg` / `surface` / `panel` / `elevated` | graphite surface stack |
| `ink` / `muted` / `faint` | text hierarchy |
| `safe` (green) | compliant / nominal |
| `warn` (amber) | warnings / degraded |
| `crit` (red) | critical interventions |
| `ice` (blue) | telemetry / analytics |

## Live data

The console runs on bundled mock data by default (the public demo always works).
To bind it to a real Kirra verifier, point the **server-side proxy** at it:

```bash
# console/.env.local  (server-side — NOT shipped to the browser)
KIRRA_API_URL=https://your-verifier.example.com
KIRRA_ADMIN_TOKEN=…    # optional — for admin / identity-gated reads
KIRRA_CLIENT_ID=…      # optional — x-kirra-client-id for the SSE stream
```

Requests go through a same-origin proxy route (`app/api/kirra/[...path]`) which
injects the token server-side, removes CORS, and only forwards an **allow-listed
set of read-only GET** paths. The hooks fall back to demo data whenever the
proxy reports demo mode (`KIRRA_API_URL` unset) or the verifier is unreachable.

Currently wired (Live Fleet `/live` + top-nav indicator):

| Source | Endpoint | Tier |
|--------|----------|------|
| connection indicator | `GET /health` | public |
| per-node posture + transition feed | `GET /fleet/posture` | public |
| audit-chain integrity | `GET /system/audit/verify` | admin (via proxy) |
| audit event ledger | `GET /console/audit` | public |

Because the proxy holds the token, the richer admin/identity-gated reads
(`/fabric/*`, `/system/audit/export`, `/system/posture/stream` SSE) are now
reachable too — wiring more screens is incremental from here.

Wire types live in `lib/api/types.ts` and mirror the verifier's serde shapes
(`FleetNodePosture`, `NodeTrustState`, `FleetPosture`, audit verify/page).

## Roadmap (build increments)

- **Drop 1 (this):** design system, app shell, component library, **Fleet Overview**.
- **Drop 2:** Safety Governor Command Center, Runtime Health.
- **Drop 3:** Telemetry visualization, Mission Operations timeline.
- **Drop 4:** Compliance Center, AI Decision Oversight, Incident Review.
- **Drop 5:** Analytics, Reports, Settings, mobile adaptations.

© 2026 Kirra Systems, LLC · Proprietary & confidential.
