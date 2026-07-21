/** @type {import('next').NextConfig} */

// Security headers for every response. The CSP is the safe-without-nonces
// subset (Next's app router injects inline bootstrap scripts, so a full
// script-src lockdown needs nonce plumbing — tracked in PRODUCTION.md):
// clickjacking, object embedding, MIME sniffing and base hijacking are all
// closed here; the console makes no third-party requests at runtime.
const securityHeaders = [
  { key: 'X-Content-Type-Options', value: 'nosniff' },
  { key: 'X-Frame-Options', value: 'DENY' },
  { key: 'Referrer-Policy', value: 'strict-origin-when-cross-origin' },
  { key: 'Permissions-Policy', value: 'camera=(), microphone=(), geolocation=()' },
  {
    key: 'Content-Security-Policy',
    value: "frame-ancestors 'none'; object-src 'none'; base-uri 'self'; form-action 'self'",
  },
]

// Two build modes:
//   • default (server)  — the production console: standalone server, live
//     verifier proxy, security headers. `npm run build` / `npm start`.
//   • KIRRA_STATIC_DEMO — a pure static export for GitHub Pages hosting: no
//     server, no proxy, demo data only, served under /console. Built by
//     scripts/build-demo.mjs (which also removes the proxy route, since route
//     handlers can't be statically exported). Headers there are set by the
//     hosting layer, not Next.
const STATIC_DEMO = process.env.KIRRA_STATIC_DEMO === '1'

const nextConfig = STATIC_DEMO
  ? {
      reactStrictMode: true,
      poweredByHeader: false,
      output: 'export',
      basePath: '/console',
      assetPrefix: '/console',
      trailingSlash: true,
      images: { unoptimized: true },
      env: { NEXT_PUBLIC_KIRRA_STATIC_DEMO: '1' },
    }
  : {
      reactStrictMode: true,
      poweredByHeader: false,
      output: 'standalone',
      async headers() {
        return [{ source: '/:path*', headers: securityHeaders }]
      },
    }

export default nextConfig
