import { AlertOctagon } from 'lucide-react'
import { Panel, Pill, StatusDot } from '@/components/ui/primitives'
import { DemoBadge } from '@/components/ui/demo-badge'
import { config, flags, operatorActions, operatorSessions, roster } from '@/lib/settings'
import type { Tone } from '@/lib/types'

export default function SettingsPage() {
  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">System & Operator Control</h1>
          <p className="font-mono text-[11px] text-faint">configuration · feature flags · operator tools · access</p>
        </div>
        <div className="flex items-center gap-2">
          <DemoBadge live={false} />
          <Pill tone="safe">fail-closed · governor gates all actions</Pill>
        </div>
      </div>

      {/* ── Operator Tools (#12) ── */}
      {/* Governed-request catalog — NOT live triggers. The console is QM-domain
          and holds no actuator authority; each item is an authenticated REQUEST
          routed to the fail-closed Governor (never console→actuator). Rendered
          as non-interactive cards (no onClick) — the wired operator→governor
          request path is tracked in #412. */}
      <Panel title="Operator Tools" subtitle="human-in-the-loop · authenticated requests routed to the Governor — never a direct actuator command">
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
          {operatorActions.map((a) => (
            <div
              key={a.id}
              className={`rounded-xl border p-4 text-left ${a.critical ? 'border-crit/40 bg-crit/[0.06]' : 'border-line bg-panel'}`}
            >
              <div className="flex items-center justify-between">
                <span className={`font-display text-[14px] font-semibold ${txt(a.tone)}`}>{a.name}</span>
                {a.critical && <AlertOctagon className="h-4 w-4 text-crit" />}
              </div>
              <p className="mt-2 text-[11px] leading-snug text-muted">{a.desc}</p>
              <span className="mt-3 inline-block rounded-sm bg-white/5 px-1.5 py-0.5 font-mono text-[9px] uppercase tracking-wider text-faint">request → governor</span>
            </div>
          ))}
        </div>
        <p className="mt-4 border-t border-line pt-3 font-mono text-[10px] leading-relaxed text-faint">
          Display-only in this build. Operator intent enters as an authenticated, signed request to the
          fail-closed Governor, which adjudicates and commands the MRC under its own authority — the
          console never touches an actuator. Wired request path tracked in #412.
        </p>

        <div className="mt-5 border-t border-line pt-4">
          <div className="mb-2 font-mono text-[10px] uppercase tracking-wider text-faint">Active operator sessions</div>
          <ul className="space-y-2">
            {operatorSessions.map((s) => (
              <li key={s.asset} className="flex items-center gap-3 rounded-lg border border-line bg-bg/40 px-3 py-2">
                <StatusDot tone={s.tone} pulse={s.tone === 'crit'} />
                <span className="font-mono text-[12px] text-ink">{s.asset}</span>
                <span className="text-[12px] text-muted">{s.mode}</span>
                <span className="ml-auto font-mono text-[10px] text-faint">{s.operator} · since {s.since}</span>
              </li>
            ))}
          </ul>
        </div>
      </Panel>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-2">
        <Panel title="System Configuration" subtitle="environment & runtime · secrets redacted" dense>
          <div className="overflow-x-auto">
            <table className="w-full min-w-[520px] text-left">
              <thead>
                <tr className="border-b border-line font-mono text-[10px] uppercase tracking-wider text-faint">
                  <th className="px-4 py-2 font-normal">Key</th>
                  <th className="px-4 py-2 font-normal">Value</th>
                  <th className="px-4 py-2 font-normal">Scope</th>
                </tr>
              </thead>
              <tbody className="font-mono text-[12px]">
                {config.map((c) => (
                  <tr key={c.key} className="border-b border-line last:border-0 hover:bg-white/[0.02]">
                    <td className="px-4 py-2.5 text-ink">{c.key}</td>
                    <td className={`px-4 py-2.5 ${txt(c.tone)}`}>{c.value}</td>
                    <td className="px-4 py-2.5">
                      <span className="rounded bg-white/5 px-1.5 py-0.5 text-[10px] uppercase text-muted">{c.scope}</span>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </Panel>

        <Panel title="Feature Flags" subtitle="runtime capability toggles" dense>
          <ul>
            {flags.map((f) => (
              <li key={f.name} className="flex items-center gap-4 border-b border-line px-4 py-3 last:border-0">
                <Toggle on={f.enabled} />
                <div className="min-w-0 flex-1">
                  <div className="truncate text-[12px] text-ink">{f.name}</div>
                  <div className="font-mono text-[10px] text-faint">{f.note}</div>
                </div>
                <span className={`font-mono text-[10px] uppercase tracking-wider ${f.enabled ? 'text-safe' : 'text-faint'}`}>{f.enabled ? 'on' : 'off'}</span>
              </li>
            ))}
          </ul>
        </Panel>
      </div>

      <Panel title="Access Control" subtitle="operators & service accounts" dense>
        <div className="overflow-x-auto">
          <table className="w-full min-w-[640px] text-left">
            <thead>
              <tr className="border-b border-line font-mono text-[10px] uppercase tracking-wider text-faint">
                <th className="px-4 py-2 font-normal">Member</th>
                <th className="px-4 py-2 font-normal">Role</th>
                <th className="px-4 py-2 font-normal">Access</th>
                <th className="px-4 py-2 font-normal">Status</th>
              </tr>
            </thead>
            <tbody className="text-[12px]">
              {roster.map((m) => (
                <tr key={m.name} className="border-b border-line last:border-0 hover:bg-white/[0.02]">
                  <td className="px-4 py-2.5 text-ink">{m.name}</td>
                  <td className="px-4 py-2.5 text-muted">{m.role}</td>
                  <td className="px-4 py-2.5 font-mono text-[11px] text-faint">{m.access}</td>
                  <td className="px-4 py-2.5">
                    <span className="flex items-center gap-1.5 font-mono text-[11px]">
                      <StatusDot tone={m.tone} />
                      <span className={m.status === 'active' ? 'text-ink' : 'text-faint'}>{m.status}</span>
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Panel>
    </div>
  )
}

function Toggle({ on }: { on: boolean }) {
  return (
    <span className={`relative inline-flex h-4 w-7 shrink-0 items-center rounded-full transition-colors ${on ? 'bg-safe/40' : 'bg-white/10'}`}>
      <span className={`inline-block h-3 w-3 transform rounded-full transition-transform ${on ? 'translate-x-3.5 bg-safe' : 'translate-x-0.5 bg-muted'}`} />
    </span>
  )
}

function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
