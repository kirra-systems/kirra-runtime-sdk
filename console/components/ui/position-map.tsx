import type { PosPoint } from '@/lib/telemetry'

export function PositionMap({ path, ego, height = 210 }: { path: PosPoint[]; ego: PosPoint; height?: number }) {
  const d = path.map((p, i) => `${i === 0 ? 'M' : 'L'} ${p.x.toFixed(1)} ${(100 - p.y).toFixed(1)}`).join(' ')
  return (
    <svg viewBox="0 0 100 100" preserveAspectRatio="none" style={{ height }} className="w-full rounded-lg" role="img" aria-label="position trace">
      <defs>
        <pattern id="pm-grid" width="8" height="8" patternUnits="userSpaceOnUse">
          <path d="M8 0H0V8" fill="none" stroke="rgba(150,166,198,0.08)" strokeWidth="0.3" />
        </pattern>
      </defs>
      <rect width="100" height="100" fill="url(#pm-grid)" />
      <path d={d} fill="none" stroke="var(--c-ice)" strokeWidth="0.7" strokeOpacity="0.55" strokeLinecap="round" strokeLinejoin="round" />
      <circle cx={ego.x} cy={100 - ego.y} r="3.6" fill="none" stroke="var(--c-safe)" strokeOpacity="0.4" strokeWidth="0.5" />
      <circle cx={ego.x} cy={100 - ego.y} r="1.6" fill="var(--c-safe)" />
    </svg>
  )
}
