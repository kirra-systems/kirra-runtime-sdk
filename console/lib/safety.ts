import type { Tone } from './types'

export interface Constraint { id: string; name: string; value: string; limit: string; util: number; tone: Tone; status: string }
export interface Violation { id: string; ts: string; rule: string; asset: string; action: string; tone: Tone }
export interface Intervention { id: string; ts: string; kind: 'CLAMP' | 'DENY' | 'MRC' | 'ALLOW'; detail: string; tone: Tone }

export const constraints: Constraint[] = [
  { id: 'c1', name: 'Speed envelope (SG1)', value: '1.2 m/s', limit: '≤ 22.4 m/s', util: 6, tone: 'safe', status: 'OK' },
  { id: 'c2', name: 'Lateral containment (SG2)', value: '0.78 m', limit: '≥ 0.40 m', util: 51, tone: 'safe', status: 'OK' },
  { id: 'c3', name: 'RSS safety distance (SG3)', value: '14.2 m', limit: '≥ 9.0 m', util: 63, tone: 'safe', status: 'OK' },
  { id: 'c4', name: 'Geofence boundary', value: 'inside', limit: 'corridor', util: 34, tone: 'safe', status: 'OK' },
  { id: 'c5', name: 'Comms heartbeat', value: '420 ms', limit: '≤ 2000 ms', util: 21, tone: 'safe', status: 'OK' },
  { id: 'c6', name: 'Sensor confidence floor', value: '0.41', limit: '≥ 0.60', util: 96, tone: 'warn', status: 'DEGRADED' },
]

export const violations: Violation[] = [
  { id: 'v1', ts: '12:04:21', rule: 'KINEMATIC_ENVELOPE_BREACH', asset: 'KIRRA-13', action: 'DENY → MRC stop', tone: 'crit' },
  { id: 'v2', ts: '11:58:03', rule: 'DEGRADED_POSTURE_KINETIC_DENIED', asset: 'KIRRA-10', action: 'DENY', tone: 'warn' },
  { id: 'v3', ts: '11:41:55', rule: 'UNKNOWN_ACTION_TYPE', asset: 'KIRRA-09', action: 'DENY', tone: 'warn' },
  { id: 'v4', ts: '10:22:10', rule: 'CYCLE_DETECTED', asset: 'fleet-dag', action: 'LOCKEDOUT', tone: 'crit' },
]

export const interventions: Intervention[] = [
  { id: 'i1', ts: '12:04:21', kind: 'DENY', detail: 'KIRRA-13 cmd_vel 999 m/s — envelope breach', tone: 'crit' },
  { id: 'i2', ts: '12:01:09', kind: 'CLAMP', detail: 'KIRRA-08 4.1 m/s → 2.0 m/s (MRC decel)', tone: 'warn' },
  { id: 'i3', ts: '11:58:03', kind: 'DENY', detail: 'KIRRA-10 kinetic write in Degraded', tone: 'warn' },
  { id: 'i4', ts: '11:30:44', kind: 'MRC', detail: 'KIRRA-13 controlled stop — human reset pending', tone: 'crit' },
  { id: 'i5', ts: '11:02:18', kind: 'ALLOW', detail: 'KIRRA-09 cmd_vel 1.2 m/s dispatched', tone: 'safe' },
]

export const verdictMix = { allow: 18420, clamp: 142, deny: 37 }

// ── Safety Envelope Visualizer (#3) ─────────────────────────────────────
// Sample commands plotted on the linear-velocity (vx) × angular-rate (vy) phase
// plane, normalized to −1…1. The Governor admits a command only inside the
// active envelope; these illustrate ALLOW / CLAMP / DENY outcomes.
export const envPoints: { vx: number; vy: number; tone: Tone; label?: string }[] = [
  { vx: 0.18, vy: 0.10, tone: 'safe', label: 'ALLOW 1.2' },
  { vx: 0.55, vy: -0.22, tone: 'safe', label: 'ALLOW 3.2' },
  { vx: 0.86, vy: 0.30, tone: 'warn', label: 'CLAMP' },
  { vx: 1.06, vy: 0.12, tone: 'crit', label: 'DENY 999' },
  { vx: -0.30, vy: 0.42, tone: 'warn', label: 'CLAMP ω' },
]

export interface EnvelopeBand { name: string; tone: Tone; note: string }
export const envelopeBands: EnvelopeBand[] = [
  { name: 'Hard kinematic limit', tone: 'ice', note: 'certified envelope · cap always wins' },
  { name: 'Nominal admitted region', tone: 'safe', note: 'all valid kinematics allowed' },
  { name: 'Degraded decel envelope', tone: 'warn', note: 'non-increasing speed · decel-to-stop' },
]
