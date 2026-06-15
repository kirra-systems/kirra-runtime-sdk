// Live-data configuration. The console talks to a real Kirra verifier service
// when NEXT_PUBLIC_KIRRA_API_URL is set at build time; otherwise it runs on the
// bundled mock data ("demo" mode) so the public deploy always works.
//
// The endpoints used here (/health, /fleet/posture) are the verifier's
// PUBLIC read-only tier — no token required, so nothing secret is shipped to
// the client. The auth-gated SSE stream is intentionally NOT consumed from the
// browser; the live event feed is derived from posture-change polling instead.

export const API_BASE = (process.env.NEXT_PUBLIC_KIRRA_API_URL ?? '').replace(/\/+$/, '')
export const IS_LIVE = API_BASE.length > 0
