import { Stat, Pill, StatusDot, Meter, Panel } from '@/components/ui/primitives'
import { DemoBadge } from '@/components/ui/demo-badge'
import { Spark } from '@/components/charts/charts'
import { HeroSafety, FleetTopology, MissionMap, ExecSummary, AuditLedger, EventFeed } from '@/components/command'
import { kpis, robots, postureTone } from '@/lib/mock'

export default function OverviewPage() {
  return (
    <div className="mx-auto max-w-[1600px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Fleet Command</h1>
          <p className="font-mono text-[11px] text-faint">PRODUCTION · us-fleet-1 · updated 2s ago</p>
        </div>
        <div className="flex items-center gap-2">
          <DemoBadge live={false} />
          <Pill tone="safe">Fleet Nominal</Pill>
          <Pill tone="ice">E-Stop Armed</Pill>
          <button className="rounded-lg border border-line bg-panel px-3 py-1.5 font-mono text-[11px] text-muted hover:text-ink">Last 24h</button>
        </div>
      </div>

      <HeroSafety />

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <div className="xl:col-span-2"><FleetTopology /></div>
        <ExecSummary />
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <div className="xl:col-span-2"><MissionMap /></div>
        <AuditLedger />
      </div>

      <div className="grid grid-cols-2 gap-4 md:grid-cols-3 xl:grid-cols-5">
        {kpis.map((k) => (
          <Stat key={k.label} label={k.label} value={k.value} unit={k.unit} delta={k.delta} tone={k.tone}>
            {k.spark && <Spark data={k.spark} color={k.tone === 'muted' ? 'ice' : k.tone} />}
          </Stat>
        ))}
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
        <EventFeed />
      </div>
    </div>
  )
}
