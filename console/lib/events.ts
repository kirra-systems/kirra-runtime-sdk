import type { EventItem, Tone } from './types'

// Event Stream — the unified fail-closed event log across every Kirra subsystem.
// Each row carries a source channel + severity tone for filtering.

export type EventSource = 'governor' | 'fleet' | 'telemetry' | 'compliance' | 'federation' | 'attestation'

export interface LogEvent extends EventItem {
  source: EventSource
  code?: string
}

export const sources: EventSource[] = ['governor', 'fleet', 'telemetry', 'compliance', 'federation', 'attestation']

export const log: LogEvent[] = [
  { id: 'e01', tone: 'crit', source: 'governor', ts: '12:04:21', code: 'KINEMATIC_ENVELOPE_BREACH', message: 'DENY — KIRRA-13 cmd_vel 999 m/s rejected at envelope check' },
  { id: 'e02', tone: 'crit', source: 'governor', ts: '12:02:40', code: 'CYCLE_DETECTED', message: 'LOCKEDOUT — cycle detected in dependency DAG; KIRRA-13 isolated' },
  { id: 'e03', tone: 'warn', source: 'fleet', ts: '12:03:58', code: 'POSTURE_DEGRADED', message: 'KIRRA-10 entered Degraded posture (sensor confidence 0.41 < 0.60 floor)' },
  { id: 'e04', tone: 'warn', source: 'governor', ts: '12:04:09', code: 'DEGRADED_DECEL_ENVELOPE', message: 'CLAMP — KIRRA-13 3.4 m/s → 2.0 m/s on MRC decel trajectory' },
  { id: 'e05', tone: 'safe', source: 'governor', ts: '12:03:30', code: 'NOMINAL_VALID_KINEMATICS', message: 'ALLOW — cmd_vel 1.2 m/s dispatched to KIRRA-09' },
  { id: 'e06', tone: 'ice', source: 'telemetry', ts: '12:03:12', message: 'DDS latency p99 = 14 ms (within FTTI budget 50 ms)' },
  { id: 'e07', tone: 'warn', source: 'telemetry', ts: '12:02:55', message: '/sensor/radar deadline miss-rate 4.1% — front radar degraded' },
  { id: 'e08', tone: 'crit', source: 'attestation', ts: '11:58:02', message: 'KIRRA-13 attestation revoked — liveliness lost, trust state Untrusted' },
  { id: 'e09', tone: 'safe', source: 'attestation', ts: '12:04:50', message: 'KIRRA-09 Ed25519 attestation verified — PCR16 match' },
  { id: 'e10', tone: 'safe', source: 'compliance', ts: '12:01:05', message: 'Audit chain verified · 184,220 links · no breaks' },
  { id: 'e11', tone: 'warn', source: 'federation', ts: '11:31:09', message: 'Federated report from peer-controller-west rejected — replay nonce already burned' },
  { id: 'e12', tone: 'safe', source: 'federation', ts: '11:40:51', message: 'Federated trust report accepted from peer-controller-west (gen 4471)' },
  { id: 'e13', tone: 'safe', source: 'governor', ts: '12:01:48', code: 'DEGRADED_READ_ONLY_PERMITTED', message: 'ALLOW — KIRRA-11 read_telemetry permitted in Degraded' },
  { id: 'e14', tone: 'warn', source: 'governor', ts: '11:58:03', code: 'DEGRADED_POSTURE_KINETIC_DENIED', message: 'DENY — KIRRA-10 kinetic write denied in Degraded posture' },
  { id: 'e15', tone: 'crit', source: 'governor', ts: '12:03:41', code: 'UNKNOWN_ACTION_TYPE', message: 'DENY — KIRRA-09 unknown action "drive_to_moon" rejected' },
  { id: 'e16', tone: 'safe', source: 'fleet', ts: '11:52:30', message: 'KIRRA-12 returned to Nominal — recovery streak 5/5 confirmed' },
  { id: 'e17', tone: 'ice', source: 'telemetry', ts: '11:50:14', message: 'Throughput 2.4 Gb/s · packet loss 0.01% · jitter 0.4 ms' },
  { id: 'e18', tone: 'safe', source: 'compliance', ts: '11:45:00', message: 'Signed firmware v2.4.1 provenance check passed on Governor partition' },
  { id: 'e19', tone: 'warn', source: 'attestation', ts: '11:43:22', message: 'KIRRA-10 PCR16 drift warning — re-attestation scheduled' },
  { id: 'e20', tone: 'safe', source: 'fleet', ts: '11:30:10', message: 'Fleet posture recalculated — gen 4470 — 6 Nominal / 1 Degraded / 1 LockedOut' },
]

export function sourceTone(s: EventSource): Tone {
  return s === 'governor' ? 'crit' : s === 'fleet' ? 'warn' : s === 'telemetry' ? 'ice' : s === 'compliance' ? 'safe' : s === 'federation' ? 'ice' : 'warn'
}
