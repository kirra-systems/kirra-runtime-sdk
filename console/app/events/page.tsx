'use client'

import { useMemo, useState } from 'react'
import { Panel, Pill, StatusDot } from '@/components/ui/primitives'
import { log, sources, sourceTone, type EventSource } from '@/lib/events'
import type { Tone } from '@/lib/types'

type SevFilter = 'all' | 'crit' | 'warn' | 'safe'

export default function EventsPage() {
  const [src, setSrc] = useState<EventSource | 'all'>('all')
  const [sev, setSev] = useState<SevFilter>('all')

  const filtered = useMemo(
    () => log.filter((e) => (src === 'all' || e.source === src) && (sev === 'all' || e.tone === sev)),
    [src, sev]
  )

  const counts = useMemo(() => ({
    crit: log.filter((e) => e.tone === 'crit').length,
    warn: log.filter((e) => e.tone === 'warn').length,
    safe: log.filter((e) => e.tone === 'safe').length,
  }), [])

  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Event Stream</h1>
          <p className="font-mono text-[11px] text-faint">unified fail-closed event log · all subsystems</p>
        </div>
        <div className="flex items-center gap-2">
          <Pill tone="crit">{counts.crit} critical</Pill>
          <Pill tone="warn">{counts.warn} warning</Pill>
          <Pill tone="safe">{counts.safe} nominal</Pill>
        </div>
      </div>

      <Panel
        title="Live Events"
        subtitle={`${filtered.length} of ${log.length} events`}
        action={
          <div className="flex items-center gap-1">
            {(['all', 'crit', 'warn', 'safe'] as SevFilter[]).map((s) => (
              <Chip key={s} active={sev === s} tone={s === 'all' ? 'muted' : (s as Tone)} onClick={() => setSev(s)}>{s === 'all' ? 'All' : s.toUpperCase()}</Chip>
            ))}
          </div>
        }
        dense
      >
        {/* source filter row */}
        <div className="flex flex-wrap items-center gap-1.5 border-b border-line px-4 py-3">
          <Chip active={src === 'all'} tone="muted" onClick={() => setSrc('all')}>all sources</Chip>
          {sources.map((s) => (
            <Chip key={s} active={src === s} tone={sourceTone(s)} onClick={() => setSrc(s)}>{s}</Chip>
          ))}
        </div>

        {filtered.length === 0 ? (
          <div className="px-4 py-12 text-center font-mono text-[12px] text-faint">no events match the current filter</div>
        ) : (
          <ul>
            {filtered.map((e) => (
              <li key={e.id} className="flex items-start gap-4 border-b border-line px-4 py-3 last:border-0 hover:bg-white/[0.02]">
                <StatusDot tone={e.tone} pulse={e.tone === 'crit'} />
                <span className="w-16 shrink-0 font-mono text-[11px] text-faint">{e.ts}</span>
                <span className={`w-24 shrink-0 font-mono text-[10px] uppercase tracking-wider ${txt(sourceTone(e.source))}`}>{e.source}</span>
                <div className="min-w-0 flex-1">
                  <p className={`text-[12px] ${e.tone === 'crit' ? 'text-ink' : 'text-muted'}`}>{e.message}</p>
                  {e.code && <span className={`mt-1 inline-block font-mono text-[10px] ${txt(e.tone)}`}>{e.code}</span>}
                </div>
              </li>
            ))}
          </ul>
        )}
      </Panel>
    </div>
  )
}

function Chip({ children, active, tone, onClick }: { children: React.ReactNode; active: boolean; tone: Tone; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className={`rounded-full px-2.5 py-1 font-mono text-[10px] uppercase tracking-wider transition-colors ${active ? `${ring(tone)} ${txt(tone)} bg-white/[0.04] ring-1` : 'text-faint hover:text-muted'}`}
    >
      {children}
    </button>
  )
}

function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
function ring(t: Tone) { return t === 'safe' ? 'ring-safe/30' : t === 'warn' ? 'ring-warn/30' : t === 'crit' ? 'ring-crit/30' : t === 'ice' ? 'ring-ice/30' : 'ring-white/10' }
