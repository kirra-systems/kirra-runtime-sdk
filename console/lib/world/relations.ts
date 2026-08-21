// The Kirra World relationship contract, hand-written and conformance-tested.
//
// THIS IS A SECOND DEFINITION of a contract whose source of truth is Rust
// (`crates/kirra-explain-types`). That is a real risk and it is answered, not
// waved at: `contracts/world_relations_v1.json` is emitted by a Rust test and
// decoded by `relations.test.mjs` with the decoder below. A Rust change that
// adds, removes, renames or reshapes a field reds one test on each side.
//
// Rust→TypeScript generation was considered and deliberately deferred. It adds
// a codegen subsystem before anyone knows whether Kirra World will have one JS
// consumer or twenty; the fixture buys the property that matters now and can be
// replaced by generation later without either side having built against it.

/** The contract version this console understands. Bumping it in Rust must be a
 *  deliberate act here too — the conformance test pins the number. */
export const RELATIONS_VIEW_VERSION = 1

/**
 * How well the evidence behind a relation still resolves.
 *
 * Four states, and they are NOT a severity scale. Every one of them describes
 * the EXPLANATION; none qualifies whether the relation holds. That separation
 * is KIRRA-WM-EVIDENCE-RETENTION-001, and it is the reason the UI may never
 * collapse a weaker state into a stronger one.
 */
export type ProvenanceStanding = 'resolved' | 'degraded' | 'dangling' | 'plural'

const PROVENANCE_STATES: readonly string[] = ['resolved', 'degraded', 'dangling', 'plural']

/** One pair the subject is currently adjudicated the same as. */
export interface RelatedPair {
  low: string
  high: string
  /** The OTHER entity — the one that is not the subject asked about. */
  other: string
  adjudicator: string
  /** Opaque. Correlates with the audit record; no endpoint accepts it back. */
  decision_marker: string
  provenance: ProvenanceStanding
}

export interface RelationsView {
  subject: string
  related: RelatedPair[]
  /** More relations exist than this page carries. Carried, never inferred. */
  truncated: boolean
}

export type RelationsOutcome =
  | { outcome: 'related'; view: RelationsView }
  | { outcome: 'not_an_entity'; reason: string }
  | { outcome: 'unavailable'; reason: string }

/** Thrown when a payload does not match the contract. Never swallowed. */
export class RelationsContractError extends Error {
  constructor(message: string) {
    super(`world relations contract: ${message}`)
    this.name = 'RelationsContractError'
  }
}

function str(o: Record<string, unknown>, key: string, where: string): string {
  const v = o[key]
  if (typeof v !== 'string') throw new RelationsContractError(`${where}.${key} is not a string`)
  return v
}

/**
 * Decode one payload, FAIL-CLOSED.
 *
 * An unknown provenance state THROWS. It is not mapped to a fallback, and that
 * is the tightening this contract exists to carry: a newly-added Rust state
 * must break this console until somebody decides how it is presented, because
 * the alternative is a new evidence state silently rendering as whatever the
 * fallback happened to be — and the fallback would be a stronger claim than the
 * truth in at least one direction.
 *
 * Unknown OUTCOME tags throw for the same reason. Unknown extra FIELDS are
 * tolerated: Rust's `deny_unknown_fields` governs what the producer accepts,
 * whereas an extra field arriving here means the producer is ahead of this
 * console, and refusing to render a relation because a field we do not use
 * appeared would take the console down for an additive change.
 */
export function decodeRelationsOutcome(raw: unknown): RelationsOutcome {
  if (typeof raw !== 'object' || raw === null) {
    throw new RelationsContractError('payload is not an object')
  }
  const o = raw as Record<string, unknown>
  const tag = o.outcome
  switch (tag) {
    case 'related': {
      const v = o.view
      if (typeof v !== 'object' || v === null) {
        throw new RelationsContractError('view is not an object')
      }
      const view = v as Record<string, unknown>
      if (typeof view.truncated !== 'boolean') {
        throw new RelationsContractError('view.truncated is not a boolean')
      }
      if (!Array.isArray(view.related)) {
        throw new RelationsContractError('view.related is not an array')
      }
      const related = view.related.map((entry, i) => {
        if (typeof entry !== 'object' || entry === null) {
          throw new RelationsContractError(`view.related[${i}] is not an object`)
        }
        const r = entry as Record<string, unknown>
        const where = `view.related[${i}]`
        const provenance = r.provenance
        if (typeof provenance !== 'string' || !PROVENANCE_STATES.includes(provenance)) {
          throw new RelationsContractError(
            `${where}.provenance is ${JSON.stringify(provenance)}, which this console does not ` +
              `know how to present. A new evidence state must be given a presentation ` +
              `deliberately — it may not fall back, because every fallback is a stronger or ` +
              `weaker claim than the truth.`,
          )
        }
        return {
          low: str(r, 'low', where),
          high: str(r, 'high', where),
          other: str(r, 'other', where),
          adjudicator: str(r, 'adjudicator', where),
          decision_marker: str(r, 'decision_marker', where),
          provenance: provenance as ProvenanceStanding,
        }
      })
      return { outcome: 'related', view: { subject: str(view, 'subject', 'view'), related, truncated: view.truncated } }
    }
    case 'not_an_entity':
      return { outcome: 'not_an_entity', reason: str(o, 'reason', 'payload') }
    case 'unavailable':
      return { outcome: 'unavailable', reason: str(o, 'reason', 'payload') }
    default:
      throw new RelationsContractError(
        `unknown outcome ${JSON.stringify(tag)} — refusing rather than guessing`,
      )
  }
}

/**
 * How each provenance state is presented.
 *
 * THE LOAD-BEARING UI INVARIANT: the console may simplify presentation, but it
 * may never collapse a weaker provenance state into a stronger one. `degraded`
 * must not read as `resolved`; `dangling` must not read as merely "no details";
 * `plural` must not display one carrier as if it were authoritative.
 *
 * Every state therefore carries its own label, its own sentence and its own
 * glyph. Colour is deliberately NOT the carrier of the distinction — it does
 * not survive greyscale, screenshots, or a reader who cannot distinguish the
 * hues — so the text alone is sufficient to tell the four apart.
 */
export const PROVENANCE_PRESENTATION: Record<
  ProvenanceStanding,
  { label: string; glyph: string; sentence: string; tone: string }
> = {
  resolved: {
    label: 'Evidence available',
    glyph: '●',
    sentence: 'The supporting evidence for this decision is present in the record.',
    tone: 'text-emerald-300 border-emerald-500/40 bg-emerald-500/10',
  },
  degraded: {
    label: 'Evidence compacted',
    glyph: '◐',
    sentence:
      'The relationship remains valid as adjudicated. Some supporting evidence has been compacted away, so the explanation is incomplete.',
    tone: 'text-amber-300 border-amber-500/40 bg-amber-500/10',
  },
  dangling: {
    label: 'Evidence unresolvable',
    glyph: '○',
    sentence:
      'The cited evidence cannot be resolved, and no compaction accounts for it. Do not read this as evidence that was successfully observed.',
    tone: 'text-rose-300 border-rose-500/40 bg-rose-500/10',
  },
  plural: {
    label: 'Evidence ambiguous',
    glyph: '◎',
    sentence:
      'Several records match the citation. No single one of them is authoritative, and none is shown as if it were.',
    tone: 'text-sky-300 border-sky-500/40 bg-sky-500/10',
  },
}
