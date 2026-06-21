import { MapPin, Container, Repeat, BatteryCharging, Radar, Pause, Lock, Clock, Circle, type LucideIcon } from 'lucide-react'
import { Panel, Pill, Meter, StatusDot } from '@/components/ui/primitives'
import { DemoBadge } from '@/components/ui/demo-badge'
import { mission, phases, waypoints, tasks, risk, operatorInterventions, gantt, ganttWindow } from '@/lib/missions'
import type { Tone } from '@/lib/types'

export default function MissionsPage() {
  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Mission Operations</h1>
          <p className="font-mono text-[11px] text-faint">{mission.id} · {mission.name} · {mission.asset}</p>
        </div>
        <div className="flex items-center gap-2">
          <DemoBadge live={false} />
          <Pill tone="ice">In progress</Pill>
          <span className="font-mono text-[11px] text-faint">ETA {mission.eta}</span>
        </div>
      </div>

      {/* ── Fleet Mission Gantt (Drop 6) ── */}
      <Panel title="Fleet Mission Gantt" subtitle={`${gantt.length} missions · ${ganttWindow.start}–${ganttWindow.end} · ${ganttWindow.label}`} action={<Pill tone="ice">now {clock(ganttWindow.nowFrac)}</Pill>}>
        {/* axis */}
        <div className="mb-2 flex items-center">
          <div className="w-40 shrink-0" />
          <div className="relative flex-1">
            <div className="flex justify-between font-mono text-[10px] text-faint">
              {['10:00', '11:00', '12:00', '13:00', '14:00'].map((h) => <span key={h}>{h}</span>)}
            </div>
          </div>
        </div>

        <div className="space-y-2.5">
          {gantt.map((row) => (
            <div key={row.id} className="flex items-center">
              <div className="w-40 shrink-0 pr-3">
                <div className="flex items-center gap-2">
                  <StatusDot tone={row.tone} pulse={row.status === 'held'} />
                  <span className="truncate font-mono text-[12px] text-ink">{row.asset}</span>
                </div>
                <div className="truncate pl-4 font-mono text-[10px] text-faint">{row.name}</div>
              </div>
              <div className="relative h-7 flex-1 overflow-hidden rounded-md border border-line bg-bg/40">
                {/* hour gridlines */}
                {[0.25, 0.5, 0.75].map((f) => (
                  <span key={f} className="absolute top-0 h-full w-px bg-white/[0.04]" style={{ left: `${f * 100}%` }} />
                ))}
                {/* now line */}
                <span className="absolute top-0 z-10 h-full w-px bg-ice/70" style={{ left: `${ganttWindow.nowFrac * 100}%` }} />
                {/* segments */}
                {row.segments.map((s, i) => (
                  <div
                    key={i}
                    className={`absolute top-1 flex h-5 items-center justify-center overflow-hidden rounded ${segBg(s.tone)} ${s.tone === 'muted' ? 'border border-dashed border-line' : ''}`}
                    style={{ left: `${s.start * 100}%`, width: `${(s.end - s.start) * 100}%` }}
                    title={s.phase}
                  >
                    <SegIcon phase={s.phase} muted={s.tone === 'muted'} />
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>

        <div className="mt-4 flex flex-wrap items-center gap-4 border-t border-line pt-3 font-mono text-[10px] text-faint">
          <Legend tone="safe" label="completed" />
          <Legend tone="ice" label="active" />
          <Legend tone="warn" label="held / degraded" />
          <Legend tone="crit" label="lockout" />
          <Legend tone="muted" label="queued" />
          <span className="ml-auto flex items-center gap-1.5"><span className="h-3 w-px bg-ice/70" /> now</span>
        </div>

        <div className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-2 font-mono text-[10px] text-faint">
          {PHASE_LEGEND.map(({ Icon, label }) => (
            <span key={label} className="flex items-center gap-1.5"><Icon className="h-3 w-3" strokeWidth={2.5} /> {label}</span>
          ))}
        </div>
      </Panel>

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

function Legend({ tone, label }: { tone: Tone; label: string }) {
  return (
    <span className="flex items-center gap-1.5">
      <span className={`h-2.5 w-2.5 rounded-sm ${segBg(tone)}`} />
      {label}
    </span>
  )
}

function segBg(t: Tone) { return t === 'safe' ? 'bg-safe' : t === 'warn' ? 'bg-warn' : t === 'crit' ? 'bg-crit' : t === 'ice' ? 'bg-ice' : 'bg-muted/40' }

// Gantt phase → micro-icon. The 18 mission phases group into a small scannable
// vocabulary so tiny timeline blocks show an icon (with a `title` tooltip + the
// legend) instead of a cropped word.
const PHASE_LEGEND: { Icon: LucideIcon; label: string }[] = [
  { Icon: MapPin, label: 'transit' },
  { Icon: Container, label: 'load / unload' },
  { Icon: Repeat, label: 'loop' },
  { Icon: BatteryCharging, label: 'charge' },
  { Icon: Radar, label: 'survey' },
  { Icon: Pause, label: 'hold' },
  { Icon: Lock, label: 'lockout' },
  { Icon: Clock, label: 'queued' },
]

function phaseIcon(phase: string): LucideIcon {
  const p = phase.toLowerCase()
  if (p.includes('lockout')) return Lock
  if (p.includes('hold')) return Pause
  if (p.includes('charge')) return BatteryCharging
  if (p.includes('loop')) return Repeat
  if (p.includes('survey')) return Radar
  if (p.includes('queued')) return Clock
  if (/transit|dispatch|return/.test(p)) return MapPin
  if (/pick|place|haul|drop|stage/.test(p)) return Container
  return Circle
}

function SegIcon({ phase, muted }: { phase: string; muted: boolean }) {
  const Icon = phaseIcon(phase)
  return <Icon className={`h-3 w-3 ${muted ? 'text-faint' : 'text-bg'}`} strokeWidth={2.5} />
}

// Convert a window fraction to a wall-clock label for the 10:00–14:00 window.
function clock(frac: number): string {
  const totalMin = 4 * 60 * frac
  const h = 10 + Math.floor(totalMin / 60)
  const m = Math.round(totalMin % 60)
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`
}
