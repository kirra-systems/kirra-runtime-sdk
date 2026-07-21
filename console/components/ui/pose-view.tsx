import type { TwinPose } from '@/lib/fleet'
import type { Tone } from '@/lib/types'

// Robot pose visualization — a top-down chassis on a perspective ground grid,
// rotated by yaw, with a heading arrow and compass ring. Pitch/roll are surfaced
// as readouts by the caller. Pure SVG (no three.js); reads as a 3D pose.
const COL: Record<string, string> = { safe: 'var(--c-safe)', warn: 'var(--c-warn)', crit: 'var(--c-crit)', ice: 'var(--c-ice)', muted: 'var(--c-faint)' }

export function PoseView({ pose, tone, height = 240 }: { pose: TwinPose; tone: Tone; height?: number }) {
  const c = COL[tone] ?? COL.ice
  const cx = 50, cy = 44
  return (
    <svg viewBox="0 0 100 84" style={{ height }} className="w-full" role="img" aria-label="robot pose">
      {/* perspective ground grid */}
      <g stroke="rgba(150,166,198,0.12)" strokeWidth="0.3" fill="none">
        {[0, 1, 2, 3, 4].map((i) => {
          const y = cy + 4 + i * 7
          const w = 16 + i * 9
          return <line key={`h${i}`} x1={cx - w} y1={y} x2={cx + w} y2={y} />
        })}
        {[-3, -2, -1, 0, 1, 2, 3].map((i) => (
          <line key={`v${i}`} x1={cx + i * 5.5} y1={cy + 4} x2={cx + i * 16} y2={cy + 32} />
        ))}
      </g>

      {/* floor shadow + compass ring */}
      <ellipse cx={cx} cy={cy + 12} rx="16" ry="4.5" fill={c} opacity="0.08" />
      <circle cx={cx} cy={cy} r="23" fill="none" stroke="rgba(150,166,198,0.16)" strokeWidth="0.3" strokeDasharray="0.5 3" />
      <text x={cx} y={cy - 25} fill="var(--c-faint)" fontSize="3" fontFamily="monospace" textAnchor="middle">N</text>

      {/* chassis rotated by yaw */}
      <g transform={`rotate(${pose.yaw} ${cx} ${cy})`}>
        <rect x={cx - 10} y={cy - 13} width="20" height="26" rx="3" fill="rgba(22,27,39,0.96)" stroke={c} strokeWidth="0.8" />
        <rect x={cx - 10} y={cy - 13} width="20" height="3" rx="1.5" fill={c} />
        <polygon points={`${cx},${cy - 19} ${cx - 3},${cy - 14} ${cx + 3},${cy - 14}`} fill={c} />
        {[[-12.5, -9], [10, -9], [-12.5, 3], [10, 3]].map(([dx, dy], k) => (
          <rect key={k} x={cx + dx} y={cy + dy} width="2.5" height="7" rx="1" fill={c} opacity="0.6" />
        ))}
        <circle cx={cx} cy={cy} r="2.4" fill={c} />
      </g>
    </svg>
  )
}
