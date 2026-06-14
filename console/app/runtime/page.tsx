import { Panel, Pill, Meter, StatusDot } from '@/components/ui/primitives'
import { Spark } from '@/components/charts/charts'
import { LatencyLines } from '@/components/charts/extra'
import { resources, latency, network, partitions, nodes, ddsTopics, ddsPeers } from '@/lib/runtime'
import type { Tone } from '@/lib/types'

export default function RuntimePage() {
  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Runtime Health</h1>
          <p className="font-mono text-[11px] text-faint">compute · transport · isolation</p>
        </div>
        <Pill tone="safe">All partitions enforced</Pill>
      </div>

      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        {resources.map((r) => (
          <div key={r.label} className="rounded-xl border border-line bg-panel p-4 shadow-panel">
            <div className="flex items-center justify-between">
              <span className="font-mono text-[11px] uppercase tracking-wider text-faint">{r.label}</span>
              <span className={`font-mono text-[11px] ${txt(r.tone)}`}>{r.pct}%</span>
            </div>
            <div className="mt-2 font-display text-2xl font-semibold text-ink">{r.pct}<span className="text-sm text-muted">%</span></div>
            <div className="mt-3"><Meter value={r.pct} tone={r.tone} /></div>
            <p className="mt-2 font-mono text-[10px] text-faint">{r.detail}</p>
          </div>
        ))}
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="DDS Latency" subtitle="p50 / p99 · ms · FTTI budget 50ms" action={<Pill tone="ice">live</Pill>}>
          <LatencyLines data={latency} height={210} />
        </Panel>
        <Panel title="Network Health">
          <div className="space-y-4">
            {network.map((n) => (
              <div key={n.label}>
                <div className="flex items-center justify-between">
                  <span className="font-mono text-[11px] uppercase tracking-wider text-faint">{n.label}</span>
                  <span className="font-mono text-xs text-ink">{n.value}<span className="text-muted"> {n.unit}</span></span>
                </div>
                <div className="mt-1"><Spark data={n.spark} color={n.tone === 'muted' ? 'ice' : n.tone} /></div>
              </div>
            ))}
          </div>
        </Panel>
      </div>

      {/* ── DDS / Network Health (Drop 6) ── */}
      <div className="grid grid-cols-1 gap-6 xl:grid-cols-3">
        <Panel className="xl:col-span-2" title="DDS Topic Health" subtitle="deadline budget vs observed p99 · Volatile durability enforced" dense action={<Pill tone="safe">QoS: Volatile</Pill>}>
          <div className="overflow-x-auto">
            <table className="w-full min-w-[640px] text-left">
              <thead>
                <tr className="border-b border-line font-mono text-[10px] uppercase tracking-wider text-faint">
                  <th className="px-4 py-2 font-normal">Topic</th>
                  <th className="px-4 py-2 font-normal">QoS</th>
                  <th className="px-4 py-2 font-normal">Pub/Sub</th>
                  <th className="px-4 py-2 font-normal">Deadline</th>
                  <th className="px-4 py-2 font-normal">Observed</th>
                  <th className="px-4 py-2 font-normal">Miss rate</th>
                </tr>
              </thead>
              <tbody className="font-mono text-[12px]">
                {ddsTopics.map((d) => (
                  <tr key={d.topic} className="border-b border-line last:border-0 hover:bg-white/[0.02]">
                    <td className="px-4 py-2.5 text-ink">{d.topic}</td>
                    <td className="px-4 py-2.5 text-ice">{d.qos}</td>
                    <td className="px-4 py-2.5 text-faint">{d.pubs}/{d.subs}</td>
                    <td className="px-4 py-2.5 text-muted">{d.deadlineMs} ms</td>
                    <td className={`px-4 py-2.5 ${d.observedMs > d.deadlineMs * 0.85 ? 'text-warn' : 'text-muted'}`}>{d.observedMs} ms</td>
                    <td className={`px-4 py-2.5 ${txt(d.tone)}`}>{d.missRate.toFixed(1)}%</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </Panel>

        <Panel title="Participant Discovery" subtitle="DDS liveliness matrix" dense>
          <ul>
            {ddsPeers.map((p) => (
              <li key={p.id} className="flex items-center gap-3 border-b border-line px-4 py-3 last:border-0">
                <StatusDot tone={p.tone} pulse={p.liveliness === 'LOST'} />
                <div className="min-w-0 flex-1">
                  <div className="truncate font-mono text-[12px] text-ink">{p.id}</div>
                  <div className="font-mono text-[10px] text-faint">{p.role}</div>
                </div>
                <div className="text-right">
                  <div className={`font-mono text-[11px] ${p.liveliness === 'LOST' ? 'text-crit' : 'text-muted'}`}>{p.liveliness}</div>
                  <div className="font-mono text-[10px] text-faint">{p.rttMs} ms · {p.lastSeen}</div>
                </div>
              </li>
            ))}
          </ul>
        </Panel>
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-2">
        <Panel title="Hypervisor & Partition Isolation" subtitle="QNX safety partition · isolated guests" dense>
          <ul>
            {partitions.map((p) => (
              <li key={p.name} className="flex items-center gap-4 border-b border-line px-4 py-3 last:border-0">
                <StatusDot tone={p.tone} />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center justify-between gap-3">
                    <span className="truncate text-[13px] text-ink">{p.name}</span>
                    <Pill tone={p.tone}>{p.isolation}</Pill>
                  </div>
                  <div className="mt-2 flex items-center gap-3">
                    <span className="font-mono text-[10px] text-faint">CPU budget</span>
                    <div className="flex-1"><Meter value={p.cpuBudget} tone={p.cpuBudget > 85 ? 'warn' : 'safe'} /></div>
                    <span className="w-9 text-right font-mono text-[10px] text-muted">{p.cpuBudget}%</span>
                  </div>
                  <p className="mt-1 font-mono text-[10px] text-faint">{p.role}</p>
                </div>
              </li>
            ))}
          </ul>
        </Panel>

        <Panel title="Node Health" subtitle={`${nodes.length} compute nodes`} dense>
          <div className="grid grid-cols-2 gap-px bg-line sm:grid-cols-3">
            {nodes.map((n) => (
              <div key={n.id} className="bg-panel p-3">
                <div className="flex items-center justify-between">
                  <span className="font-mono text-[11px] text-ink">{n.id}</span>
                  <StatusDot tone={n.tone} />
                </div>
                <div className="mt-2 space-y-1.5 font-mono text-[10px] text-faint">
                  <Row2 k="cpu" v={`${n.cpu}%`} />
                  <Row2 k="mem" v={`${n.mem}%`} />
                  <Row2 k="temp" v={`${n.tempC}°C`} />
                </div>
              </div>
            ))}
          </div>
        </Panel>
      </div>
    </div>
  )
}

function txt(t: Tone) { return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted' }
function Row2({ k, v }: { k: string; v: string }) {
  return (
    <div className="flex items-center justify-between">
      <span>{k}</span>
      <span className="text-muted">{v}</span>
    </div>
  )
}
