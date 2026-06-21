'use client'

import { useMemo, useState } from 'react'
import { Download, Search } from 'lucide-react'
import { Panel, Pill, StatusDot } from '@/components/ui/primitives'
import { DemoBadge } from '@/components/ui/demo-badge'
import { MultiLine } from '@/components/charts/multiline'
import { signals, signalById, timeAxis, assets, ranges, anomalies, pearson } from '@/lib/explorer'
import type { Tone } from '@/lib/types'

const DEFAULT = ['velocity', 'dds_latency', 'wheel_slip']

export default function ExplorerPage() {
  const [asset, setAsset] = useState('KIRRA-09')
  const [range, setRange] = useState<(typeof ranges)[number]>('24h')
  const [sel, setSel] = useState<string[]>(DEFAULT)

  const chosen = useMemo(() => sel.map(signalById), [sel])

  const rows = useMemo(() => timeAxis.map((t, i) => {
    const r: { t: string; [k: string]: string | number } = { t }
    for (const s of chosen) r[s.id] = s.norm[i]
    return r
  }), [chosen])

  const series = chosen.map((s) => ({ key: s.id, label: `${s.label} (${s.unit})`, color: s.color }))

  // Pairwise Pearson correlation over raw values of the selected signals.
  const corr = useMemo(
    () => chosen.map((a) => chosen.map((b) => pearson(a.values, b.values))),
    [chosen]
  )

  function toggle(id: string) {
    setSel((p) => (p.includes(id) ? (p.length > 1 ? p.filter((x) => x !== id) : p) : [...p, id]))
  }

  function exportCsv() {
    const header = ['time', ...chosen.map((s) => `${s.id}_${s.unit}`)].join(',')
    const body = timeAxis.map((t, i) => [t, ...chosen.map((s) => s.values[i])].join(',')).join('\n')
    const blob = new Blob([`${header}\n${body}`], { type: 'text/csv' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = `kirra-telemetry-${asset}-${range}.csv`
    a.click()
    URL.revokeObjectURL(url)
  }

  const tail = timeAxis.map((_, i) => i).slice(-8)

  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Telemetry Explorer</h1>
          <p className="font-mono text-[11px] text-faint">data lake · time-aligned signals · correlation & anomaly detection</p>
        </div>
        <div className="flex items-center gap-2">
          <DemoBadge live={false} />
          <button onClick={exportCsv} className="flex items-center gap-2 rounded-lg border border-ice/40 bg-ice/10 px-4 py-1.5 font-mono text-[11px] uppercase tracking-wider text-ice hover:bg-ice/20">
            <Download className="h-3.5 w-3.5" /> Export CSV
          </button>
        </div>
      </div>

      {/* Query bar */}
      <Panel>
        <div className="flex flex-wrap items-center gap-4">
          <div className="flex items-center gap-2">
            <Search className="h-3.5 w-3.5 text-faint" />
            <span className="font-mono text-[10px] uppercase tracking-wider text-faint">asset</span>
            <div className="flex flex-wrap gap-1">
              {assets.map((a) => (
                <Chip key={a} active={asset === a} tone="ice" onClick={() => setAsset(a)}>{a}</Chip>
              ))}
            </div>
          </div>
          <div className="flex items-center gap-2">
            <span className="font-mono text-[10px] uppercase tracking-wider text-faint">range</span>
            {ranges.map((r) => (
              <Chip key={r} active={range === r} tone="ice" onClick={() => setRange(r)}>{r}</Chip>
            ))}
          </div>
        </div>
        <div className="mt-3 border-t border-line pt-3">
          <div className="mb-2 font-mono text-[10px] uppercase tracking-wider text-faint">signals · {chosen.length} selected</div>
          <div className="flex flex-wrap gap-1.5">
            {signals.map((s) => (
              <Chip key={s.id} active={sel.includes(s.id)} tone={s.tone} onClick={() => toggle(s.id)}>
                {s.label} <span className="opacity-60">{s.unit}</span>
              </Chip>
            ))}
          </div>
        </div>
      </Panel>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Correlation View" subtitle={`${asset} · ${range} · normalized 0–100% for overlay`} action={<Pill tone="ice">{chosen.length} signals</Pill>}>
          <MultiLine data={rows} series={series} height={280} />
        </Panel>

        <Panel title="Correlation Matrix" subtitle="Pearson r · raw values">
          <div className="overflow-x-auto">
            <table className="w-full text-left">
              <thead>
                <tr className="font-mono text-[9px] uppercase tracking-wider text-faint">
                  <th className="py-1 pr-2 font-normal" />
                  {chosen.map((s) => (
                    <th key={s.id} className="px-1 py-1 text-center font-normal" title={s.label}>{s.id.slice(0, 4)}</th>
                  ))}
                </tr>
              </thead>
              <tbody className="font-mono text-[11px]">
                {chosen.map((a, i) => (
                  <tr key={a.id}>
                    <td className="py-1 pr-2 text-faint" title={a.label}>{a.id.slice(0, 8)}</td>
                    {chosen.map((b, j) => {
                      const r = corr[i][j]
                      return (
                        <td key={b.id} className="px-1 py-1 text-center">
                          <span className="inline-block w-9 rounded px-1 py-0.5" style={{ background: corrBg(r), color: Math.abs(r) > 0.55 ? '#080a10' : '#9aa6bd' }}>
                            {r.toFixed(2)}
                          </span>
                        </td>
                      )
                    })}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <p className="mt-3 font-mono text-[10px] leading-relaxed text-faint">
            Strong negative correlation between LiDAR returns and wheel slip / DDS latency marks the perception dropout at 16:30.
          </p>
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Time-Aligned Samples" subtitle="raw values · most recent 8 epochs" dense>
          <div className="overflow-x-auto">
            <table className="w-full min-w-[560px] text-left">
              <thead>
                <tr className="border-b border-line font-mono text-[10px] uppercase tracking-wider text-faint">
                  <th className="px-4 py-2 font-normal">Time</th>
                  {chosen.map((s) => (
                    <th key={s.id} className="px-4 py-2 font-normal">{s.label}<span className="text-faint"> ({s.unit})</span></th>
                  ))}
                </tr>
              </thead>
              <tbody className="font-mono text-[12px]">
                {tail.map((i) => (
                  <tr key={i} className="border-b border-line last:border-0 hover:bg-white/[0.02]">
                    <td className="px-4 py-2 text-faint">{timeAxis[i]}</td>
                    {chosen.map((s) => (
                      <td key={s.id} className={`px-4 py-2 ${s.anomalies.includes(i) ? `${txt(s.tone)} font-semibold` : 'text-ink'}`}>{s.values[i]}</td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </Panel>

        <Panel title="Detected Anomalies" subtitle="z-score & dropout flags" dense>
          <ul>
            {anomalies.map((a, i) => (
              <li key={i} className="flex items-center gap-3 border-b border-line px-4 py-3 last:border-0">
                <StatusDot tone={a.tone} pulse={a.severity === 'high'} />
                <div className="min-w-0 flex-1">
                  <div className="truncate text-[12px] text-ink">{a.label}</div>
                  <div className="font-mono text-[10px] text-faint">{a.time} · {a.value} {a.unit}</div>
                </div>
                <span className={`font-mono text-[10px] uppercase tracking-wider ${txt(a.tone)}`}>{a.severity}</span>
              </li>
            ))}
          </ul>
        </Panel>
      </div>
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

// blue (negative) → graphite (zero) → green (positive), opacity by |r|.
function corrBg(r: number) {
  const a = Math.min(0.9, Math.abs(r) * 0.9 + 0.06)
  if (r >= 0) return `rgba(47,230,166,${a})`
  return `rgba(92,198,255,${a})`
}

function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
function ring(t: Tone) { return t === 'safe' ? 'ring-safe/30' : t === 'warn' ? 'ring-warn/30' : t === 'crit' ? 'ring-crit/30' : t === 'ice' ? 'ring-ice/30' : 'ring-white/10' }
