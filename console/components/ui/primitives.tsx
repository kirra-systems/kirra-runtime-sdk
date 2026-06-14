import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'
import type { Tone } from '@/lib/types'

const toneText: Record<Tone, string> = { safe: 'text-safe', warn: 'text-warn', crit: 'text-crit', ice: 'text-ice', muted: 'text-muted' }
const toneBg: Record<Tone, string> = { safe: 'bg-safe/10', warn: 'bg-warn/10', crit: 'bg-crit/10', ice: 'bg-ice/10', muted: 'bg-white/5' }
const toneRing: Record<Tone, string> = { safe: 'ring-safe/30', warn: 'ring-warn/30', crit: 'ring-crit/30', ice: 'ring-ice/30', muted: 'ring-white/10' }
const toneDot: Record<Tone, string> = { safe: 'bg-safe', warn: 'bg-warn', crit: 'bg-crit', ice: 'bg-ice', muted: 'bg-muted' }

export function Panel({ title, subtitle, action, children, className, dense }: { title?: string; subtitle?: string; action?: ReactNode; children: ReactNode; className?: string; dense?: boolean }) {
  return (
    <section className={cn('rounded-xl border border-line bg-panel shadow-panel', className)}>
      {(title || action) && (
        <header className="flex items-center justify-between gap-3 border-b border-line px-4 py-3">
          <div>
            {title && <h3 className="font-display text-[13px] font-semibold tracking-wide text-ink">{title}</h3>}
            {subtitle && <p className="mt-0.5 font-mono text-[11px] text-faint">{subtitle}</p>}
          </div>
          {action}
        </header>
      )}
      <div className={cn(dense ? 'p-0' : 'p-4')}>{children}</div>
    </section>
  )
}

export function StatusDot({ tone, pulse }: { tone: Tone; pulse?: boolean }) {
  return (
    <span className={cn('relative inline-flex h-2 w-2 rounded-full', toneDot[tone])}>
      {pulse && <span className={cn('absolute inset-0 animate-ping rounded-full opacity-60', toneDot[tone])} />}
    </span>
  )
}

export function Pill({ tone, children }: { tone: Tone; children: ReactNode }) {
  return (
    <span className={cn('inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 font-mono text-[10px] uppercase tracking-wider ring-1', toneText[tone], toneBg[tone], toneRing[tone])}>
      <StatusDot tone={tone} />
      {children}
    </span>
  )
}

export function Stat({ label, value, unit, delta, tone = 'ice', children }: { label: string; value: string | number; unit?: string; delta?: string; tone?: Tone; children?: ReactNode }) {
  return (
    <div className="rounded-xl border border-line bg-panel p-4 shadow-panel">
      <div className="flex items-center justify-between">
        <span className="font-mono text-[11px] uppercase tracking-wider text-faint">{label}</span>
        {delta && <span className={cn('font-mono text-[11px]', toneText[tone])}>{delta}</span>}
      </div>
      <div className="mt-2 flex items-baseline gap-1.5">
        <span className="font-display text-[28px] font-semibold leading-none text-ink">{value}</span>
        {unit && <span className="font-mono text-xs text-muted">{unit}</span>}
      </div>
      {children && <div className="mt-3">{children}</div>}
    </div>
  )
}

export function Meter({ value, tone = 'safe' }: { value: number; tone?: Tone }) {
  return (
    <div className="h-1.5 w-full overflow-hidden rounded-full bg-white/5">
      <div className={cn('h-full rounded-full', toneDot[tone])} style={{ width: `${Math.max(2, Math.min(100, value))}%` }} />
    </div>
  )
}

export function SectionLabel({ children, right }: { children: ReactNode; right?: ReactNode }) {
  return (
    <div className="mb-3 flex items-center justify-between">
      <h2 className="font-mono text-[11px] uppercase tracking-[0.2em] text-faint">{children}</h2>
      {right}
    </div>
  )
}
