import type { Tone } from '@/lib/types'

// Safety Envelope Visualizer — a 2D phase plot of the admitted command region.
// X = linear velocity (−max…+max), Y = steering / angular rate (−max…+max).
// The hard kinematic limit is the outer boundary; the Degraded decel envelope is
// the shrunken inner region; sample commands are plotted as ALLOW/CLAMP/DENY.

export interface EnvPoint { vx: number; vy: number; tone: Tone; label?: string }

const dotFill: Record<string, string> = { safe: 'var(--c-safe)', warn: 'var(--c-warn)', crit: 'var(--c-crit)', ice: 'var(--c-ice)', muted: 'var(--c-faint)' }

export function EnvelopePlot({ points, height = 300 }: { points: EnvPoint[]; height?: number }) {
  // map a -1..1 coord to the 4..96 viewBox range
  const mx = (v: number) => 50 + v * 46
  const my = (v: number) => 50 - v * 46
  return (
    <svg viewBox="0 0 100 100" style={{ height }} className="w-full" role="img" aria-label="operating envelope">
      <defs>
        <pattern id="env-grid" width="10" height="10" patternUnits="userSpaceOnUse">
          <path d="M10 0H0V10" fill="none" stroke="rgba(150,166,198,0.06)" strokeWidth="0.3" />
        </pattern>
      </defs>
      <rect x="4" y="4" width="92" height="92" fill="url(#env-grid)" />

      {/* hard kinematic limit */}
      <rect x="7" y="7" width="86" height="86" rx="14" fill="rgba(92,198,255,0.04)" stroke="rgba(92,198,255,0.45)" strokeWidth="0.6" strokeDasharray="2 1.5" />
      {/* nominal admitted region */}
      <rect x="12" y="14" width="76" height="72" rx="12" fill="rgba(47,230,166,0.05)" stroke="rgba(47,230,166,0.4)" strokeWidth="0.6" />
      {/* degraded decel envelope */}
      <rect x="34" y="30" width="32" height="40" rx="8" fill="rgba(255,176,32,0.06)" stroke="rgba(255,176,32,0.5)" strokeWidth="0.6" />

      {/* axes */}
      <line x1="50" y1="7" x2="50" y2="93" stroke="rgba(150,166,198,0.18)" strokeWidth="0.4" />
      <line x1="7" y1="50" x2="93" y2="50" stroke="rgba(150,166,198,0.18)" strokeWidth="0.4" />

      {/* sample commands */}
      {points.map((p, i) => (
        <g key={i}>
          <circle cx={mx(p.vx)} cy={my(p.vy)} r="2.6" fill="none" stroke={dotFill[p.tone]} strokeOpacity="0.4" strokeWidth="0.5" />
          <circle cx={mx(p.vx)} cy={my(p.vy)} r="1.3" fill={dotFill[p.tone]} />
          {p.label && (
            <text x={mx(p.vx) + 3.4} y={my(p.vy) + 1} fill={dotFill[p.tone]} fontSize="2.6" fontFamily="monospace">{p.label}</text>
          )}
        </g>
      ))}

      {/* axis captions */}
      <text x="94" y="48" fill="var(--c-faint)" fontSize="2.6" fontFamily="monospace" textAnchor="end">+v</text>
      <text x="6.5" y="48" fill="var(--c-faint)" fontSize="2.6" fontFamily="monospace">−v</text>
      <text x="51" y="10" fill="var(--c-faint)" fontSize="2.6" fontFamily="monospace">+ω</text>
      <text x="51" y="93" fill="var(--c-faint)" fontSize="2.6" fontFamily="monospace">−ω</text>
    </svg>
  )
}
