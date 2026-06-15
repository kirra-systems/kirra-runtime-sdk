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
To bind it to a real Kirra verifier service, set its base URL at build time:

```bash
# console/.env.local
NEXT_PUBLIC_KIRRA_API_URL=https://your-verifier.example.com
```

When set, the **Live Fleet** screen (`/live`) and the top-nav connection
indicator poll the verifier's **public read endpoints** and fall back to demo
data on any error:

| Endpoint | Use |
|----------|-----|
| `GET /health` | connection indicator (`Live · connected` / `Backend offline` / `Demo data`) |
| `GET /fleet/posture` | live per-node posture table; transitions become the event feed |

These endpoints are the verifier's **public read-only tier** — no token is
shipped to the browser. The auth-gated SSE stream (`/system/posture/stream`) is
intentionally **not** consumed client-side; the live event feed is derived from
posture-change polling instead.

**CORS:** if the console and verifier are on different origins, the verifier
must send CORS headers (or sit behind the same origin / a reverse proxy). A
server-side proxy route (to also carry the admin token for the SSE stream and
mutation routes) is the natural next increment.

Wire types live in `lib/api/types.ts` and mirror the verifier's serde shapes
(`FleetNodePosture`, `NodeTrustState`, `FleetPosture`).

## Roadmap (build increments)

- **Drop 1 (this):** design system, app shell, component library, **Fleet Overview**.
- **Drop 2:** Safety Governor Command Center, Runtime Health.
- **Drop 3:** Telemetry visualization, Mission Operations timeline.
- **Drop 4:** Compliance Center, AI Decision Oversight, Incident Review.
- **Drop 5:** Analytics, Reports, Settings, mobile adaptations.

© 2026 Kirra Systems, LLC · Proprietary & confidential.
