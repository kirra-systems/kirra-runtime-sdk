'use client'

import { useEffect, useState } from 'react'
import { Play, Pause, SkipBack, RotateCcw } from 'lucide-react'
import { Panel, Pill, StatusDot, Meter } from '@/components/ui/primitives'
import { ReplayMap } from '@/components/ui/replay-map'
import { useIncidentHistory } from '@/lib/api/hooks'
import { replay, featured, rootCause, replaySpatial, replayActors, replayHazard } from '@/lib/incidents'
import { postureTone } from '@/lib/mock'
import type { Tone } from '@/lib/types'

const MAX_SPEED = 2.6

export default function IncidentsPage() {
  const [i, setI] = useState(6) // start at the trigger frame (t = 0)
  const [playing, setPlaying] = useState(false)
  const frame = replay[i]
  const spatial = replaySpatial[i]
  const history = useIncidentHistory(80)

  useEffect(() => {
    if (!playing) return
    if (i >= replay.length - 1) { setPlaying(false); return }
    const id = setTimeout(() => setI((p) => Math.min(replay.length - 1, p + 1)), 900)
    return () => clearTimeout(id)
  }, [playing, i])

  const restart = () => { setI(0); setPlaying(true) }

  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Incident Review & Replay</h1>
          <p className="font-mono text-[11px] text-faint">{featured.id} · {featured.asset} · {featured.title}</p>
        </div>
        <div className="flex items-center gap-2">
          <Pill tone="crit">{featured.reason}</Pill>
          <Pill tone="ice">{featured.status}</Pill>
        </div>
      </div>

      {/* Replay deck */}
      <Panel
        title="Replay"
        subtitle={`t = ${frame.t > 0 ? '+' : ''}${frame.t}s · ${frame.clock}`}
        action={
          <div className="flex items-center gap-1.5">
            <DeckBtn onClick={() => { setPlaying(false); setI(0) }} label="To start"><SkipBack className="h-3.5 w-3.5" /></DeckBtn>
            <DeckBtn onClick={() => setPlaying((p) => !p)} label={playing ? 'Pause' : 'Play'} primary>{playing ? <Pause className="h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5" />}</DeckBtn>
            <DeckBtn onClick={restart} label="Replay"><RotateCcw className="h-3.5 w-3.5" /></DeckBtn>
          </div>
        }
      >
        {/* Timeline strip */}
        <div className="mb-5">
          <div className="relative flex items-end gap-1">
            {replay.map((f, idx) => {
              const active = idx === i
              const h = 14 + (f.speed / MAX_SPEED) * 46
              return (
                <button
                  key={idx}
                  onClick={() => { setPlaying(false); setI(idx) }}
                  className="group relative flex flex-1 flex-col items-center justify-end"
                  style={{ height: 64 }}
                  aria-label={`frame ${f.clock}`}
                >
                  <span
                    className={`w-full rounded-sm transition-all ${barBg(f.tone)} ${active ? 'opacity-100' : 'opacity-40 group-hover:opacity-70'}`}
                    style={{ height: h }}
                  />
                  {f.t === 0 && <span className="absolute -top-1 h-1.5 w-1.5 rounded-full bg-crit" />}
                </button>
              )
            })}
          </div>
          <div className="mt-2 flex items-center justify-between font-mono text-[10px] text-faint">
            <span>{replay[0].clock} · lead-up</span>
            <span className="text-crit">▲ trigger</span>
            <span>{replay[replay.length - 1].clock} · safe stop</span>
          </div>
          <input
            type="range" min={0} max={replay.length - 1} value={i}
            onChange={(e) => { setPlaying(false); setI(Number(e.target.value)) }}
            className="mt-3 w-full accent-ice"
            aria-label="scrub timeline"
          />
        </div>

        {/* Synced state at frame i */}
        <div className="grid grid-cols-1 gap-4 md:grid-cols-3">
          <StateCard label="Posture">
            <div className="flex items-center gap-2">
              <StatusDot tone={postureTone(frame.posture)} pulse={frame.posture !== 'Nominal'} />
              <span className={`font-display text-2xl font-semibold ${txt(postureTone(frame.posture))}`}>{frame.posture}</span>
            </div>
          </StateCard>

          <StateCard label="Commanded speed">
            <div className="flex items-baseline gap-1.5">
              <span className="font-display text-2xl font-semibold text-ink">{frame.speed.toFixed(1)}</span>
              <span className="font-mono text-xs text-muted">m/s</span>
            </div>
            <div className="mt-2 h-1.5 w-full overflow-hidden rounded-full bg-white/5">
              <div className={`h-full rounded-full ${barBg(frame.tone)}`} style={{ width: `${(frame.speed / MAX_SPEED) * 100}%` }} />
            </div>
          </StateCard>

          <StateCard label="Governor verdict">
            <span className={`inline-block rounded px-2 py-1 font-mono text-[13px] font-semibold ${badge(frame.tone)}`}>{frame.verdict}</span>
          </StateCard>
        </div>

        <div className="mt-4 rounded-lg border border-line bg-bg/40 p-3">
          <div className="font-mono text-[10px] uppercase tracking-wider text-faint">Event at {frame.clock}</div>
          <p className={`mt-1.5 text-[13px] ${txt(frame.tone)}`}>{frame.event}</p>
        </div>
      </Panel>

      {/* ── Spatial replay + sensor playback (#8) ── */}
      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Spatial Replay" subtitle="position over time · ego advances with the scrubber" action={<Pill tone="ice">top-down</Pill>}>
          <ReplayMap frames={replaySpatial} actors={replayActors} hazard={replayHazard} index={i} height={320} />
          <div className="mt-3 flex flex-wrap items-center gap-4 border-t border-line pt-3 font-mono text-[10px] text-faint">
            <span className="flex items-center gap-1.5"><span className="h-2 w-2 rounded-full bg-ice" /> traveled path</span>
            <span className="flex items-center gap-1.5"><span className="h-2 w-px bg-muted" /> planned</span>
            <span className="flex items-center gap-1.5"><span className="h-2 w-2 rounded-sm bg-crit/60" /> keep-out zone</span>
            <span className="ml-auto">front cone = radar health · {spatial.confidence < 0.6 ? 'confidence below floor' : 'nominal'}</span>
          </div>
        </Panel>

        <Panel title="Sensor Playback" subtitle={`t = ${frame.t > 0 ? '+' : ''}${frame.t}s · ${frame.clock}`}>
          <div className="space-y-3">
            <SensorRow label="LiDAR (top)" tone={spatial.lidar} />
            <SensorRow label="Radar (front)" tone={spatial.radar} />
            <SensorRow label="Camera ×6" tone={spatial.camera} />
          </div>
          <div className="mt-4 border-t border-line pt-3">
            <div className="flex items-center justify-between">
              <span className="font-mono text-[10px] uppercase tracking-wider text-faint">Localization confidence</span>
              <span className={`font-mono text-[12px] ${confTone(spatial.confidence) === 'safe' ? 'text-safe' : confTone(spatial.confidence) === 'warn' ? 'text-warn' : 'text-crit'}`}>{spatial.confidence.toFixed(2)}</span>
            </div>
            <div className="mt-2"><Meter value={spatial.confidence * 100} tone={confTone(spatial.confidence)} /></div>
            <p className="mt-1.5 font-mono text-[10px] text-faint">floor ≥ 0.60 · {spatial.confidence < 0.6 ? 'breached → Degraded' : 'within floor'}</p>
          </div>
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Frame Log" subtitle="full second-by-second reconstruction" dense>
          <ul>
            {replay.map((f, idx) => (
              <li
                key={idx}
                className={`flex items-center gap-4 border-b border-line px-4 py-2.5 last:border-0 ${idx === i ? 'bg-white/[0.04]' : idx > i ? 'opacity-40' : ''}`}
              >
                <span className="w-10 shrink-0 font-mono text-[11px] text-faint">{f.t > 0 ? '+' : ''}{f.t}s</span>
                <span className="w-16 shrink-0 font-mono text-[11px] text-muted">{f.clock}</span>
                <span className={`w-12 shrink-0 rounded px-1.5 py-0.5 text-center font-mono text-[10px] font-semibold ${badge(f.tone)}`}>{f.verdict}</span>
                <span className="min-w-0 flex-1 truncate text-[12px] text-ink">{f.event}</span>
              </li>
            ))}
          </ul>
        </Panel>

        <Panel title="Root-Cause Analysis" subtitle="post-incident finding">
          <ol className="space-y-4">
            {rootCause.map((r, idx) => (
              <li key={r.id} className="relative flex gap-3 pl-1">
                {idx < rootCause.length - 1 && <span className="absolute left-[11px] top-5 h-full w-px bg-line" />}
                <span className={`mt-0.5 h-2.5 w-2.5 shrink-0 rounded-full ${barBg(r.tone)}`} />
                <div className="min-w-0">
                  <div className={`font-mono text-[10px] uppercase tracking-wider ${txt(r.tone)}`}>{r.label}</div>
                  <p className="mt-1 text-[12px] text-muted">{r.detail}</p>
                </div>
              </li>
            ))}
          </ol>
        </Panel>
      </div>

      <Panel
        title="Incident History"
        subtitle={history.source === 'live' ? 'derived from the audit ledger · GET /console/audit' : 'all logged fail-closed events'}
        action={history.source === 'live' ? <Pill tone="safe">live</Pill> : <Pill tone="ice">demo</Pill>}
        dense
      >
        <div className="overflow-x-auto">
          <table className="w-full min-w-[760px] text-left">
            <thead>
              <tr className="border-b border-line font-mono text-[10px] uppercase tracking-wider text-faint">
                <th className="px-4 py-2 font-normal">ID</th>
                <th className="px-4 py-2 font-normal">Timestamp</th>
                <th className="px-4 py-2 font-normal">Asset</th>
                <th className="px-4 py-2 font-normal">Incident</th>
                <th className="px-4 py-2 font-normal">Duration</th>
                <th className="px-4 py-2 font-normal">Status</th>
              </tr>
            </thead>
            <tbody className="font-mono text-[12px]">
              {history.rows.map((inc) => (
                <tr key={inc.id} className={`border-b border-line last:border-0 hover:bg-white/[0.02] ${inc.id === featured.id ? 'bg-white/[0.03]' : ''}`}>
                  <td className={`px-4 py-2.5 ${txt(inc.tone)}`}>{inc.id}</td>
                  <td className="px-4 py-2.5 text-faint">{inc.ts}</td>
                  <td className="px-4 py-2.5 text-ink">{inc.asset}</td>
                  <td className="px-4 py-2.5 text-muted">{inc.title}</td>
                  <td className="px-4 py-2.5 text-muted">{inc.duration}</td>
                  <td className="px-4 py-2.5 text-ink">{inc.status}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Panel>
    </div>
  )
}

function DeckBtn({ children, onClick, label, primary }: { children: React.ReactNode; onClick: () => void; label: string; primary?: boolean }) {
  return (
    <button
      onClick={onClick}
      aria-label={label}
      className={`flex h-8 w-8 items-center justify-center rounded-lg border transition-colors ${primary ? 'border-ice/40 bg-ice/10 text-ice hover:bg-ice/20' : 'border-line text-faint hover:bg-white/[0.04] hover:text-ink'}`}
    >
      {children}
    </button>
  )
}

function StateCard({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="rounded-lg border border-line bg-panel p-4">
      <div className="font-mono text-[10px] uppercase tracking-wider text-faint">{label}</div>
      <div className="mt-2">{children}</div>
    </div>
  )
}

function SensorRow({ label, tone }: { label: string; tone: Tone }) {
  return (
    <div className="flex items-center gap-3 rounded-lg border border-line bg-bg/40 px-3 py-2">
      <StatusDot tone={tone} pulse={tone === 'crit'} />
      <span className="flex-1 text-[12px] text-ink">{label}</span>
      <span className={`font-mono text-[10px] uppercase tracking-wider ${txt(tone)}`}>{tone === 'safe' ? 'OK' : tone === 'warn' ? 'intermittent' : 'fault'}</span>
    </div>
  )
}

function confTone(v: number): Tone { return v < 0.3 ? 'crit' : v < 0.6 ? 'warn' : 'safe' }

function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
function badge(t: Tone) { return t === 'safe' ? 'bg-safe/15 text-safe' : t === 'warn' ? 'bg-warn/15 text-warn' : t === 'crit' ? 'bg-crit/15 text-crit' : 'bg-ice/15 text-ice' }
function barBg(t: Tone) { return t === 'safe' ? 'bg-safe' : t === 'warn' ? 'bg-warn' : t === 'crit' ? 'bg-crit' : t === 'ice' ? 'bg-ice' : 'bg-muted' }
