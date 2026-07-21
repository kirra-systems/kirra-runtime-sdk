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

const nextConfig = {
  reactStrictMode: true,
  poweredByHeader: false,
  // Standalone output → a self-contained server bundle for container deploys.
  output: 'standalone',
  async headers() {
    return [{ source: '/:path*', headers: securityHeaders }]
  },
}
export default nextConfig
