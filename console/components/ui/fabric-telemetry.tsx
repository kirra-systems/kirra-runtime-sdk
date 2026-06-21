'use client'

import { Panel, Pill, Meter, StatusDot } from '@/components/ui/primitives'
import { useFabricTelemetry } from '@/lib/api/hooks'
import type { Tone } from '@/lib/types'

// Live fabric governance telemetry (GET /fabric/telemetry, admin via proxy).
// Self-contained client panel so the server-rendered Runtime page is untouched.
export function FabricTelemetryPanel() {
  const { data, source } = useFabricTelemetry(10000)
  const postureRows = Object.entries(data.assets_by_posture)
  const typeRows = Object.entries(data.assets_by_type)
  const denialPct = data.fabric_denial_rate * 100

  return (
    <Panel
      title="Fabric Telemetry"
      subtitle="governed-command throughput · GET /fabric/telemetry"
      action={source === 'live' ? <Pill tone="safe">live</Pill> : <Pill tone="ice">demo</Pill>}
    >
      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        <Metric label="Active assets" value={`${data.active_assets} / ${data.total_assets}`} tone="ice" />
        <Metric label="Commands · min" value={data.total_commands_per_minute.toFixed(0)} tone="safe" />
        <Metric label="Denial rate" value={`${denialPct.toFixed(2)}%`} tone={denialPct > 5 ? 'crit' : denialPct > 1 ? 'warn' : 'safe'} />
        <Metric label="Top denials" value={data.highest_denial_asset ?? '—'} tone={data.highest_denial_asset ? 'warn' : 'muted'} />
      </div>

      <div className="mt-5 grid grid-cols-1 gap-6 sm:grid-cols-2">
        <div>
          <div className="mb-2 font-mono text-[10px] uppercase tracking-wider text-faint">Assets by posture</div>
          <ul className="space-y-2.5">
            {postureRows.map(([posture, count]) => {
              const tone = postureToneOf(posture)
              return (
                <li key={posture}>
                  <div className="flex items-center justify-between gap-3">
                    <span className="flex items-center gap-2 text-[12px] text-ink"><StatusDot tone={tone} />{posture}</span>
                    <span className="font-mono text-[11px] text-muted">{count}</span>
                  </div>
                  <div className="mt-1.5"><Meter value={data.total_assets ? (count / data.total_assets) * 100 : 0} tone={tone} /></div>
                </li>
              )
            })}
          </ul>
        </div>

        <div>
          <div className="mb-2 font-mono text-[10px] uppercase tracking-wider text-faint">Assets by type</div>
          <ul>
            {typeRows.map(([type, count]) => (
              <li key={type} className="flex items-center justify-between border-b border-line py-2 last:border-0 font-mono text-[12px]">
                <span className="text-ink">{type}</span>
                <span className="text-muted">{count}</span>
              </li>
            ))}
          </ul>
        </div>
      </div>
    </Panel>
  )
}

function Metric({ label, value, tone }: { label: string; value: string; tone: Tone }) {
  return (
    <div className="rounded-lg border border-line bg-bg/40 p-3">
      <div className="font-mono text-[10px] uppercase tracking-wider text-faint">{label}</div>
      <div className={`mt-1.5 font-display text-[20px] font-semibold leading-none ${value.length > 9 ? 'text-[14px]' : ''} ${txt(tone)}`}>{value}</div>
    </div>
  )
}

function postureToneOf(p: string): Tone {
  return p === 'Nominal' ? 'safe' : p === 'Degraded' ? 'warn' : p === 'LockedOut' ? 'crit' : 'muted'
}
function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
