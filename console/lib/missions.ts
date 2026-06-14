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
