'use client'

import { Pill } from '@/components/ui/primitives'
import { useVersions } from '@/lib/api/hooks'
import type { Tone } from '@/lib/types'

// Live fleet version-adoption bar (GET /console/versions, #398). Self-contained
// client block so the server-rendered Reports page stays untouched — mirrors the
// FabricTelemetryPanel pattern. Falls back to a bundled demo snapshot when the
// proxy is in demo mode or the backend is unreachable. The surrounding
// rollout-rings / staged-deploy / OTA section stays mock (out of #398 scope).
export function VersionAdoptionBar() {
  const { data, source } = useVersions(20000)
  // Newest versions render greener; older / unknown trend toward warn.
  const rows = data.versions.map((v, i) => ({ ...v, tone: toneFor(i, data.versions.length) }))

  return (
    <div className="mt-5 border-t border-line pt-4">
      <div className="mb-2 flex items-center justify-between">
        <span className="font-mono text-[10px] uppercase tracking-wider text-faint">Fleet version adoption</span>
        {source === 'live' ? <Pill tone="safe">live</Pill> : <Pill tone="warn">demo</Pill>}
      </div>
      <div className="flex h-3 overflow-hidden rounded-full bg-white/5">
        {rows.map((v) => (
          <div key={v.version} className={dotBg(v.tone)} style={{ width: `${v.pct}%` }} title={`${v.version} ${v.pct}% · ${v.count} nodes`} />
        ))}
        {data.unknown > 0 && (
          <div className={dotBg('muted')} style={{ width: `${data.total ? (data.unknown / data.total) * 100 : 0}%` }} title={`unknown · ${data.unknown} nodes`} />
        )}
      </div>
      <div className="mt-3 flex flex-wrap gap-4 font-mono text-[11px]">
        {rows.map((v) => (
          <span key={v.version} className="flex items-center gap-1.5">
            <span className={`h-2 w-2 rounded-full ${dotBg(v.tone)}`} />
            <span className="text-ink">{v.version}</span>
            <span className="text-faint">{v.pct}% · {v.count}</span>
          </span>
        ))}
        {data.unknown > 0 && (
          <span className="flex items-center gap-1.5">
            <span className={`h-2 w-2 rounded-full ${dotBg('muted')}`} />
            <span className="text-ink">unknown</span>
            <span className="text-faint">{data.unknown}</span>
          </span>
        )}
      </div>
    </div>
  )
}

function toneFor(i: number, n: number): Tone {
  if (i === 0) return 'safe'
  if (i === n - 1) return 'warn'
  return 'ice'
}
function dotBg(t: Tone) { return t === 'safe' ? 'bg-safe' : t === 'warn' ? 'bg-warn' : t === 'crit' ? 'bg-crit' : t === 'ice' ? 'bg-ice' : 'bg-muted' }
