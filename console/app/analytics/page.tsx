'use client'

import { Panel, Pill, Meter } from '@/components/ui/primitives'
import { DemoBadge } from '@/components/ui/demo-badge'
import { TrendArea, Spark } from '@/components/charts/charts'
import { DualLine } from '@/components/charts/extra2'
import { useAnalytics } from '@/lib/api/hooks'
import { kpiWall } from '@/lib/analytics'
import type { Tone } from '@/lib/types'

// Analytics — historical operational trend (#396). This is NOT ML risk
// forecasting: every series is a backward-looking rollup of what already
// happened (posture transitions, denial rate, interventions, flapping) over the
// selected window. Wired to GET /console/analytics?window_ms= with the bundled
// demo snapshot as fallback. The KPI wall stays mock (out of #396 scope).
const WINDOW_MS = 30 * 24 * 60 * 60 * 1000 // 30-day window

export default function AnalyticsPage() {
  const { data, source } = useAnalytics(WINDOW_MS)

  // Posture transitions → a per-bucket time series for the trend chart.
  const transitionSeries = data.posture_transitions.map((b) => ({
    t: new Date(b.bucket_start_ms).toLocaleDateString(undefined, { month: 'short', day: 'numeric' }),
    degraded: b.to_degraded,
    lockedout: b.to_lockedout,
  }))
  // Denial-rate series → a percentage area trend.
  const denialSeries = data.denial_rate_series.map((p) => ({
    t: new Date(p.bucket_start_ms).toLocaleDateString(undefined, { month: 'short', day: 'numeric' }),
    v: Math.round(p.denial_rate * 10000) / 100,
  }))
  const maxIntervention = Math.max(1, ...data.interventions_by_asset.map((a) => a.clamps + a.denies))
  const maxFlap = Math.max(1, ...data.flapping_top.map((f) => f.transitions))

  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Analytics &amp; Historical Trends</h1>
          <p className="font-mono text-[11px] text-faint">executive KPIs · fleet trends · observed-history rollups</p>
        </div>
        <div className="flex items-center gap-2">
          <DemoBadge live={source === 'live'} />
          <Pill tone="ice">30-day window</Pill>
        </div>
      </div>

      {/* Executive KPI Wall (#9) — bundled mock, out of #396 scope. */}
      <div className="grid grid-cols-2 gap-4 lg:grid-cols-3 xl:grid-cols-6">
        {kpiWall.map((k) => (
          <div key={k.label} className="rounded-xl border border-line bg-panel p-4 shadow-panel">
            <div className="font-mono text-[10px] uppercase tracking-wider text-faint">{k.label}</div>
            <div className="mt-2 flex items-baseline gap-1">
              <span className="font-display text-[24px] font-semibold leading-none text-ink">{k.value}</span>
              {k.unit && <span className="font-mono text-[11px] text-muted">{k.unit}</span>}
            </div>
            <div className={`mt-1 font-mono text-[10px] ${txt(k.tone)}`}>{k.delta}</div>
            <div className="mt-2"><Spark data={k.spark} color={k.tone === 'muted' ? 'ice' : k.tone} /></div>
          </div>
        ))}
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel
          className="xl:col-span-2"
          title="Posture Transitions"
          subtitle="observed transitions per bucket · to-Degraded vs to-LockedOut · historical"
          action={source === 'live' ? <Pill tone="safe">live</Pill> : <Pill tone="warn">demo</Pill>}
        >
          <DualLine
            data={transitionSeries as unknown as { t: string; [k: string]: string | number }[]}
            keys={['degraded', 'lockedout']}
            colors={['var(--c-warn)', 'var(--c-crit)']}
            height={230}
          />
          <p className="mt-2 font-mono text-[10px] text-faint">Backward-looking counts of fleet posture transitions over the {WINDOW_MS / (24 * 60 * 60 * 1000)}-day window — not a forecast.</p>
        </Panel>

        <Panel title="Top Flapping Nodes" subtitle="most posture transitions in window" dense>
          {data.flapping_top.length === 0 ? (
            <p className="px-4 py-6 font-mono text-[11px] text-faint">No flapping nodes in window.</p>
          ) : (
            <ul className="px-4 py-2">
              {data.flapping_top.map((f) => (
                <li key={f.node_id} className="border-b border-line py-3 last:border-0">
                  <div className="flex items-center justify-between gap-3">
                    <span className="font-mono text-[12px] text-ink">{f.node_id}</span>
                    <span className="font-mono text-[11px] text-muted">{f.transitions} transitions</span>
                  </div>
                  <div className="mt-2"><Meter value={(f.transitions / maxFlap) * 100} tone={f.transitions > maxFlap * 0.66 ? 'crit' : 'warn'} /></div>
                </li>
              ))}
            </ul>
          )}
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel
          className="xl:col-span-2"
          title="Denial Rate"
          subtitle="governed-command denial rate · % · windowed history"
          action={source === 'live' ? <Pill tone="safe">live</Pill> : <Pill tone="warn">demo</Pill>}
        >
          <TrendArea data={denialSeries} color="warn" height={210} />
        </Panel>

        <Panel title="Interventions by Asset" subtitle="clamps + denies in window" dense>
          {data.interventions_by_asset.length === 0 ? (
            <p className="px-4 py-6 font-mono text-[11px] text-faint">No interventions in window.</p>
          ) : (
            <ul className="px-4 py-2">
              {data.interventions_by_asset.map((a) => {
                const total = a.clamps + a.denies
                const tone: Tone = a.denies > 0 ? 'crit' : a.clamps > 0 ? 'warn' : 'safe'
                return (
                  <li key={a.asset_id} className="border-b border-line py-3 last:border-0">
                    <div className="flex items-center justify-between gap-3">
                      <span className="font-mono text-[12px] text-ink">{a.asset_id}</span>
                      <span className="font-mono text-[11px] text-muted">{a.clamps} clamp · {a.denies} deny</span>
                    </div>
                    <div className="mt-2"><Meter value={(total / maxIntervention) * 100} tone={tone} /></div>
                  </li>
                )
              })}
            </ul>
          )}
        </Panel>
      </div>
    </div>
  )
}

function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
