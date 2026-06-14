import type { GlobalSite, WeatherZone, GeoZone } from '@/lib/global'

// Stylized world map with layered operational overlays. Abstract graticule —
// suggestion, not cartography. Layers (toggleable): risk (weather blobs +
// geofence zones), network links, and an activity/intervention heatmap.

const C: Record<string, string> = { safe: '#2fe6a6', warn: '#ffb020', crit: '#ff5468', ice: '#5cc6ff', muted: '#69728a' }
const TONES = ['safe', 'warn', 'crit', 'ice'] as const

const LAND = [
  'M8,18 Q14,12 22,15 Q28,17 26,24 Q20,30 13,27 Q6,24 8,18 Z',
  'M30,26 Q33,20 38,22 Q40,30 36,38 Q32,42 30,36 Q28,30 30,26 Z',
  'M44,14 Q54,9 64,13 Q70,16 66,22 Q56,26 48,22 Q43,19 44,14 Z',
  'M70,24 Q78,20 86,23 Q92,27 88,33 Q80,36 73,32 Q68,28 70,24 Z',
]

export function GlobalMap({
  sites, weather, geofences, showHeatmap, showRisk, showNetwork, height = 360,
}: {
  sites: GlobalSite[]
  weather: WeatherZone[]
  geofences: GeoZone[]
  showHeatmap: boolean
  showRisk: boolean
  showNetwork: boolean
  height?: number
}) {
  const hub = sites.find((s) => s.hub) ?? sites[0]
  const maxInt = Math.max(1, ...sites.map((s) => s.interventions))
  const markerR = (assets: number) => 1.3 + (assets / 142) * 2.1

  return (
    <svg viewBox="0 0 100 50" style={{ height }} className="w-full" role="img" aria-label="global operations map">
      <defs>
        {TONES.map((t) => (
          <radialGradient key={t} id={`glow-${t}`} cx="50%" cy="50%" r="50%">
            <stop offset="0%" stopColor={C[t]} stopOpacity="0.5" />
            <stop offset="65%" stopColor={C[t]} stopOpacity="0.12" />
            <stop offset="100%" stopColor={C[t]} stopOpacity="0" />
          </radialGradient>
        ))}
        <pattern id="gm-grat" width="8.33" height="8.33" patternUnits="userSpaceOnUse">
          <path d="M8.33 0H0V8.33" fill="none" stroke="rgba(92,198,255,0.07)" strokeWidth="0.25" />
        </pattern>
      </defs>

      <rect x="2" y="2" width="96" height="46" rx="3" fill="url(#gm-grat)" stroke="rgba(150,166,198,0.12)" strokeWidth="0.3" />
      {LAND.map((d, i) => (
        <path key={i} d={d} fill="rgba(154,166,189,0.07)" stroke="rgba(154,166,189,0.18)" strokeWidth="0.3" />
      ))}

      {/* risk overlay: weather systems + geofence zones */}
      {showRisk && weather.map((w) => (
        <g key={w.id}>
          <circle cx={w.x} cy={w.y} r={w.r} fill={`url(#glow-${w.tone})`} />
          <text x={w.x} y={w.y - w.r + 2} fill={C[w.tone]} fontSize="2.1" fontFamily="monospace" textAnchor="middle" opacity="0.8">{w.label}</text>
        </g>
      ))}
      {showRisk && geofences.map((g) => (
        <g key={g.id}>
          <rect x={g.x} y={g.y} width={g.w} height={g.h} rx="1.5" fill="none" stroke={C[g.tone]} strokeOpacity="0.5" strokeWidth="0.35" strokeDasharray="1.4 1" />
          <text x={g.x + g.w / 2} y={g.y + g.h + 2.4} fill={C[g.tone]} fontSize="1.9" fontFamily="monospace" textAnchor="middle" opacity="0.7">{g.label}</text>
        </g>
      ))}

      {/* network links back to the control hub */}
      {showNetwork && sites.filter((s) => !s.hub).map((s) => {
        const midX = (s.x + hub.x) / 2
        const midY = Math.min(s.y, hub.y) - 8
        return <path key={`net-${s.id}`} d={`M${s.x},${s.y} Q${midX},${midY} ${hub.x},${hub.y}`} fill="none" stroke={C[s.tone]} strokeOpacity="0.35" strokeWidth="0.4" strokeDasharray="1.5 1.5" />
      })}

      {/* activity / intervention heatmap */}
      {showHeatmap && sites.map((s) => {
        const intensity = s.interventions / maxInt
        const r = 5 + intensity * 11
        return <circle key={`heat-${s.id}`} cx={s.x} cy={s.y} r={r} fill={`url(#glow-${s.tone})`} opacity={0.55 + intensity * 0.35} />
      })}

      {/* site markers */}
      {sites.map((s) => (
        <g key={s.id}>
          <circle cx={s.x} cy={s.y} r={markerR(s.assets) + 1.4} fill="none" stroke={C[s.tone]} strokeOpacity="0.35" strokeWidth="0.4" />
          <circle cx={s.x} cy={s.y} r={markerR(s.assets)} fill={C[s.tone]} />
          <text x={s.x} y={s.y - markerR(s.assets) - 1.8} fill="#e9edf6" fontSize="2.4" fontFamily="monospace" textAnchor="middle">{s.name}</text>
          <text x={s.x} y={s.y + markerR(s.assets) + 3.4} fill="#69728a" fontSize="2" fontFamily="monospace" textAnchor="middle">{s.active}/{s.assets}</text>
        </g>
      ))}
    </svg>
  )
}
