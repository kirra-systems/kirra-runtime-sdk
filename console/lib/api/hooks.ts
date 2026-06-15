'use client'

import { useEffect, useRef, useState } from 'react'
import { IS_LIVE } from './config'
import { kirra } from './client'
import type { FleetNodePosture, FleetPostureState, PostureStreamEvent } from './types'
import { robots } from '@/lib/mock'

export type Source = 'live' | 'demo'
export type LinkStatus = 'connecting' | 'ok' | 'down' | 'demo'

// Demo fleet derived from the bundled mock roster, shaped like the wire type.
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

// Polls /health so the shell can show a live connection indicator.
export function useHealth(pollMs = 10000): { status: LinkStatus } {
  const [status, setStatus] = useState<LinkStatus>(IS_LIVE ? 'connecting' : 'demo')
  useEffect(() => {
    if (!IS_LIVE) return
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const ping = async () => {
      try {
        const h = await kirra.health(ctrl.signal)
        setStatus(h.status === 'ok' ? 'ok' : 'down')
      } catch (e) {
        if ((e as { name?: string })?.name !== 'AbortError') setStatus('down')
      }
      timer = setTimeout(ping, pollMs)
    }
    ping()
    return () => { ctrl.abort(); clearTimeout(timer) }
  }, [pollMs])
  return { status }
}

function mkEvent(p: FleetNodePosture, type: string): PostureStreamEvent {
  return { event_type: type, node_id: p.node_id, emitted_at_ms: Date.now(), posture: p }
}

// Live fleet posture + a derived event feed. In live mode it polls
// /fleet/posture and emits an event whenever a node's posture transitions; in
// demo mode it serves the mock roster and rotates synthetic events so the feed
// still breathes. Always falls back to demo on a fetch error.
export function useLiveFleet(pollMs = 5000): {
  fleet: FleetNodePosture[]
  events: PostureStreamEvent[]
  source: Source
  error: string | null
  updatedAt: number | null
} {
  const [fleet, setFleet] = useState<FleetNodePosture[]>(() => demoFleet())
  const [events, setEvents] = useState<PostureStreamEvent[]>([])
  const [source, setSource] = useState<Source>(IS_LIVE ? 'live' : 'demo')
  const [error, setError] = useState<string | null>(null)
  const [updatedAt, setUpdatedAt] = useState<number | null>(null)
  const prev = useRef<Map<string, FleetPostureState>>(new Map())

  useEffect(() => {
    // ── Demo mode: static fleet + rotating synthetic events ──
    if (!IS_LIVE) {
      const f = demoFleet()
      let k = 0
      const id = setInterval(() => {
        const p = f[k % f.length]; k += 1
        setEvents((cur) => [mkEvent(p, 'POSTURE_RECALCULATED'), ...cur].slice(0, 40))
      }, 2600)
      return () => clearInterval(id)
    }

    // ── Live mode: poll, diff, derive transition events ──
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const load = async () => {
      try {
        const { fleet: next } = await kirra.fleetPosture(ctrl.signal)
        const seeded = prev.current.size > 0
        const fresh: PostureStreamEvent[] = []
        for (const n of next) {
          const before = prev.current.get(n.node_id)
          if (seeded && before !== undefined && before !== n.propagated_status) {
            fresh.push(mkEvent(n, 'POSTURE_TRANSITION'))
          }
          prev.current.set(n.node_id, n.propagated_status)
        }
        setFleet(next)
        if (fresh.length) setEvents((cur) => [...fresh, ...cur].slice(0, 40))
        setSource('live'); setError(null); setUpdatedAt(Date.now())
      } catch (e) {
        if ((e as { name?: string })?.name !== 'AbortError') {
          setError(String((e as Error)?.message ?? e))
          setSource('demo')
          setFleet(demoFleet())
        }
      }
      timer = setTimeout(load, pollMs)
    }
    load()
    return () => { ctrl.abort(); clearTimeout(timer) }
  }, [pollMs])

  return { fleet, events, source, error, updatedAt }
}
