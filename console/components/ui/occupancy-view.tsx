import type { Tone } from '@/lib/types'

// Environmental Awareness — an ego-centric occupancy view. The asset sits at the
// bottom-center facing up; sensor coverage (360° LiDAR, front radar cone, camera
// fans) is drawn, with detected actors and a hazard keep-out zone overlaid.

export interface Actor { x: number; y: number; kind: 'person' | 'vehicle' | 'static'; tone: Tone; label?: string }

const fill: Record<string, string> = { safe: 'var(--c-safe)', warn: 'var(--c-warn)', crit: 'var(--c-crit)', ice: 'var(--c-ice)', muted: 'var(--c-faint)' }

export function OccupancyView({ actors, height = 320 }: { actors: Actor[]; height?: number }) {
  const ex = 50, ey = 84 // ego position
  return (
    <svg viewBox="0 0 100 100" style={{ height }} className="w-full" role="img" aria-label="environmental awareness">
      <defs>
        <radialGradient id="occ-lidar" cx="50%" cy="84%" r="60%">
          <stop offset="0%" stopColor="rgba(92,198,255,0.10)" />
          <stop offset="100%" stopColor="rgba(92,198,255,0)" />
        </radialGradient>
        <pattern id="occ-grid" width="10" height="10" patternUnits="userSpaceOnUse">
          <path d="M10 0H0V10" fill="none" stroke="rgba(150,166,198,0.05)" strokeWidth="0.3" />
        </pattern>
      </defs>
      <rect width="100" height="100" fill="url(#occ-grid)" />

      {/* 360 LiDAR coverage */}
      <circle cx={ex} cy={ey} r="60" fill="url(#occ-lidar)" />
      {[20, 38, 56].map((r) => (
        <circle key={r} cx={ex} cy={ey} r={r} fill="none" stroke="rgba(92,198,255,0.14)" strokeWidth="0.3" strokeDasharray="1 2" />
      ))}

      {/* front radar cone */}
      <path d={`M${ex},${ey} L${ex - 22},${ey - 70} A74,74 0 0,1 ${ex + 22},${ey - 70} Z`} fill="rgba(47,230,166,0.05)" stroke="rgba(47,230,166,0.22)" strokeWidth="0.3" />
      {/* camera fans */}
      <path d={`M${ex},${ey} L${ex - 30},${ey - 30} A42,42 0 0,1 ${ex - 6},${ey - 42} Z`} fill="rgba(154,166,189,0.04)" stroke="rgba(154,166,189,0.12)" strokeWidth="0.25" />
      <path d={`M${ex},${ey} L${ex + 6},${ey - 42} A42,42 0 0,1 ${ex + 30},${ey - 30} Z`} fill="rgba(154,166,189,0.04)" stroke="rgba(154,166,189,0.12)" strokeWidth="0.25" />

      {/* drivable corridor */}
      <path d={`M${ex - 9},${ey} L${ex - 6},20 L${ex + 6},20 L${ex + 9},${ey} Z`} fill="rgba(47,230,166,0.04)" stroke="rgba(47,230,166,0.2)" strokeWidth="0.3" strokeDasharray="2 1.5" />

      {/* hazard keep-out zone */}
      <rect x="62" y="30" width="22" height="20" rx="2" fill="rgba(255,84,104,0.07)" stroke="rgba(255,84,104,0.4)" strokeWidth="0.4" strokeDasharray="1.5 1" />
      <text x="73" y="41" fill="var(--c-crit)" fontSize="2.6" fontFamily="monospace" textAnchor="middle">HAZARD</text>

      {/* detected actors */}
      {actors.map((a, i) => (
        <g key={i}>
          <circle cx={a.x} cy={a.y} r="2.8" fill="none" stroke={fill[a.tone]} strokeOpacity="0.35" strokeWidth="0.4" />
          {a.kind === 'vehicle' ? (
            <rect x={a.x - 1.6} y={a.y - 1.6} width="3.2" height="3.2" rx="0.5" fill={fill[a.tone]} />
          ) : (
            <circle cx={a.x} cy={a.y} r="1.4" fill={fill[a.tone]} />
          )}
          {a.label && <text x={a.x + 3.4} y={a.y + 1} fill={fill[a.tone]} fontSize="2.4" fontFamily="monospace">{a.label}</text>}
        </g>
      ))}

      {/* ego asset */}
      <g>
        <polygon points={`${ex},${ey - 4} ${ex - 3},${ey + 3} ${ex + 3},${ey + 3}`} fill="var(--c-ice)" />
        <circle cx={ex} cy={ey} r="5.5" fill="none" stroke="var(--c-ice)" strokeOpacity="0.5" strokeWidth="0.4" />
        <text x={ex} y={ey + 9} fill="var(--c-faint)" fontSize="2.4" fontFamily="monospace" textAnchor="middle">ego · KIRRA-09</text>
      </g>
    </svg>
  )
}
