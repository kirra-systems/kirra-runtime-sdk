// Deterministic PRNG (mulberry32) for demo data generation.
//
// Demo series are generated at module scope and rendered by server components,
// so they MUST be byte-identical between the server render and the client
// hydration pass — `Math.random()` there produces React hydration mismatches
// (minified error #418). Every mock module derives its own stream from a
// distinct seed.
export function seededRand(seed: number): () => number {
  let s = seed | 0
  return () => {
    s = (s + 0x6d2b79f5) | 0
    let t = Math.imul(s ^ (s >>> 15), 1 | s)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}
