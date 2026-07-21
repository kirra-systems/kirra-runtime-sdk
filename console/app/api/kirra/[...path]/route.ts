import { type NextRequest, NextResponse } from 'next/server'

export const runtime = 'nodejs'
export const dynamic = 'force-dynamic'

// Server-side proxy to the Kirra verifier. Keeps the admin token / client-id on
// the server (never shipped to the browser), removes CORS (same-origin), and
// lets the console read admin- and identity-gated endpoints. Read-only: only
// GET, and only an allow-listed set of verifier read paths are forwarded.
//
// Production hardening (all server-side, none of it reaches the bundle):
//   - upstream timeout (UPSTREAM_TIMEOUT_MS); the SSE stream path is exempt
//     from the read timeout (long-lived by design) but still aborts when the
//     client disconnects
//   - per-IP sliding-window rate limit (in-memory; per-instance — front a
//     multi-replica deployment with an edge limiter, see PRODUCTION.md)
//   - structured JSON request logs with a request id, echoed to the client
//     as x-request-id for support correlation
//   - upstream error details are logged server-side and NEVER returned to the
//     client (generic 502/504 body only)
//
// Server env (NOT NEXT_PUBLIC — stays server-side):
//   KIRRA_API_URL       base URL of the verifier (unset → demo mode)
//   KIRRA_ADMIN_TOKEN   Bearer token for admin/identity-gated reads
//   KIRRA_CLIENT_ID     x-kirra-client-id for identity-gated reads

const ALLOW: RegExp[] = [
  /^health$/, /^ready$/,
  /^fleet\/posture$/, /^fleet\/posture\/[^/]+$/,
  /^fleet\/history\/[^/]+$/, /^fleet\/flapping\/[^/]+$/,
  /^attestation\/status\/[^/]+$/,
  /^federation\/reports\/[^/]+$/,
  /^system\/audit\/verify$/, /^system\/audit\/causal\/verify$/, /^system\/audit\/export$/,
  /^fabric\/(assets|state|telemetry|causal-log)$/, /^fabric\/telemetry\/[^/]+$/, /^fabric\/causal-log\/[^/]+$/,
  /^console\/(fleet|audit|escalations)$/,
  /^console\/(runtime|analytics|sites|versions)$/,
  /^system\/posture\/stream$/,
]

const STREAM_PATH = /^system\/posture\/stream$/
const UPSTREAM_TIMEOUT_MS = 10_000
const MAX_QUERY_LENGTH = 512

// Per-IP sliding window: RATE_LIMIT requests per RATE_WINDOW_MS, per instance.
const RATE_LIMIT = 240
const RATE_WINDOW_MS = 60_000
const rateBuckets = new Map<string, number[]>()
function rateLimited(ip: string): boolean {
  const now = Date.now()
  const hits = (rateBuckets.get(ip) ?? []).filter((t) => now - t < RATE_WINDOW_MS)
  if (hits.length >= RATE_LIMIT) {
    rateBuckets.set(ip, hits)
    return true
  }
  hits.push(now)
  rateBuckets.set(ip, hits)
  // Bound the map so an address scan can't grow memory unboundedly.
  if (rateBuckets.size > 10_000) {
    for (const key of rateBuckets.keys()) {
      if ((rateBuckets.get(key) ?? []).every((t) => now - t >= RATE_WINDOW_MS)) rateBuckets.delete(key)
    }
  }
  return false
}

function log(fields: Record<string, unknown>) {
  // Single-line structured JSON on stdout — pick up with any log shipper.
  console.log(JSON.stringify({ ts: new Date().toISOString(), src: 'kirra-proxy', ...fields }))
}

function demo(reqId: string) {
  return NextResponse.json(
    { status: 'demo' },
    { status: 503, headers: { 'x-kirra-mode': 'demo', 'x-request-id': reqId } },
  )
}

export async function GET(req: NextRequest, { params }: { params: Promise<{ path: string[] }> }) {
  const started = Date.now()
  const reqId = crypto.randomUUID()
  const ip = req.headers.get('x-forwarded-for')?.split(',')[0]?.trim() ?? 'local'
  const { path } = await params
  const rel = (path ?? []).join('/')

  const respond = (res: NextResponse, extra?: Record<string, unknown>) => {
    res.headers.set('x-request-id', reqId)
    log({ reqId, path: rel, status: res.status, ms: Date.now() - started, ...extra })
    return res
  }

  if (rateLimited(ip)) {
    return respond(
      NextResponse.json({ error: 'rate limited' }, { status: 429, headers: { 'retry-after': '10' } }),
      { rateLimited: true },
    )
  }

  const base = process.env.KIRRA_API_URL?.replace(/\/+$/, '')
  if (!base) return respond(demo(reqId), { mode: 'demo' })

  if (!ALLOW.some((re) => re.test(rel))) {
    return respond(NextResponse.json({ error: 'path not allowed' }, { status: 403 }))
  }
  if (req.nextUrl.search.length > MAX_QUERY_LENGTH) {
    return respond(NextResponse.json({ error: 'query too long' }, { status: 414 }))
  }

  const headers: Record<string, string> = { accept: req.headers.get('accept') ?? 'application/json' }
  if (process.env.KIRRA_ADMIN_TOKEN) headers.authorization = `Bearer ${process.env.KIRRA_ADMIN_TOKEN}`
  if (process.env.KIRRA_CLIENT_ID) headers['x-kirra-client-id'] = process.env.KIRRA_CLIENT_ID

  // The SSE stream is long-lived: only the client's own disconnect aborts it.
  // Every other read gets a hard upstream timeout so a hung verifier can't
  // pin console requests open.
  const isStream = STREAM_PATH.test(rel)
  const signal = isStream ? req.signal : AbortSignal.any([req.signal, AbortSignal.timeout(UPSTREAM_TIMEOUT_MS)])

  try {
    const upstream = await fetch(`${base}/${rel}${req.nextUrl.search}`, {
      method: 'GET',
      headers,
      cache: 'no-store',
      signal,
    })
    // Pass the body straight through (works for JSON and text/event-stream).
    const out = new Headers()
    const ct = upstream.headers.get('content-type')
    if (ct) out.set('content-type', ct)
    out.set('cache-control', 'no-store')
    return respond(new NextResponse(upstream.body, { status: upstream.status, headers: out }), {
      upstream: upstream.status,
      stream: isStream || undefined,
    })
  } catch (e) {
    const detail = String((e as Error)?.message ?? e)
    const timedOut = (e as Error)?.name === 'TimeoutError' || detail.includes('timeout')
    // Detail stays in the server log; the client gets a generic body + reqId.
    return respond(
      NextResponse.json(
        { error: timedOut ? 'verifier timeout' : 'verifier unreachable', reqId },
        { status: timedOut ? 504 : 502 },
      ),
      { error: detail },
    )
  }
}
