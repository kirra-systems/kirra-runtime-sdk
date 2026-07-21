'use client'

import { useHealth } from '@/lib/api/hooks'
import { Pill } from '@/components/ui/primitives'

/** Page-level data-source badge for screens that mix live panels with
    simulated context. Driven by the actual verifier binding — never a
    hardcoded `live={false}`. When the verifier is reachable, the live-backed
    panels on the page are live and the rest is labeled simulated; with no
    backend, everything is simulated and the badge says so. */
export function SourceBadge() {
  const { status } = useHealth()
  if (status === 'ok') return <Pill tone="safe">Live panels · simulated context</Pill>
  if (status === 'connecting') return <Pill tone="warn">Connecting…</Pill>
  return <Pill tone="warn">Simulated data</Pill>
}
