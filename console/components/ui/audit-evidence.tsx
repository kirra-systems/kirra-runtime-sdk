'use client'

import { ShieldCheck, ShieldAlert } from 'lucide-react'
import { Panel, Pill } from '@/components/ui/primitives'
import { useAuditChain } from '@/lib/api/hooks'

// Live signed-evidence summary for the Reports page, backed by the audit chain
// (GET /system/audit/verify, admin via the proxy). This is the one genuinely
// real artifact behind "generated reports": the tamper-evident hash-chained
// ledger. Self-contained client panel; falls back to demo when no backend.
export function AuditEvidence() {
  const { verify, source } = useAuditChain()
  const intact = verify.chain_intact && verify.verified
  const tone = intact ? 'safe' : 'crit'

  return (
    <Panel
      title="Audit Evidence"
      subtitle={source === 'live' ? 'live · GET /system/audit/verify · tamper-evident ledger' : 'demo · hash-chained ledger'}
      action={source === 'live' ? <Pill tone="safe">live</Pill> : <Pill tone="ice">demo</Pill>}
    >
      <div className="mb-4 flex items-center gap-3">
        {intact ? <ShieldCheck className="h-6 w-6 text-safe" /> : <ShieldAlert className="h-6 w-6 text-crit" />}
        <div>
          <div className={`font-display text-lg font-semibold ${intact ? 'text-safe' : 'text-crit'}`}>
            {intact ? 'Chain verified' : 'Chain integrity FAILED'}
          </div>
          <div className="font-mono text-[10px] text-faint">head {verify.head_status} · {verify.total_entries.toLocaleString('en-US')} entries</div>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        <Stat label="Total entries" value={verify.total_entries.toLocaleString('en-US')} tone="ice" />
        <Stat label="Signed" value={verify.signed_entries.toLocaleString('en-US')} tone={verify.unsigned_entries === 0 ? 'safe' : 'warn'} />
        <Stat label="Unsigned" value={verify.unsigned_entries.toLocaleString('en-US')} tone={verify.unsigned_entries === 0 ? 'safe' : 'warn'} />
        <Stat label="Signatures" value={verify.signature_valid ? 'valid' : 'invalid'} tone={verify.signature_valid ? 'safe' : 'crit'} />
      </div>

      <div className="mt-4 flex items-center justify-between border-t border-line pt-3">
        <span className="font-mono text-[10px] uppercase tracking-wider text-faint">Latest hash</span>
        <span className="truncate font-mono text-[11px] text-muted">{verify.latest_hash}</span>
      </div>
    </Panel>
  )
}

function Stat({ label, value, tone }: { label: string; value: string; tone: 'safe' | 'warn' | 'crit' | 'ice' }) {
  const txt = tone === 'safe' ? 'text-safe' : tone === 'warn' ? 'text-warn' : tone === 'crit' ? 'text-crit' : 'text-ice'
  return (
    <div className="rounded-lg border border-line bg-bg/40 p-3">
      <div className="font-mono text-[10px] uppercase tracking-wider text-faint">{label}</div>
      <div className={`mt-1 font-display text-[18px] font-semibold leading-none ${txt}`}>{value}</div>
    </div>
  )
}
