// Deterministic formatting for anything that can be server-rendered.
//
// Locale/timezone-dependent formatting (bare toLocaleTimeString etc.) renders
// one string on the build machine and a different one in the operator's
// browser → React hydration mismatches and, worse, ambiguous timestamps.
// Fleet operations read UTC; these formatters pin locale AND zone so the same
// ms value always renders the same text everywhere.
const timeFmt = new Intl.DateTimeFormat('en-GB', {
  timeZone: 'UTC',
  hour12: false,
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
})
const dateTimeFmt = new Intl.DateTimeFormat('en-GB', {
  timeZone: 'UTC',
  hour12: false,
  year: 'numeric',
  month: 'short',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
})

export const utcTime = (ms: number) => timeFmt.format(new Date(ms))
export const utcDateTime = (ms: number) => `${dateTimeFmt.format(new Date(ms))} UTC`

/** Fixed epoch for demo-mode timestamps (2026-07-01T12:00:00Z): demo data must
    be byte-stable across server render and hydration, so it never derives from
    the wall clock. */
export const DEMO_EPOCH = 1782907200000
