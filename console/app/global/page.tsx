'use client'

import { useState } from 'react'
import { CloudRain, Network, Flame } from 'lucide-react'
import { Panel, Pill, Meter, StatusDot } from '@/components/ui/primitives'
import { GlobalMap } from '@/components/ui/global-map'
import { sites, weatherZones, geofences, crossSiteAlerts, regionRisk, totals } from '@/lib/global'
import type { Tone } from '@/lib/types'

export default function GlobalPage() {
  const [heatmap, setHeatmap] = useState(true)
  const [risk, setRisk] = useState(true)
  const [network, setNetwork] = useState(false)

  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Global Operations</h1>
          <p className="font-mono text-[11px] text-faint">strategic view · {totals.sites} sites · {totals.assets} assets · {totals.active} active</p>
        </div>
        <div className="flex items-center gap-2">
          <Pill tone="warn">2 regions elevated</Pill>
          <Pill tone="safe">federation in sync</Pill>
        </div>
      </div>

      {/* top-line metrics */}
      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        <Metric label="Sites online" value={`${totals.sites}`} sub="all regions" tone="safe" />
        <Metric label="Active assets" value={`${totals.active}`} sub={`of ${totals.assets}`} tone="ice" />
        <Metric label="Interventions · 24h" value={`${totals.interventions}`} sub="fleet-wide" tone="warn" />
        <Metric label="Regions at risk" value="2" sub="US-West · APAC" tone="warn" />
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel
          className="xl:col-span-2"
          title="Global Operations Map"
          subtitle="fleet distribution · overlays"
          action={
            <div className="flex items-center gap-1.5">
              <Toggle on={heatmap} tone="warn" onClick={() => setHeatmap((v) => !v)}><Flame className="h-3 w-3" /> Heatmap</Toggle>
              <Toggle on={risk} tone="ice" onClick={() => setRisk((v) => !v)}><CloudRain className="h-3 w-3" /> Risk</Toggle>
              <Toggle on={network} tone="safe" onClick={() => setNetwork((v) => !v)}><Network className="h-3 w-3" /> Network</Toggle>
            </div>
          }
        >
          <GlobalMap sites={sites} weather={weatherZones} geofences={geofences} showHeatmap={heatmap} showRisk={risk} showNetwork={network} height={380} />
          <div className="mt-3 flex flex-wrap items-center gap-4 border-t border-line pt-3 font-mono text-[10px] text-faint">
            <span className="flex items-center gap-1.5"><span className="h-2 w-2 rounded-full bg-safe" /> low risk</span>
            <span className="flex items-center gap-1.5"><span className="h-2 w-2 rounded-full bg-warn" /> elevated</span>
            <span className="flex items-center gap-1.5"><span className="h-2 w-2 rounded-full bg-ice/70" /> weather system</span>
            <span className="ml-auto">marker size ∝ fleet size · heatmap ∝ interventions</span>
          </div>
        </Panel>

        <Panel title="Cross-Site Alerts" subtitle="regional · last hour" dense>
          <ul>
            {crossSiteAlerts.map((a) => (
              <li key={a.id} className="flex items-start gap-3 border-b border-line px-4 py-3 last:border-0">
                <StatusDot tone={a.tone} pulse={a.tone === 'warn'} />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2 font-mono text-[10px] text-faint">
                    <span>{a.ts}</span><span className={txt(a.tone)}>{a.region}</span>
                  </div>
                  <p className="mt-0.5 text-[12px] text-muted">{a.message}</p>
                </div>
              </li>
            ))}
          </ul>
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Regional Risk" subtitle="weather · network · geofence" dense>
          <div className="overflow-x-auto">
            <table className="w-full min-w-[560px] text-left">
              <thead>
                <tr className="border-b border-line font-mono text-[10px] uppercase tracking-wider text-faint">
                  <th className="px-4 py-2 font-normal">Region</th>
                  <th className="px-4 py-2 font-normal">Weather</th>
                  <th className="px-4 py-2 font-normal">Network</th>
                  <th className="px-4 py-2 font-normal">Geofence</th>
                  <th className="px-4 py-2 font-normal">Note</th>
                </tr>
              </thead>
              <tbody className="font-mono text-[12px]">
                {regionRisk.map((r) => (
                  <tr key={r.region} className="border-b border-line last:border-0 hover:bg-white/[0.02]">
                    <td className="px-4 py-2.5 text-ink">{r.region}</td>
                    <td className="px-4 py-2.5"><Dot tone={r.weather} /></td>
                    <td className="px-4 py-2.5"><Dot tone={r.network} /></td>
                    <td className="px-4 py-2.5"><Dot tone={r.geofence} /></td>
                    <td className="px-4 py-2.5 text-muted">{r.note}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </Panel>

        <Panel title="Fleet Distribution" subtitle="assets by site" dense>
          <ul className="px-4 py-2">
            {sites.map((s) => (
              <li key={s.id} className="border-b border-line py-3 last:border-0">
                <div className="flex items-center justify-between gap-3">
                  <span className="flex items-center gap-2 text-[12px] text-ink"><StatusDot tone={s.tone} />{s.name}</span>
                  <span className="font-mono text-[11px] text-muted">{s.assets}</span>
                </div>
                <div className="mt-2"><Meter value={(s.assets / totals.assets) * 100} tone={s.tone} /></div>
                <div className="mt-1 font-mono text-[10px] text-faint">{s.weather} · net {s.network} · {s.interventions} interventions/24h</div>
              </li>
            ))}
          </ul>
        </Panel>
      </div>
    </div>
  )
}

function Metric({ label, value, sub, tone }: { label: string; value: string; sub: string; tone: Tone }) {
  return (
    <div className="rounded-xl border border-line bg-panel p-4 shadow-panel">
      <div className="font-mono text-[11px] uppercase tracking-wider text-faint">{label}</div>
      <div className={`mt-1.5 font-display text-[26px] font-semibold leading-none ${txt(tone)}`}>{value}</div>
      <div className="mt-1 font-mono text-[10px] text-faint">{sub}</div>
    </div>
  )
}

function Toggle({ children, on, tone, onClick }: { children: React.ReactNode; on: boolean; tone: Tone; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className={`flex items-center gap-1.5 rounded-full px-2.5 py-1 font-mono text-[10px] uppercase tracking-wider transition-colors ${on ? `${ring(tone)} ${txt(tone)} bg-white/[0.04] ring-1` : 'text-faint hover:text-muted'}`}
    >
      {children}
    </button>
  )
}

function Dot({ tone }: { tone: Tone }) {
  return <span className="flex items-center gap-1.5"><span className={`h-2 w-2 rounded-full ${dotBg(tone)}`} /><span className={txt(tone)}>{tone === 'safe' ? 'OK' : tone === 'warn' ? 'watch' : tone === 'crit' ? 'alert' : 'note'}</span></span>
}

function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
function dotBg(t: Tone) { return t === 'safe' ? 'bg-safe' : t === 'warn' ? 'bg-warn' : t === 'crit' ? 'bg-crit' : t === 'ice' ? 'bg-ice' : 'bg-muted' }
function ring(t: Tone) { return t === 'safe' ? 'ring-safe/30' : t === 'warn' ? 'ring-warn/30' : t === 'crit' ? 'ring-crit/30' : t === 'ice' ? 'ring-ice/30' : 'ring-white/10' }
