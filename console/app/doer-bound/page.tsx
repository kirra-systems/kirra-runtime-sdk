import { Panel, Pill, StatusDot, Stat } from '@/components/ui/primitives'
import { DemoBadge } from '@/components/ui/demo-badge'
import { TrajectoryPlot, type DoerPath } from '@/components/ui/trajectory-plot'
import { doerBound, toneFor, blockedDoer } from '@/lib/doer-bound'

export default function DoerBoundPage() {
  const { blocked, clear, egoX, goalX, corridorHalfWidth, intent } = doerBound
  const occy = blockedDoer('occy')
  const reckless = blockedDoer('reckless')
  const recklessClear = clear.doers[0]

  const blockedPaths: DoerPath[] = [
    { label: 'occy', path: occy.path, tone: toneFor(occy.admitted) },
    { label: 'reckless', path: reckless.path, tone: toneFor(reckless.admitted) },
  ]
  const clearPaths: DoerPath[] = [
    { label: 'reckless', path: recklessClear.path, tone: toneFor(recklessClear.admitted) },
  ]

  return (
    <div className="mx-auto max-w-[1500px] space-y-6 p-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold text-ink">Doer Bound</h1>
          <p className="font-mono text-[11px] text-faint">
            KIRRA bounds any planner behind the seam · real #131-checker verdicts
          </p>
        </div>
        <div className="flex items-center gap-2">
          <DemoBadge live={false} />
          <Pill tone="ice">intent: {intent}</Pill>
          <Pill tone="safe">checker: fail-closed</Pill>
        </div>
      </div>

      {/* Thesis */}
      <Panel title="The invariant bound" subtitle="same Mick intent · same world · two doers">
        <p className="text-[13px] leading-relaxed text-muted">
          The planner is a swappable <span className="text-ink">doer</span> behind one generic seam.
          Mick proposes the intent <span className="font-mono text-ice">{intent}</span>; the world (corridor,
          obstacle, envelope) comes from perception. Whatever the doer proposes, <span className="text-ink">KIRRA
          is the bound</span>: the careful planner (Occy) is admitted, and a reckless / learned stand-in that
          drives straight through the obstacle is <span className="text-crit">rejected</span> — then the
          always-available safe-stop takes over. No unsafe trajectory reaches the actuator, whoever authored it.
        </p>
      </Panel>

      {/* Bird's-eye comparison */}
      <Panel
        title="Blocked road"
        subtitle={`stopped car at x=${blocked.obstacleX} m · ego at x=${egoX} · goal at x=${goalX}`}
        action={
          <div className="flex items-center gap-3 font-mono text-[10px] uppercase tracking-wider text-faint">
            <span className="flex items-center gap-1"><i className="inline-block h-2 w-2 rounded-full" style={{ background: '#2fe6a6' }} />occy · admitted</span>
            <span className="flex items-center gap-1"><i className="inline-block h-2 w-2 rounded-full" style={{ background: '#ff5468' }} />reckless · rejected</span>
          </div>
        }
      >
        <TrajectoryPlot paths={blockedPaths} egoX={egoX} goalX={goalX} halfWidth={corridorHalfWidth} obstacleX={blocked.obstacleX} />
      </Panel>

      {/* Per-doer verdicts */}
      <div className="grid grid-cols-1 gap-6 lg:grid-cols-3">
        <Panel title="Occy — reference planner" subtitle="careful, safety-aware">
          <div className="flex items-center justify-between">
            <span className="font-display text-2xl font-semibold text-safe">{occy.verdict}</span>
            <StatusDot tone={toneFor(occy.admitted)} />
          </div>
          <div className="mt-3 grid grid-cols-2 gap-3">
            <Stat label="reach" value={occy.reach_m.toFixed(1)} unit="m" tone="ice" />
            <Stat label="vs obstacle" value={`stops short`} tone="safe" />
          </div>
          <p className="mt-3 text-[12px] leading-relaxed text-muted">
            Stops short of the car well before reaching it — KIRRA admits the plan.
          </p>
        </Panel>

        <Panel title="RecklessDoer — black-box stand-in" subtitle="ignores obstacle / corridor / lane">
          <div className="flex items-center justify-between">
            <span className="font-display text-2xl font-semibold text-crit">{reckless.verdict}</span>
            <StatusDot tone={toneFor(reckless.admitted)} pulse />
          </div>
          <div className="mt-3 grid grid-cols-2 gap-3">
            <Stat label="reach" value={reckless.reach_m.toFixed(1)} unit="m" tone="warn" />
            <Stat label="vs obstacle" value={`drives through`} tone="crit" />
          </div>
          <p className="mt-3 text-[12px] leading-relaxed text-muted">
            Drives straight through the car. KIRRA <span className="text-crit">rejects</span> it — the same
            bound that admitted Occy.
          </p>
        </Panel>

        <Panel title="On rejection → safe-stop" subtitle="the always-available fallback">
          <div className="flex items-center justify-between">
            <span className="font-display text-2xl font-semibold text-safe">{blocked.fallback.verdict}</span>
            <StatusDot tone={toneFor(blocked.fallback.admitted)} />
          </div>
          <p className="mt-3 text-[12px] leading-relaxed text-muted">
            When a proposal is vetoed, the architecture falls back to a controlled safe-stop the checker
            accepts. The unsafe intent never actuates.
          </p>
        </Panel>
      </div>

      {/* Precision */}
      <Panel
        title="Clear road — the bound is precise"
        subtitle="the very same RecklessDoer, no obstacle"
        action={<Pill tone={toneFor(recklessClear.admitted)}>{recklessClear.verdict}</Pill>}
      >
        <TrajectoryPlot paths={clearPaths} egoX={egoX} goalX={goalX} halfWidth={corridorHalfWidth} />
        <p className="mt-3 text-[12px] leading-relaxed text-muted">
          With nothing to hit, the reckless doer is <span className="text-safe">admitted</span> — KIRRA rejects
          unsafe <span className="text-ink">outputs</span>, not the doer itself. That precision is what makes
          &ldquo;swap in a learned planner, the safety case is unchanged&rdquo; a real claim rather than blanket paranoia.
        </p>
      </Panel>
    </div>
  )
}
