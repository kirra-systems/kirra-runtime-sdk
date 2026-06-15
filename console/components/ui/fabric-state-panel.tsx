'use client'

import { Panel, Pill } from '@/components/ui/primitives'
import { useFabricState } from '@/lib/api/hooks'
import { postureTone } from '@/lib/api/types'
import type { Tone } from '@/lib/types'

// Per-asset fabric governance state (GET /fabric/state, admin via the proxy):
// the asset's posture in the cross-asset fabric DAG. Self-contained client panel
// so the server-rendered twin page stays a server component. Admin-gated, so it
// shows demo data unless the deploy is configured with a verifier token.
export function FabricStatePanel({ nodeId }: { nodeId: string }) {
  const { state, fabricGen, source } = useFabricState(nodeId)
  const tone: Tone = state.inFabric ? postureTone(state.posture) : 'muted'

  return (
    <Panel
      title="Fabric State"
      subtitle={source === 'live' ? `live · GET /fabric/state · gen ${fabricGen}` : 'demo · cross-asset DAG'}
      action={source === 'live' ? <Pill tone="safe">live</Pill> : <Pill tone="ice">demo</Pill>}
    >
      {state.inFabric ? (
        <div className="space-y-3">
          <div className="flex items-center justify-between">
            <span className="font-mono text-[11px] uppercase tracking-wider text-faint">Fabric posture</span>
            <span className={`font-mono text-[12px] ${txt(tone)}`}>{state.posture}</span>
          </div>
          <KV k="Asset generation" v={`${state.generation}`} />
          <KV k="Contributing nodes" v={state.contributingNodes.length ? state.contributingNodes.join(', ') : '—'} />
          <div className="flex items-center justify-between border-t border-line pt-3">
            <span className="font-mono text-[11px] uppercase tracking-wider text-faint">Blocked by</span>
            <span className={`font-mono text-[12px] ${state.blockedBy.length ? 'text-crit' : 'text-muted'}`}>
              {state.blockedBy.length ? state.blockedBy.join(', ') : 'nothing'}
            </span>
          </div>
        </div>
      ) : (
        <p className="py-2 font-mono text-[11px] text-faint">asset not registered in the fabric</p>
      )}
    </Panel>
  )
}

function KV({ k, v }: { k: string; v: string }) {
  return (
    <div className="flex items-center justify-between gap-3">
      <span className="font-mono text-[11px] uppercase tracking-wider text-faint">{k}</span>
      <span className="truncate font-mono text-xs text-ink">{v}</span>
    </div>
  )
}

function txt(t: Tone) {
  return t === 'safe' ? 'text-safe' : t === 'warn' ? 'text-warn' : t === 'crit' ? 'text-crit' : t === 'ice' ? 'text-ice' : 'text-muted'
}
