import Link from 'next/link'
import { notFound } from 'next/navigation'
import { ChevronLeft } from 'lucide-react'
import { Panel, Pill, Meter, StatusDot } from '@/components/ui/primitives'
import { PositionMap } from '@/components/ui/position-map'
import { PoseView } from '@/components/ui/pose-view'
import { PostureTrend } from '@/components/ui/posture-trend'
import { Spark } from '@/components/charts/charts'
import { twinById, twins } from '@/lib/fleet'
import { postureTone } from '@/lib/mock'
import type { Tone, SeriesPoint } from '@/lib/types'

export function generateStaticParams() {
  return twins.map((t) => ({ id: t.id }))
}

export default async function TwinPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params
  const t = twinById(id)
  if (!t) notFound()

  const tone = postureTone(t.posture)

  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div className="flex items-center gap-3">
          <Link href="/fleet" className="flex h-8 w-8 items-center justify-center rounded-lg border border-line text-faint hover:bg-white/[0.04] hover:text-ink">
            <ChevronLeft className="h-4 w-4" />
          </Link>
          <div>
            <h1 className="font-display text-xl font-semibold text-ink">{t.name} <span className="font-mono text-[13px] font-normal text-faint">Digital Twin</span></h1>
            <p className="font-mono text-[11px] text-faint">{t.model} · firmware {t.firmware} · uptime {t.uptime}</p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <Pill tone={tone}>{t.posture}</Pill>
          <Pill tone={t.attestation.tone}>AK {t.attestation.status}</Pill>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
        <Metric label="Battery" value={`${t.battery}%`} tone={t.battery < 25 ? 'crit' : t.battery < 50 ? 'warn' : 'safe'} meter={t.battery} />
        <Metric label="Power draw" value={`${t.drawW} W`} tone="ice" />
        <Metric label="Est. range" value={`${t.rangeKm.toFixed(1)} km`} tone="ice" />
        <Metric label="Status" value={t.status} tone={tone} />
      </div>

      {/* Pose + live telemetry */}
      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel title="Pose & Attitude" subtitle="orientation · IMU">
          <PoseView pose={t.pose} tone={tone} height={228} />
          <div className="mt-2 grid grid-cols-4 gap-2 border-t border-line pt-3 text-center font-mono text-[10px] text-faint">
            <div><div className="text-sm text-ink">{t.pose.yaw}°</div>yaw</div>
            <div><div className="text-sm text-ink">{t.pose.pitch}°</div>pitch</div>
            <div><div className="text-sm text-ink">{t.pose.roll}°</div>roll</div>
            <div><div className="text-sm text-ice">{t.pose.heading.split(' · ')[0]}</div>head</div>
          </div>
        </Panel>

        <Panel className="xl:col-span-2" title="Live Telemetry" subtitle="velocity · acceleration · localization confidence" action={<Pill tone="ice">50 Hz</Pill>}>
          <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
            <Vital label="Velocity" unit="m/s" data={t.vitals.velocity} color="ice" />
            <Vital label="Acceleration" unit="m/s²" data={t.vitals.accel} color="safe" />
            <Vital label="Localization conf." unit="%" data={t.vitals.localization} color={confColor(t.vitals.localization)} />
          </div>
        </Panel>
      </div>

      {/* ── Live posture-history trend ── */}
      <PostureTrend nodeId={t.name} />

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Kinematic Envelope" subtitle="proposed vs certified hard limits">
          <ul className="space-y-4">
            {t.envelope.map((e) => (
              <li key={e.axis}>
                <div className="flex items-center justify-between gap-3">
                  <span className="text-[13px] text-ink">{e.axis}</span>
                  <span className="font-mono text-[11px] text-muted">{e.current} <span className="text-faint">/ {e.limit}</span></span>
                </div>
                <div className="mt-2 flex items-center gap-3">
                  <div className="flex-1"><Meter value={e.util} tone={e.tone} /></div>
                  <span className={`w-10 text-right font-mono text-[10px] ${txt(e.tone)}`}>{e.util}%</span>
                </div>
              </li>
            ))}
          </ul>
        </Panel>

        <Panel title="Subsystem Health" dense>
          <ul>
            {t.subsystems.map((s) => (
              <li key={s.name} className="flex items-center gap-3 border-b border-line px-4 py-2.5 last:border-0">
                <StatusDot tone={s.tone} pulse={s.tone === 'crit'} />
                <span className="flex-1 truncate text-[12px] text-ink">{s.name}</span>
                <span className="font-mono text-[11px] text-muted">{s.health}%</span>
                <span className={`w-20 text-right font-mono text-[10px] uppercase tracking-wider ${txt(s.tone)}`}>{s.status}</span>
              </li>
            ))}
          </ul>
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Position Trace" subtitle="last 40 fixes · ego marker live">
          <PositionMap path={t.path} ego={t.ego} height={240} />
        </Panel>

        <div className="space-y-6">
          <Panel title="Hardware Attestation" subtitle="per-node Ed25519 proof">
            <div className="space-y-3">
              <KV k="Node id" v={t.attestation.nodeId} />
              <KV k="AK digest" v={t.attestation.akDigest} />
              <KV k="PCR16" v={t.attestation.pcr16} />
              <KV k="Last verified" v={t.attestation.lastVerified} />
              <div className="flex items-center justify-between border-t border-line pt-3">
                <span className="font-mono text-[11px] uppercase tracking-wider text-faint">Trust state</span>
                <span className={`font-mono text-[12px] ${txt(t.attestation.tone)}`}>{t.attestation.status}</span>
              </div>
            </div>
          </Panel>
        </div>
      </div>

      {/* Thermal + actuator load + mission timeline */}
      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel title="Thermal" subtitle="component temperatures" dense>
          <ul>
            {t.thermals.map((th) => (
              <li key={th.name} className="flex items-center gap-3 border-b border-line px-4 py-3 last:border-0">
                <StatusDot tone={th.tone} pulse={th.tone === 'crit'} />
                <span className="flex-1 truncate text-[12px] text-ink">{th.name}</span>
                <div className="w-24"><Meter value={Math.min(100, (th.tempC / 90) * 100)} tone={th.tone} /></div>
                <span className={`w-12 text-right font-mono text-[12px] ${txt(th.tone)}`}>{th.tempC}°C</span>
              </li>
            ))}
          </ul>
        </Panel>

        <Panel title="Actuator Load" subtitle="per-channel duty cycle">
          <ul className="space-y-4">
            {t.actuators.map((a) => (
              <li key={a.name}>
                <div className="flex items-center justify-between gap-3">
                  <span className="text-[12px] text-ink">{a.name}</span>
                  <span className={`font-mono text-[11px] ${txt(a.tone)}`}>{a.loadPct}%</span>
                </div>
                <div className="mt-2"><Meter value={a.loadPct} tone={a.tone} /></div>
              </li>
            ))}
          </ul>
        </Panel>

        <Panel title="Mission Timeline" subtitle="current assignment" dense>
          <ol className="px-4 py-3">
            {t.missionPhases.map((p, idx) => {
              const pt: Tone = p.status === 'done' ? 'safe' : p.status === 'active' ? (/lockout|hold/i.test(p.name) ? 'crit' : 'ice') : 'muted'
              return (
                <li key={p.name} className="relative flex gap-3 pb-5 pl-1 last:pb-0">
                  {idx < t.missionPhases.length - 1 && <span className="absolute left-[5px] top-4 h-full w-px bg-line" />}
                  <span className={`mt-1 h-2.5 w-2.5 shrink-0 rounded-full ${dotBg(pt)}`} />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center justify-between gap-2">
                      <span className="text-[12px] text-ink">{p.name}</span>
                      <span className={`font-mono text-[10px] uppercase tracking-wider ${txt(pt)}`}>{p.status}</span>
                    </div>
                    {p.status === 'active' && p.pct > 0 && <div className="mt-2"><Meter value={p.pct} tone={pt} /></div>}
                  </div>
                </li>
              )
            })}
          </ol>
        </Panel>
      </div>

      <Panel title="Command Stream" subtitle="recent actuator commands · governor verdict" dense>
        <div className="overflow-x-auto">
          <table className="w-full min-w-[640px] text-left">
            <thead>
              <tr className="border-b border-line font-mono text-[10px] uppercase tracking-wider text-faint">
                <th className="px-4 py-2 font-normal">Time</th>
                <th className="px-4 py-2 font-normal">Channel</th>
                <th className="px-4 py-2 font-normal">Value</th>
                <th className="px-4 py-2 font-normal">Verdict</th>
              </tr>
            </thead>
            <tbody className="font-mono text-[12px]">
              {t.commands.map((c, i) => (
                <tr key={i} className="border-b border-line last:border-0 hover:bg-white/[0.02]">
                  <td className="px-4 py-2.5 text-faint">{c.ts}</td>
                  <td className="px-4 py-2.5 text-ink">{c.channel}</td>
                  <td className="px-4 py-2.5 text-muted">{c.value}</td>
                  <td className="px-4 py-2.5"><span className={`rounded px-1.5 py-0.5 text-[10px] font-semibold ${badge(c.tone)}`}>{c.verdict}</span></td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Panel>
    </div>
  )
}

function Metric({ label, value, tone, meter }: { label: string; value: string; tone: Tone; meter?: number }) {
  return (
    <div className="rounded-xl border border-line bg-panel p-4 shadow-panel">
      <div className="font-mono text-[11px] uppercase tracking-wider text-faint">{label}</div>
      <div className={`mt-1.5 font-display text-[20px] font-semibold leading-none ${value.length > 8 ? 'text-[15px]' : ''} ${txt(tone)}`}>{value}</div>
      {meter !== undefined && <div className="mt-3"><Meter value={meter} tone={tone} /></div>}
    </div>
  )
}

function KV({ k, v }: { k: string; v: string }) {
  return (
    <div className="flex items-center justify-between">
      <span className="font-mono text-[11px] uppercase tracking-wider text-faint">{k}</span>
      <span className="font-mono text-xs text-ink">{v}</span>
    </div>
  )
}

function Vital({ label, unit, data, color }: { label: string; unit: string; data: SeriesPoint[]; color: 'safe' | 'warn' | 'crit' | 'ice' }) {
  const last = data[data.length - 1]?.v ?? 0
  return (
    <div className="rounded-lg border border-line bg-bg/40 p-3">
      <div className="flex items-center justify-between">
        <span className="font-mono text-[10px] uppercase tracking-wider text-faint">{label}</span>
        <span className="font-mono text-[12px] text-ink">{last}<span className="text-muted"> {unit}</span></span>
      </div>
      <div className="mt-2"><Spark data={data} color={color} /></div>
    </div>
  )
}

function confColor(data: SeriesPoint[]): 'safe' | 'warn' | 'crit' | 'ice' {
  const last = data[data.length - 1]?.v ?? 100
  return last < 20 ? 'crit' : last < 60 ? 'warn' : 'safe'
}

function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
function dotBg(t: Tone) { return t === 'safe' ? 'bg-safe' : t === 'warn' ? 'bg-warn' : t === 'crit' ? 'bg-crit' : t === 'ice' ? 'bg-ice' : 'bg-muted' }
function badge(t: Tone) { return t === 'safe' ? 'bg-safe/15 text-safe' : t === 'warn' ? 'bg-warn/15 text-warn' : t === 'crit' ? 'bg-crit/15 text-crit' : 'bg-ice/15 text-ice' }
