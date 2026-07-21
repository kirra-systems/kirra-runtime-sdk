import { Panel, Pill, Meter, StatusDot } from '@/components/ui/primitives'
import { DemoBadge } from '@/components/ui/demo-badge'
import { TrendArea } from '@/components/charts/charts'
import { DualLine } from '@/components/charts/extra2'
import { PositionMap } from '@/components/ui/position-map'
import { OccupancyView } from '@/components/ui/occupancy-view'
import { velocity, battery, actuator, sensors, actuatorLog, path, ego, actors, awareness } from '@/lib/telemetry'
import type { Tone } from '@/lib/types'

export default function TelemetryPage() {
  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Telemetry</h1>
          <p className="font-mono text-[11px] text-faint">KIRRA-09 · streaming · 50 Hz</p>
        </div>
        <div className="flex items-center gap-2">
          <DemoBadge live={false} />
          <Pill tone="ice">live stream</Pill>
          <button className="rounded-lg border border-line bg-panel px-3 py-1.5 font-mono text-[11px] text-muted hover:text-ink">KIRRA-09 ▾</button>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-4 md:grid-cols-4">
        <Quick label="Velocity" value="1.2" unit="m/s" />
        <Quick label="Heading" value="041" unit="°" />
        <Quick label="Battery" value="73" unit="%" />
        <Quick label="Link RSSI" value="−61" unit="dBm" />
      </div>

      {/* ── Environmental Awareness (#14) ── */}
      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Environmental Awareness" subtitle="ego-centric occupancy · sensor fusion · detected actors" action={<Pill tone="ice">perception</Pill>}>
          <OccupancyView actors={actors} height={340} />
        </Panel>
        <Panel title="Perception Summary" subtitle="situational state">
          <div className="space-y-3">
            {awareness.map((a) => (
              <div key={a.label} className="flex items-center justify-between rounded-lg border border-line bg-bg/40 px-3 py-2.5">
                <span className="font-mono text-[11px] uppercase tracking-wider text-faint">{a.label}</span>
                <span className={`font-mono text-[13px] ${txt(a.tone)}`}>{a.value}</span>
              </div>
            ))}
          </div>
          <div className="mt-4 border-t border-line pt-3 font-mono text-[10px] leading-relaxed text-muted">
            360° LiDAR + front radar + camera fans fuse into the occupancy grid. The Governor gates any command that would breach the human-proximity or hazard keep-out envelope.
          </div>
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-2">
        <Panel title="Velocity" subtitle="m/s · slow-loop" action={<Pill tone="ice">live</Pill>}>
          <TrendArea data={velocity} color="ice" height={180} />
        </Panel>
        <Panel title="Battery" subtitle="% state of charge">
          <TrendArea data={battery} color="safe" height={180} />
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Actuator Commands" subtitle="steering° / throttle% · post-governor">
          <DualLine data={actuator} keys={['steer', 'throttle']} colors={['var(--c-ice)', 'var(--c-warn)']} height={200} />
        </Panel>
        <Panel title="Position" subtitle="local frame · drivable corridor" dense>
          <div className="p-2"><PositionMap path={path} ego={ego} height={208} /></div>
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Sensor Health" subtitle="perception stack" dense>
          <div className="grid grid-cols-1 sm:grid-cols-2">
            {sensors.map((s) => (
              <div key={s.name} className="flex items-center gap-3 border-b border-line px-4 py-3">
                <StatusDot tone={s.tone} />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center justify-between gap-2">
                    <span className="truncate text-[13px] text-ink">{s.name}</span>
                    <span className={`font-mono text-[10px] uppercase ${s.tone === 'safe' ? 'text-safe' : 'text-warn'}`}>{s.status}</span>
                  </div>
                  <div className="mt-1.5"><Meter value={s.health} tone={s.tone} /></div>
                </div>
              </div>
            ))}
          </div>
        </Panel>
        <Panel title="Command Log" subtitle="actuator dispatch" dense>
          <ul className="divide-y divide-line font-mono text-[11px]">
            {actuatorLog.map((a, i) => (
              <li key={i} className="flex items-center justify-between px-4 py-2.5">
                <div><span className="text-faint">{a.ts}</span> <span className="text-ink">{a.channel}</span></div>
                <span className={a.tone === 'warn' ? 'text-warn' : a.tone === 'safe' ? 'text-safe' : 'text-ice'}>{a.value}</span>
              </li>
            ))}
          </ul>
        </Panel>
      </div>
    </div>
  )
}

function Quick({ label, value, unit }: { label: string; value: string; unit: string }) {
  return (
    <div className="rounded-xl border border-line bg-panel p-4 shadow-panel">
      <div className="font-mono text-[11px] uppercase tracking-wider text-faint">{label}</div>
      <div className="mt-1 flex items-baseline gap-1">
        <span className="font-display text-2xl font-semibold text-ink">{value}</span>
        <span className="font-mono text-xs text-muted">{unit}</span>
      </div>
    </div>
  )
}

function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
