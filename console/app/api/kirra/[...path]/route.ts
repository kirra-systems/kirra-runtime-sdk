import { type NextRequest, NextResponse } from 'next/server'

export const runtime = 'nodejs'
export const dynamic = 'force-dynamic'

// Server-side proxy to the Kirra verifier. Keeps the admin token / client-id on
// the server (never shipped to the browser), removes CORS (same-origin), and
// lets the console read admin- and identity-gated endpoints. Read-only: only
// GET, and only an allow-listed set of verifier read paths are forwarded.
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
  /^system\/posture\/stream$/,
]

function demo() {
  return NextResponse.json({ status: 'demo' }, { status: 503, headers: { 'x-kirra-mode': 'demo' } })
}

export async function GET(req: NextRequest, { params }: { params: Promise<{ path: string[] }> }) {
  const base = process.env.KIRRA_API_URL?.replace(/\/+$/, '')
  if (!base) return demo()

  const { path } = await params
  const rel = (path ?? []).join('/')
  if (!ALLOW.some((re) => re.test(rel))) {
    return NextResponse.json({ error: 'path not allowed' }, { status: 403 })
  }

  const headers: Record<string, string> = { accept: req.headers.get('accept') ?? 'application/json' }
  if (process.env.KIRRA_ADMIN_TOKEN) headers.authorization = `Bearer ${process.env.KIRRA_ADMIN_TOKEN}`
  if (process.env.KIRRA_CLIENT_ID) headers['x-kirra-client-id'] = process.env.KIRRA_CLIENT_ID

  try {
    const upstream = await fetch(`${base}/${rel}${req.nextUrl.search}`, {
      method: 'GET',
      headers,
      cache: 'no-store',
      signal: req.signal,
    })
    // Pass the body straight through (works for JSON and text/event-stream).
    const out = new Headers()
    const ct = upstream.headers.get('content-type')
    if (ct) out.set('content-type', ct)
    out.set('cache-control', 'no-store')
    return new NextResponse(upstream.body, { status: upstream.status, headers: out })
  } catch (e) {
    return NextResponse.json({ error: 'verifier unreachable', detail: String((e as Error)?.message ?? e) }, { status: 502 })
  }
}
