export type Tone = 'safe' | 'warn' | 'crit' | 'ice' | 'muted'
export type Posture = 'Nominal' | 'Degraded' | 'LockedOut'

export interface SeriesPoint {
  t: string
  v: number
}

export interface KPI {
  label: string
  value: string | number
  unit?: string
  delta?: string
  tone: Tone
  spark?: SeriesPoint[]
}

export interface Robot {
  id: string
  name: string
  model: string
  posture: Posture
  status: 'online' | 'standby' | 'offline'
  battery: number
  mission: string
  latencyMs: number
}

export interface EventItem {
  id: string
  tone: Tone
  source: string
  ts: string
  message: string
}
