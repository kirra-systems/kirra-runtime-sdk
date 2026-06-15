'use client'

import { useEffect, useRef, useState } from 'react'
import { kirra, DemoMode } from './client'
import type { AuditEntry, AuditVerify, FleetNodePosture, FleetPostureState, PostureStreamEvent } from './types'
import { robots } from '@/lib/mock'

export type Source = 'live' | 'demo'
export type LinkStatus = 'connecting' | 'ok' | 'down' | 'demo'

const isDemo = (e: unknown) => e instanceof DemoMode
const isAbort = (e: unknown) => (e as { name?: string })?.name === 'AbortError'

// ── Demo fallbacks (shaped exactly like the wire types) ────────────────────
function demoFleet(): FleetNodePosture[] {
  return robots.map((r) => ({
    node_id: r.name,
    local_status:
      r.posture === 'LockedOut' ? { Untrusted: 'lockout · human reset required' }
      : r.posture === 'Degraded' ? { Untrusted: 'sensor confidence < 0.60 floor' }
      : 'Trusted',
    propagated_status: r.posture as FleetPostureState,
    blocked_by: r.posture === 'LockedOut' ? ['fleet-dag'] : [],
  }))
}

const DEMO_VERIFY: AuditVerify = {
  chain_intact: true, total_entries: 184220, latest_hash: '0x9f3a…c1',
  signing_enabled: true, signed_entries: 184220, unsigned_entries: 0,
  signature_valid: true, public_key_b64: 'demo', head_verified: true,
  head_status: 'verified', verified: true,
}
function demoAudit(): AuditEntry[] {
  const now = Date.now()
  const rows: Array<[string, string, string]> = [
    ['KINEMATIC_ENVELOPE_BREACH', 'governor', 'KIRRA-13 cmd_vel 999 m/s → DENY'],
    ['POSTURE_TRANSITION', 'fleet', 'KIRRA-10 → Degraded (confidence 0.41)'],
    ['ATTESTATION_TRUSTED', 'attestation', 'KIRRA-09 Ed25519 verified'],
    ['FEDERATION_REPORT_ACCEPTED', 'federation', 'peer-controller-west gen 4471'],
    ['NOMINAL_VALID_KINEMATICS', 'governor', 'KIRRA-08 cmd_vel 1.2 m/s → ALLOW'],
  ]
  return rows.map(([event_type, source, payload], i) => ({
    id: 184220 - i, timestamp_ms: now - i * 41000, event_type, source, payload,
    prev_hash: '0x…', entry_hash: '0x…', signature_b64: 'demo', signature_status: 'verified',
  }))
}

function mkEvent(p: FleetNodePosture, type: string): PostureStreamEvent {
  return { event_type: type, node_id: p.node_id, emitted_at_ms: Date.now(), posture: p }
}

// ── Hooks ─────────────────────────────────────────────────────

// Polls /health through the proxy for the shell connection indicator.
export function useHealth(pollMs = 10000): { status: LinkStatus } {
  const [status, setStatus] = useState<LinkStatus>('connecting')
  useEffect(() => {
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const ping = async () => {
      try {
        const h = await kirra.health(ctrl.signal)
        setStatus(h.status === 'ok' ? 'ok' : 'down')
        timer = setTimeout(ping, pollMs)
      } catch (e) {
        if (isAbort(e)) return
        if (isDemo(e)) { setStatus('demo'); return } // no backend — stop
        setStatus('down'); timer = setTimeout(ping, pollMs)
      }
    }
    ping()
    return () => { ctrl.abort(); clearTimeout(timer) }
  }, [pollMs])
  return { status }
}

// Live fleet posture + a derived posture-transition event feed. Polls the proxy;
// emits an event when a node's propagated posture changes. Falls back to demo
// data and rotating synthetic events when no backend is configured / reachable.
export function useLiveFleet(pollMs = 5000): {
  fleet: FleetNodePosture[]
  events: PostureStreamEvent[]
  source: Source
  error: string | null
  updatedAt: number | null
} {
  const [fleet, setFleet] = useState<FleetNodePosture[]>(() => demoFleet())
  const [events, setEvents] = useState<PostureStreamEvent[]>([])
  const [source, setSource] = useState<Source>('demo')
  const [error, setError] = useState<string | null>(null)
  const [updatedAt, setUpdatedAt] = useState<number | null>(null)
  const prev = useRef<Map<string, FleetPostureState>>(new Map())
  const demoTimer = useRef<ReturnType<typeof setInterval> | null>(null)

  useEffect(() => {
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>

    const startDemo = () => {
      if (demoTimer.current) return
      const f = demoFleet(); setFleet(f); setSource('demo')
      let k = 0
      demoTimer.current = setInterval(() => {
        const p = f[k % f.length]; k += 1
        setEvents((cur) => [mkEvent(p, 'POSTURE_RECALCULATED'), ...cur].slice(0, 40))
      }, 2600)
    }

    const load = async () => {
      try {
        const { fleet: next } = await kirra.fleetPosture(ctrl.signal)
        if (demoTimer.current) { clearInterval(demoTimer.current); demoTimer.current = null }
        const seeded = prev.current.size > 0
        const fresh: PostureStreamEvent[] = []
        for (const n of next) {
          const before = prev.current.get(n.node_id)
          if (seeded && before !== undefined && before !== n.propagated_status) fresh.push(mkEvent(n, 'POSTURE_TRANSITION'))
          prev.current.set(n.node_id, n.propagated_status)
        }
        setFleet(next); setSource('live'); setError(null); setUpdatedAt(Date.now())
        if (fresh.length) setEvents((cur) => [...fresh, ...cur].slice(0, 40))
        timer = setTimeout(load, pollMs)
      } catch (e) {
        if (isAbort(e)) return
        if (isDemo(e)) { startDemo(); return }       // no backend — settle into demo
        setError(String((e as Error)?.message ?? e)) // backend down — show demo, keep retrying
        startDemo(); timer = setTimeout(load, pollMs)
      }
    }
    load()
    return () => {
      ctrl.abort(); clearTimeout(timer)
      if (demoTimer.current) { clearInterval(demoTimer.current); demoTimer.current = null }
    }
  }, [pollMs])

  return { fleet, events, source, error, updatedAt }
}

// Audit-chain integrity + recent entries (verify is admin-gated, entries are
// public /console/audit — both flow through the proxy).
export function useAuditChain(pollMs = 15000): {
  verify: AuditVerify
  entries: AuditEntry[]
  source: Source
} {
  const [verify, setVerify] = useState<AuditVerify>(DEMO_VERIFY)
  const [entries, setEntries] = useState<AuditEntry[]>(() => demoAudit())
  const [source, setSource] = useState<Source>('demo')

  useEffect(() => {
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const load = async () => {
      try {
        const [v, page] = await Promise.all([kirra.auditVerify(ctrl.signal), kirra.auditPage(12, ctrl.signal)])
        setVerify(v); setEntries(page.entries); setSource('live')
        timer = setTimeout(load, pollMs)
      } catch (e) {
        if (isAbort(e)) return
        if (isDemo(e)) { setSource('demo'); return }
        setSource('demo'); timer = setTimeout(load, pollMs)
      }
    }
    load()
    return () => { ctrl.abort(); clearTimeout(timer) }
  }, [pollMs])

  return { verify, entries, source }
}
