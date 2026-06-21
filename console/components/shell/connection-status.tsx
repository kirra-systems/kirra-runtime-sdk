'use client'

import { useHealth } from '@/lib/api/hooks'
import { Pill } from '@/components/ui/primitives'

// Shell indicator: reflects whether the console is bound to a live Kirra
// verifier (NEXT_PUBLIC_KIRRA_API_URL) and whether it is reachable.
export function ConnectionStatus() {
  const { status } = useHealth()
  if (status === 'demo') return <Pill tone="ice">Demo data</Pill>
  if (status === 'ok') return <Pill tone="safe">Live · connected</Pill>
  if (status === 'connecting') return <Pill tone="warn">Connecting…</Pill>
  return <Pill tone="crit">Backend offline</Pill>
}
