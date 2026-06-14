import type { Tone } from './types'

export const hero: { score: string; metrics: { label: string; value: string; tone: Tone; meter?: number }[] } = {
  score: '99.998',
  metrics: [
    { label: 'Governor State', value: 'Active', tone: 'safe' },
    { label: 'E-Stop Readiness', value: 'Verified', tone: 'safe' },
    { label: 'FTTI Budget', value: '84% free', tone: 'safe', meter: 84 },
    { label: 'Audit Chain', value: 'Valid', tone: 'safe' },
    { label: 'Fleet Risk', value: 'Low', tone: 'safe' },
    { label: 'MRC Profile', value: 'Decel-Stop', tone: 'ice' },
  ],
}

export const decisions: { label: string; value: number; tone: Tone }[] = [
  { label: 'Allowed', value: 18442, tone: 'safe' },
  { label: 'Denied', value: 23, tone: 'crit' },
  { label: 'Overrides', value: 5, tone: 'warn' },
  { label: 'Lockouts', value: 1, tone: 'crit' },
]

export const topology: { stage: string; sub: string; health: number; tone: Tone; governor: boolean }[] = [
  { stage: 'Planner', sub: 'Autoware · 14 streams', health: 99, tone: 'safe', governor: false },
  { stage: 'Governor', sub: 'verdict · 0.4 ms', health: 100, tone: 'safe', governor: true },
  { stage: 'Runtime', sub: 'QNX · isolated', health: 98, tone: 'safe', governor: false },
  { stage: 'Actuators', sub: '142 endpoints', health: 99, tone: 'safe', governor: false },
]

export const exec: { label: string; value: string; pct?: number }[] = [
  { label: 'Availability', value: '99.98%', pct: 99.98 },
  { label: 'Safety Compliance', value: '100%', pct: 100 },
  { label: 'Mission Success', value: '99.4%', pct: 99.4 },
  { label: 'Interventions (24h)', value: '3' },
  { label: 'MTBF', value: '481 hrs' },
]

export const audit = { blocks: '184,220', integrity: 'Verified', breaks: 0, last: '2s ago', root: 'f92a1b3c4d5e6f70…9e4dca21' }

export const constraintChecks: { name: string; pass: string; fail: number; tone: Tone }[] = [
  { name: 'Kinematic envelope', pass: '18,402', fail: 18, tone: 'safe' },
  { name: 'Hazard zone', pass: '6,210', fail: 3, tone: 'safe' },
  { name: 'Human proximity', pass: '4,887', fail: 2, tone: 'warn' },
  { name: 'DDS deadline', pass: '∞', fail: 0, tone: 'safe' },
]

export interface CmdEvent { id: string; sev: 'CRITICAL' | 'WARNING' | 'INFO' | 'NOMINAL'; subsystem: string; asset: string; title: string; disposition: string; ts: string; tone: Tone }

export const feed: CmdEvent[] = [
  { id: '1', sev: 'CRITICAL', subsystem: 'Governor', asset: 'KIRRA-13', title: 'Kinematic envelope breach — cmd_vel 999 m/s', disposition: 'Action blocked', ts: '12:04:21', tone: 'crit' },
  { id: '2', sev: 'WARNING', subsystem: 'Fleet', asset: 'KIRRA-10', title: 'Degraded posture — sensor confidence 0.41', disposition: 'Decel-to-stop', ts: '12:03:58', tone: 'warn' },
  { id: '3', sev: 'NOMINAL', subsystem: 'Governor', asset: 'KIRRA-09', title: 'cmd_vel 1.2 m/s — within envelope', disposition: 'Dispatched', ts: '12:03:30', tone: 'safe' },
  { id: '4', sev: 'INFO', subsystem: 'Telemetry', asset: 'fleet', title: 'DDS latency p99 = 14 ms', disposition: 'Within FTTI', ts: '12:03:12', tone: 'ice' },
  { id: '5', sev: 'CRITICAL', subsystem: 'Governor', asset: 'KIRRA-13', title: 'Cycle detected in dependency DAG', disposition: 'Lockout · isolated', ts: '12:02:40', tone: 'crit' },
  { id: '6', sev: 'NOMINAL', subsystem: 'Compliance', asset: 'ledger', title: 'Audit chain verified — 184,220 links', disposition: 'No breaks', ts: '12:01:05', tone: 'safe' },
]

export const mapRobots: { id: string; x: number; y: number; tone: Tone }[] = [
  { id: '09', x: 20, y: 32, tone: 'safe' },
  { id: '07', x: 34, y: 64, tone: 'safe' },
  { id: '11', x: 48, y: 46, tone: 'safe' },
  { id: '10', x: 62, y: 26, tone: 'warn' },
  { id: '14', x: 16, y: 78, tone: 'safe' },
  { id: '13', x: 80, y: 70, tone: 'crit' },
]
