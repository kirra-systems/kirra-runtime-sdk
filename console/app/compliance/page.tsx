import { Panel, Pill, Meter, StatusDot } from '@/components/ui/primitives'
import { certs, tests, sbom, firmware, policyLogs } from '@/lib/compliance'
import type { Tone } from '@/lib/types'

export default function CompliancePage() {
  const totalPassed = tests.reduce((a, t) => a + t.passed, 0)
  const totalTests = tests.reduce((a, t) => a + t.total, 0)
  const coverage = ((totalPassed / totalTests) * 100).toFixed(1)

  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Compliance & Certification</h1>
          <p className="font-mono text-[11px] text-faint">functional-safety evidence · audit-ready posture</p>
        </div>
        <div className="flex items-center gap-2">
          <Pill tone="safe">ASIL D · certified</Pill>
          <Pill tone="ice">{coverage}% verification coverage</Pill>
        </div>
      </div>

      <div className="grid grid-cols-1 gap-6 sm:grid-cols-2 xl:grid-cols-4">
        {certs.map((c) => (
          <div key={c.id} className="rounded-xl border border-line bg-panel p-4 shadow-panel">
            <div className="flex items-start justify-between">
              <div>
                <div className="font-display text-[15px] font-semibold text-ink">{c.standard}</div>
                <div className="mt-0.5 font-mono text-[11px] text-faint">{c.level}</div>
              </div>
              <StatusDot tone={c.tone} pulse={c.status === 'In Audit'} />
            </div>
            <p className="mt-3 text-[12px] leading-snug text-muted">{c.scope}</p>
            <div className="mt-4 flex items-center justify-between border-t border-line pt-3">
              <span className={`font-mono text-[10px] uppercase tracking-wider ${txt(c.tone)}`}>{c.status}</span>
              <span className="font-mono text-[10px] text-faint">exp {c.expires}</span>
            </div>
          </div>
        ))}
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Verification Test Results" subtitle="safety-case evidence suites">
          <ul className="space-y-4">
            {tests.map((t) => {
              const pct = (t.passed / t.total) * 100
              return (
                <li key={t.id}>
                  <div className="flex items-center justify-between gap-3">
                    <span className="text-[13px] text-ink">{t.name}</span>
                    <span className="font-mono text-[11px] text-muted">{t.passed.toLocaleString()} <span className="text-faint">/ {t.total.toLocaleString()}</span></span>
                  </div>
                  <div className="mt-2 flex items-center gap-3">
                    <div className="flex-1"><Meter value={pct} tone={t.tone} /></div>
                    <span className={`font-mono text-[10px] ${txt(t.tone)}`}>{pct.toFixed(1)}%</span>
                  </div>
                </li>
              )
            })}
          </ul>
        </Panel>

        <Panel title="Software Bill of Materials" subtitle="cert-scoped dependency provenance" dense>
          <ul>
            {sbom.map((s) => (
              <li key={s.id} className="flex items-center gap-3 border-b border-line px-4 py-2.5 last:border-0">
                <StatusDot tone={s.risk} />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center justify-between gap-2">
                    <span className="truncate font-mono text-[12px] text-ink">{s.component}</span>
                    <span className="font-mono text-[11px] text-muted">{s.version}</span>
                  </div>
                  <div className="mt-0.5 flex items-center justify-between gap-2">
                    <span className="font-mono text-[10px] text-faint">{s.license}</span>
                    <span className={`font-mono text-[10px] uppercase tracking-wider ${txt(s.risk)}`}>{s.note}</span>
                  </div>
                </div>
              </li>
            ))}
          </ul>
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-2">
        <Panel title="Signed Firmware Provenance" subtitle="Ed25519 release attestation" dense>
          <ul>
            {firmware.map((f) => (
              <li key={f.id} className="flex items-center gap-4 border-b border-line px-4 py-3 last:border-0">
                <StatusDot tone={f.tone} />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center justify-between gap-3">
                    <span className="truncate text-[13px] text-ink">{f.target}</span>
                    <span className="font-mono text-[11px] text-muted">v{f.version}</span>
                  </div>
                  <div className="mt-1 flex items-center justify-between gap-3 font-mono text-[10px] text-faint">
                    <span>{f.digest} · {f.signer}</span>
                    <span>{f.ts}</span>
                  </div>
                </div>
              </li>
            ))}
          </ul>
        </Panel>

        <Panel title="Policy Enforcement Log" subtitle="fail-closed authorization events" dense>
          <ul>
            {policyLogs.map((p) => (
              <li key={p.id} className="flex items-center gap-4 border-b border-line px-4 py-3 last:border-0">
                <span className={`rounded px-1.5 py-0.5 font-mono text-[10px] font-semibold ${badge(p.tone)}`}>{p.outcome}</span>
                <div className="min-w-0 flex-1">
                  <p className="truncate text-[12px] text-ink">{p.policy}</p>
                  <p className="mt-0.5 font-mono text-[10px] text-faint">{p.actor}</p>
                </div>
                <span className="font-mono text-[10px] text-faint">{p.ts}</span>
              </li>
            ))}
          </ul>
        </Panel>
      </div>
    </div>
  )
}

function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
function badge(t: Tone) { return t === 'safe' ? 'bg-safe/15 text-safe' : t === 'warn' ? 'bg-warn/15 text-warn' : t === 'crit' ? 'bg-crit/15 text-crit' : 'bg-ice/15 text-ice' }
