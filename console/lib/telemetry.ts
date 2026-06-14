import type { SeriesPoint, Tone } from './types'

function g(n: number, b: number, j: number, drift = 0): SeriesPoint[] {
  const o: SeriesPoint[] = []
  let v = b
  for (let i = 0; i < n; i++) {
    v += (Math.random() - 0.5) * j + drift
    v = Math.max(0, v)
    o.push({ t: `${String(i).padStart(2, '0')}:00`, v: Math.round(v * 10) / 10 })
  }
  return o
}

export interface Sensor { name: string; health: number; status: string; tone: Tone }
export interface ActuatorCmd { ts: string; channel: string; value: string; tone: Tone }
export interface PosPoint { x: number; y: number }
export type ActPoint = { t: string; steer: number; throttle: number }

export const velocity: SeriesPoint[] = g(30, 1.2, 0.5)
export const battery: SeriesPoint[] = g(30, 74, 0.6, -0.4)

export const actuator: ActPoint[] = Array.from({ length: 30 }).map((_, i) => ({
  t: `${String(i).padStart(2, '0')}:00`,
  steer: Math.round(Math.sin(i / 4) * 18),
  throttle: 30 + Math.round(Math.abs(Math.sin(i / 5)) * 40),
}))

export const sensors: Sensor[] = [
  { name: 'LiDAR (top)', health: 99, status: 'OK', tone: 'safe' },
  { name: 'Camera ×6', health: 97, status: 'OK', tone: 'safe' },
  { name: 'IMU', health: 100, status: 'OK', tone: 'safe' },
  { name: 'GNSS / RTK', health: 92, status: 'FIX', tone: 'safe' },
  { name: 'Radar (front)', health: 64, status: 'DEGRADED', tone: 'warn' },
  { name: 'Wheel odometry', health: 98, status: 'OK', tone: 'safe' },
]

export const actuatorLog: ActuatorCmd[] = [
  { ts: '12:04:51', channel: 'cmd_vel.linear', value: '1.20 m/s', tone: 'safe' },
  { ts: '12:04:51', channel: 'cmd_vel.angular', value: '0.08 rad/s', tone: 'safe' },
  { ts: '12:04:50', channel: 'steer', value: '+4.2°', tone: 'ice' },
  { ts: '12:04:50', channel: 'throttle', value: '38%', tone: 'ice' },
  { ts: '12:04:49', channel: 'cmd_vel.linear', value: 'CLAMP → 2.0 m/s', tone: 'warn' },
  { ts: '12:04:48', channel: 'cmd_vel.linear', value: '1.18 m/s', tone: 'safe' },
]

export const path: PosPoint[] = Array.from({ length: 40 }).map((_, i) => {
  const t = i / 39
  return { x: 8 + t * 84, y: 50 + Math.sin(t * Math.PI * 3) * 28 + (Math.random() - 0.5) * 2 }
})
export const ego: PosPoint = path[25]

// ── Environmental Awareness (#14) ──────────────────────────────────────
// Detected actors in the ego frame for the occupancy view. Coords are in the
// 0..100 occupancy viewBox; the ego sits at (50, 84) facing up.
export const actors: { x: number; y: number; kind: 'person' | 'vehicle' | 'static'; tone: Tone; label?: string }[] = [
  { x: 44, y: 50, kind: 'person', tone: 'warn', label: 'pedestrian · 9 m' },
  { x: 58, y: 60, kind: 'vehicle', tone: 'ice', label: 'AMR · 6 m' },
  { x: 36, y: 38, kind: 'static', tone: 'muted', label: 'pallet' },
  { x: 70, y: 44, kind: 'static', tone: 'crit', label: 'keep-out' },
  { x: 52, y: 30, kind: 'person', tone: 'safe', label: 'worker · 18 m' },
]

export interface AwarenessStat { label: string; value: string; tone: Tone }
export const awareness: AwarenessStat[] = [
  { label: 'Tracked actors', value: '5', tone: 'ice' },
  { label: 'Nearest pedestrian', value: '9.0 m', tone: 'warn' },
  { label: 'Drivable corridor', value: 'clear', tone: 'safe' },
  { label: 'Hazard zones', value: '1 active', tone: 'crit' },
]
