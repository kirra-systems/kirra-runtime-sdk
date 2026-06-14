import type { Tone } from './types'

// AI Decision Oversight — every model-proposed action is adjudicated by the
// fail-closed Governor. This is the explainability surface: what the model
// asked for, which checks ran, and why the verdict landed where it did.

export type Verdict = 'ALLOW' | 'CLAMP' | 'DENY'

export interface DecisionTally { label: string; value: number; tone: Tone; share: number }

export const tally: DecisionTally[] = [
  { label: 'Allowed', value: 18420, tone: 'safe', share: 99.04 },
  { label: 'Clamped', value: 142, tone: 'warn', share: 0.76 },
  { label: 'Denied', value: 37, tone: 'crit', share: 0.20 },
]

export interface TraceStep {
  id: string
  name: string
  detail: string
  outcome: 'pass' | 'fail' | 'info'
}

// A single adjudicated proposal, walked check-by-check through the pipeline.
export const traceSubject = {
  asset: 'KIRRA-13',
  model: 'autoware.planner · v4.2',
  actionType: 'drive_to_pose',
  proposed: '{ "linear": 3.4, "angular": 0.2, "frame": "base_link" }',
  verdict: 'CLAMP' as Verdict,
  reason: 'DEGRADED_DECEL_ENVELOPE',
  ts: '12:04:21.882',
  latencyUs: 38,
}

export const trace: TraceStep[] = [
  { id: 's1', name: 'Schema & action type', detail: 'drive_to_pose ∈ known action registry', outcome: 'pass' },
  { id: 's2', name: 'Posture gate', detail: 'fleet posture = Degraded → kinetic writes restricted', outcome: 'info' },
  { id: 's3', name: 'Kinematic envelope (SG1)', detail: 'proposed 3.4 m/s ≤ hard cap 22.4 m/s', outcome: 'pass' },
  { id: 's4', name: 'Decel-to-stop bound', detail: '3.4 m/s > current 2.0 m/s — speed increase in Degraded', outcome: 'fail' },
  { id: 's5', name: 'Clamp resolution', detail: 'clamped to 2.0 m/s on MRC decel trajectory', outcome: 'info' },
  { id: 's6', name: 'Audit commit', detail: 'verdict hash-chained → ledger #0x9f3a…c1', outcome: 'pass' },
]

export interface DecisionRow {
  id: string
  ts: string
  asset: string
  actionType: string
  verdict: Verdict
  reason: string
  tone: Tone
}

export const recent: DecisionRow[] = [
  { id: 'd1', ts: '12:04:21', asset: 'KIRRA-13', actionType: 'drive_to_pose', verdict: 'CLAMP', reason: 'DEGRADED_DECEL_ENVELOPE', tone: 'warn' },
  { id: 'd2', ts: '12:04:18', asset: 'KIRRA-08', actionType: 'cmd_vel', verdict: 'ALLOW', reason: 'NOMINAL_VALID_KINEMATICS', tone: 'safe' },
  { id: 'd3', ts: '12:04:09', asset: 'KIRRA-11', actionType: 'read_telemetry', verdict: 'ALLOW', reason: 'DEGRADED_READ_ONLY_PERMITTED', tone: 'safe' },
  { id: 'd4', ts: '12:03:55', asset: 'KIRRA-10', actionType: 'grasp_object', verdict: 'DENY', reason: 'DEGRADED_POSTURE_KINETIC_DENIED', tone: 'warn' },
  { id: 'd5', ts: '12:03:41', asset: 'KIRRA-09', actionType: 'drive_to_moon', verdict: 'DENY', reason: 'UNKNOWN_ACTION_TYPE', tone: 'crit' },
  { id: 'd6', ts: '12:03:30', asset: 'KIRRA-07', actionType: 'cmd_vel', verdict: 'ALLOW', reason: 'NOMINAL_VALID_KINEMATICS', tone: 'safe' },
  { id: 'd7', ts: '12:03:12', asset: 'KIRRA-13', actionType: 'cmd_vel', verdict: 'DENY', reason: 'KINEMATIC_ENVELOPE_BREACH', tone: 'crit' },
]

// Explainability factors — the weighted inputs the Governor surfaced for the
// adjudicated subject, so an operator can see *why* without reading model internals.
export interface Factor { id: string; label: string; weight: number; tone: Tone; note: string }

export const factors: Factor[] = [
  { id: 'f1', label: 'Fleet posture', weight: 92, tone: 'warn', note: 'Degraded — dominant constraint' },
  { id: 'f2', label: 'Sensor confidence floor', weight: 71, tone: 'warn', note: '0.41 < 0.60 floor on KIRRA-13' },
  { id: 'f3', label: 'Proposed Δspeed', weight: 58, tone: 'warn', note: '+1.4 m/s above current' },
  { id: 'f4', label: 'Envelope headroom', weight: 14, tone: 'safe', note: 'far from hard cap' },
  { id: 'f5', label: 'Human proximity', weight: 9, tone: 'safe', note: 'no actor in 9 m corridor' },
]
