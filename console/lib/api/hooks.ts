'use client'

import { useEffect, useRef, useState } from 'react'
import { kirra, DemoMode } from './client'
import { PROXY_BASE } from './config'
import type { AuditEntry, AuditVerify, AssetPosture, ConsoleAnalytics, ConsoleRuntime, ConsoleSites, ConsoleVersions, FabricTelemetry, FederatedReport, FleetNodePosture, FleetPostureState, NodeHistoryEntry, PostureStreamEvent } from './types'
import { robots } from '@/lib/mock'
import { log as demoEvents, sources as demoSources } from '@/lib/events'
import { incidents as demoIncidents } from '@/lib/incidents'
import { twins as demoTwins } from '@/lib/fleet'
import { recent as demoDecisions, tally as demoTally, type DecisionRow, type DecisionTally, type Verdict } from '@/lib/oversight'
import type { Tone, SeriesPoint } from '@/lib/types'
import { DEMO_EPOCH, utcDateTime, utcTime } from '@/lib/format'

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

// ── Demo fallbacks (shaped exactly like the wire types) ─────────────────────
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
  const now = DEMO_EPOCH
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

// ── Hooks ────────────────────────────────────

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
  transport: 'stream' | 'poll' | 'demo'
} {
  const [fleet, setFleet] = useState<FleetNodePosture[]>(() => demoFleet())
  const [events, setEvents] = useState<PostureStreamEvent[]>([])
  const [source, setSource] = useState<Source>('demo')
  const [error, setError] = useState<string | null>(null)
  const [updatedAt, setUpdatedAt] = useState<number | null>(null)
  const [transport, setTransport] = useState<'stream' | 'poll' | 'demo'>('demo')
  const prev = useRef<Map<string, FleetPostureState>>(new Map())
  const demoTimer = useRef<ReturnType<typeof setInterval> | null>(null)

  useEffect(() => {
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    let esTimer: ReturnType<typeof setTimeout>
    let refetchTimer: ReturnType<typeof setTimeout> | null = null
    let es: EventSource | null = null
    let esBackoff = 5000
    let streaming = false
    let disposed = false

    const startDemo = () => {
      setTransport('demo')
      if (demoTimer.current) return
      const f = demoFleet(); setFleet(f); setSource('demo')
      let k = 0
      demoTimer.current = setInterval(() => {
        const p = f[k % f.length]; k += 1
        setEvents((cur) => [mkEvent(p, 'POSTURE_RECALCULATED'), ...cur].slice(0, 40))
      }, 2600)
    }

    // SSE upgrade: once a live snapshot succeeds, attach to the verifier's
    // real posture stream (GET /system/posture/stream through the proxy).
    // Events arrive push-fashion; each one schedules a coalesced snapshot
    // refetch so the fleet table stays authoritative. While the stream is
    // open, polling stretches to a 30s safety heartbeat. Any stream error
    // falls back to full-rate polling and retries the stream with backoff —
    // the console is never worse off than the pre-SSE behavior.
    const openStream = () => {
      // Static-demo builds have no proxy/stream; stay on the demo path.
      if (process.env.NEXT_PUBLIC_KIRRA_STATIC_DEMO === '1') return
      if (disposed || es || typeof EventSource === 'undefined') return
      const sse = new EventSource(`${PROXY_BASE}/system/posture/stream`)
      es = sse
      sse.onopen = () => { streaming = true; esBackoff = 5000; setTransport('stream') }
      sse.onmessage = (m) => {
        try {
          const ev = JSON.parse(m.data) as PostureStreamEvent
          if (typeof ev?.event_type === 'string') {
            setEvents((cur) => [ev, ...cur].slice(0, 40))
            if (!refetchTimer) refetchTimer = setTimeout(() => { refetchTimer = null; void load(true) }, 250)
          }
        } catch { /* ignore malformed frames — the snapshot loop stays authoritative */ }
      }
      sse.onerror = () => {
        sse.close()
        es = null
        if (streaming) { streaming = false; setTransport('poll') }
        if (!disposed) {
          esTimer = setTimeout(openStream, esBackoff)
          esBackoff = Math.min(esBackoff * 2, 60000)
        }
      }
    }

    const load = async (oneShot = false) => {
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
        if (!streaming) setTransport('poll')
        if (fresh.length && !streaming) setEvents((cur) => [...fresh, ...cur].slice(0, 40))
        openStream()
        if (!oneShot) timer = setTimeout(() => void load(), streaming ? Math.max(pollMs, 30000) : pollMs)
      } catch (e) {
        if (isAbort(e)) return
        if (isDemo(e)) { startDemo(); return }       // no backend — settle into demo
        setError(String((e as Error)?.message ?? e)) // backend down — show demo, keep retrying
        startDemo()
        if (!oneShot) timer = setTimeout(() => void load(), pollMs)
      }
    }
    void load()
    return () => {
      disposed = true
      ctrl.abort(); clearTimeout(timer); clearTimeout(esTimer)
      if (refetchTimer) clearTimeout(refetchTimer)
      es?.close(); es = null
      if (demoTimer.current) { clearInterval(demoTimer.current); demoTimer.current = null }
    }
  }, [pollMs])

  return { fleet, events, source, error, updatedAt, transport }
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
          ts: utcTime(e.timestamp_ms),
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
            ts: utcDateTime(e.timestamp_ms),
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
  computed_at_ms: DEMO_EPOCH,
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
  const now = DEMO_EPOCH
  const points = levels.map((v, i) => ({ t: utcTime(now - (levels.length - i) * 1800000), v }))
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
        const points: SeriesPoint[] = chrono.map((e) => ({ t: utcTime(e.created_at_ms), v: entryLevel(e) }))
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

// Cross-controller federated trust reports for an asset (GET
// /federation/reports/{asset_id}, public). Normalizes the double-encoded posture
// and flags freshness against each report's expiry. Demo fallback otherwise.
export interface FedReportRow { source: string; posture: FleetPostureState; expired: boolean; expiresLabel: string }

function normPosture(raw: string): FleetPostureState {
  const s = raw.replace(/^"+|"+$/g, '')
  return s === 'Degraded' ? 'Degraded' : s === 'LockedOut' ? 'LockedOut' : 'Nominal'
}

function fedRow(r: FederatedReport, now: number): FedReportRow {
  const expired = r.expires_at_ms <= now
  const secs = Math.round(Math.abs(r.expires_at_ms - now) / 1000)
  return { source: r.source_controller_id, posture: normPosture(r.posture), expired, expiresLabel: expired ? `expired ${secs}s ago` : `valid ${secs}s` }
}

function demoFedReports(nodeId: string): FedReportRow[] {
  const twin = demoTwins.find((t) => t.name === nodeId)
  const posture = (twin?.posture ?? 'Nominal') as FleetPostureState
  return [
    { source: 'peer-controller-west', posture, expired: false, expiresLabel: 'valid 3s' },
    { source: 'peer-controller-east', posture: 'Nominal', expired: false, expiresLabel: 'valid 4s' },
  ]
}

export function useFederationReports(nodeId: string, pollMs = 15000): { rows: FedReportRow[]; source: Source } {
  const [rows, setRows] = useState<FedReportRow[]>(() => demoFedReports(nodeId))
  const [source, setSource] = useState<Source>('demo')

  useEffect(() => {
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const load = async () => {
      try {
        const { reports } = await kirra.federationReports(nodeId, ctrl.signal)
        const now = Date.now()
        setRows(reports.map((r) => fedRow(r, now))); setSource('live')
        timer = setTimeout(load, pollMs)
      } catch (e) {
        if (isAbort(e)) return
        if (isDemo(e)) { setRows(demoFedReports(nodeId)); setSource('demo'); return }
        setRows(demoFedReports(nodeId)); setSource('demo'); timer = setTimeout(load, pollMs)
      }
    }
    load()
    return () => { ctrl.abort(); clearTimeout(timer) }
  }, [nodeId, pollMs])

  return { rows, source }
}

// Per-asset fabric governance state (GET /fabric/state, admin via the proxy):
// the asset's posture in the cross-asset fabric DAG, its generation, the nodes
// contributing to it, and what (if anything) blocks it. Demo fallback otherwise.
export interface FabricAssetState {
  inFabric: boolean
  posture: FleetPostureState
  generation: number
  contributingNodes: string[]
  blockedBy: string[]
}

function demoFabricState(nodeId: string): { state: FabricAssetState; fabricGen: number } {
  const twin = demoTwins.find((t) => t.name === nodeId)
  const posture = (twin?.posture ?? 'Nominal') as FleetPostureState
  return {
    state: {
      inFabric: true,
      posture,
      generation: 4471,
      contributingNodes: [nodeId, 'fleet-dag'],
      blockedBy: posture === 'LockedOut' ? ['cross_asset_propagation'] : [],
    },
    fabricGen: 4471,
  }
}

export function useFabricState(nodeId: string, pollMs = 12000): {
  state: FabricAssetState
  fabricGen: number
  source: Source
} {
  const [data, setData] = useState(() => ({ ...demoFabricState(nodeId), source: 'demo' as Source }))

  useEffect(() => {
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const load = async () => {
      try {
        const fs = await kirra.fabricState(ctrl.signal)
        const a: AssetPosture | undefined = fs.assets.find((x) => x.asset_id === nodeId)
        const state: FabricAssetState = a
          ? { inFabric: true, posture: a.posture, generation: a.generation, contributingNodes: a.contributing_nodes, blockedBy: a.blocked_by }
          : { inFabric: false, posture: 'Nominal', generation: 0, contributingNodes: [], blockedBy: [] }
        setData({ state, fabricGen: fs.fabric_generation, source: 'live' })
        timer = setTimeout(load, pollMs)
      } catch (e) {
        if (isAbort(e)) return
        if (isDemo(e)) { setData({ ...demoFabricState(nodeId), source: 'demo' }); return }
        setData({ ...demoFabricState(nodeId), source: 'demo' }); timer = setTimeout(load, pollMs)
      }
    }
    load()
    return () => { ctrl.abort(); clearTimeout(timer) }
  }, [nodeId, pollMs])

  return data
}

// AI Decision Oversight — governor verdicts (ALLOW / CLAMP / DENY) and their
// reason codes are recorded in the audit ledger (/console/audit, public). Filter
// the ledger to adjudication events and tally them. Demo fallback otherwise.
function classifyVerdict(eventType: string): Verdict | null {
  if (/CLAMP/i.test(eventType)) return 'CLAMP'
  if (/DENY|DENIED|BREACH|REJECT|BLOCKED|UNKNOWN_ACTION/i.test(eventType)) return 'DENY'
  if (/ALLOW|ADMITTED|VALID|NOMINAL|PERMITTED/i.test(eventType)) return 'ALLOW'
  return null
}
const verdictTone = (v: Verdict): Tone => (v === 'ALLOW' ? 'safe' : v === 'CLAMP' ? 'warn' : 'crit')
function parseAction(payload: string, source: string): string {
  return payload.match(/cmd_vel|drive_to_\w+|grasp_\w+|read_telemetry|motion[_ ]?command/i)?.[0] ?? source
}

export function useDecisions(limit = 60): { recent: DecisionRow[]; tally: DecisionTally[]; source: Source } {
  const [recent, setRecent] = useState<DecisionRow[]>(() => demoDecisions)
  const [tally, setTally] = useState<DecisionTally[]>(() => demoTally)
  const [source, setSource] = useState<Source>('demo')

  useEffect(() => {
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const settleDemo = () => { setRecent(demoDecisions); setTally(demoTally); setSource('demo') }
    const load = async () => {
      try {
        const page = await kirra.auditPage(limit, ctrl.signal)
        const rows: DecisionRow[] = []
        const counts = { ALLOW: 0, CLAMP: 0, DENY: 0 }
        for (const e of page.entries) {
          const v = classifyVerdict(e.event_type)
          if (!v) continue
          counts[v] += 1
          rows.push({
            id: String(e.id),
            ts: utcTime(e.timestamp_ms),
            asset: e.payload.match(/KIRRA-\d+/)?.[0] ?? '—',
            actionType: parseAction(e.payload, e.source),
            verdict: v,
            reason: e.event_type,
            tone: verdictTone(v),
          })
        }
        const total = counts.ALLOW + counts.CLAMP + counts.DENY
        const pct = (n: number) => (total ? Math.round((n / total) * 10000) / 100 : 0)
        setRecent(rows)
        setTally([
          { label: 'Allowed', value: counts.ALLOW, tone: 'safe', share: pct(counts.ALLOW) },
          { label: 'Clamped', value: counts.CLAMP, tone: 'warn', share: pct(counts.CLAMP) },
          { label: 'Denied', value: counts.DENY, tone: 'crit', share: pct(counts.DENY) },
        ])
        setSource('live')
        timer = setTimeout(load, 8000)
      } catch (e) {
        if (isAbort(e)) return
        if (isDemo(e)) { settleDemo(); return }
        settleDemo(); timer = setTimeout(load, 8000)
      }
    }
    load()
    return () => { ctrl.abort(); clearTimeout(timer) }
  }, [limit])

  return { recent, tally, source }
}

// ── Console aggregate endpoints (#394) ───────────────────────────────
// Four read-only console rollups served by the verifier. Each follows the same
// poll/abort/demo-fallback shape as the hooks above: a bundled demo snapshot is
// the initial state and the fallback whenever the proxy is in demo mode or the
// backend is unreachable.

// Runtime health — GET /console/runtime (#395). Operational snapshot of the
// verifier: mode, uptime, posture generation/cache, fabric denial rate, etc.
const DEMO_RUNTIME: ConsoleRuntime = {
  mode: 'Active',
  uptime_ms: 1000 * 60 * 60 * 73 + 1000 * 60 * 14,
  posture_generation: 4471,
  last_recalc_ms: DEMO_EPOCH - 2200,
  posture_cache_ttl_ms: 5000,
  total_nodes: 38,
  fabric_assets: 8,
  fabric_denial_rate: 0.012,
  audit_entries: 184220,
  broadcast_subscribers: 3,
  ha_heartbeat_age_ms: 1840,
}

export function useRuntimeHealth(pollMs = 8000): { data: ConsoleRuntime; source: Source } {
  const [data, setData] = useState<ConsoleRuntime>(DEMO_RUNTIME)
  const [source, setSource] = useState<Source>('demo')

  useEffect(() => {
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const load = async () => {
      try {
        const r = await kirra.runtime(ctrl.signal)
        setData(r); setSource('live')
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

// Analytics — GET /console/analytics?window_ms= (#396). Historical trend (NOT a
// forecast): bucketed posture transitions, denial-rate series, per-asset
// interventions, flapping nodes over the requested window.
function demoAnalytics(windowMs: number): ConsoleAnalytics {
  const now = Date.now()
  const buckets = 12
  const step = Math.max(1, Math.floor(windowMs / buckets))
  const posture_transitions = Array.from({ length: buckets }).map((_, i) => ({
    bucket_start_ms: now - (buckets - i) * step,
    to_degraded: Math.round(Math.abs(Math.sin(i / 2)) * 3),
    to_lockedout: i % 5 === 0 ? 1 : 0,
    to_nominal: Math.round(Math.abs(Math.cos(i / 2)) * 3),
  }))
  const denial_rate_series = Array.from({ length: buckets }).map((_, i) => ({
    bucket_start_ms: now - (buckets - i) * step,
    denial_rate: Math.round((0.008 + Math.abs(Math.sin(i / 3)) * 0.02) * 1000) / 1000,
  }))
  return {
    window_ms: windowMs,
    posture_transitions,
    denial_rate_series,
    interventions_by_asset: [
      { asset_id: 'KIRRA-13', clamps: 14, denies: 6 },
      { asset_id: 'KIRRA-10', clamps: 9, denies: 2 },
      { asset_id: 'KIRRA-09', clamps: 3, denies: 0 },
    ],
    flapping_top: [
      { node_id: 'KIRRA-13', transitions: 11 },
      { node_id: 'KIRRA-10', transitions: 5 },
    ],
  }
}

export function useAnalytics(windowMs = 30 * 24 * 60 * 60 * 1000, pollMs = 30000): { data: ConsoleAnalytics; source: Source } {
  const [data, setData] = useState<ConsoleAnalytics>(() => demoAnalytics(windowMs))
  const [source, setSource] = useState<Source>('demo')

  useEffect(() => {
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const load = async () => {
      try {
        const a = await kirra.analytics(windowMs, ctrl.signal)
        setData(a); setSource('live')
        timer = setTimeout(load, pollMs)
      } catch (e) {
        if (isAbort(e)) return
        if (isDemo(e)) { setData(demoAnalytics(windowMs)); setSource('demo'); return }
        setData(demoAnalytics(windowMs)); setSource('demo'); timer = setTimeout(load, pollMs)
      }
    }
    load()
    return () => { ctrl.abort(); clearTimeout(timer) }
  }, [windowMs, pollMs])

  return { data, source }
}

// Site distribution — GET /console/sites (#397). Fleet posture rolled up per
// site, plus the count of nodes with no site assignment.
const DEMO_SITES: ConsoleSites = {
  sites: [
    { site: 'San Francisco', total: 142, nominal: 131, degraded: 9, lockedout: 2 },
    { site: 'Austin', total: 88, nominal: 86, degraded: 2, lockedout: 0 },
    { site: 'Rotterdam', total: 64, nominal: 63, degraded: 1, lockedout: 0 },
    { site: 'Singapore', total: 51, nominal: 47, degraded: 4, lockedout: 0 },
    { site: 'Tokyo', total: 37, nominal: 36, degraded: 1, lockedout: 0 },
  ],
  unassigned: 0,
}

export function useSites(pollMs = 15000): { data: ConsoleSites; source: Source } {
  const [data, setData] = useState<ConsoleSites>(DEMO_SITES)
  const [source, setSource] = useState<Source>('demo')

  useEffect(() => {
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const load = async () => {
      try {
        const s = await kirra.sites(ctrl.signal)
        setData(s); setSource('live')
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

// Version adoption — GET /console/versions (#398). Software-version share across
// the fleet, plus the nodes reporting an unknown version.
const DEMO_VERSIONS: ConsoleVersions = {
  versions: [
    { version: 'v2.4.1', count: 271, pct: 71 },
    { version: 'v2.4.0', count: 99, pct: 26 },
    { version: 'v2.3.6', count: 11, pct: 3 },
  ],
  total: 382,
  unknown: 1,
}

export function useVersions(pollMs = 20000): { data: ConsoleVersions; source: Source } {
  const [data, setData] = useState<ConsoleVersions>(DEMO_VERSIONS)
  const [source, setSource] = useState<Source>('demo')

  useEffect(() => {
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const load = async () => {
      try {
        const v = await kirra.versions(ctrl.signal)
        setData(v); setSource('live')
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
