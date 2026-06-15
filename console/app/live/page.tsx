'use client'

import { Panel, Pill, StatusDot } from '@/components/ui/primitives'
import { useLiveFleet } from '@/lib/api/hooks'
import { postureTone, trustLabel, trustReason, trustTone, type FleetPostureState } from '@/lib/api/types'
import type { Tone } from '@/lib/types'

export default function LivePage() {
  const { fleet, events, source, error, updatedAt } = useLiveFleet(5000)
  const counts = fleet.reduce(
    (a, n) => { a[n.propagated_status] = (a[n.propagated_status] ?? 0) + 1; return a },
    {} as Record<FleetPostureState, number>
  )

  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Live Fleet</h1>
          <p className="font-mono text-[11px] text-faint">
            verifier · GET /fleet/posture · {source === 'live' ? `updated ${updatedAt ? new Date(updatedAt).toLocaleTimeString() : '—'}` : 'mock fallback'}
          </p>
        </div>
        <div className="flex items-center gap-2">
          {source === 'live'
            ? <Pill tone="safe">Live · connected</Pill>
            : <Pill tone="ice">Demo data</Pill>}
        </div>
      </div>

      {/* Data-source banner */}
      <div className={`rounded-xl border p-4 ${source === 'live' ? 'border-safe/30 bg-safe/[0.04]' : 'border-line bg-panel'}`}>
        {source === 'live' ? (
          <p className="text-[13px] text-muted">
            Connected to a live Kirra verifier. Posture is polled every 5 s; the event feed is derived from posture transitions.
          </p>
        ) : (
          <p className="text-[13px] text-muted">
            Running on bundled demo data. Set <code className="rounded bg-bg/60 px-1.5 py-0.5 font-mono text-[12px] text-ice">NEXT_PUBLIC_KIRRA_API_URL</code> to a verifier base URL to go live — these read endpoints are public (no token).
            {error && <span className="text-warn"> · last attempt: {error}</span>}
          </p>
        )}
      </div>

      <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
        <Metric label="Nodes" value={`${fleet.length}`} tone="ice" />
        <Metric label="Nominal" value={`${counts.Nominal ?? 0}`} tone="safe" />
        <Metric label="Degraded" value={`${counts.Degraded ?? 0}`} tone="warn" />
        <Metric label="Locked out" value={`${counts.LockedOut ?? 0}`} tone="crit" />
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Fleet Posture" subtitle="propagated posture · per node · gray/black DAG" dense>
          <div className="overflow-x-auto">
            <table className="w-full min-w-[640px] text-left">
              <thead>
                <tr className="border-b border-line font-mono text-[10px] uppercase tracking-wider text-faint">
                  <th className="px-4 py-2 font-normal">Node</th>
                  <th className="px-4 py-2 font-normal">Local trust</th>
                  <th className="px-4 py-2 font-normal">Propagated</th>
                  <th className="px-4 py-2 font-normal">Blocked by</th>
                </tr>
              </thead>
              <tbody className="font-mono text-[12px]">
                {fleet.map((n) => (
                  <tr key={n.node_id} className="border-b border-line last:border-0 hover:bg-white/[0.02]">
                    <td className="px-4 py-2.5 text-ink">{n.node_id}</td>
                    <td className={`px-4 py-2.5 ${txt(trustTone(n.local_status))}`} title={trustReason(n.local_status) ?? ''}>
                      {trustLabel(n.local_status)}
                    </td>
                    <td className="px-4 py-2.5">
                      <span className={`rounded px-1.5 py-0.5 text-[10px] font-semibold ${badge(postureTone(n.propagated_status))}`}>{n.propagated_status}</span>
                    </td>
                    <td className="px-4 py-2.5 text-faint">{n.blocked_by.length ? n.blocked_by.join(', ') : '—'}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </Panel>

        <Panel title="Posture Events" subtitle={source === 'live' ? 'transitions · live' : 'synthetic · demo'} dense>
          {events.length === 0 ? (
            <div className="px-4 py-10 text-center font-mono text-[12px] text-faint">awaiting posture transitions…</div>
          ) : (
            <ul>
              {events.map((e, idx) => {
                const tone = e.posture ? postureTone(e.posture.propagated_status) : 'muted'
                return (
                  <li key={`${e.node_id}-${e.emitted_at_ms}-${idx}`} className="flex items-start gap-3 border-b border-line px-4 py-3 last:border-0">
                    <StatusDot tone={tone} pulse={tone === 'crit'} />
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2 font-mono text-[10px] text-faint">
                        <span>{new Date(e.emitted_at_ms).toLocaleTimeString()}</span>
                        <span className={txt(tone)}>{e.event_type}</span>
                      </div>
                      <p className="mt-0.5 truncate font-mono text-[12px] text-ink">
                        {e.node_id ?? 'fleet'} {e.posture ? `→ ${e.posture.propagated_status}` : ''}
                      </p>
                    </div>
                  </li>
                )
              })}
            </ul>
          )}
        </Panel>
      </div>
    </div>
  )
}

function Metric({ label, value, tone }: { label: string; value: string; tone: Tone }) {
  return (
    <div className="rounded-xl border border-line bg-panel p-4 shadow-panel">
      <div className="font-mono text-[11px] uppercase tracking-wider text-faint">{label}</div>
      <div className={`mt-1.5 font-display text-[26px] font-semibold leading-none ${txt(tone)}`}>{value}</div>
    </div>
  )
}

function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
function badge(t: Tone) { return t === 'safe' ? 'bg-safe/15 text-safe' : t === 'warn' ? 'bg-warn/15 text-warn' : t === 'crit' ? 'bg-crit/15 text-crit' : 'bg-ice/15 text-ice' }
