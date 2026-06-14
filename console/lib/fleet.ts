import type { Tone, Posture } from './types'
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
  }
})

export function twinById(id: string): Twin | undefined {
  return twins.find((t) => t.id === id)
}
