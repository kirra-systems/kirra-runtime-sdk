'use client'

import { Panel, Pill, StatusDot } from '@/components/ui/primitives'
import { useFederationReports } from '@/lib/api/hooks'
import { postureTone } from '@/lib/api/types'
import type { Tone } from '@/lib/types'

// Cross-controller federated trust reports for one asset (GET
// /federation/reports/{asset_id}, public). Self-contained client panel so the
// server-rendered twin page stays a server component.
export function FederationPanel({ nodeId }: { nodeId: string }) {
  const { rows, source } = useFederationReports(nodeId)

  return (
    <Panel
      title="Federated Trust"
      subtitle={source === 'live' ? 'live · GET /federation/reports' : 'demo · cross-controller'}
      action={source === 'live' ? <Pill tone="safe">live</Pill> : <Pill tone="ice">demo</Pill>}
    >
      {rows.length === 0 ? (
        <p className="py-2 font-mono text-[11px] text-faint">no federated reports for this asset</p>
      ) : (
        <ul className="space-y-2.5">
          {rows.map((r, i) => {
            const tone: Tone = r.expired ? 'muted' : postureTone(r.posture)
            return (
              <li key={`${r.source}-${i}`} className="flex items-center gap-3">
                <StatusDot tone={tone} />
                <span className="flex-1 truncate font-mono text-[12px] text-ink">{r.source}</span>
                <span className={`font-mono text-[11px] ${txt(tone)}`}>{r.posture}</span>
                <span className="w-28 text-right font-mono text-[10px] text-faint">{r.expiresLabel}</span>
              </li>
            )
          })}
        </ul>
      )}
    </Panel>
  )
}

function txt(t: Tone) {
  return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted'
}
