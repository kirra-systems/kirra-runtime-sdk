import Link from 'next/link'
import { Panel, Pill, Meter, StatusDot } from '@/components/ui/primitives'
import { WorldMap } from '@/components/ui/world-map'
import { twins, sites, siteRows } from '@/lib/fleet'
import { postureTone } from '@/lib/mock'
import type { Tone } from '@/lib/types'

export default function FleetPage() {
  const online = twins.filter((t) => t.posture !== 'LockedOut').length
  const degraded = twins.filter((t) => t.posture === 'Degraded').length
  const locked = twins.filter((t) => t.posture === 'LockedOut').length

  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Fleet Operations</h1>
          <p className="font-mono text-[11px] text-faint">{twins.length} assets · select an asset for its digital twin</p>
        </div>
        <div className="flex items-center gap-2">
          <Pill tone="safe">{online} active</Pill>
          {degraded > 0 && <Pill tone="warn">{degraded} degraded</Pill>}
          {locked > 0 && <Pill tone="crit">{locked} locked out</Pill>}
        </div>
      </div>

      {/* ── Global Ops Map (#1) ── */}
      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Global Operations" subtitle={`${siteRows.length} sites · ${siteRows.reduce((a, s) => a + s.assets, 0)} assets worldwide`} action={<Pill tone="ice">all regions</Pill>}>
          <WorldMap sites={sites} height={300} />
        </Panel>
        <Panel title="Sites" subtitle="aggregate posture by region" dense>
          <ul>
            {siteRows.map((s) => (
              <li key={s.id} className="flex items-center gap-3 border-b border-line px-4 py-3 last:border-0">
                <StatusDot tone={s.tone} pulse={s.tone !== 'safe'} />
                <div className="min-w-0 flex-1">
                  <div className="truncate text-[13px] text-ink">{s.name}</div>
                  <div className="font-mono text-[10px] text-faint">{s.region}</div>
                </div>
                <div className="text-right font-mono text-[11px]">
                  <div className="text-ink">{s.assets}</div>
                  <div className={s.degraded > 0 ? 'text-warn' : 'text-faint'}>{s.degraded > 0 ? `${s.degraded} degraded` : 'all nominal'}</div>
                </div>
              </li>
            ))}
          </ul>
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
        {twins.map((t) => {
          const tone = postureTone(t.posture)
          return (
            <Link key={t.id} href={`/fleet/${t.id}`} className="group rounded-xl border border-line bg-panel p-4 shadow-panel transition-colors hover:border-line-strong hover:bg-elevated">
              <div className="flex items-start justify-between">
                <div>
                  <div className="font-display text-[15px] font-semibold text-ink">{t.name}</div>
                  <div className="mt-0.5 font-mono text-[11px] text-faint">{t.model}</div>
                </div>
                <StatusDot tone={tone} pulse={t.posture !== 'Nominal'} />
              </div>

              <div className="mt-4 flex items-center justify-between">
                <span className={`font-mono text-[11px] uppercase tracking-wider ${txt(tone)}`}>{t.posture}</span>
                <span className="font-mono text-[11px] text-muted">{t.status}</span>
              </div>

              <div className="mt-3">
                <div className="flex items-center justify-between font-mono text-[10px] text-faint">
                  <span>battery</span><span>{t.battery}%</span>
                </div>
                <div className="mt-1.5"><Meter value={t.battery} tone={t.battery < 25 ? 'crit' : t.battery < 50 ? 'warn' : 'safe'} /></div>
              </div>

              <div className="mt-4 grid grid-cols-2 gap-2 border-t border-line pt-3 font-mono text-[10px] text-faint">
                <div><div className="text-ink">{t.rangeKm.toFixed(1)} km</div>range</div>
                <div className="text-right"><div className="text-ink">{t.drawW} W</div>draw</div>
              </div>

              <div className="mt-3 flex items-center justify-between font-mono text-[10px] text-faint">
                <span>twin →</span>
                <span className={txt(t.attestation.tone)}>{t.attestation.status}</span>
              </div>
            </Link>
          )
        })}
      </div>

      <Panel title="Fleet Roster" subtitle="all assets · tabular" dense>
        <div className="overflow-x-auto">
          <table className="w-full min-w-[760px] text-left">
            <thead>
              <tr className="border-b border-line font-mono text-[10px] uppercase tracking-wider text-faint">
                <th className="px-4 py-2 font-normal">Asset</th>
                <th className="px-4 py-2 font-normal">Model</th>
                <th className="px-4 py-2 font-normal">Posture</th>
                <th className="px-4 py-2 font-normal">Battery</th>
                <th className="px-4 py-2 font-normal">Attestation</th>
                <th className="px-4 py-2 font-normal">Uptime</th>
              </tr>
            </thead>
            <tbody className="font-mono text-[12px]">
              {twins.map((t) => (
                <tr key={t.id} className="border-b border-line last:border-0 hover:bg-white/[0.02]">
                  <td className="px-4 py-2.5"><Link href={`/fleet/${t.id}`} className="text-ink hover:text-ice">{t.name}</Link></td>
                  <td className="px-4 py-2.5 text-muted">{t.model}</td>
                  <td className={`px-4 py-2.5 ${txt(postureTone(t.posture))}`}>{t.posture}</td>
                  <td className="px-4 py-2.5 text-muted">{t.battery}%</td>
                  <td className={`px-4 py-2.5 ${txt(t.attestation.tone)}`}>{t.attestation.status}</td>
                  <td className="px-4 py-2.5 text-faint">{t.uptime}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Panel>
    </div>
  )
}

function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
