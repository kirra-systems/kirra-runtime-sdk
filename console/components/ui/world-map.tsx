import type { Tone } from '@/lib/types'

// Global Ops Map — a stylized world view of fleet sites. Not a precise geographic
// projection; an abstract graticule with site markers and arcs back to the
// control hub, color-coded by site posture.

export interface Site { id: string; name: string; region: string; x: number; y: number; assets: number; tone: Tone; hub?: boolean }

const stroke: Record<string, string> = { safe: 'var(--c-safe)', warn: 'var(--c-warn)', crit: 'var(--c-crit)', ice: 'var(--c-ice)', muted: 'var(--c-faint)' }

export function WorldMap({ sites, height = 300 }: { sites: Site[]; height?: number }) {
  const hub = sites.find((s) => s.hub) ?? sites[0]
  // rough continent silhouettes (abstract — suggestion, not cartography)
  const land = [
    'M8,18 Q14,12 22,15 Q28,17 26,24 Q20,30 13,27 Q6,24 8,18 Z',
    'M30,26 Q33,20 38,22 Q40,30 36,38 Q32,42 30,36 Q28,30 30,26 Z',
    'M44,14 Q54,9 64,13 Q70,16 66,22 Q56,26 48,22 Q43,19 44,14 Z',
    'M70,24 Q78,20 86,23 Q92,27 88,33 Q80,36 73,32 Q68,28 70,24 Z',
  ]
  return (
    <svg viewBox="0 0 100 50" style={{ height }} className="w-full" role="img" aria-label="global operations map">
      <defs>
        <pattern id="wm-grat" width="8.33" height="8.33" patternUnits="userSpaceOnUse">
          <path d="M8.33 0H0V8.33" fill="none" stroke="rgba(92,198,255,0.07)" strokeWidth="0.25" />
        </pattern>
      </defs>
      <rect x="2" y="2" width="96" height="46" rx="3" fill="url(#wm-grat)" stroke="rgba(150,166,198,0.12)" strokeWidth="0.3" />

      {land.map((d, i) => (
        <path key={i} d={d} fill="rgba(154,166,189,0.07)" stroke="rgba(154,166,189,0.18)" strokeWidth="0.3" />
      ))}

      {/* arcs back to the control hub */}
      {sites.filter((s) => !s.hub).map((s) => {
        const midX = (s.x + hub.x) / 2
        const midY = Math.min(s.y, hub.y) - 8
        return <path key={`arc-${s.id}`} d={`M${s.x},${s.y} Q${midX},${midY} ${hub.x},${hub.y}`} fill="none" stroke={stroke[s.tone]} strokeOpacity="0.3" strokeWidth="0.35" strokeDasharray="1.5 1.5" />
      })}

      {/* site markers */}
      {sites.map((s) => (
        <g key={s.id}>
          <circle cx={s.x} cy={s.y} r={s.hub ? 2.4 : 1.8} fill="none" stroke={stroke[s.tone]} strokeOpacity="0.35" strokeWidth="0.4" />
          <circle cx={s.x} cy={s.y} r={s.hub ? 1.2 : 0.9} fill={stroke[s.tone]} />
          <text x={s.x} y={s.y - 3} fill="var(--c-bright)" fontSize="2.4" fontFamily="monospace" textAnchor="middle">{s.name}</text>
          <text x={s.x} y={s.y + 4.4} fill="var(--c-faint)" fontSize="2" fontFamily="monospace" textAnchor="middle">{s.assets} assets</text>
        </g>
      ))}
    </svg>
  )
}
