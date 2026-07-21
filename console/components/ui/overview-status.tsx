'use client'

import { useMemo } from 'react'
import { useAuditChain, useLiveFleet } from '@/lib/api/hooks'
import { DemoBadge } from '@/components/ui/demo-badge'
import { Pill } from '@/components/ui/primitives'

/** Answer-first status strip for the Overview header.
    Every value here is REAL console state — live verifier data when bound,
    otherwise the demo dataset with the badge saying exactly that. Nothing in
    this strip is a hardcoded reassurance. */
export function OverviewStatus() {
  const { fleet, source, updatedAt } = useLiveFleet()
  const { verify, source: auditSource } = useAuditChain()

  const posture = useMemo(() => {
    const states = fleet.map((n) => n.propagated_status)
    if (states.some((s) => s === 'LockedOut')) return { label: 'FLEET LOCKED OUT', tone: 'crit' as const }
    if (states.some((s) => s === 'Degraded')) return { label: 'FLEET DEGRADED', tone: 'warn' as const }
    if (fleet.length === 0) return { label: 'NO NODES', tone: 'warn' as const }
    return { label: 'FLEET NOMINAL', tone: 'safe' as const }
  }, [fleet])

  const trusted = fleet.filter((n) => n.propagated_status === 'Nominal').length
  const age = updatedAt ? Math.max(0, Math.round((Date.now() - updatedAt) / 1000)) : null

  return (
    <div className="flex flex-wrap items-center gap-2">
      <DemoBadge live={source === 'live'} />
      <Pill tone={posture.tone}>{posture.label}</Pill>
      <Pill tone="muted">
        {trusted}/{fleet.length} nominal
      </Pill>
      <Pill tone={verify.chain_intact ? 'safe' : 'crit'}>
        audit {verify.chain_intact ? 'intact' : 'BROKEN'}
        {auditSource === 'demo' ? ' · demo' : ''}
      </Pill>
      {age !== null && (
        <span className="font-mono text-[11px] text-faint" aria-live="polite">
          updated {age}s ago
        </span>
      )}
    </div>
  )
}
