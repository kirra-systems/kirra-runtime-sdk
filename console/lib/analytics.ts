import { seededRand } from '@/lib/seeded'
const rand = seededRand(101)
import type { SeriesPoint, Tone } from './types'

// Analytics & Risk Forecasting — executive KPI wall, fleet-wide trends, and a
// forward-looking operational-risk projection with its driving factors.

function g(n: number, b: number, j: number, drift = 0): SeriesPoint[] {
  const o: SeriesPoint[] = []
  let v = b
  for (let i = 0; i < n; i++) {
    v += (rand() - 0.5) * j + drift
    v = Math.max(0, v)
    o.push({ t: `${String(i % 24).padStart(2, '0')}:00`, v: Math.round(v * 10) / 10 })
  }
  return o
}

export interface ExecKpi { label: string; value: string; unit?: string; delta: string; tone: Tone; spark: SeriesPoint[] }

export const kpiWall: ExecKpi[] = [
  { label: 'Fleet availability', value: '99.98', unit: '%', delta: '+0.02 · 30d', tone: 'safe', spark: g(24, 99, 0.3) },
  { label: 'Safety interventions', value: '3', unit: '24h', delta: '+1', tone: 'warn', spark: g(24, 2, 1) },
  { label: 'Mean verdict latency', value: '38', unit: 'µs', delta: '−4 µs', tone: 'safe', spark: g(24, 40, 4) },
  { label: 'Envelope excursions', value: '0', unit: '30d', delta: 'zero', tone: 'safe', spark: g(24, 0.2, 0.4) },
  { label: 'Missions completed', value: '1,284', delta: '+96 · 7d', tone: 'ice', spark: g(24, 50, 6) },
  { label: 'Audit-chain integrity', value: '100', unit: '%', delta: 'no breaks', tone: 'safe', spark: g(24, 100, 0.1) },
]

export const throughputTrend: SeriesPoint[] = g(24, 820, 90)
export const interventionTrend: SeriesPoint[] = g(24, 4, 2)

// Operational-risk forecast. `observed` is the realized composite risk index;
// `forecast` projects the next horizon; `upper` is the 90% confidence ceiling.
export type RiskRow = { t: string; observed: number | null; forecast: number; upper: number }

export const riskForecast: RiskRow[] = (() => {
  const rows: RiskRow[] = []
  let obs = 16
  for (let i = 0; i < 12; i++) {
    obs += (rand() - 0.45) * 3
    obs = Math.max(6, Math.min(40, obs))
    rows.push({ t: `${String((8 + i) % 24).padStart(2, '0')}:00`, observed: Math.round(obs), forecast: Math.round(obs), upper: Math.round(obs + 4) })
  }
  // forward horizon — observed drops out, forecast climbs (congestion + radar drift)
  let fc = rows[rows.length - 1].forecast
  for (let i = 0; i < 6; i++) {
    fc += 2.4 + rand()
    rows.push({ t: `${String((20 + i) % 24).padStart(2, '0')}:00`, observed: null, forecast: Math.round(fc), upper: Math.round(fc + 6 + i) })
  }
  return rows
})()

export interface RiskDriver { name: string; contribution: number; trend: 'rising' | 'flat' | 'falling'; tone: Tone }

export const riskDrivers: RiskDriver[] = [
  { name: 'Radar sensor degradation', contribution: 38, trend: 'rising', tone: 'warn' },
  { name: 'Aisle congestion (shift change)', contribution: 27, trend: 'rising', tone: 'warn' },
  { name: 'Battery depletion (KIRRA-13/10)', contribution: 18, trend: 'flat', tone: 'ice' },
  { name: 'DDS jitter', contribution: 11, trend: 'falling', tone: 'safe' },
  { name: 'Pedestrian proximity', contribution: 6, trend: 'flat', tone: 'safe' },
]

export interface Projection { window: string; band: string; score: number; tone: Tone; note: string }

export const projections: Projection[] = [
  { window: 'Next 1h', band: 'LOW', score: 19, tone: 'safe', note: 'within nominal operating envelope' },
  { window: 'Next 4h', band: 'MEDIUM', score: 34, tone: 'warn', note: 'radar drift + shift-change congestion' },
  { window: 'Next 12h', band: 'MEDIUM', score: 41, tone: 'warn', note: 'recommend radar service window before 22:00' },
]
