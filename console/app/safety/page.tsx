import { Panel, Pill, Meter, StatusDot } from '@/components/ui/primitives'
import { ScoreRing } from '@/components/charts/charts'
import { constraints, violations, interventions, verdictMix } from '@/lib/safety'
import type { Tone } from '@/lib/types'

export default function SafetyPage() {
  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Safety Governor</h1>
          <p className="font-mono text-[11px] text-faint">command center · fail-closed verdict engine</p>
        </div>
        <div className="flex items-center gap-2">
          <Pill tone="safe">State: Nominal</Pill>
          <button className="rounded-lg border border-crit/40 bg-crit/10 px-4 py-1.5 font-mono text-[11px] uppercase tracking-wider text-crit hover:bg-crit/20">Emergency Stop</button>
        </div>
      </div>

      <div className="grid grid-cols-1 gap-6 lg:grid-cols-3">
        <Panel title="Safety Score" subtitle="composite governance index">
          <ScoreRing value={98} color="safe" label="of 100" />
          <div className="mt-2 grid grid-cols-3 gap-2 text-center font-mono text-[10px] uppercase tracking-wider text-faint">
            <div><div className="text-sm text-safe">A+</div>envelope</div>
            <div><div className="text-sm text-safe">100%</div>coverage</div>
            <div><div className="text-sm text-warn">3</div>open</div>
          </div>
        </Panel>

        <Panel title="Emergency Stop Readiness">
          <div className="space-y-4">
            <div className="flex items-center justify-between">
              <span className="font-display text-2xl font-semibold text-safe">ARMED</span>
              <StatusDot tone="safe" pulse />
            </div>
            <KV k="Stop-path latency" v="42 ms" />
            <KV k="Last self-test" v="08:00:00 · pass" />
            <KV k="Redundant channels" v="2 / 2 healthy" />
            <KV k="MRC profile" v="decel-to-stop · hold" />
          </div>
        </Panel>

        <Panel title="Verdict Throughput" subtitle="last 24h">
          <div className="mb-3 flex items-baseline gap-2">
            <span className="font-display text-3xl font-semibold text-ink">{(verdictMix.allow + verdictMix.clamp + verdictMix.deny).toLocaleString()}</span>
            <span className="font-mono text-xs text-muted">decisions</span>
          </div>
          <VerdictBar allow={verdictMix.allow} clamp={verdictMix.clamp} deny={verdictMix.deny} />
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-2">
        <Panel title="Constraint Monitoring" subtitle="active safety goals & envelopes" dense>
          <ul>
            {constraints.map((c) => (
              <li key={c.id} className="flex items-center gap-4 border-b border-line px-4 py-3 last:border-0">
                <StatusDot tone={c.tone} />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center justify-between gap-3">
                    <span className="truncate text-[13px] text-ink">{c.name}</span>
                    <span className="font-mono text-[11px] text-muted">{c.value} <span className="text-faint">/ {c.limit}</span></span>
                  </div>
                  <div className="mt-2 flex items-center gap-3">
                    <div className="flex-1"><Meter value={c.util} tone={c.tone} /></div>
                    <span className={`font-mono text-[10px] uppercase tracking-wider ${txt(c.tone)}`}>{c.status}</span>
                  </div>
                </div>
              </li>
            ))}
          </ul>
        </Panel>

        <Panel title="Intervention History" subtitle="governor actions" dense>
          <ul className="px-4 py-2">
            {interventions.map((it, idx) => (
              <li key={it.id} className="relative flex gap-4 pb-5 pl-2 last:pb-2">
                {idx < interventions.length - 1 && <span className="absolute left-[11px] top-4 h-full w-px bg-line" />}
                <span className={`mt-1 h-2.5 w-2.5 shrink-0 rounded-full ${dotBg(it.tone)}`} />
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    <span className={`rounded px-1.5 py-0.5 font-mono text-[10px] font-semibold ${badge(it.tone)}`}>{it.kind}</span>
                    <span className="font-mono text-[10px] text-faint">{it.ts}</span>
                  </div>
                  <p className="mt-1 text-[12px] text-muted">{it.detail}</p>
                </div>
              </li>
            ))}
          </ul>
        </Panel>
      </div>

      <Panel title="Rule Violations" subtitle="fail-closed denials & lockouts" dense>
        <div className="overflow-x-auto">
          <table className="w-full min-w-[640px] text-left">
            <thead>
              <tr className="border-b border-line font-mono text-[10px] uppercase tracking-wider text-faint">
                <th className="px-4 py-2 font-normal">Time</th>
                <th className="px-4 py-2 font-normal">Rule</th>
                <th className="px-4 py-2 font-normal">Asset</th>
                <th className="px-4 py-2 font-normal">Action</th>
              </tr>
            </thead>
            <tbody className="font-mono text-[12px]">
              {violations.map((v) => (
                <tr key={v.id} className="border-b border-line last:border-0 hover:bg-white/[0.02]">
                  <td className="px-4 py-2.5 text-faint">{v.ts}</td>
                  <td className={`px-4 py-2.5 ${v.tone === 'crit' ? 'text-crit' : 'text-warn'}`}>{v.rule}</td>
                  <td className="px-4 py-2.5 text-ink">{v.asset}</td>
                  <td className="px-4 py-2.5 text-muted">{v.action}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Panel>
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

function VerdictBar({ allow, clamp, deny }: { allow: number; clamp: number; deny: number }) {
  const total = allow + clamp + deny
  const p = (n: number) => `${(n / total) * 100}%`
  return (
    <div>
      <div className="flex h-2 overflow-hidden rounded-full bg-white/5">
        <div className="bg-safe" style={{ width: p(allow) }} />
        <div className="bg-warn" style={{ width: p(clamp) }} />
        <div className="bg-crit" style={{ width: p(deny) }} />
      </div>
      <div className="mt-3 grid grid-cols-3 gap-2 font-mono text-[11px]">
        <Leg tone="safe" label="ALLOW" value={allow} />
        <Leg tone="warn" label="CLAMP" value={clamp} />
        <Leg tone="crit" label="DENY" value={deny} />
      </div>
    </div>
  )
}

function Leg({ tone, label, value }: { tone: Tone; label: string; value: number }) {
  return (
    <div>
      <div className="flex items-center gap-1.5">
        <span className={`h-2 w-2 rounded-full ${dotBg(tone)}`} />
        <span className="text-faint">{label}</span>
      </div>
      <div className="mt-0.5 text-ink">{value.toLocaleString()}</div>
    </div>
  )
}

function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
function dotBg(t: Tone) { return t === 'safe' ? 'bg-safe' : t === 'warn' ? 'bg-warn' : t === 'crit' ? 'bg-crit' : t === 'ice' ? 'bg-ice' : 'bg-muted' }
function badge(t: Tone) { return t === 'safe' ? 'bg-safe/15 text-safe' : t === 'warn' ? 'bg-warn/15 text-warn' : t === 'crit' ? 'bg-crit/15 text-crit' : 'bg-ice/15 text-ice' }
