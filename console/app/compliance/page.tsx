'use client'

import { Panel, Pill, Meter, StatusDot } from '@/components/ui/primitives'
import { DemoBadge } from '@/components/ui/demo-badge'
import { useRawModal } from '@/components/ui/raw-modal'
import { useAuditChain } from '@/lib/api/hooks'
import { certs, tests, sbom, firmware } from '@/lib/compliance'
import type { Tone } from '@/lib/types'
import { utcTime } from '@/lib/format'

export default function CompliancePage() {
  const audit = useAuditChain(15000)
  const raw = useRawModal()
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
          <DemoBadge live={audit.source === 'live'} />
          {audit.source === 'live' ? <Pill tone="safe">audit chain · live</Pill> : <Pill tone="ice">audit chain · demo</Pill>}
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

      {/* ── Live from the verifier: tamper-evident audit chain ── */}
      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel
          title="Audit Chain Integrity"
          subtitle="SHA-256 hash-chained ledger · GET /system/audit/verify"
          action={audit.source === 'live' ? <Pill tone="safe">live</Pill> : <Pill tone="ice">demo</Pill>}
        >
          <div className="flex items-center gap-3">
            <StatusDot tone={audit.verify.chain_intact ? 'safe' : 'crit'} pulse={!audit.verify.chain_intact} />
            <span className={`font-display text-2xl font-semibold ${audit.verify.chain_intact ? 'text-safe' : 'text-crit'}`}>
              {audit.verify.chain_intact ? 'Chain intact' : 'Chain broken'}
            </span>
          </div>
          <div className="mt-4 space-y-3">
            <KV k="Total entries" v={audit.verify.total_entries.toLocaleString('en-US')} />
            <KV k="Signed / unsigned" v={`${audit.verify.signed_entries.toLocaleString('en-US')} / ${audit.verify.unsigned_entries.toLocaleString('en-US')}`} />
            <KV k="Signature valid" v={audit.verify.signature_valid ? 'yes' : 'no'} tone={audit.verify.signature_valid ? 'safe' : 'crit'} />
            <KV k="Head" v={audit.verify.head_status} />
            <KV k="Latest hash" v={audit.verify.latest_hash} />
          </div>
        </Panel>

        <Panel className="xl:col-span-2" title="Audit Ledger" subtitle="tamper-evident enforcement events · GET /console/audit" dense>
          <ul>
            {audit.entries.map((e) => (
              <li
                key={e.id}
                onClick={() => raw.open({ title: e.event_type, subtitle: `audit entry #${e.id} · ${e.source}`, data: e })}
                className="flex cursor-pointer items-center gap-4 border-b border-line px-4 py-3 last:border-0 hover:bg-ink/[0.02]"
                title="tap for raw entry"
              >
                <StatusDot tone={auditTone(e.event_type)} />
                <span className="w-20 shrink-0 font-mono text-[10px] text-faint">{utcTime(e.timestamp_ms)}</span>
                <span className="w-24 shrink-0 font-mono text-[10px] uppercase tracking-wider text-muted">{e.source}</span>
                <div className="min-w-0 flex-1">
                  <div className={`truncate font-mono text-[11px] ${txt(auditTone(e.event_type))}`}>{e.event_type}</div>
                  <div className="truncate font-mono text-[10px] text-faint">{e.payload}</div>
                </div>
                <span className={`shrink-0 font-mono text-[9px] uppercase ${e.signature_status === 'verified' ? 'text-safe' : 'text-faint'}`}>{e.signature_status}</span>
              </li>
            ))}
          </ul>
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Verification Test Results" subtitle="safety-case evidence suites · static artifact">
          <ul className="space-y-4">
            {tests.map((t) => {
              const pct = (t.passed / t.total) * 100
              return (
                <li key={t.id}>
                  <div className="flex items-center justify-between gap-3">
                    <span className="text-[13px] text-ink">{t.name}</span>
                    <span className="font-mono text-[11px] text-muted">{t.passed.toLocaleString('en-US')} <span className="text-faint">/ {t.total.toLocaleString('en-US')}</span></span>
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

        <Panel title="Software Bill of Materials" subtitle="cert-scoped · static artifact" dense>
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

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="Signed Firmware Provenance" subtitle="Ed25519 release attestation · static artifact" dense>
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

        <Panel title="Evidence Sources" subtitle="what's live vs static">
          <ul className="space-y-3">
            <SourceRow label="Audit chain & ledger" detail={audit.source === 'live' ? 'live · verifier' : 'demo'} tone={audit.source === 'live' ? 'safe' : 'ice'} live={audit.source === 'live'} />
            <SourceRow label="Certifications (ISO/IEC/UL)" detail="static evidence" tone="muted" />
            <SourceRow label="Verification tests" detail="CI artifact" tone="muted" />
            <SourceRow label="Software bill of materials" detail="build artifact" tone="muted" />
            <SourceRow label="Signed firmware" detail="release attestation" tone="muted" />
          </ul>
          <p className="mt-4 border-t border-line pt-3 font-mono text-[10px] leading-relaxed text-faint">
            The audit chain is read live from the verifier&apos;s tamper-evident ledger. Certifications, tests, SBOM, and firmware provenance are point-in-time evidence artifacts, not runtime endpoints.
          </p>
        </Panel>
      </div>
      {raw.modal}
    </div>
  )
}

function KV({ k, v, tone }: { k: string; v: string; tone?: Tone }) {
  return (
    <div className="flex items-center justify-between">
      <span className="font-mono text-[11px] uppercase tracking-wider text-faint">{k}</span>
      <span className={`font-mono text-xs ${tone ? txt(tone) : 'text-ink'}`}>{v}</span>
    </div>
  )
}

function SourceRow({ label, detail, tone, live }: { label: string; detail: string; tone: Tone; live?: boolean }) {
  return (
    <li className="flex items-center justify-between">
      <span className="flex items-center gap-2 text-[12px] text-ink"><StatusDot tone={tone} pulse={live} />{label}</span>
      <span className={`font-mono text-[10px] uppercase tracking-wider ${txt(tone)}`}>{detail}</span>
    </li>
  )
}

function auditTone(eventType: string): Tone {
  if (/BREACH|DENY|LOCKEDOUT|CYCLE|REVOK|FAULT|BLOCKED/i.test(eventType)) return 'crit'
  if (/DEGRADED|CLAMP|TRANSITION|WARN/i.test(eventType)) return 'warn'
  if (/FEDERATION|DDS|LATENCY/i.test(eventType)) return 'ice'
  return 'safe'
}

function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
