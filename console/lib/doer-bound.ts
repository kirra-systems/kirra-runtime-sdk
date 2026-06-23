import data from './doer-bound.json'
import type { Tone } from './types'

// "KIRRA bounds a black-box doer" demo data.
//
// NOT hand-drawn: every number here is the real verdict of the #131 checker on a
// real planner output, emitted by the Rust pipeline and committed as a fixture.
// Regenerate with:
//   cargo run -p kirra-planner --example doer_bound_fixture > console/lib/doer-bound.json
// (bundled rather than live because the checker is Rust and this console deploys as
// a static frontend — hence the page's "SIMULATED DATA" provenance badge.)

export interface DoerResult {
  doer: string
  reach_m: number
  verdict: string
  admitted: boolean
  path: [number, number][]
}

export interface DoerBoundFixture {
  intent: string
  egoX: number
  goalX: number
  corridorHalfWidth: number
  blocked: {
    obstacleX: number
    doers: DoerResult[]
    fallback: { verdict: string; admitted: boolean }
  }
  clear: { doers: DoerResult[] }
}

export const doerBound = data as unknown as DoerBoundFixture

/** Verdict colour: admitted → green, rejected → red. */
export const toneFor = (admitted: boolean): Tone => (admitted ? 'safe' : 'crit')

export const blockedDoer = (name: string): DoerResult =>
  doerBound.blocked.doers.find((d) => d.doer === name)!
