import type { Tone } from '@/lib/types'

// Bird's-eye plot of the demo: a straight corridor along +X, an optional stopped
// obstacle, the ego start and goal, and each doer's proposed path. The colour of a
// path is KIRRA's verdict on it (green = admitted, red = rejected) — so you read the
// safety outcome straight off the geometry.

const STROKE: Record<Tone, string> = {
  safe: '#2fe6a6',
  warn: '#ffb020',
  crit: '#ff5468',
  ice: '#5cc6ff',
  muted: '#9aa6bd',
}

export interface DoerPath {
  label: string
  path: [number, number][]
  tone: Tone
}

export function TrajectoryPlot({
  paths,
  egoX,
  goalX,
  halfWidth,
  obstacleX,
  xMax = 45,
  height = 190,
}: {
  paths: DoerPath[]
  egoX: number
  goalX: number
  halfWidth: number
  obstacleX?: number
  xMax?: number
  height?: number
}) {
  const W = 100
  const H = 30
  const sx = (x: number) => (x / xMax) * W
  const sy = (y: number) => H / 2 - (y / 6) * (H / 2 - 2)
  const poly = (p: [number, number][]) => p.map(([x, y]) => `${sx(x).toFixed(2)},${sy(y).toFixed(2)}`).join(' ')

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ height }} className="w-full" role="img" aria-label="bird's-eye trajectory comparison">
      {/* corridor band */}
      <rect
        x={0}
        y={sy(halfWidth)}
        width={W}
        height={sy(-halfWidth) - sy(halfWidth)}
        fill="rgba(92,198,255,0.05)"
        stroke="rgba(92,198,255,0.22)"
        strokeWidth={0.2}
      />
      {/* lane centerline */}
      <line x1={0} y1={sy(0)} x2={W} y2={sy(0)} stroke="rgba(150,166,198,0.18)" strokeWidth={0.2} strokeDasharray="1 1" />

      {/* goal line */}
      <line x1={sx(goalX)} y1={sy(halfWidth)} x2={sx(goalX)} y2={sy(-halfWidth)} stroke="rgba(47,230,166,0.4)" strokeWidth={0.3} strokeDasharray="0.8 0.8" />
      <text x={sx(goalX)} y={sy(halfWidth) - 0.8} fill="#2fe6a6" fontSize={2} textAnchor="middle" fontFamily="monospace">goal</text>

      {/* obstacle */}
      {obstacleX !== undefined && (
        <>
          <rect x={sx(obstacleX) - 1.5} y={sy(0) - 1.5} width={3} height={3} rx={0.5} fill="rgba(255,84,104,0.9)" />
          <text x={sx(obstacleX)} y={sy(0) + 4.4} fill="#ff5468" fontSize={2} textAnchor="middle" fontFamily="monospace">car</text>
        </>
      )}

      {/* ego start */}
      <circle cx={sx(egoX)} cy={sy(0)} r={1} fill="#5cc6ff" />
      <text x={sx(egoX)} y={sy(0) - 2.2} fill="#5cc6ff" fontSize={2} textAnchor="middle" fontFamily="monospace">ego</text>

      {/* doer paths (coloured by KIRRA verdict) */}
      {paths.map((d) => (
        <polyline
          key={d.label}
          points={poly(d.path)}
          fill="none"
          stroke={STROKE[d.tone]}
          strokeWidth={0.8}
          strokeLinejoin="round"
          strokeLinecap="round"
        />
      ))}
    </svg>
  )
}
