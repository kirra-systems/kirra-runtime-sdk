'use client'

import { Panel, Pill } from '@/components/ui/primitives'
import { Spark } from '@/components/charts/charts'
import { useNodeHistory } from '@/lib/api/hooks'

// Live posture-history trend for a single asset (GET /fleet/history/{node_id},
// public). Self-contained client panel so the server-rendered twin page is
// untouched. Trend is the numeric posture level over time (Nominal=2…Locked=0).
export function PostureTrend({ nodeId }: { nodeId: string }) {
  const { points, events, lastReason, source } = useNodeHistory(nodeId)
  const current = points.length ? points[points.length - 1].v : 2
  const tone = current >= 2 ? 'safe' : current === 1 ? 'warn' : 'crit'
  const label = current >= 2 ? 'Nominal' : current === 1 ? 'Degraded' : 'LockedOut'

  return (
    <Panel
      title="Posture History"
      subtitle={`${source === 'live' ? 'live · GET /fleet/history' : 'demo'} · ${events} events`}
      action={source === 'live' ? <Pill tone="safe">live</Pill> : <Pill tone="ice">demo</Pill>}
    >
      <div className="mb-3 flex items-baseline gap-2">
        <span className={`font-display text-xl font-semibold ${txt(tone)}`}>{label}</span>
        <span className="font-mono text-[11px] text-faint">current posture</span>
      </div>
      <Spark data={points} color={tone} />
      <div className="mt-3 flex items-center justify-between border-t border-line pt-3 font-mono text-[10px] text-faint">
        <span>Nominal → Degraded → LockedOut</span>
        <span className={lastReason ? txt(tone) : ''}>{lastReason ?? 'no faults in window'}</span>
      </div>
    </Panel>
  )
}

function txt(t: 'safe' | 'warn' | 'crit') { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : 'text-crit' }
