'use client'

import { useEffect, useRef, useState } from 'react'
import { kirra, DemoMode } from './client'
import type { AuditEntry, AuditVerify, FabricTelemetry, FleetNodePosture, FleetPostureState, NodeHistoryEntry, PostureStreamEvent } from './types'
import { robots } from '@/lib/mock'
import { log as demoEvents, sources as demoSources } from '@/lib/events'
import { incidents as demoIncidents } from '@/lib/incidents'
import { twins as demoTwins } from '@/lib/fleet'
import type { Tone, SeriesPoint } from '@/lib/types'

// Shared severity classifier for verifier event/audit types.
export function eventTone(eventType: string): Tone {
  if (/BREACH|DENY|LOCKEDOUT|CYCLE|REVOK|FAULT|BLOCKED/i.test(eventType)) return 'crit'
  if (/DEGRADED|CLAMP|TRANSITION|WARN/i.test(eventType)) return 'warn'
  if (/FEDERATION|DDS|LATENCY/i.test(eventType)) return 'ice'
  return 'safe'
}

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

// ── Hooks ─────────────────────────────────────────────────

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

// Unified event feed for the Event Stream: live from the audit ledger
// (/console/audit, public) with the mock log as fallback. Returns a flat row
// shape plus the distinct source channels present in the data.
export interface FeedRow { id: string; tone: Tone; source: string; ts: string; message: string; code?: string }

function demoFeed(): FeedRow[] {
  return demoEvents.map((e) => ({ id: e.id, tone: e.tone, source: e.source, ts: e.ts, message: e.message, code: e.code }))
}

export function useEventFeed(limit = 60): { rows: FeedRow[]; sources: string[]; source: Source } {
  const [rows, setRows] = useState<FeedRow[]>(() => demoFeed())
  const [sources, setSources] = useState<string[]>([...demoSources])
  const [source, setSource] = useState<Source>('demo')

  useEffect(() => {
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const load = async () => {
      try {
        const page = await kirra.auditPage(limit, ctrl.signal)
        const r: FeedRow[] = page.entries.map((e) => ({
          id: String(e.id),
          tone: eventTone(e.event_type),
          source: e.source,
          ts: new Date(e.timestamp_ms).toLocaleTimeString(),
          message: e.payload,
          code: e.event_type,
        }))
        setRows(r)
        setSources(Array.from(new Set(r.map((x) => x.source))))
        setSource('live')
        timer = setTimeout(load, 8000)
      } catch (e) {
        if (isAbort(e)) return
        if (isDemo(e)) { setRows(demoFeed()); setSources([...demoSources]); setSource('demo'); return }
        setRows(demoFeed()); setSources([...demoSources]); setSource('demo'); timer = setTimeout(load, 8000)
      }
    }
    load()
    return () => { ctrl.abort(); clearTimeout(timer) }
  }, [limit])

  return { rows, sources, source }
}

// Incident history derived from the audit ledger (/console/audit, public):
// crit/warn events become incident rows. Mock incidents are the fallback.
export interface IncidentRow { id: string; ts: string; asset: string; title: string; duration: string; status: string; tone: Tone }

function demoIncidentRows(): IncidentRow[] {
  return demoIncidents.map((i) => ({ id: i.id, ts: i.ts, asset: i.asset, title: i.title, duration: `${i.durationS}s`, status: i.status, tone: i.tone }))
}

export function useIncidentHistory(limit = 80): { rows: IncidentRow[]; source: Source } {
  const [rows, setRows] = useState<IncidentRow[]>(() => demoIncidentRows())
  const [source, setSource] = useState<Source>('demo')

  useEffect(() => {
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const load = async () => {
      try {
        const page = await kirra.auditPage(limit, ctrl.signal)
        const inc: IncidentRow[] = page.entries
          .filter((e) => { const t = eventTone(e.event_type); return t === 'crit' || t === 'warn' })
          .map((e) => ({
            id: `INC-${e.id}`,
            ts: new Date(e.timestamp_ms).toLocaleString(),
            asset: e.payload.match(/KIRRA-\d+|fleet-dag/)?.[0] ?? '—',
            title: e.event_type,
            duration: '—',
            status: 'logged',
            tone: eventTone(e.event_type),
          }))
        setRows(inc); setSource('live')
        timer = setTimeout(load, 12000)
      } catch (e) {
        if (isAbort(e)) return
        if (isDemo(e)) { setRows(demoIncidentRows()); setSource('demo'); return }
        setRows(demoIncidentRows()); setSource('demo'); timer = setTimeout(load, 12000)
      }
    }
    load()
    return () => { ctrl.abort(); clearTimeout(timer) }
  }, [limit])

  return { rows, source }
}

// Fabric governance telemetry — GET /fabric/telemetry (admin via the proxy):
// commands/min, denial rate, asset distribution. Demo fallback otherwise.
const DEMO_FABRIC: FabricTelemetry = {
  total_assets: 8,
  active_assets: 6,
  total_commands_per_minute: 312.4,
  fabric_denial_rate: 0.012,
  assets_by_type: { 'AMR-400': 2, 'Atlas-X': 2, 'Spot-V2': 2, 'Forklift-A': 2 },
  assets_by_posture: { Nominal: 6, Degraded: 1, LockedOut: 1 },
  highest_denial_asset: 'KIRRA-13',
  computed_at_ms: Date.now(),
}

export function useFabricTelemetry(pollMs = 10000): { data: FabricTelemetry; source: Source } {
  const [data, setData] = useState<FabricTelemetry>(DEMO_FABRIC)
  const [source, setSource] = useState<Source>('demo')

  useEffect(() => {
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const load = async () => {
      try {
        const t = await kirra.fabricTelemetry(ctrl.signal)
        setData(t); setSource('live')
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

  return { data, source }
}

// Per-node posture history (GET /fleet/history/{node_id}, public) → a numeric
// posture-level trend (Nominal=2, Degraded=1, LockedOut=0) for a sparkline.
const postureLevel = (p: FleetPostureState): number => (p === 'Nominal' ? 2 : p === 'Degraded' ? 1 : 0)

function entryLevel(e: NodeHistoryEntry): number {
  if (e.posture) return postureLevel(e.posture.propagated_status)
  return eventTone(e.event_type) === 'crit' ? 0 : eventTone(e.event_type) === 'warn' ? 1 : 2
}

function demoNodeTrend(nodeId: string): { points: SeriesPoint[]; events: number; lastReason: string | null } {
  const twin = demoTwins.find((t) => t.name === nodeId)
  const end = twin ? postureLevel(twin.posture) : 2
  // Mostly Nominal, converging to the node's current level over the last few steps.
  const levels = [2, 2, 2, 2, 2, 2, 2, 2, Math.min(2, end + 1), end, end, end]
  const now = Date.now()
  const points = levels.map((v, i) => ({ t: new Date(now - (levels.length - i) * 1800000).toLocaleTimeString(), v }))
  const lastReason = end === 0 ? 'lockout · human reset required' : end === 1 ? 'sensor confidence < 0.60 floor' : null
  return { points, events: levels.length, lastReason }
}

export function useNodeHistory(nodeId: string, pollMs = 20000): {
  points: SeriesPoint[]
  events: number
  lastReason: string | null
  source: Source
} {
  const [state, setState] = useState(() => ({ ...demoNodeTrend(nodeId), source: 'demo' as Source }))

  useEffect(() => {
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const load = async () => {
      try {
        const { history } = await kirra.nodeHistory(nodeId, ctrl.signal)
        const chrono = [...history].reverse() // API returns newest-first
        const points: SeriesPoint[] = chrono.map((e) => ({ t: new Date(e.created_at_ms).toLocaleTimeString(), v: entryLevel(e) }))
        setState({ points, events: history.length, lastReason: history[0]?.reason ?? null, source: 'live' })
        timer = setTimeout(load, pollMs)
      } catch (e) {
        if (isAbort(e)) return
        if (isDemo(e)) { setState({ ...demoNodeTrend(nodeId), source: 'demo' }); return }
        setState({ ...demoNodeTrend(nodeId), source: 'demo' }); timer = setTimeout(load, pollMs)
      }
    }
    load()
    return () => { ctrl.abort(); clearTimeout(timer) }
  }, [nodeId, pollMs])

  return state
}
