'use client'

// Last-resort boundary: catches failures in the root layout itself. Renders a
// bare-HTML recovery page (no layout, no fonts — those may be what failed).
export default function GlobalError({ error, reset }: { error: Error & { digest?: string }; reset: () => void }) {
  return (
    <html lang="en">
      <body style={{ background: '#05070d', color: '#e8eef7', fontFamily: 'monospace', display: 'grid', placeItems: 'center', minHeight: '100vh', margin: 0 }}>
        <div style={{ textAlign: 'center', maxWidth: 420, padding: 24 }}>
          <p style={{ color: '#f0655d', letterSpacing: '0.2em', fontSize: 11 }}>CONSOLE FAILED TO RENDER</p>
          <p style={{ fontSize: 14, color: '#a7b4c6' }}>
            The console shell hit an unrecoverable error. This affects the UI only — the verifier and fleet
            are not controlled by this surface.
          </p>
          {error.digest && <p style={{ fontSize: 10, color: '#8a97ab' }}>digest {error.digest}</p>}
          <button
            onClick={reset}
            style={{ marginTop: 16, background: 'transparent', color: '#3fd9c9', border: '1px solid #2a3a55', borderRadius: 8, padding: '8px 16px', cursor: 'pointer' }}
          >
            Reload console
          </button>
        </div>
      </body>
    </html>
  )
}
