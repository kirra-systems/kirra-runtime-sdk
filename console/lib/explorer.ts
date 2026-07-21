import type { Tone } from './types'

// Telemetry Explorer / Data Lake — a queryable, time-aligned signal store.
// Signals are deterministic (seeded RNG) so server-render and client hydration
// match exactly. Each signal carries raw values, a 0..100 normalized track for
// overlay/correlation, and flagged anomaly indices.

function mulberry32(seed: number) {
  return function () {
    seed |= 0
    seed = (seed + 0x6d2b79f5) | 0
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

const N = 48 // 30-min samples across 24h

export const timeAxis: string[] = Array.from({ length: N }, (_, i) => {
  const mins = i * 30
  return `${String(Math.floor(mins / 60)).padStart(2, '0')}:${String(mins % 60).padStart(2, '0')}`
})

export const assets = ['KIRRA-07', 'KIRRA-08', 'KIRRA-09', 'KIRRA-10', 'KIRRA-11', 'KIRRA-12', 'KIRRA-13', 'KIRRA-14']
export const ranges = ['6h', '12h', '24h'] as const

export interface ExplorerSignal {
  id: string
  label: string
  unit: string
  color: string
  tone: Tone
  values: number[]
  norm: number[]
  anomalies: number[] // indices flagged anomalous
}

function build(id: string, label: string, unit: string, color: string, tone: Tone, seed: number, shape: (i: number, r: () => number) => number, spikes: { at: number; to: number }[] = []): ExplorerSignal {
  const r = mulberry32(seed)
  const values: number[] = []
  for (let i = 0; i < N; i++) values.push(shape(i, r))
  for (const s of spikes) values[s.at] = s.to
  const min = Math.min(...values), max = Math.max(...values)
  const span = max - min || 1
  const norm = values.map((v) => Math.round(((v - min) / span) * 100))
  const anomalies = spikes.map((s) => s.at)
  return { id, label, unit, color, tone, values: values.map((v) => Math.round(v * 100) / 100), norm, anomalies }
}

export const signals: ExplorerSignal[] = [
  build('velocity', 'Velocity', 'm/s', 'var(--c-ice)', 'ice', 11, (i, r) => 1.2 + Math.sin(i / 5) * 0.6 + (r() - 0.5) * 0.4),
  build('accel', 'Acceleration', 'm/s²', 'var(--c-faint)', 'muted', 23, (i, r) => Math.cos(i / 5) * 0.5 + (r() - 0.5) * 0.3),
  build('localization', 'Localization conf.', '%', 'var(--c-safe)', 'safe', 31, (i, r) => 94 + (r() - 0.5) * 4, [{ at: 30, to: 61 }]),
  build('battery', 'Battery', '%', 'var(--c-safe)', 'safe', 7, (i, r) => 90 - i * 0.9 + (r() - 0.5) * 0.6),
  build('dds_latency', 'DDS latency p99', 'ms', 'var(--c-warn)', 'warn', 19, (i, r) => 12 + Math.sin(i / 7) * 3 + (r() - 0.5) * 2, [{ at: 33, to: 47 }]),
  build('lidar_points', 'LiDAR returns', 'k pts', 'var(--c-ice)', 'ice', 5, (i, r) => 28 + (r() - 0.5) * 2, [{ at: 33, to: 12 }]),
  build('wheel_slip', 'Wheel slip', '%', 'var(--c-crit)', 'crit', 13, (i, r) => 2 + (r() - 0.5) * 1.2, [{ at: 31, to: 17 }, { at: 32, to: 14 }]),
  build('steering', 'Steering', '°', 'var(--c-faint)', 'muted', 29, (i, r) => Math.sin(i / 4) * 16 + (r() - 0.5) * 3),
]

export function signalById(id: string): ExplorerSignal {
  return signals.find((s) => s.id === id)!
}

// Pearson correlation over the raw values of two signals.
export function pearson(a: number[], b: number[]): number {
  const n = a.length
  const ma = a.reduce((x, y) => x + y, 0) / n
  const mb = b.reduce((x, y) => x + y, 0) / n
  let num = 0, da = 0, db = 0
  for (let i = 0; i < n; i++) {
    const xa = a[i] - ma, xb = b[i] - mb
    num += xa * xb; da += xa * xa; db += xb * xb
  }
  const den = Math.sqrt(da * db) || 1
  return num / den
}

export interface Anomaly { signalId: string; label: string; time: string; value: number; unit: string; severity: 'high' | 'medium'; tone: Tone }

export const anomalies: Anomaly[] = signals.flatMap((s) =>
  s.anomalies.map((idx) => ({
    signalId: s.id,
    label: s.label,
    time: timeAxis[idx],
    value: s.values[idx],
    unit: s.unit,
    severity: (s.tone === 'crit' ? 'high' : 'medium') as 'high' | 'medium',
    tone: s.tone,
  }))
).sort((a, b) => a.time.localeCompare(b.time))
