// The console reads the verifier through a same-origin server proxy
// (app/api/kirra/[...path]). Whether live data is available is decided at
// REQUEST time by the proxy (server env KIRRA_API_URL) — not at build time — so
// there is no NEXT_PUBLIC flag and nothing backend-specific in the bundle.
// When the proxy reports demo mode (header x-kirra-mode: demo / 503), the hooks
// fall back to bundled mock data.

export const PROXY_BASE = '/api/kirra'
