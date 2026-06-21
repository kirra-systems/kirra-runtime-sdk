import type { Tone, Posture, SeriesPoint } from './types'
import type { PosPoint } from './telemetry'

// Robot Digital Twin — the per-asset deep view: subsystem health, the live
// kinematic envelope (proposed vs certified hard limits), hardware attestation,
// the recent actuator command stream, and a position trace.

export interface Subsystem { name: string; health: number; status: string; tone: Tone }
export interface EnvelopeAxis { axis: string; current: string; limit: string; util: number; tone: Tone }
export interface TwinCommand { ts: string; channel: string; value: string; verdict: 'ALLOW' | 'CLAMP' | 'DENY'; tone: Tone }

export interface TwinAttestation {
  nodeId: string
  akDigest: string
  pcr16: string
  lastVerified: string
  status: 'Trusted' | 'Untrusted' | 'Unknown'
  tone: Tone
}

export interface TwinPose { yaw: number; pitch: number; roll: number; heading: string }
export interface TwinVitals { velocity: SeriesPoint[]; accel: SeriesPoint[]; localization: SeriesPoint[] }
export interface Thermal { name: string; tempC: number; tone: Tone }
export interface ActuatorLoad { name: string; loadPct: number; tone: Tone }
export interface MissionPhase { name: string; status: 'done' | 'active' | 'pending'; pct: number }

export interface Twin {
  id: string
  name: string
  model: string
  posture: Posture
  status: string
  uptime: string
  firmware: string
  battery: number
  drawW: number
  rangeKm: number
  subsystems: Subsystem[]
  envelope: EnvelopeAxis[]
  attestation: TwinAttestation
  commands: TwinCommand[]
  path: PosPoint[]
  ego: PosPoint
  pose: TwinPose
  vitals: TwinVitals
  thermals: Thermal[]
  actuators: ActuatorLoad[]
  missionPhases: MissionPhase[]
}

function trace(seed: number): PosPoint[] {
  return Array.from({ length: 40 }).map((_, i) => {
    const t = i / 39
    return { x: 8 + t * 84, y: 50 + Math.sin(t * Math.PI * 2.2 + seed) * 26 + Math.sin(i / 3 + seed) * 2 }
  })
}

const base: Record<string, Partial<Twin>> = {
  degraded: {
    posture: 'Degraded',
    subsystems: [
      { name: 'LiDAR (top)', health: 99, status: 'OK', tone: 'safe' },
      { name: 'Radar (front)', health: 58, status: 'DEGRADED', tone: 'warn' },
      { name: 'IMU', health: 100, status: 'OK', tone: 'safe' },
      { name: 'Drive train', health: 94, status: 'OK', tone: 'safe' },
      { name: 'Brake channel A/B', health: 100, status: '2/2 OK', tone: 'safe' },
      { name: 'Comms (DDS)', health: 88, status: 'OK', tone: 'safe' },
    ],
    envelope: [
      { axis: 'Linear velocity', current: '1.8 m/s', limit: '≤ 2.0 m/s (decel)', util: 90, tone: 'warn' },
      { axis: 'Angular velocity', current: '0.10 rad/s', limit: '≤ 0.80 rad/s', util: 13, tone: 'safe' },
      { axis: 'Lateral accel', current: '0.6 m/s²', limit: '≤ 2.5 m/s²', util: 24, tone: 'safe' },
      { axis: 'Sensor confidence', current: '0.52', limit: '≥ 0.60', util: 87, tone: 'warn' },
    ],
  },
  lockedout: {
    posture: 'LockedOut',
    status: 'standby',
    subsystems: [
      { name: 'LiDAR (top)', health: 96, status: 'OK', tone: 'safe' },
      { name: 'Radar (front)', health: 0, status: 'FAULT', tone: 'crit' },
      { name: 'IMU', health: 100, status: 'OK', tone: 'safe' },
      { name: 'Drive train', health: 0, status: 'HELD', tone: 'crit' },
      { name: 'Brake channel A/B', health: 100, status: 'ENGAGED', tone: 'safe' },
      { name: 'Comms (DDS)', health: 71, status: 'OK', tone: 'warn' },
    ],
    envelope: [
      { axis: 'Linear velocity', current: '0.0 m/s', limit: 'all motion denied', util: 100, tone: 'crit' },
      { axis: 'Angular velocity', current: '0.0 rad/s', limit: 'all motion denied', util: 100, tone: 'crit' },
      { axis: 'Lateral accel', current: '0.0 m/s²', limit: 'all motion denied', util: 100, tone: 'crit' },
      { axis: 'Sensor confidence', current: '0.11', limit: '≥ 0.60', util: 100, tone: 'crit' },
    ],
  },
  nominal: {
    posture: 'Nominal',
    subsystems: [
      { name: 'LiDAR (top)', health: 99, status: 'OK', tone: 'safe' },
      { name: 'Radar (front)', health: 97, status: 'OK', tone: 'safe' },
      { name: 'IMU', health: 100, status: 'OK', tone: 'safe' },
      { name: 'Drive train', health: 98, status: 'OK', tone: 'safe' },
      { name: 'Brake channel A/B', health: 100, status: '2/2 OK', tone: 'safe' },
      { name: 'Comms (DDS)', health: 99, status: 'OK', tone: 'safe' },
    ],
    envelope: [
      { axis: 'Linear velocity', current: '3.2 m/s', limit: '≤ 22.4 m/s', util: 14, tone: 'safe' },
      { axis: 'Angular velocity', current: '0.22 rad/s', limit: '≤ 0.80 rad/s', util: 28, tone: 'safe' },
      { axis: 'Lateral accel', current: '0.9 m/s²', limit: '≤ 2.5 m/s²', util: 36, tone: 'safe' },
      { axis: 'Sensor confidence', current: '0.94', limit: '≥ 0.60', util: 30, tone: 'safe' },
    ],
  },
}

const commandsNominal: TwinCommand[] = [
  { ts: '12:04:51', channel: 'cmd_vel.linear', value: '3.20 m/s', verdict: 'ALLOW', tone: 'safe' },
  { ts: '12:04:51', channel: 'cmd_vel.angular', value: '0.22 rad/s', verdict: 'ALLOW', tone: 'safe' },
  { ts: '12:04:50', channel: 'drive_to_pose', value: 'aisle-7 → dock-3', verdict: 'ALLOW', tone: 'safe' },
  { ts: '12:04:49', channel: 'cmd_vel.linear', value: '3.18 m/s', verdict: 'ALLOW', tone: 'safe' },
]
const commandsDegraded: TwinCommand[] = [
  { ts: '12:04:51', channel: 'cmd_vel.linear', value: '3.4 → 2.0 m/s', verdict: 'CLAMP', tone: 'warn' },
  { ts: '12:04:50', channel: 'grasp_object', value: 'kinetic write', verdict: 'DENY', tone: 'warn' },
  { ts: '12:04:49', channel: 'read_telemetry', value: 'pose + battery', verdict: 'ALLOW', tone: 'safe' },
  { ts: '12:04:48', channel: 'cmd_vel.linear', value: '1.80 m/s', verdict: 'ALLOW', tone: 'safe' },
]
const commandsLocked: TwinCommand[] = [
  { ts: '12:02:40', channel: 'cmd_vel.linear', value: '999 m/s', verdict: 'DENY', tone: 'crit' },
  { ts: '12:02:40', channel: 'MRC', value: 'controlled stop', verdict: 'DENY', tone: 'crit' },
  { ts: '12:02:41', channel: 'any motion', value: 'human reset pending', verdict: 'DENY', tone: 'crit' },
]

// Per-kind pose, thermals, actuator load, and mission timeline.
interface KindDetail { pose: TwinPose; thermals: Thermal[]; actuators: ActuatorLoad[]; missionPhases: MissionPhase[] }
const detail: Record<string, KindDetail> = {
  nominal: {
    pose: { yaw: 41, pitch: 2, roll: -1, heading: 'NE · aisle-7' },
    thermals: [
      { name: 'Drive motor', tempC: 48, tone: 'safe' },
      { name: 'Battery pack', tempC: 32, tone: 'safe' },
      { name: 'Compute (Orin)', tempC: 61, tone: 'safe' },
      { name: 'Brake assembly', tempC: 40, tone: 'safe' },
    ],
    actuators: [
      { name: 'Drive · left', loadPct: 38, tone: 'safe' },
      { name: 'Drive · right', loadPct: 41, tone: 'safe' },
      { name: 'Steering', loadPct: 22, tone: 'safe' },
      { name: 'Lift / mast', loadPct: 0, tone: 'safe' },
    ],
    missionPhases: [
      { name: 'Dispatch', status: 'done', pct: 100 },
      { name: 'Transit', status: 'done', pct: 100 },
      { name: 'Pick & place', status: 'active', pct: 62 },
      { name: 'Return', status: 'pending', pct: 0 },
    ],
  },
  degraded: {
    pose: { yaw: 118, pitch: 1, roll: 0, heading: 'SE · holding' },
    thermals: [
      { name: 'Drive motor', tempC: 58, tone: 'warn' },
      { name: 'Battery pack', tempC: 41, tone: 'safe' },
      { name: 'Compute (Orin)', tempC: 66, tone: 'warn' },
      { name: 'Brake assembly', tempC: 47, tone: 'safe' },
    ],
    actuators: [
      { name: 'Drive · left', loadPct: 64, tone: 'warn' },
      { name: 'Drive · right', loadPct: 61, tone: 'warn' },
      { name: 'Steering', loadPct: 34, tone: 'safe' },
      { name: 'Lift / mast', loadPct: 0, tone: 'safe' },
    ],
    missionPhases: [
      { name: 'Survey', status: 'done', pct: 100 },
      { name: 'Inspect', status: 'active', pct: 40 },
      { name: 'HOLD (degraded)', status: 'active', pct: 0 },
      { name: 'Resume', status: 'pending', pct: 0 },
    ],
  },
  lockedout: {
    pose: { yaw: 0, pitch: 0, roll: 0, heading: 'HOLD · stopped' },
    thermals: [
      { name: 'Drive motor', tempC: 30, tone: 'safe' },
      { name: 'Battery pack', tempC: 78, tone: 'crit' },
      { name: 'Compute (Orin)', tempC: 55, tone: 'safe' },
      { name: 'Brake assembly', tempC: 33, tone: 'safe' },
    ],
    actuators: [
      { name: 'Drive · left', loadPct: 0, tone: 'crit' },
      { name: 'Drive · right', loadPct: 0, tone: 'crit' },
      { name: 'Steering', loadPct: 0, tone: 'crit' },
      { name: 'Brake · A/B', loadPct: 100, tone: 'safe' },
    ],
    missionPhases: [
      { name: 'Transit', status: 'done', pct: 100 },
      { name: 'LOCKOUT', status: 'active', pct: 100 },
      { name: 'Human reset', status: 'pending', pct: 0 },
    ],
  },
}

// Deterministic 24-sample vitals (no RNG — stable across server render + build).
function vitalsFor(kind: string, seed: number): TwinVitals {
  const N = 24
  const s = (fn: (i: number) => number): SeriesPoint[] =>
    Array.from({ length: N }, (_, i) => ({ t: String(i).padStart(2, '0'), v: Math.round(fn(i) * 100) / 100 }))
  if (kind === 'lockedout') {
    return {
      velocity: s((i) => Math.max(0, 1.6 - i * 0.12)),
      accel: s((i) => (i < 6 ? -0.4 : 0)),
      localization: s(() => 11 + Math.sin(seed) * 2),
    }
  }
  if (kind === 'degraded') {
    return {
      velocity: s((i) => Math.max(0, 2.0 + Math.sin(i / 5 + seed) * 0.25 - i * 0.01)),
      accel: s((i) => Math.cos(i / 5 + seed) * 0.3),
      localization: s((i) => 54 + Math.sin(i / 6 + seed) * 4),
    }
  }
  return {
    velocity: s((i) => 3.0 + Math.sin(i / 5 + seed) * 0.6),
    accel: s((i) => Math.cos(i / 4 + seed) * 0.4),
    localization: s((i) => 93 + Math.sin(i / 7 + seed) * 3),
  }
}

// Map the 8 roster robots → digital twins with posture-appropriate detail.
const roster = [
  { id: 'r1', name: 'KIRRA-07', model: 'Atlas-X', kind: 'nominal', battery: 88, draw: 240, range: 11.4 },
  { id: 'r2', name: 'KIRRA-08', model: 'Spot-V2', kind: 'nominal', battery: 64, draw: 180, range: 7.9 },
  { id: 'r3', name: 'KIRRA-09', model: 'AMR-400', kind: 'nominal', battery: 41, draw: 310, range: 5.1 },
  { id: 'r4', name: 'KIRRA-10', model: 'Forklift-A', kind: 'degraded', battery: 22, draw: 520, range: 2.0 },
  { id: 'r5', name: 'KIRRA-11', model: 'Atlas-X', kind: 'nominal', battery: 73, draw: 250, range: 9.0 },
  { id: 'r6', name: 'KIRRA-12', model: 'Spot-V2', kind: 'nominal', battery: 95, draw: 175, range: 12.2 },
  { id: 'r7', name: 'KIRRA-13', model: 'AMR-400', kind: 'lockedout', battery: 12, draw: 60, range: 0.0 },
  { id: 'r8', name: 'KIRRA-14', model: 'Forklift-A', kind: 'nominal', battery: 57, draw: 300, range: 6.6 },
]

function attestationFor(kind: string, name: string): TwinAttestation {
  if (kind === 'lockedout') return { nodeId: name, akDigest: 'ak:7c1a…9e', pcr16: 'sha256:0xa1…fe', lastVerified: '11:58:02', status: 'Untrusted', tone: 'crit' }
  if (kind === 'degraded') return { nodeId: name, akDigest: 'ak:33b8…04', pcr16: 'sha256:0x4c…21', lastVerified: '12:03:40', status: 'Trusted', tone: 'safe' }
  return { nodeId: name, akDigest: 'ak:a9f0…71', pcr16: 'sha256:0x9f…c1', lastVerified: '12:04:50', status: 'Trusted', tone: 'safe' }
}

export const twins: Twin[] = roster.map((r, i) => {
  const b = base[r.kind]!
  const commands = r.kind === 'lockedout' ? commandsLocked : r.kind === 'degraded' ? commandsDegraded : commandsNominal
  const path = trace(i * 1.3)
  return {
    id: r.id,
    name: r.name,
    model: r.model,
    posture: b.posture!,
    status: r.kind === 'lockedout' ? 'standby · HOLD' : 'online',
    uptime: r.kind === 'lockedout' ? '— halted —' : ['41d 09h', '12d 22h', '6d 04h', '2d 11h', '88d 01h', '33d 17h', '—', '19d 06h'][i],
    firmware: r.kind === 'nominal' ? 'v2.4.1' : 'v2.4.1',
    battery: r.battery,
    drawW: r.draw,
    rangeKm: r.range,
    subsystems: b.subsystems!,
    envelope: b.envelope!,
    attestation: attestationFor(r.kind, r.name),
    commands,
    path,
    ego: path[26],
    pose: detail[r.kind].pose,
    vitals: vitalsFor(r.kind, i * 1.3),
    thermals: detail[r.kind].thermals,
    actuators: detail[r.kind].actuators,
    missionPhases: detail[r.kind].missionPhases,
  }
})

export function twinById(id: string): Twin | undefined {
  return twins.find((t) => t.id === id)
}

// ── Global Ops Map (#1) ─────────────────────────────────────────────────
// Fleet sites worldwide, color-coded by aggregate site posture. `hub` is the
// control center all sites arc back to. Coords are in a 0..100 × 0..50 viewBox.
export const sites: { id: string; name: string; region: string; x: number; y: number; assets: number; tone: Tone; hub?: boolean }[] = [
  { id: 'sf', name: 'San Francisco', region: 'US-West · HQ', x: 16, y: 22, assets: 142, tone: 'warn', hub: true },
  { id: 'aus', name: 'Austin', region: 'US-Central', x: 26, y: 28, assets: 88, tone: 'safe' },
  { id: 'rot', name: 'Rotterdam', region: 'EU-West', x: 50, y: 17, assets: 64, tone: 'safe' },
  { id: 'sin', name: 'Singapore', region: 'APAC', x: 76, y: 34, assets: 51, tone: 'safe' },
  { id: 'tok', name: 'Tokyo', region: 'APAC-North', x: 86, y: 22, assets: 37, tone: 'safe' },
]

export interface SiteRow { id: string; name: string; region: string; assets: number; degraded: number; tone: Tone }

export const siteRows: SiteRow[] = [
  { id: 'sf', name: 'San Francisco', region: 'US-West · HQ', assets: 142, degraded: 2, tone: 'warn' },
  { id: 'aus', name: 'Austin', region: 'US-Central', assets: 88, degraded: 0, tone: 'safe' },
  { id: 'rot', name: 'Rotterdam', region: 'EU-West', assets: 64, degraded: 0, tone: 'safe' },
  { id: 'sin', name: 'Singapore', region: 'APAC', assets: 51, degraded: 0, tone: 'safe' },
  { id: 'tok', name: 'Tokyo', region: 'APAC-North', assets: 37, degraded: 0, tone: 'safe' },
]
