import type { SpatialFrame, ReplayActor } from '@/lib/incidents'

// Forensic spatial replay — a top-down reconstruction of the asset's path around
// an incident. The ego advances per scrubber frame; the traveled path brightens,
// the planned path stays faint, and the front radar cone recolors with sensor
// health. Pure SVG, driven by the page's frame index.
const C: Record<string, string> = { safe: '#2fe6a6', warn: '#ffb020', crit: '#ff5468', ice: '#5cc6ff', muted: '#9aa6bd' }

export function ReplayMap({
  frames, actors, hazard, index, height = 320,
}: {
  frames: SpatialFrame[]
  actors: ReplayActor[]
  hazard: { x: number; y: number; w: number; h: number; label: string }
  index: number
  height?: number
}) {
  const ego = frames[index].ego
  const planned = frames.map((f) => `${f.ego.x},${f.ego.y}`).join(' ')
  const traveled = frames.slice(0, index + 1).map((f) => `${f.ego.x},${f.ego.y}`).join(' ')
  const radar = C[frames[index].radar]
  const breached = index >= 6

  return (
    <svg viewBox="0 0 100 100" style={{ height }} className="w-full" role="img" aria-label="incident spatial replay">
      <defs>
        <pattern id="rm-grid" width="8" height="8" patternUnits="userSpaceOnUse">
          <path d="M8 0H0V8" fill="none" stroke="rgba(150,166,198,0.06)" strokeWidth="0.3" />
        </pattern>
      </defs>
      <rect width="100" height="100" fill="url(#rm-grid)" />

      {/* drivable corridor */}
      <rect x="6" y="50" width="88" height="18" rx="2" fill="rgba(47,230,166,0.03)" stroke="rgba(47,230,166,0.15)" strokeWidth="0.3" strokeDasharray="2 1.5" />

      {/* hazard keep-out zone */}
      <rect x={hazard.x} y={hazard.y} width={hazard.w} height={hazard.h} rx="2" fill={breached ? 'rgba(255,84,104,0.10)' : 'rgba(255,84,104,0.05)'} stroke={C.crit} strokeOpacity={breached ? 0.6 : 0.35} strokeWidth="0.4" strokeDasharray="1.5 1" />
      <text x={hazard.x + hazard.w / 2} y={hazard.y - 1.5} fill={C.crit} fontSize="2.6" fontFamily="monospace" textAnchor="middle" opacity="0.8">{hazard.label}</text>

      {/* planned vs traveled path */}
      <polyline points={planned} fill="none" stroke="rgba(150,166,198,0.25)" strokeWidth="0.5" strokeDasharray="1.5 1.5" />
      <polyline points={traveled} fill="none" stroke={C.ice} strokeWidth="0.9" strokeLinecap="round" strokeLinejoin="round" />
      {frames.slice(0, index + 1).map((f, k) => (
        <circle key={k} cx={f.ego.x} cy={f.ego.y} r="0.8" fill={C.ice} opacity={0.25 + (k / (index + 1)) * 0.5} />
      ))}

      {/* detected actors */}
      {actors.map((a) => (
        <g key={a.id}>
          <circle cx={a.x} cy={a.y} r="2.6" fill="none" stroke={C[a.tone]} strokeOpacity="0.35" strokeWidth="0.4" />
          {a.kind === 'vehicle'
            ? <rect x={a.x - 1.6} y={a.y - 1.6} width="3.2" height="3.2" rx="0.5" fill={C[a.tone]} />
            : <circle cx={a.x} cy={a.y} r="1.4" fill={C[a.tone]} />}
          <text x={a.x + 3.2} y={a.y + 1} fill={C[a.tone]} fontSize="2.4" fontFamily="monospace">{a.label}</text>
        </g>
      ))}

      {/* ego: lidar ring + radar cone + chassis */}
      <g transform={`rotate(${ego.heading} ${ego.x} ${ego.y})`}>
        <circle cx={ego.x} cy={ego.y} r="9" fill="none" stroke={C.ice} strokeOpacity="0.18" strokeWidth="0.3" strokeDasharray="1 2" />
        <path d={`M${ego.x},${ego.y} L${ego.x - 6},${ego.y - 15} A16,16 0 0,1 ${ego.x + 6},${ego.y - 15} Z`} fill={radar} fillOpacity="0.10" stroke={radar} strokeOpacity="0.35" strokeWidth="0.3" />
        <rect x={ego.x - 2.6} y={ego.y - 3.4} width="5.2" height="6.8" rx="1" fill="rgba(22,27,39,0.96)" stroke={breached ? C.crit : C.ice} strokeWidth="0.6" />
        <polygon points={`${ego.x},${ego.y - 5} ${ego.x - 1.6},${ego.y - 3.2} ${ego.x + 1.6},${ego.y - 3.2}`} fill={breached ? C.crit : C.ice} />
      </g>
      <text x={ego.x} y={ego.y + 7} fill="#9aa6bd" fontSize="2.4" fontFamily="monospace" textAnchor="middle">KIRRA-13</text>
    </svg>
  )
}
