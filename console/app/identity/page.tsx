'use client'

import { useState } from 'react'
import { Panel } from '@/components/ui/primitives'
import {
  decodeRelationsOutcome,
  PROVENANCE_PRESENTATION,
  RelationsContractError,
  type RelationsOutcome,
} from '@/lib/world/relations'

// Read-only view of what Kirra World currently considers the same asset.
//
// SCOPE, deliberately small: it answers one operator question — "what is this
// the same as, who decided, and can that decision still be explained" — and
// nothing else. There is no adjudication control here. Deciding identity is
// KIRRA-WM-PROMOTION-001 territory and requires an authenticated operator; a
// console button would be the second way to do it and the unauthenticated one.
//
// Every fetch goes to the console's own server route, never to Kirra World.
// See app/api/world/relations/[subject]/route.ts for why.

type State =
  | { kind: 'idle' }
  | { kind: 'loading' }
  | { kind: 'answered'; outcome: RelationsOutcome }
  | { kind: 'broken'; detail: string }

export default function IdentityPage() {
  const [subject, setSubject] = useState('')
  const [state, setState] = useState<State>({ kind: 'idle' })

  async function look(e: React.FormEvent) {
    e.preventDefault()
    const q = subject.trim()
    if (!q) return
    setState({ kind: 'loading' })
    try {
      const res = await fetch(`/api/world/relations/${encodeURIComponent(q)}`, {
        cache: 'no-store',
      })
      // A non-2xx still carries the contract's own tagged body, so it is
      // decoded rather than turned into a generic error — that is what keeps
      // "no relations" and "the service is down" distinguishable.
      setState({ kind: 'answered', outcome: decodeRelationsOutcome(await res.json()) })
    } catch (err) {
      // A CONTRACT failure is not a transport failure and must not be shown as
      // one: it means this console and Kirra World disagree about the shape of
      // the world, and rendering a partial view would be the collapse the
      // decoder refused.
      setState({
        kind: 'broken',
        detail:
          err instanceof RelationsContractError
            ? `${err.message} — this console will not render a view it cannot fully decode.`
            : 'Kirra World could not be reached.',
      })
    }
  }

  return (
    <div className="mx-auto max-w-[1100px] space-y-6 p-6">
      <div>
        <h1 className="font-display text-xl font-semibold text-ink">Asset Identity</h1>
        <p className="font-mono text-[11px] text-faint">
          kirra world · read-only · promoted same_as relations
        </p>
      </div>

      <Panel title="Look up an asset" subtitle="what is this the same as?">
        <form onSubmit={look} className="flex flex-wrap gap-2">
          <input
            value={subject}
            onChange={(e) => setSubject(e.target.value)}
            placeholder="track-a"
            aria-label="Asset identifier"
            className="min-w-[16rem] flex-1 rounded border border-white/10 bg-black/30 px-3 py-2 font-mono text-sm text-ink outline-none focus:border-white/30"
          />
          <button
            type="submit"
            className="rounded border border-white/15 bg-white/5 px-4 py-2 font-mono text-sm text-ink hover:bg-white/10"
          >
            Look up
          </button>
        </form>
      </Panel>

      {state.kind === 'loading' && (
        <Panel title="Asking Kirra World">
          <p className="font-mono text-[12px] text-faint">…</p>
        </Panel>
      )}

      {state.kind === 'broken' && (
        <Panel title="No answer">
          <p className="text-sm text-rose-300">{state.detail}</p>
        </Panel>
      )}

      {state.kind === 'answered' && <Answer outcome={state.outcome} />}
    </div>
  )
}

function Answer({ outcome }: { outcome: RelationsOutcome }) {
  if (outcome.outcome === 'not_an_entity') {
    return (
      <Panel title="Not an asset identity">
        {/* Distinct from "related to nothing" on purpose: told the latter, an
            operator concludes the asset exists. */}
        <p className="text-sm text-amber-300">{outcome.reason}</p>
      </Panel>
    )
  }
  if (outcome.outcome === 'unavailable') {
    return (
      <Panel title="Kirra World unavailable">
        <p className="text-sm text-rose-300">{outcome.reason}</p>
        <p className="mt-2 font-mono text-[11px] text-faint">
          This is not an answer about the asset. Nothing here says whether it has relations.
        </p>
      </Panel>
    )
  }

  const { view } = outcome
  if (view.related.length === 0) {
    return (
      <Panel title={`${view.subject} — no relations`}>
        <p className="text-sm text-ink">
          Kirra World holds no promoted <span className="font-mono">same_as</span> relation for this
          asset.
        </p>
      </Panel>
    )
  }

  return (
    <Panel
      title={`${view.subject} — ${view.related.length} relation${view.related.length === 1 ? '' : 's'}`}
      subtitle="adjudicated by an operator"
    >
      <div className="space-y-3">
        {view.related.map((r) => {
          const p = PROVENANCE_PRESENTATION[r.provenance]
          return (
            <div
              key={`${r.low}|${r.high}`}
              className="rounded border border-white/10 bg-black/20 p-3"
            >
              <div className="flex flex-wrap items-baseline justify-between gap-2">
                <div className="font-mono text-sm text-ink">
                  same as <span className="font-semibold">{r.other}</span>
                </div>
                {/* The provenance badge. Glyph + LABEL, never colour alone —
                    the distinction has to survive greyscale, a screenshot and
                    a reader who cannot separate the hues. */}
                <span
                  className={`rounded border px-2 py-0.5 font-mono text-[11px] ${p.tone}`}
                  title={p.sentence}
                >
                  <span aria-hidden="true">{p.glyph}</span> {p.label}
                </span>
              </div>
              <p className="mt-1 text-[12px] text-faint">{p.sentence}</p>
              <div className="mt-2 grid grid-cols-1 gap-x-6 gap-y-1 font-mono text-[11px] text-faint sm:grid-cols-2">
                <div>
                  decided by <span className="text-ink">{r.adjudicator}</span>
                </div>
                <div>
                  decision <span className="text-ink">{r.decision_marker}</span>
                </div>
              </div>
            </div>
          )
        })}
        {view.truncated && (
          <p className="font-mono text-[11px] text-amber-300">
            More relations exist than are shown here.
          </p>
        )}
      </div>
    </Panel>
  )
}
