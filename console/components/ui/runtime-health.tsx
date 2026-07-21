'use client'

import { useEffect, useState } from 'react'
import { Panel, Pill } from '@/components/ui/primitives'
import { useRuntimeHealth } from '@/lib/api/hooks'
import { DEMO_EPOCH } from '@/lib/format'
import type { Tone } from '@/lib/types'

// Live verifier runtime snapshot (GET /console/runtime, #395). Self-contained
// client panel so the server-rendered Runtime page is untouched — mirrors the
// FabricTelemetryPanel pattern. Falls back to a bundled demo snapshot when the
// proxy is in demo mode or the backend is unreachable.
export function RuntimeHealthPanel() {
  const { data, source } = useRuntimeHealth(8000)
  const denialPct = data.fabric_denial_rate * 100
  // Age must never derive from Date.now() during render: the server-rendered
  // text would differ from the hydration pass (React #418). A mounted clock
  // drives the live age; demo data is anchored to its own fixed epoch so the
  // snapshot renders deterministically.
  const [now, setNow] = useState<number | null>(null)
  useEffect(() => {
    setNow(Date.now())
    const t = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(t)
  }, [])
  const ref = source === 'demo' ? DEMO_EPOCH : now
  const recalcAge = ref === null ? null : Math.max(0, ref - data.last_recalc_ms)

  return (
    <Panel
      title="Verifier Runtime"
      subtitle="operational snapshot · GET /console/runtime"
      action={source === 'live' ? <Pill tone="safe">live</Pill> : <Pill tone="warn">demo</Pill>}
    >
      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        <Metric label="Mode" value={data.mode} tone={data.mode === 'Active' ? 'safe' : 'ice'} />
        <Metric label="Uptime" value={fmtDuration(data.uptime_ms)} tone="ice" />
        <Metric label="Posture gen" value={`#${data.posture_generation}`} tone="safe" />
        <Metric
          label="Last recalc"
          value={recalcAge === null ? '—' : `${fmtAge(recalcAge)} ago`}
          tone={recalcAge !== null && recalcAge > data.posture_cache_ttl_ms ? 'warn' : 'safe'}
        />
        <Metric label="Cache TTL" value={`${(data.posture_cache_ttl_ms / 1000).toFixed(0)}s`} tone="muted" />
        <Metric label="Nodes" value={`${data.total_nodes}`} tone="ice" />
        <Metric label="Fabric assets" value={`${data.fabric_assets}`} tone="ice" />
        <Metric label="Denial rate" value={`${denialPct.toFixed(2)}%`} tone={denialPct > 5 ? 'crit' : denialPct > 1 ? 'warn' : 'safe'} />
        <Metric label="Audit entries" value={fmtCount(data.audit_entries)} tone="safe" />
        <Metric label="SSE subscribers" value={`${data.broadcast_subscribers}`} tone="muted" />
        <Metric
          label="HA heartbeat age"
          value={data.ha_heartbeat_age_ms == null ? '—' : `${fmtAge(data.ha_heartbeat_age_ms)} ago`}
          tone={data.ha_heartbeat_age_ms == null ? 'muted' : data.ha_heartbeat_age_ms > 10000 ? 'crit' : data.ha_heartbeat_age_ms > 5000 ? 'warn' : 'safe'}
        />
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

function fmtDuration(ms: number): string {
  const s = Math.floor(ms / 1000)
  const d = Math.floor(s / 86400)
  const h = Math.floor((s % 86400) / 3600)
  const m = Math.floor((s % 3600) / 60)
  if (d > 0) return `${d}d ${h}h`
  if (h > 0) return `${h}h ${m}m`
  return `${m}m`
}
function fmtAge(ms: number): string {
  if (ms < 1000) return `${ms}ms`
  const s = ms / 1000
  return s < 60 ? `${s.toFixed(1)}s` : `${Math.floor(s / 60)}m`
}
function fmtCount(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(1)}k` : `${n}`
}
function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
