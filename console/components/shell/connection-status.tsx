'use client'

import { useHealth } from '@/lib/api/hooks'
import { Pill } from '@/components/ui/primitives'

// Shell indicator: reflects whether the console is bound to a live Kirra
// verifier (server-side KIRRA_API_URL via the /api/kirra proxy) and whether
// it is reachable. Nothing backend-specific ships in the browser bundle.
export function ConnectionStatus() {
  const { status } = useHealth()
  if (status === 'demo') return <Pill tone="ice">Demo data</Pill>
  if (status === 'ok') return <Pill tone="safe">Live · connected</Pill>
  if (status === 'connecting') return <Pill tone="warn">Connecting…</Pill>
  return <Pill tone="crit">Backend offline</Pill>
}
