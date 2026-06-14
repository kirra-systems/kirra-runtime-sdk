import type { ReactNode } from 'react'
import { Panel, Stat, Pill, StatusDot, Meter } from '@/components/ui/primitives'
import { TrendArea, Spark, ScoreRing } from '@/components/charts/charts'
import { kpis, robots, events, series, postureTone } from '@/lib/mock'

export default function OverviewPage() {
  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Fleet Overview</h1>
          <p className="font-mono text-[11px] text-faint">PRODUCTION · us-fleet-1 · updated 2s ago</p>
        </div>
        <div className="flex items-center gap-2">
          <Pill tone="safe">Fleet Nominal</Pill>
          <Pill tone="ice">E-Stop Armed</Pill>
          <button className="rounded-lg border border-line bg-panel px-3 py-1.5 font-mono text-[11px] text-muted hover:text-ink">Last 24h</button>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-4 md:grid-cols-3 xl:grid-cols-5">
        {kpis.map((k) => (
          <Stat key={k.label} label={k.label} value={k.value} unit={k.unit} delta={k.delta} tone={k.tone}>
            {k.spark && <Spark data={k.spark} color={k.tone === 'muted' ? 'ice' : k.tone} />}
          </Stat>
        ))}
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Fleet Throughput" subtitle="commands evaluated · per second" action={<Pill tone="ice">live</Pill>}>
          <TrendArea data={series.throughput} color="ice" height={220} />
        </Panel>
        <Panel title="Safety Governor" subtitle="real-time verdict engine">
          <div className="flex items-center gap-4">
            <div className="w-1/2"><ScoreRing value={98} color="safe" label="Safety Score" /></div>
            <div className="w-1/2 space-y-3">
              <Row label="State" value={<Pill tone="safe">Nominal</Pill>} />
              <Row label="E-Stop" value={<span className="font-mono text-xs text-safe">ARMED</span>} />
              <Row label="Constraints" value={<span className="font-mono text-xs text-ink">42 / 42</span>} />
              <Row label="Violations 24h" value={<span className="font-mono text-xs text-warn">3</span>} />
            </div>
          </div>
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Active Fleet" subtitle={`${robots.length} units`} dense>
          <div className="overflow-x-auto">
            <table className="w-full min-w-[640px] text-left">
              <thead>
                <tr className="border-b border-line font-mono text-[10px] uppercase tracking-wider text-faint">
                  <th className="px-4 py-2 font-normal">Unit</th>
                  <th className="px-4 py-2 font-normal">Posture</th>
                  <th className="px-4 py-2 font-normal">Mission</th>
                  <th className="px-4 py-2 font-normal">Battery</th>
                  <th className="px-4 py-2 font-normal">Latency</th>
                  <th className="px-4 py-2 font-normal">Status</th>
                </tr>
              </thead>
              <tbody className="font-mono text-[12px]">
                {robots.map((r) => (
                  <tr key={r.id} className="border-b border-line last:border-0 hover:bg-white/[0.02]">
                    <td className="px-4 py-2.5"><div className="text-ink">{r.name}</div><div className="text-[10px] text-faint">{r.model}</div></td>
                    <td className="px-4 py-2.5"><Pill tone={postureTone(r.posture)}>{r.posture}</Pill></td>
                    <td className="px-4 py-2.5 text-muted">{r.mission}</td>
                    <td className="px-4 py-2.5"><div className="flex items-center gap-2"><span className="w-9 text-ink">{r.battery}%</span><div className="w-16"><Meter value={r.battery} tone={r.battery < 25 ? 'crit' : r.battery < 50 ? 'warn' : 'safe'} /></div></div></td>
                    <td className="px-4 py-2.5 text-muted">{r.latencyMs}ms</td>
                    <td className="px-4 py-2.5"><span className="inline-flex items-center gap-2 text-muted"><StatusDot tone={r.status === 'online' ? 'safe' : r.status === 'standby' ? 'warn' : 'muted'} pulse={r.status === 'online'} />{r.status}</span></td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </Panel>

        <Panel title="Event Stream" subtitle="governor + fleet" action={<Pill tone="ice">live</Pill>} dense>
          <ul className="divide-y divide-line">
            {events.map((e) => (
              <li key={e.id} className="flex gap-3 px-4 py-3">
                <span className="mt-1.5"><StatusDot tone={e.tone} /></span>
                <div className="min-w-0">
                  <p className="text-[12px] leading-snug text-ink">{e.message}</p>
                  <p className="mt-0.5 font-mono text-[10px] text-faint">{e.source} · {e.ts}</p>
                </div>
              </li>
            ))}
          </ul>
        </Panel>
      </div>
    </div>
  )
}

function Row({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="flex items-center justify-between">
      <span className="font-mono text-[11px] uppercase tracking-wider text-faint">{label}</span>
      {value}
    </div>
  )
}
