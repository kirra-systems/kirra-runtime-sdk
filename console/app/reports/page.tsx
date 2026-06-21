import { Download, FileText, ShieldCheck } from 'lucide-react'
import { Panel, Pill, Meter, StatusDot } from '@/components/ui/primitives'
import { DemoBadge } from '@/components/ui/demo-badge'
import { AuditEvidence } from '@/components/ui/audit-evidence'
import { VersionAdoptionBar } from '@/components/ui/version-adoption'
import { reports, scheduled, rollout, rolloutVersion } from '@/lib/reports'
import type { Tone } from '@/lib/types'

export default function ReportsPage() {
  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Reports & Releases</h1>
          <p className="font-mono text-[11px] text-faint">generated evidence · scheduled exports · software rollout</p>
        </div>
        <div className="flex items-center gap-2">
          <DemoBadge live={false} />
          <button className="flex items-center gap-2 rounded-lg border border-ice/40 bg-ice/10 px-4 py-1.5 font-mono text-[11px] uppercase tracking-wider text-ice hover:bg-ice/20">
            <FileText className="h-3.5 w-3.5" /> Generate report
          </button>
        </div>
      </div>

      {/* ── Live signed-evidence summary from the audit chain ── */}
      <AuditEvidence />

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Generated Reports" subtitle="signed evidence documents" dense>
          <ul>
            {reports.map((r) => (
              <li key={r.id} className="flex items-center gap-4 border-b border-line px-4 py-3 last:border-0 hover:bg-white/[0.02]">
                <StatusDot tone={r.tone} />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="truncate text-[13px] text-ink">{r.title}</span>
                    {r.signed && <ShieldCheck className="h-3.5 w-3.5 shrink-0 text-safe" />}
                  </div>
                  <div className="mt-0.5 flex items-center gap-2 font-mono text-[10px] text-faint">
                    <span>{r.id}</span><span>·</span><span>{r.kind}</span><span>·</span><span>{r.period}</span><span>·</span><span>{r.ts}</span>
                  </div>
                </div>
                <span className={`rounded px-1.5 py-0.5 font-mono text-[10px] ${badge(r.tone)}`}>{r.format}</span>
                <button className="flex h-7 w-7 items-center justify-center rounded-lg border border-line text-faint hover:bg-white/[0.04] hover:text-ink" aria-label="download">
                  <Download className="h-3.5 w-3.5" />
                </button>
              </li>
            ))}
          </ul>
        </Panel>

        <Panel title="Scheduled Exports" subtitle="recurring deliveries" dense>
          <ul>
            {scheduled.map((s) => (
              <li key={s.id} className="flex items-center gap-3 border-b border-line px-4 py-3 last:border-0">
                <StatusDot tone={s.tone} />
                <div className="min-w-0 flex-1">
                  <div className="truncate text-[12px] text-ink">{s.name}</div>
                  <div className="font-mono text-[10px] text-faint">{s.cadence}</div>
                </div>
                <span className="font-mono text-[10px] text-muted">{s.next}</span>
              </li>
            ))}
          </ul>
        </Panel>
      </div>

      {/* ── Software Rollout / OTA (#11) ── */}
      <Panel
        title="Software Rollout"
        subtitle={`${rolloutVersion.version} · ${rolloutVersion.channel} channel · staged · health-gated`}
        action={<Pill tone="safe">signed release</Pill>}
      >
        <div className="grid grid-cols-1 gap-4 lg:grid-cols-4">
          {rollout.map((r) => (
            <div key={r.id} className="rounded-xl border border-line bg-panel p-4">
              <div className="flex items-center justify-between">
                <span className="font-mono text-[11px] text-ink">{r.ring}</span>
                <span className={`rounded px-1.5 py-0.5 font-mono text-[9px] uppercase ${badge(r.tone)}`}>{r.status}</span>
              </div>
              <div className="mt-3 flex items-baseline gap-1.5">
                <span className="font-display text-2xl font-semibold text-ink">{r.adoption}</span>
                <span className="font-mono text-[11px] text-muted">% · {r.assets} assets</span>
              </div>
              <div className="mt-3"><Meter value={r.adoption} tone={r.tone} /></div>
              <p className="mt-2 font-mono text-[10px] text-faint">{r.note}</p>
            </div>
          ))}
        </div>

        {/* ── Live fleet version-adoption (GET /console/versions, #398) ── */}
        <VersionAdoptionBar />
      </Panel>
    </div>
  )
}

function badge(t: Tone) { return t === 'safe' ? 'bg-safe/15 text-safe' : t === 'warn' ? 'bg-warn/15 text-warn' : t === 'crit' ? 'bg-crit/15 text-crit' : t === 'ice' ? 'bg-ice/15 text-ice' : 'bg-white/5 text-muted' }
