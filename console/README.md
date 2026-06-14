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

## Roadmap (build increments)

- **Drop 1 (this):** design system, app shell, component library, **Fleet Overview**.
- **Drop 2:** Safety Governor Command Center, Runtime Health.
- **Drop 3:** Telemetry visualization, Mission Operations timeline.
- **Drop 4:** Compliance Center, AI Decision Oversight, Incident Review.
- **Drop 5:** Analytics, Reports, Settings, mobile adaptations.

© 2026 Kirra Systems, LLC · Proprietary & confidential.
