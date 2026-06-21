import { Fragment } from 'react'
import { Panel, Pill, Meter, StatusDot } from '@/components/ui/primitives'
import { hero, decisions, topology, exec, audit, feed, mapRobots } from '@/lib/command'
import type { Tone } from '@/lib/types'

const dot = (t: Tone) => (t === 'safe' ? 'bg-safe' : t === 'warn' ? 'bg-warn' : t === 'crit' ? 'bg-crit' : t === 'ice' ? 'bg-ice' : 'bg-muted')
const txt = (t: Tone) => (t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted')
const badge = (t: Tone) => (t === 'crit' ? 'bg-crit/15 text-crit' : t === 'warn' ? 'bg-warn/15 text-warn' : t === 'safe' ? 'bg-safe/15 text-safe' : 'bg-ice/15 text-ice')

export function HeroSafety() {
  return (
    <section className="relative overflow-hidden rounded-2xl border border-line bg-gradient-to-br from-elevated/60 to-surface p-6 shadow-panel">
      <div className="pointer-events-none absolute -left-24 -top-28 h-72 w-72 rounded-full bg-safe/10 blur-3xl" />
      <div className="relative grid gap-8 lg:grid-cols-[1fr_1.25fr]">
        <div>
          <div className="flex items-center gap-2"><span className="h-1 w-6 rounded bg-safe" /><p className="font-mono text-[11px] uppercase tracking-[0.25em] text-faint">Fleet Safety Status</p></div>
          <div className="mt-4 flex items-end gap-1">
            <span className="font-display text-[64px] font-semibold leading-none text-ink">{hero.score}</span>
            <span className="mb-2 font-display text-2xl text-muted">%</span>
          </div>
          <div className="mt-3 inline-flex items-center gap-2 rounded-full bg-safe/10 px-3 py-1 ring-1 ring-safe/30">
            <StatusDot tone="safe" pulse /><span className="font-mono text-[12px] uppercase tracking-wider text-safe">Secure · Governor enforcing</span>
          </div>
          <div className="mt-7 grid grid-cols-4 gap-3 border-t border-line pt-5">
            {decisions.map((d) => (
              <div key={d.label}>
                <div className={`font-display text-xl font-semibold ${txt(d.tone)}`}>{d.value.toLocaleString()}</div>
                <div className="font-mono text-[10px] uppercase tracking-wider text-faint">{d.label}</div>
              </div>
            ))}
          </div>
        </div>
        <div className="grid grid-cols-1 gap-px self-center overflow-hidden rounded-xl border border-line bg-line sm:grid-cols-2">
          {hero.metrics.map((m) => (
            <div key={m.label} className="flex items-center justify-between gap-3 bg-panel px-4 py-3.5">
              <span className="font-mono text-[11px] uppercase tracking-wider text-faint">{m.label}</span>
              {m.meter !== undefined ? (
                <div className="flex items-center gap-2"><div className="w-16"><Meter value={m.meter} tone={m.tone} /></div><span className={`font-mono text-xs ${txt(m.tone)}`}>{m.value}</span></div>
              ) : (
                <span className={`font-mono text-xs font-semibold ${txt(m.tone)}`}>{m.value}</span>
              )}
            </div>
          ))}
        </div>
      </div>
    </section>
  )
}

export function FleetTopology() {
  return (
    <Panel title="Governance Topology" subtitle="every command flows through the governor">
      <div className="flex flex-col gap-2 md:flex-row md:items-stretch">
        {topology.map((n, i) => (
          <Fragment key={n.stage}>
            <div className={`flex-1 rounded-xl border p-3 ${n.governor ? 'border-safe/40 bg-safe/5' : 'border-line bg-elevated/40'}`}>
              <div className="flex items-center justify-between">
                <span className={`text-[13px] font-semibold ${n.governor ? 'text-safe' : 'text-ink'}`}>{n.stage}</span>
                <StatusDot tone={n.tone} pulse={n.governor} />
              </div>
              <div className="mt-1 font-mono text-[10px] text-faint">{n.sub}</div>
              <div className="mt-2.5 flex items-center gap-2"><div className="flex-1"><Meter value={n.health} tone={n.tone} /></div><span className="font-mono text-[10px] text-muted">{n.health}%</span></div>
            </div>
            {i < topology.length - 1 && (
              <div className="flex items-center justify-center text-faint"><span className="md:hidden">▼</span><span className="hidden md:inline">▶</span></div>
            )}
          </Fragment>
        ))}
      </div>
    </Panel>
  )
}

function Legend({ c, t }: { c: string; t: string }) {
  return <span className="inline-flex items-center gap-1.5"><span className={`h-2 w-2 rounded-full ${c}`} />{t}</span>
}

export function MissionMap() {
  return (
    <Panel title="Fleet Map" subtitle="us-fleet-1 · warehouse A · live positions" action={<Pill tone="ice">live</Pill>} dense>
      <svg viewBox="0 0 160 90" preserveAspectRatio="xMidYMid meet" className="w-full" style={{ height: 296 }} role="img" aria-label="fleet map">
        <defs><pattern id="mm-grid" width="8" height="8" patternUnits="userSpaceOnUse"><path d="M8 0H0V8" fill="none" stroke="rgba(150,166,198,0.06)" strokeWidth="0.3" /></pattern></defs>
        <rect x="2" y="2" width="156" height="86" rx="2" fill="url(#mm-grid)" stroke="rgba(150,166,198,0.18)" strokeWidth="0.4" />
        {[12, 40, 68, 96, 124].map((x) => (
          <Fragment key={x}>
            <rect x={x} y="14" width="8" height="26" fill="rgba(150,166,198,0.06)" stroke="rgba(150,166,198,0.12)" strokeWidth="0.3" />
            <rect x={x} y="50" width="8" height="26" fill="rgba(150,166,198,0.06)" stroke="rgba(150,166,198,0.12)" strokeWidth="0.3" />
          </Fragment>
        ))}
        <rect x="106" y="54" width="40" height="28" fill="rgba(255,176,32,0.07)" stroke="rgba(255,176,32,0.4)" strokeWidth="0.4" strokeDasharray="2 1.5" />
        <text x="108" y="59" fill="#ffb020" fontSize="3" fontFamily="monospace">HAZARD</text>
        <rect x="120" y="62" width="22" height="18" fill="rgba(255,84,104,0.10)" stroke="rgba(255,84,104,0.5)" strokeWidth="0.4" />
        <text x="122" y="67" fill="#ff5468" fontSize="3" fontFamily="monospace">LOCKOUT</text>
        <path d="M20 70 L40 55 L70 50 L100 40 L130 30" fill="none" stroke="#5cc6ff" strokeWidth="0.6" strokeOpacity="0.5" strokeDasharray="2 2" />
        {mapRobots.map((r) => {
          const x = 4 + (r.x / 100) * 152
          const y = 4 + (r.y / 100) * 82
          const c = r.tone === 'safe' ? '#2fe6a6' : r.tone === 'warn' ? '#ffb020' : '#ff5468'
          return (
            <Fragment key={r.id}>
              <circle cx={x} cy={y} r="3" fill="none" stroke={c} strokeOpacity="0.35" strokeWidth="0.4" />
              <circle cx={x} cy={y} r="1.5" fill={c} />
              <text x={x + 3} y={y + 1} fill="#9aa6bd" fontSize="2.6" fontFamily="monospace">{r.id}</text>
            </Fragment>
          )
        })}
      </svg>
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 border-t border-line px-4 py-3 font-mono text-[10px] text-faint">
        <Legend c="bg-safe" t="nominal" /><Legend c="bg-warn" t="degraded" /><Legend c="bg-crit" t="locked out" />
        <span className="ml-auto">hazard zone · lockout area · planned route</span>
      </div>
    </Panel>
  )
}

export function ExecSummary() {
  return (
    <Panel title="Fleet Health" subtitle="executive summary">
      <ul className="space-y-3.5">
        {exec.map((e) => (
          <li key={e.label}>
            <div className="flex items-center justify-between"><span className="text-[13px] text-muted">{e.label}</span><span className="font-display text-sm font-semibold text-ink">{e.value}</span></div>
            {e.pct !== undefined && <div className="mt-1.5"><Meter value={e.pct} tone="safe" /></div>}
          </li>
        ))}
      </ul>
    </Panel>
  )
}

function KV({ k, v, tone }: { k: string; v: string; tone?: Tone }) {
  return (
    <div>
      <div className="font-mono text-[10px] uppercase tracking-wider text-faint">{k}</div>
      <div className={`mt-0.5 font-mono text-[13px] ${tone ? txt(tone) : 'text-ink'}`}>{v}</div>
    </div>
  )
}

export function AuditLedger() {
  return (
    <Panel title="Audit Ledger" subtitle="SHA-256 hash-chained" action={<Pill tone="safe">verified</Pill>}>
      <div className="grid grid-cols-2 gap-4">
        <KV k="Blocks" v={audit.blocks} />
        <KV k="Integrity" v={audit.integrity} tone="safe" />
        <KV k="Breaks" v={String(audit.breaks)} tone="safe" />
        <KV k="Last verify" v={audit.last} />
      </div>
      <div className="mt-4 rounded-lg border border-line bg-bg/40 px-3 py-2">
        <div className="font-mono text-[10px] uppercase tracking-wider text-faint">SHA-256 root</div>
        <div className="mt-1 break-all font-mono text-[12px] text-ice">{audit.root}</div>
      </div>
    </Panel>
  )
}

export function EventFeed() {
  return (
    <Panel title="Event Stream" subtitle="governor · fleet · telemetry" action={<Pill tone="ice">live</Pill>} dense>
      <ul className="divide-y divide-line">
        {feed.map((e) => (
          <li key={e.id} className="px-4 py-3">
            <div className="flex items-center gap-2">
              <span className={`rounded px-1.5 py-0.5 font-mono text-[9px] font-semibold uppercase tracking-wider ${badge(e.tone)}`}>{e.sev}</span>
              <span className="font-mono text-[11px] text-muted">{e.subsystem}</span>
              <span className="font-mono text-[11px] text-ink">{e.asset}</span>
              <span className="ml-auto font-mono text-[10px] text-faint">{e.ts}</span>
            </div>
            <p className="mt-1.5 text-[13px] leading-snug text-ink">{e.title}</p>
            <div className="mt-1 flex items-center gap-1.5"><span className={`h-1.5 w-1.5 rounded-full ${dot(e.tone)}`} /><span className={`font-mono text-[11px] ${txt(e.tone)}`}>{e.disposition}</span></div>
          </li>
        ))}
      </ul>
    </Panel>
  )
}
