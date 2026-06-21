import type { Tone } from './types'

export interface Phase { name: string; pct: number; status: 'done' | 'active' | 'pending' }
export interface Waypoint { id: string; name: string; status: 'done' | 'active' | 'pending'; eta: string }
export interface Task { name: string; pct: number; tone: Tone }
export interface RiskFactor { name: string; level: string; tone: Tone }

export const mission = { id: 'MSN-4471', name: 'Yard Logistics Loop', asset: 'KIRRA-09', progress: 64, started: '10:42', eta: '13:15', operator: 'J. Looney' }

export const phases: Phase[] = [
  { name: 'Dispatch', pct: 100, status: 'done' },
  { name: 'Transit A', pct: 100, status: 'done' },
  { name: 'Pick', pct: 100, status: 'done' },
  { name: 'Transit B', pct: 60, status: 'active' },
  { name: 'Place', pct: 0, status: 'pending' },
  { name: 'Return', pct: 0, status: 'pending' },
]

export const waypoints: Waypoint[] = [
  { id: 'WP-01', name: 'Depot exit', status: 'done', eta: '—' },
  { id: 'WP-02', name: 'Aisle 4 junction', status: 'done', eta: '—' },
  { id: 'WP-03', name: 'Pick bay 12', status: 'done', eta: '—' },
  { id: 'WP-04', name: 'Cross-dock', status: 'active', eta: '2m 10s' },
  { id: 'WP-05', name: 'Place bay 7', status: 'pending', eta: '6m 40s' },
  { id: 'WP-06', name: 'Depot return', status: 'pending', eta: '12m 05s' },
]

export const tasks: Task[] = [
  { name: 'Payload secured', pct: 100, tone: 'safe' },
  { name: 'Route adherence', pct: 96, tone: 'safe' },
  { name: 'Clearance checks', pct: 88, tone: 'safe' },
  { name: 'Handoff confirmation', pct: 40, tone: 'warn' },
]

export const risk: { score: number; band: string; factors: RiskFactor[] } = {
  score: 18,
  band: 'LOW',
  factors: [
    { name: 'Pedestrian proximity', level: 'LOW', tone: 'safe' },
    { name: 'Path congestion', level: 'MEDIUM', tone: 'warn' },
    { name: 'Sensor degradation (radar)', level: 'MEDIUM', tone: 'warn' },
    { name: 'Comms stability', level: 'LOW', tone: 'safe' },
  ],
}

export const operatorInterventions: { ts: string; detail: string; tone: Tone }[] = [
  { ts: '11:58', detail: 'Manual hold — crossing clearance', tone: 'warn' },
  { ts: '11:12', detail: 'Re-route approved (aisle 4 blocked)', tone: 'ice' },
]

// ── Mission Gantt (Drop 6) ─────────────────────────────────────────────────
// Concurrent fleet missions across a shared time window. Each segment's
// start/end are fractions (0..1) of the window; `nowFrac` is the playhead.

export const ganttWindow = { start: '10:00', end: '14:00', label: '4h window', nowFrac: 0.62 }

export interface GanttSeg { phase: string; start: number; end: number; tone: Tone }
export interface GanttRow {
  id: string
  asset: string
  name: string
  status: 'active' | 'queued' | 'done' | 'held'
  tone: Tone
  segments: GanttSeg[]
}

export const gantt: GanttRow[] = [
  {
    id: 'MSN-4471', asset: 'KIRRA-09', name: 'Yard Logistics Loop', status: 'active', tone: 'ice',
    segments: [
      { phase: 'Dispatch', start: 0.10, end: 0.16, tone: 'safe' },
      { phase: 'Transit A', start: 0.16, end: 0.34, tone: 'safe' },
      { phase: 'Pick', start: 0.34, end: 0.46, tone: 'safe' },
      { phase: 'Transit B', start: 0.46, end: 0.66, tone: 'ice' },
      { phase: 'Place', start: 0.66, end: 0.80, tone: 'muted' },
      { phase: 'Return', start: 0.80, end: 0.94, tone: 'muted' },
    ],
  },
  {
    id: 'MSN-4468', asset: 'KIRRA-07', name: 'Perimeter Patrol B', status: 'active', tone: 'ice',
    segments: [
      { phase: 'Loop 1', start: 0.04, end: 0.30, tone: 'safe' },
      { phase: 'Loop 2', start: 0.30, end: 0.56, tone: 'safe' },
      { phase: 'Loop 3', start: 0.56, end: 0.72, tone: 'ice' },
      { phase: 'Charge', start: 0.72, end: 0.84, tone: 'muted' },
    ],
  },
  {
    id: 'MSN-4470', asset: 'KIRRA-12', name: 'Pallet Run 18', status: 'active', tone: 'ice',
    segments: [
      { phase: 'Stage', start: 0.20, end: 0.30, tone: 'safe' },
      { phase: 'Haul', start: 0.30, end: 0.60, tone: 'ice' },
      { phase: 'Drop', start: 0.60, end: 0.74, tone: 'muted' },
    ],
  },
  {
    id: 'MSN-4465', asset: 'KIRRA-10', name: 'Aisle Inspection', status: 'held', tone: 'warn',
    segments: [
      { phase: 'Survey', start: 0.08, end: 0.40, tone: 'safe' },
      { phase: 'HOLD (degraded)', start: 0.40, end: 0.62, tone: 'warn' },
    ],
  },
  {
    id: 'MSN-4461', asset: 'KIRRA-13', name: 'Yard Transit', status: 'held', tone: 'crit',
    segments: [
      { phase: 'Transit', start: 0.02, end: 0.50, tone: 'safe' },
      { phase: 'LOCKOUT', start: 0.50, end: 0.62, tone: 'crit' },
    ],
  },
  {
    id: 'MSN-4474', asset: 'KIRRA-14', name: 'Survey Sweep', status: 'queued', tone: 'muted',
    segments: [
      { phase: 'Queued', start: 0.70, end: 0.96, tone: 'muted' },
    ],
  },
]
