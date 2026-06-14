import { Panel, Pill, Meter, StatusDot } from '@/components/ui/primitives'
import { tally, trace, traceSubject, recent, factors } from '@/lib/oversight'
import type { Tone } from '@/lib/types'

export default function OversightPage() {
  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">AI Decision Oversight</h1>
          <p className="font-mono text-[11px] text-faint">explainability · every model action adjudicated by the Governor</p>
        </div>
        <div className="flex items-center gap-2">
          <Pill tone="ice">autoware.planner · v4.2</Pill>
          <Pill tone="safe">99.0% allow rate · 24h</Pill>
        </div>
      </div>

      <div className="grid grid-cols-1 gap-6 sm:grid-cols-3">
        {tally.map((t) => (
          <div key={t.label} className="rounded-xl border border-line bg-panel p-4 shadow-panel">
            <div className="flex items-center justify-between">
              <span className="font-mono text-[11px] uppercase tracking-wider text-faint">{t.label}</span>
              <StatusDot tone={t.tone} />
            </div>
            <div className="mt-2 flex items-baseline gap-2">
              <span className="font-display text-[28px] font-semibold leading-none text-ink">{t.value.toLocaleString()}</span>
              <span className={`font-mono text-xs ${txt(t.tone)}`}>{t.share}%</span>
            </div>
            <div className="mt-3"><Meter value={t.share} tone={t.tone} /></div>
          </div>
        ))}
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Decision Trace" subtitle={`${traceSubject.asset} · ${traceSubject.actionType} · ${traceSubject.ts}`} action={<span className={`rounded px-2 py-1 font-mono text-[10px] font-semibold ${badge(verdictTone(traceSubject.verdict))}`}>{traceSubject.verdict} · {traceSubject.reason}</span>}>
          <div className="mb-4 grid grid-cols-1 gap-3 sm:grid-cols-2">
            <IO label="Model proposed" value={traceSubject.proposed} />
            <IO label="Governor verdict" value={`${traceSubject.verdict} → clamp to MRC decel · ${traceSubject.latencyUs} µs`} tone={verdictTone(traceSubject.verdict)} />
          </div>
          <ol>
            {trace.map((s, idx) => (
              <li key={s.id} className="relative flex gap-4 pb-5 pl-1 last:pb-0">
                {idx < trace.length - 1 && <span className="absolute left-[12px] top-6 h-full w-px bg-line" />}
                <span className={`mt-0.5 flex h-6 w-6 shrink-0 items-center justify-center rounded-full font-mono text-[12px] ${stepChip(s.outcome)}`}>{stepIcon(s.outcome)}</span>
                <div className="min-w-0 pt-0.5">
                  <div className="flex items-center gap-2">
                    <span className="text-[13px] text-ink">{s.name}</span>
                    <span className={`font-mono text-[10px] uppercase tracking-wider ${stepText(s.outcome)}`}>{s.outcome}</span>
                  </div>
                  <p className="mt-1 font-mono text-[11px] text-muted">{s.detail}</p>
                </div>
              </li>
            ))}
          </ol>
        </Panel>

        <Panel title="Explainability Factors" subtitle="weighted inputs to the verdict">
          <ul className="space-y-4">
            {factors.map((f) => (
              <li key={f.id}>
                <div className="flex items-center justify-between gap-3">
                  <span className="text-[12px] text-ink">{f.label}</span>
                  <span className={`font-mono text-[11px] ${txt(f.tone)}`}>{f.weight}</span>
                </div>
                <div className="mt-2"><Meter value={f.weight} tone={f.tone} /></div>
                <p className="mt-1.5 font-mono text-[10px] text-faint">{f.note}</p>
              </li>
            ))}
          </ul>
        </Panel>
      </div>

      <Panel title="Recent Decisions" subtitle="live adjudication stream" dense>
        <div className="overflow-x-auto">
          <table className="w-full min-w-[680px] text-left">
            <thead>
              <tr className="border-b border-line font-mono text-[10px] uppercase tracking-wider text-faint">
                <th className="px-4 py-2 font-normal">Time</th>
                <th className="px-4 py-2 font-normal">Asset</th>
                <th className="px-4 py-2 font-normal">Action</th>
                <th className="px-4 py-2 font-normal">Verdict</th>
                <th className="px-4 py-2 font-normal">Reason</th>
              </tr>
            </thead>
            <tbody className="font-mono text-[12px]">
              {recent.map((d) => (
                <tr key={d.id} className="border-b border-line last:border-0 hover:bg-white/[0.02]">
                  <td className="px-4 py-2.5 text-faint">{d.ts}</td>
                  <td className="px-4 py-2.5 text-ink">{d.asset}</td>
                  <td className="px-4 py-2.5 text-muted">{d.actionType}</td>
                  <td className="px-4 py-2.5"><span className={`rounded px-1.5 py-0.5 text-[10px] font-semibold ${badge(d.tone)}`}>{d.verdict}</span></td>
                  <td className={`px-4 py-2.5 ${txt(d.tone)}`}>{d.reason}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Panel>
    </div>
  )
}

function IO({ label, value, tone = 'muted' }: { label: string; value: string; tone?: Tone }) {
  return (
    <div className="rounded-lg border border-line bg-bg/40 p-3">
      <div className="font-mono text-[10px] uppercase tracking-wider text-faint">{label}</div>
      <div className={`mt-1.5 break-words font-mono text-[12px] ${tone === 'muted' ? 'text-ink' : txt(tone)}`}>{value}</div>
    </div>
  )
}

function verdictTone(v: string): Tone { return v === 'ALLOW' ? 'safe' : v === 'CLAMP' ? 'warn' : 'crit' }
function stepIcon(o: string) { return o === 'pass' ? '✓' : o === 'fail' ? '✕' : '•' }
function stepChip(o: string) { return o === 'pass' ? 'bg-safe/15 text-safe' : o === 'fail' ? 'bg-crit/15 text-crit' : 'bg-ice/15 text-ice' }
function stepText(o: string) { return o === 'pass' ? 'text-safe' : o === 'fail' ? 'text-crit' : 'text-ice' }
function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
function badge(t: Tone) { return t === 'safe' ? 'bg-safe/15 text-safe' : t === 'warn' ? 'bg-warn/15 text-warn' : t === 'crit' ? 'bg-crit/15 text-crit' : 'bg-ice/15 text-ice' }
