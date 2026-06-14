import type { Tone, Posture } from './types'

// Incident Review & Replay — post-event reconstruction. Each incident carries a
// frame-by-frame replay timeline so an operator can scrub the seconds around a
// fail-closed event and watch posture, speed, and the verdict stream evolve.

export interface Incident {
  id: string
  ts: string
  title: string
  asset: string
  reason: string
  posture: Posture
  status: 'Resolved' | 'Open' | 'Human-reset'
  tone: Tone
  durationS: number
}

export const incidents: Incident[] = [
  { id: 'INC-2041', ts: '2026-06-14 12:02:40', title: 'Envelope breach → DAG lockout', asset: 'KIRRA-13', reason: 'KINEMATIC_ENVELOPE_BREACH', posture: 'LockedOut', status: 'Human-reset', tone: 'crit', durationS: 12 },
  { id: 'INC-2038', ts: '2026-06-14 11:58:03', title: 'Sensor confidence floor breach', asset: 'KIRRA-10', reason: 'DEGRADED_POSTURE_KINETIC_DENIED', posture: 'Degraded', status: 'Resolved', tone: 'warn', durationS: 41 },
  { id: 'INC-2030', ts: '2026-06-14 10:22:10', title: 'Dependency cycle detected', asset: 'fleet-dag', reason: 'CYCLE_DETECTED', posture: 'LockedOut', status: 'Resolved', tone: 'crit', durationS: 8 },
  { id: 'INC-2024', ts: '2026-06-13 19:41:55', title: 'Unknown action type rejected', asset: 'KIRRA-09', reason: 'UNKNOWN_ACTION_TYPE', posture: 'Nominal', status: 'Resolved', tone: 'warn', durationS: 1 },
]

export interface ReplayFrame {
  t: number // seconds relative to incident trigger (negative = lead-up)
  clock: string
  speed: number // m/s
  posture: Posture
  verdict: 'ALLOW' | 'CLAMP' | 'DENY' | '—'
  event: string
  tone: Tone
}

// The featured replay: INC-2041, the KIRRA-13 envelope breach → lockout.
export const featured = incidents[0]

export const replay: ReplayFrame[] = [
  { t: -6, clock: '12:02:34', speed: 1.2, posture: 'Nominal', verdict: 'ALLOW', event: 'cmd_vel 1.2 m/s dispatched — nominal cruise', tone: 'safe' },
  { t: -5, clock: '12:02:35', speed: 1.4, posture: 'Nominal', verdict: 'ALLOW', event: 'cmd_vel 1.4 m/s — within envelope', tone: 'safe' },
  { t: -4, clock: '12:02:36', speed: 1.6, posture: 'Nominal', verdict: 'ALLOW', event: 'radar return intermittent — confidence 0.71', tone: 'safe' },
  { t: -3, clock: '12:02:37', speed: 1.9, posture: 'Nominal', verdict: 'ALLOW', event: 'planner requests acceleration to 2.4 m/s', tone: 'safe' },
  { t: -2, clock: '12:02:38', speed: 2.2, posture: 'Degraded', verdict: 'CLAMP', event: 'confidence 0.54 < floor → posture Degraded; speed clamped 2.0 m/s', tone: 'warn' },
  { t: -1, clock: '12:02:39', speed: 2.0, posture: 'Degraded', verdict: 'CLAMP', event: 'planner re-requests 3.1 m/s → decel-bound denied', tone: 'warn' },
  { t: 0, clock: '12:02:40', speed: 2.0, posture: 'Degraded', verdict: 'DENY', event: 'malformed cmd_vel 999 m/s → KINEMATIC_ENVELOPE_BREACH', tone: 'crit' },
  { t: 1, clock: '12:02:41', speed: 1.1, posture: 'LockedOut', verdict: 'DENY', event: 'cycle detected in dependency DAG → KIRRA-13 isolated', tone: 'crit' },
  { t: 2, clock: '12:02:42', speed: 0.3, posture: 'LockedOut', verdict: 'DENY', event: 'MRC controlled stop engaged — brakes A/B', tone: 'crit' },
  { t: 3, clock: '12:02:43', speed: 0.0, posture: 'LockedOut', verdict: '—', event: 'asset at full stop — HOLD; human reset required', tone: 'crit' },
]

// ── Spatial replay (#8) ────────────────────────────────────────────────
// Per-frame ego pose for forensic position-over-time playback, plus detected
// actors, the hazard keep-out zone, and frame-by-frame sensor health. Indices
// align 1:1 with `replay` above. Coords are top-down in a 0..100 viewBox.

export interface ReplayActor { id: string; x: number; y: number; kind: 'person' | 'vehicle' | 'static'; label: string; tone: Tone }
export const replayActors: ReplayActor[] = [
  { id: 'worker', x: 74, y: 72, kind: 'person', label: 'worker · 9 m', tone: 'warn' },
  { id: 'amr', x: 64, y: 50, kind: 'vehicle', label: 'AMR-08', tone: 'ice' },
  { id: 'pallet', x: 44, y: 42, kind: 'static', label: 'pallet', tone: 'muted' },
]

export const replayHazard = { x: 56, y: 46, w: 18, h: 24, label: 'keep-out' }

export interface SpatialFrame {
  ego: { x: number; y: number; heading: number }
  lidar: Tone
  radar: Tone
  camera: Tone
  confidence: number
}

export const replaySpatial: SpatialFrame[] = [
  { ego: { x: 12, y: 60, heading: 88 }, lidar: 'safe', radar: 'safe', camera: 'safe', confidence: 0.78 },
  { ego: { x: 18, y: 59, heading: 89 }, lidar: 'safe', radar: 'safe', camera: 'safe', confidence: 0.74 },
  { ego: { x: 25, y: 59, heading: 90 }, lidar: 'safe', radar: 'warn', camera: 'safe', confidence: 0.71 },
  { ego: { x: 33, y: 58, heading: 90 }, lidar: 'safe', radar: 'warn', camera: 'safe', confidence: 0.66 },
  { ego: { x: 42, y: 58, heading: 91 }, lidar: 'safe', radar: 'warn', camera: 'safe', confidence: 0.54 },
  { ego: { x: 49, y: 57, heading: 91 }, lidar: 'safe', radar: 'warn', camera: 'safe', confidence: 0.50 },
  { ego: { x: 55, y: 57, heading: 92 }, lidar: 'safe', radar: 'crit', camera: 'safe', confidence: 0.41 },
  { ego: { x: 60, y: 56, heading: 92 }, lidar: 'safe', radar: 'warn', camera: 'safe', confidence: 0.30 },
  { ego: { x: 62.5, y: 56, heading: 92 }, lidar: 'safe', radar: 'warn', camera: 'safe', confidence: 0.18 },
  { ego: { x: 63.5, y: 56, heading: 92 }, lidar: 'warn', radar: 'warn', camera: 'safe', confidence: 0.11 },
]

export interface RootCause { id: string; label: string; detail: string; tone: Tone }

export const rootCause: RootCause[] = [
  { id: 'rc1', label: 'Trigger', detail: 'Upstream planner emitted cmd_vel linear = 999 m/s', tone: 'crit' },
  { id: 'rc2', label: 'Contributing', detail: 'Front radar degraded → sensor confidence fell below 0.60 floor', tone: 'warn' },
  { id: 'rc3', label: 'Governor action', detail: 'DENY at envelope check; DAG lockout isolated the node; MRC stop', tone: 'safe' },
  { id: 'rc4', label: 'Outcome', detail: 'No motion past breach. Decel-to-stop in 3.0 s. Zero envelope excursion.', tone: 'safe' },
]
