import { Panel, Pill, Meter, StatusDot } from '@/components/ui/primitives'
import { mission, phases, waypoints, tasks, risk, operatorInterventions } from '@/lib/missions'

export default function MissionsPage() {
  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Mission Operations</h1>
          <p className="font-mono text-[11px] text-faint">{mission.id} · {mission.name} · {mission.asset}</p>
        </div>
        <div className="flex items-center gap-2">
          <Pill tone="ice">In progress</Pill>
          <span className="font-mono text-[11px] text-faint">ETA {mission.eta}</span>
        </div>
      </div>

      <Panel title="Mission Timeline" subtitle={`${mission.progress}% complete · started ${mission.started}`}>
        <div className="mb-4 h-1.5 w-full overflow-hidden rounded-full bg-white/5">
          <div className="h-full rounded-full bg-ice" style={{ width: `${mission.progress}%` }} />
        </div>
        <div className="grid grid-cols-3 gap-3 sm:grid-cols-6">
          {phases.map((p) => (
            <div key={p.name} className="rounded-lg border border-line bg-elevated/40 p-3">
              <div className="flex items-center gap-2">
                <span className={`h-2 w-2 rounded-full ${p.status === 'done' ? 'bg-safe' : p.status === 'active' ? 'bg-ice' : 'bg-muted'}`} />
                <span className="font-mono text-[10px] uppercase tracking-wider text-faint">{p.status}</span>
              </div>
              <div className="mt-1 text-[12px] text-ink">{p.name}</div>
              <div className="mt-2"><Meter value={p.pct} tone={p.status === 'active' ? 'ice' : p.status === 'done' ? 'safe' : 'muted'} /></div>
            </div>
          ))}
        </div>
      </Panel>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel title="Waypoint Progress" subtitle="route plan" dense>
          <ul>
            {waypoints.map((w) => (
              <li key={w.id} className="flex items-center gap-4 border-b border-line px-4 py-3 last:border-0">
                <span className={`h-2.5 w-2.5 rounded-full ${w.status === 'done' ? 'bg-safe' : w.status === 'active' ? 'bg-ice' : 'bg-muted'}`} />
                <div className="min-w-0 flex-1">
                  <div className="text-[13px] text-ink">{w.name}</div>
                  <div className="font-mono text-[10px] text-faint">{w.id}</div>
                </div>
                <span className="font-mono text-[11px] text-muted">
                  {w.status === 'active' ? <span className="text-ice">{w.eta}</span> : w.status === 'pending' ? w.eta : <span className="text-safe">done</span>}
                </span>
              </li>
            ))}
          </ul>
        </Panel>

        <Panel title="Task Completion">
          <div className="space-y-4">
            {tasks.map((t) => (
              <div key={t.name}>
                <div className="flex items-center justify-between">
                  <span className="text-[13px] text-ink">{t.name}</span>
                  <span className={`font-mono text-[11px] ${t.tone === 'safe' ? 'text-safe' : 'text-warn'}`}>{t.pct}%</span>
                </div>
                <div className="mt-1.5"><Meter value={t.pct} tone={t.tone} /></div>
              </div>
            ))}
          </div>
        </Panel>

        <Panel title="Risk Assessment" subtitle="composite operational risk">
          <div className="mb-4 flex items-end gap-3">
            <span className="font-display text-4xl font-semibold text-safe">{risk.score}</span>
            <span className="mb-1 font-mono text-[11px] uppercase tracking-wider text-safe">{risk.band} risk</span>
          </div>
          <ul className="space-y-2">
            {risk.factors.map((f) => (
              <li key={f.name} className="flex items-center justify-between">
                <span className="text-[12px] text-muted">{f.name}</span>
                <span className={`rounded px-1.5 py-0.5 font-mono text-[10px] uppercase ${f.tone === 'safe' ? 'bg-safe/15 text-safe' : f.tone === 'warn' ? 'bg-warn/15 text-warn' : 'bg-crit/15 text-crit'}`}>{f.level}</span>
              </li>
            ))}
          </ul>
        </Panel>
      </div>

      <Panel title="Operator Interventions" subtitle="human-in-the-loop actions" dense>
        <ul className="divide-y divide-line">
          {operatorInterventions.map((o, i) => (
            <li key={i} className="flex items-center gap-3 px-4 py-3">
              <StatusDot tone={o.tone} />
              <span className="font-mono text-[11px] text-faint">{o.ts}</span>
              <span className="text-[12px] text-muted">{o.detail}</span>
            </li>
          ))}
        </ul>
      </Panel>
    </div>
  )
}
