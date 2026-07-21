import type { Tone } from '@/lib/types'

// System Topology Map — a node-link view of the runtime: the QNX Governor
// partition at the trust core, isolated guests, the DDS bus, and edge assets.

export interface TopoNode { id: string; label: string; sub?: string; x: number; y: number; tone: Tone; core?: boolean }
export interface TopoEdge { from: string; to: string; tone: Tone }

const stroke: Record<string, string> = { safe: 'var(--c-safe)', warn: 'var(--c-warn)', crit: 'var(--c-crit)', ice: 'var(--c-ice)', muted: 'var(--c-faint)' }

export function TopologyMap({ nodes, edges, height = 340 }: { nodes: TopoNode[]; edges: TopoEdge[]; height?: number }) {
  const byId = (id: string) => nodes.find((n) => n.id === id)!
  return (
    <svg viewBox="0 0 100 70" style={{ height }} className="w-full" role="img" aria-label="system topology">
      {/* edges */}
      {edges.map((e, i) => {
        const a = byId(e.from), b = byId(e.to)
        return <line key={i} x1={a.x} y1={a.y} x2={b.x} y2={b.y} stroke={stroke[e.tone]} strokeOpacity={e.tone === 'crit' ? 0.6 : 0.28} strokeWidth={e.tone === 'crit' ? 0.6 : 0.4} strokeDasharray={e.tone === 'crit' ? '1.5 1' : undefined} />
      })}
      {/* nodes */}
      {nodes.map((n) => (
        <g key={n.id}>
          {n.core && <circle cx={n.x} cy={n.y} r="8.4" fill="none" stroke={stroke[n.tone]} strokeOpacity="0.25" strokeWidth="0.4" />}
          <circle cx={n.x} cy={n.y} r={n.core ? 6 : 4.2} fill="rgba(16,20,30,0.95)" stroke={stroke[n.tone]} strokeWidth={n.core ? 0.8 : 0.5} />
          <circle cx={n.x} cy={n.y} r="1.1" fill={stroke[n.tone]} />
          <text x={n.x} y={n.y + (n.core ? 9.6 : 7)} fill="var(--c-bright)" fontSize="2.5" fontFamily="monospace" textAnchor="middle">{n.label}</text>
          {n.sub && <text x={n.x} y={n.y + (n.core ? 12.4 : 9.6)} fill="var(--c-faint)" fontSize="2" fontFamily="monospace" textAnchor="middle">{n.sub}</text>}
        </g>
      ))}
    </svg>
  )
}
