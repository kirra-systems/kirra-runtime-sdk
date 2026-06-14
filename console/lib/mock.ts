import type { KPI, Robot, EventItem, SeriesPoint, Posture, Tone } from '@/lib/types'

function gen(n: number, base: number, jitter: number, drift = 0): SeriesPoint[] {
  const out: SeriesPoint[] = []
  let v = base
  for (let i = 0; i < n; i++) {
    v += (Math.random() - 0.5) * jitter + drift
    v = Math.max(0, v)
    out.push({ t: `${String(i % 24).padStart(2, '0')}:00`, v: Math.round(v) })
  }
  return out
}

export const series = {
  throughput: gen(24, 820, 120),
  velocity: gen(24, 12, 4),
}

export const kpis: KPI[] = [
  { label: 'Active Robots', value: 142, unit: 'online', delta: '+6', tone: 'safe', spark: gen(20, 130, 8) },
  { label: 'Connected Systems', value: 38, unit: 'nodes', delta: '+1', tone: 'ice', spark: gen(20, 36, 2) },
  { label: 'Autonomous Missions', value: 27, unit: 'running', delta: '−2', tone: 'ice', spark: gen(20, 28, 3) },
  { label: 'Safety Interventions', value: 3, unit: '24h', delta: '+1', tone: 'warn', spark: gen(20, 2, 1) },
  { label: 'Runtime Uptime', value: '99.98', unit: '%', delta: '30d', tone: 'safe', spark: gen(20, 99, 0.4) },
]

const POS: Posture[] = ['Nominal', 'Nominal', 'Nominal', 'Degraded', 'Nominal', 'Nominal', 'LockedOut', 'Nominal']

export const robots: Robot[] = Array.from({ length: 8 }).map((_, i) => ({
  id: `r${i + 1}`,
  name: `KIRRA-${String(i + 7).padStart(2, '0')}`,
  model: ['Atlas-X', 'Spot-V2', 'AMR-400', 'Forklift-A'][i % 4],
  posture: POS[i],
  status: (POS[i] === 'LockedOut' ? 'standby' : i % 5 === 4 ? 'standby' : 'online') as Robot['status'],
  battery: [88, 64, 41, 22, 73, 95, 12, 57][i],
  mission: ['Patrol Loop B', 'Pick & Place', 'Yard Transit', 'Inspection', 'Charging', 'Pallet Run', '— HOLD —', 'Survey'][i],
  latencyMs: [8, 11, 14, 22, 9, 7, 31, 12][i],
}))

export const events: EventItem[] = [
  { id: 'e1', tone: 'crit', source: 'governor', ts: '12:04:21', message: 'DENY · KINEMATIC_ENVELOPE_BREACH — KIRRA-13 cmd_vel 999 m/s' },
  { id: 'e2', tone: 'warn', source: 'fleet', ts: '12:03:58', message: 'KIRRA-10 entered Degraded posture (sensor confidence 0.41)' },
  { id: 'e3', tone: 'safe', source: 'governor', ts: '12:03:30', message: 'ALLOW · cmd_vel 1.2 m/s dispatched to KIRRA-09' },
  { id: 'e4', tone: 'ice', source: 'telemetry', ts: '12:03:12', message: 'DDS latency p99 = 14ms (within FTTI budget)' },
  { id: 'e5', tone: 'crit', source: 'governor', ts: '12:02:40', message: 'LOCKEDOUT · cycle detected in dependency DAG — KIRRA-13 isolated' },
  { id: 'e6', tone: 'safe', source: 'compliance', ts: '12:01:05', message: 'Audit chain verified · 184,220 links · no breaks' },
]

export function postureTone(p: Posture): Tone {
  return p === 'Nominal' ? 'safe' : p === 'Degraded' ? 'warn' : 'crit'
}
