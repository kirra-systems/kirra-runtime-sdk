import type { SeriesPoint, Tone } from './types'

function g(n: number, b: number, j: number): SeriesPoint[] {
  const o: SeriesPoint[] = []
  let v = b
  for (let i = 0; i < n; i++) {
    v += (Math.random() - 0.5) * j
    v = Math.max(0, v)
    o.push({ t: `${String(i).padStart(2, '0')}:00`, v: Math.round(v) })
  }
  return o
}

export interface Resource { label: string; pct: number; detail: string; tone: Tone }
export interface Partition { name: string; role: string; cpuBudget: number; isolation: 'ENFORCED' | 'DEGRADED'; tone: Tone }
export interface NodeHealth { id: string; cpu: number; mem: number; tempC: number; tone: Tone }
export interface NetStat { label: string; value: string; unit: string; tone: Tone; spark: SeriesPoint[] }
export interface LatPoint { t: string; p50: number; p99: number }

export const resources: Resource[] = [
  { label: 'CPU', pct: 38, detail: '12 cores · QNX SMP', tone: 'safe' },
  { label: 'Memory', pct: 54, detail: '18.2 / 32 GB', tone: 'safe' },
  { label: 'GPU', pct: 61, detail: 'Orin · inference', tone: 'safe' },
  { label: 'Disk I/O', pct: 23, detail: 'audit ledger WAL', tone: 'safe' },
]

export const latency: LatPoint[] = Array.from({ length: 24 }).map((_, i) => {
  const base = 7 + Math.round(Math.sin(i / 3) * 2)
  return { t: `${String(i).padStart(2, '0')}:00`, p50: base + Math.round(Math.random() * 2), p99: base + 8 + Math.round(Math.random() * 8) }
})

export const network: NetStat[] = [
  { label: 'Throughput', value: '2.4', unit: 'Gb/s', tone: 'safe', spark: g(20, 24, 4) },
  { label: 'Packet loss', value: '0.01', unit: '%', tone: 'safe', spark: g(20, 2, 1) },
  { label: 'Jitter', value: '0.4', unit: 'ms', tone: 'safe', spark: g(20, 4, 2) },
  { label: 'Connected nodes', value: '38 / 38', unit: '', tone: 'safe', spark: g(20, 38, 1) },
]

export const partitions: Partition[] = [
  { name: 'Governor (QNX safety)', role: 'ASIL-D · FIFO scheduling', cpuBudget: 34, isolation: 'ENFORCED', tone: 'safe' },
  { name: 'Autoware guest', role: 'planner · isolated VM', cpuBudget: 58, isolation: 'ENFORCED', tone: 'safe' },
  { name: 'Telemetry / Capture', role: 'best-effort', cpuBudget: 19, isolation: 'ENFORCED', tone: 'safe' },
  { name: 'Comms bridge (DDS)', role: 'Volatile QoS', cpuBudget: 27, isolation: 'ENFORCED', tone: 'safe' },
]

export const nodes: NodeHealth[] = [
  { id: 'node-1', cpu: 31, mem: 40, tempC: 48 },
  { id: 'node-2', cpu: 44, mem: 55, tempC: 52 },
  { id: 'node-3', cpu: 38, mem: 48, tempC: 50 },
  { id: 'node-4', cpu: 62, mem: 71, tempC: 61 },
  { id: 'node-5', cpu: 28, mem: 35, tempC: 46 },
  { id: 'node-6', cpu: 49, mem: 52, tempC: 54 },
].map((n) => ({ ...n, tone: (n.cpu > 80 || n.tempC > 70 ? 'crit' : n.cpu > 60 || n.tempC > 60 ? 'warn' : 'safe') as Tone }))

// ── DDS / Network Health (Drop 6) ──────────────────────────────────────────
// Per-topic transport health. Actuator/command topics are Volatile durability
// (the fail-closed invariant — a late publisher must never replay a stale
// actuator sample). Deadline budgets are watched against observed p99.

export interface DdsTopic {
  topic: string
  qos: 'Volatile'
  deadlineMs: number
  observedMs: number
  missRate: number // % of samples missing the deadline
  pubs: number
  subs: number
  tone: Tone
}

export const ddsTopics: DdsTopic[] = [
  { topic: '/governor/verdict', qos: 'Volatile', deadlineMs: 20, observedMs: 6, missRate: 0.0, pubs: 1, subs: 6, tone: 'safe' },
  { topic: '/cmd_vel', qos: 'Volatile', deadlineMs: 50, observedMs: 14, missRate: 0.0, pubs: 8, subs: 8, tone: 'safe' },
  { topic: '/fleet/posture', qos: 'Volatile', deadlineMs: 100, observedMs: 22, missRate: 0.0, pubs: 1, subs: 38, tone: 'safe' },
  { topic: '/telemetry/pose', qos: 'Volatile', deadlineMs: 100, observedMs: 31, missRate: 0.2, pubs: 8, subs: 4, tone: 'safe' },
  { topic: '/sensor/lidar', qos: 'Volatile', deadlineMs: 100, observedMs: 88, missRate: 1.4, pubs: 8, subs: 8, tone: 'warn' },
  { topic: '/sensor/radar', qos: 'Volatile', deadlineMs: 100, observedMs: 97, missRate: 4.1, pubs: 8, subs: 8, tone: 'warn' },
]

export interface DdsPeer { id: string; role: string; rttMs: number; liveliness: 'ALIVE' | 'LOST'; lastSeen: string; tone: Tone }

export const ddsPeers: DdsPeer[] = [
  { id: 'participant-gov-0', role: 'Governor partition', rttMs: 2, liveliness: 'ALIVE', lastSeen: 'now', tone: 'safe' },
  { id: 'participant-plan-1', role: 'Autoware planner', rttMs: 5, liveliness: 'ALIVE', lastSeen: 'now', tone: 'safe' },
  { id: 'participant-edge-7', role: 'KIRRA-13 edge', rttMs: 41, liveliness: 'LOST', lastSeen: '11:58:02', tone: 'crit' },
  { id: 'participant-edge-4', role: 'KIRRA-10 edge', rttMs: 18, liveliness: 'ALIVE', lastSeen: 'now', tone: 'warn' },
  { id: 'participant-tel-2', role: 'Telemetry capture', rttMs: 7, liveliness: 'ALIVE', lastSeen: 'now', tone: 'safe' },
  { id: 'participant-fed-w', role: 'Peer controller (west)', rttMs: 24, liveliness: 'ALIVE', lastSeen: 'now', tone: 'safe' },
]
