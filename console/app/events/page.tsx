'use client'

import { useMemo, useState } from 'react'
import { Panel, Pill, StatusDot } from '@/components/ui/primitives'
import { DemoBadge } from '@/components/ui/demo-badge'
import { useEventFeed } from '@/lib/api/hooks'
import type { Tone } from '@/lib/types'

type SevFilter = 'all' | 'crit' | 'warn' | 'safe'

export default function EventsPage() {
  const { rows, sources, source } = useEventFeed(60)
  const [src, setSrc] = useState<string | 'all'>('all')
  const [sev, setSev] = useState<SevFilter>('all')

  const filtered = useMemo(
    () => rows.filter((e) => (src === 'all' || e.source === src) && (sev === 'all' || e.tone === sev)),
    [rows, src, sev]
  )

  const counts = useMemo(() => ({
    crit: rows.filter((e) => e.tone === 'crit').length,
    warn: rows.filter((e) => e.tone === 'warn').length,
    safe: rows.filter((e) => e.tone === 'safe').length,
  }), [rows])

  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Event Stream</h1>
          <p className="font-mono text-[11px] text-faint">
            {source === 'live' ? 'live · GET /console/audit · tamper-evident ledger' : 'unified fail-closed event log · demo'}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <DemoBadge live={source === 'live'} />
          {source === 'live' ? <Pill tone="safe">live</Pill> : <Pill tone="ice">demo</Pill>}
          <Pill tone="crit">{counts.crit} critical</Pill>
          <Pill tone="warn">{counts.warn} warning</Pill>
          <Pill tone="safe">{counts.safe} nominal</Pill>
        </div>
      </div>

      <Panel
        title="Live Events"
        subtitle={`${filtered.length} of ${rows.length} events`}
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
            <Chip key={s} active={src === s} tone={srcTone(s)} onClick={() => setSrc(s)}>{s}</Chip>
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
                <span className={`w-24 shrink-0 truncate font-mono text-[10px] uppercase tracking-wider ${txt(srcTone(e.source))}`}>{e.source}</span>
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

// Tone for a source channel — known verifier sources mapped, others neutral.
function srcTone(s: string): Tone {
  const k = s.toLowerCase()
  if (k.includes('governor')) return 'crit'
  if (k.includes('fleet') || k.includes('attestation')) return 'warn'
  if (k.includes('telemetry') || k.includes('federation') || k.includes('fabric')) return 'ice'
  if (k.includes('compliance') || k.includes('audit')) return 'safe'
  return 'muted'
}

function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
function ring(t: Tone) { return t === 'safe' ? 'ring-safe/30' : t === 'warn' ? 'ring-warn/30' : t === 'crit' ? 'ring-crit/30' : t === 'ice' ? 'ring-ice/30' : 'ring-white/10' }
