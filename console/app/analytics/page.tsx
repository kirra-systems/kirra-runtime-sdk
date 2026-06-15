import { Panel, Pill, Meter } from '@/components/ui/primitives'
import { DemoBadge } from '@/components/ui/demo-badge'
import { TrendArea, Spark } from '@/components/charts/charts'
import { DualLine } from '@/components/charts/extra2'
import { kpiWall, throughputTrend, riskForecast, riskDrivers, projections } from '@/lib/analytics'
import type { Tone } from '@/lib/types'

export default function AnalyticsPage() {
  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Analytics & Risk Forecasting</h1>
          <p className="font-mono text-[11px] text-faint">executive KPIs · fleet trends · forward risk projection</p>
        </div>
        <div className="flex items-center gap-2">
          <DemoBadge live={false} />
          <Pill tone="ice">30-day window</Pill>
        </div>
      </div>

      {/* Executive KPI Wall (#9) */}
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
        <Panel className="xl:col-span-2" title="Operational Risk Forecast" subtitle="composite risk index · observed vs projected (90% ceiling)" action={<Pill tone="warn">rising</Pill>}>
          <DualLine
            data={riskForecast as unknown as { t: string; [k: string]: string | number }[]}
            keys={['observed', 'forecast']}
            colors={['#5cc6ff', '#ffb020']}
            height={230}
          />
          <p className="mt-2 font-mono text-[10px] text-faint">Observed (ice) holds through 19:00; forecast (amber) projects a climb into the evening shift change.</p>
        </Panel>

        <Panel title="Risk Projection" subtitle="banded outlook">
          <ul className="space-y-3">
            {projections.map((p) => (
              <li key={p.window} className="rounded-lg border border-line bg-bg/40 p-3">
                <div className="flex items-center justify-between">
                  <span className="font-mono text-[11px] uppercase tracking-wider text-faint">{p.window}</span>
                  <span className={`rounded px-1.5 py-0.5 font-mono text-[10px] uppercase ${badge(p.tone)}`}>{p.band}</span>
                </div>
                <div className="mt-2 flex items-end gap-2">
                  <span className={`font-display text-2xl font-semibold ${txt(p.tone)}`}>{p.score}</span>
                  <span className="mb-1 font-mono text-[10px] text-faint">/ 100 risk</span>
                </div>
                <p className="mt-1 text-[11px] text-muted">{p.note}</p>
              </li>
            ))}
          </ul>
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Fleet Throughput" subtitle="commands / min · 24h">
          <TrendArea data={throughputTrend} color="ice" height={210} />
        </Panel>

        <Panel title="Risk Drivers" subtitle="contribution to projected risk" dense>
          <ul className="px-4 py-2">
            {riskDrivers.map((d) => (
              <li key={d.name} className="border-b border-line py-3 last:border-0">
                <div className="flex items-center justify-between gap-3">
                  <span className="text-[12px] text-ink">{d.name}</span>
                  <span className="flex items-center gap-1.5 font-mono text-[11px] text-muted">
                    <span className={txt(d.tone)}>{arrow(d.trend)}</span>{d.contribution}%
                  </span>
                </div>
                <div className="mt-2"><Meter value={d.contribution} tone={d.tone} /></div>
              </li>
            ))}
          </ul>
        </Panel>
      </div>
    </div>
  )
}

function arrow(t: 'rising' | 'flat' | 'falling') { return t === 'rising' ? '▲' : t === 'falling' ? '▼' : '▬' }
function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
function badge(t: Tone) { return t === 'safe' ? 'bg-safe/15 text-safe' : t === 'warn' ? 'bg-warn/15 text-warn' : t === 'crit' ? 'bg-crit/15 text-crit' : 'bg-ice/15 text-ice' }
