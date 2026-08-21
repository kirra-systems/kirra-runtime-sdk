import { type NextRequest, NextResponse } from 'next/server'

export const runtime = 'nodejs'
export const dynamic = 'force-dynamic'

// Server-side proxy to kirra-world-explain-service.
//
//   Browser → THIS ROUTE → kirra-world-explain-service (loopback) → QueryEngine
//
// THE BROWSER NEVER TALKS TO KIRRA WORLD. That is the whole point of the route
// existing rather than the page fetching the World service directly.
//
// The World process has no authentication of its own — it refuses a
// non-loopback bind unless KIRRA_WORLD_EXPLAIN_ALLOW_NONLOCAL=1 precisely
// because of that. Pointing a browser at it would mean setting that flag, which
// converts an on-box service into an exposed one to save a hop. The console
// already owns browser-facing auth, session handling, origin policy and future
// access control; this route is where that ownership is exercised.
//
// Server env (NOT NEXT_PUBLIC — stays server-side):
//   KIRRA_WORLD_URL   base URL of the World explain service. Unset → demo mode.
//                     Expected to be loopback; see the note above.

const UPSTREAM_TIMEOUT_MS = 10_000

// A subject is one path segment. The World service refuses an encoded or
// multi-segment subject, and this refuses the same shapes BEFORE the hop —
// fail-closed at the edge, so a malformed request never reaches the World
// process at all. The two refusals agreeing is not duplication: this one keeps
// junk off the wire, and the World one is what holds if anything else ever
// calls it.
const SUBJECT = /^[A-Za-z0-9._:-]{1,128}$/

function log(fields: Record<string, unknown>) {
  console.log(JSON.stringify({ ts: new Date().toISOString(), src: 'kirra-world-proxy', ...fields }))
}

export async function GET(
  req: NextRequest,
  { params }: { params: Promise<{ subject: string }> },
) {
  const started = Date.now()
  const reqId = crypto.randomUUID()
  const { subject } = await params

  const respond = (res: NextResponse, extra?: Record<string, unknown>) => {
    res.headers.set('x-request-id', reqId)
    log({ reqId, subject, status: res.status, ms: Date.now() - started, ...extra })
    return res
  }

  if (!SUBJECT.test(subject ?? '')) {
    // The World service's own vocabulary, so the console renders one shape
    // whether the refusal happened here or upstream.
    return respond(
      NextResponse.json(
        { outcome: 'not_an_entity', reason: 'the subject must be one unencoded path segment' },
        { status: 400 },
      ),
    )
  }

  const base = process.env.KIRRA_WORLD_URL?.replace(/\/+$/, '')
  if (!base) {
    return respond(
      NextResponse.json(
        { outcome: 'unavailable', reason: 'Kirra World is not configured for this console' },
        { status: 503, headers: { 'x-kirra-mode': 'demo' } },
      ),
      { mode: 'demo' },
    )
  }

  try {
    const upstream = await fetch(`${base}/relations/${subject}`, {
      method: 'GET',
      headers: { accept: 'application/json' },
      cache: 'no-store',
      signal: AbortSignal.any([req.signal, AbortSignal.timeout(UPSTREAM_TIMEOUT_MS)]),
    })
    const body = await upstream.text()
    // Passed through verbatim: the World service already speaks the tagged
    // outcome the console decodes, and re-wrapping it here would create a
    // second place the contract is expressed.
    return respond(
      new NextResponse(body, {
        status: upstream.status,
        headers: { 'content-type': 'application/json', 'cache-control': 'no-store' },
      }),
      { upstream: upstream.status },
    )
  } catch (e) {
    const detail = String((e as Error)?.message ?? e)
    const timedOut = (e as Error)?.name === 'TimeoutError' || detail.includes('timeout')
    // Detail stays in the server log. The client gets the contract's own
    // `unavailable` case — never something it could mistake for an answer.
    return respond(
      NextResponse.json(
        {
          outcome: 'unavailable',
          reason: timedOut ? 'Kirra World did not respond in time' : 'Kirra World is unreachable',
        },
        { status: timedOut ? 504 : 502 },
      ),
      { error: detail },
    )
  }
}
