'use client'

import { useEffect } from 'react'

// Route-segment error boundary: a rendering failure in one screen must never
// blank the whole console. The operator gets a branded, actionable recovery
// surface; the error itself goes to the console (and any attached reporter).
export default function RouteError({ error, reset }: { error: Error & { digest?: string }; reset: () => void }) {
  useEffect(() => {
    console.error(JSON.stringify({ src: 'kirra-console', kind: 'route-error', digest: error.digest, message: error.message }))
  }, [error])
  return (
    <div className="grid min-h-[60vh] place-items-center p-6">
      <div className="max-w-md rounded-xl border border-line bg-panel p-6 text-center shadow-panel">
        <p className="font-mono text-[10px] uppercase tracking-[0.2em] text-crit">screen failed to render</p>
        <h2 className="mt-2 font-display text-lg font-semibold text-ink">This view hit an error.</h2>
        <p className="mt-2 text-[13px] text-muted">
          The rest of the console is unaffected. Retry the view, or navigate elsewhere — nothing about the
          fleet itself has changed.
        </p>
        {error.digest && <p className="mt-2 font-mono text-[10px] text-faint">digest {error.digest}</p>}
        <button
          onClick={reset}
          className="mt-4 rounded-lg border border-line bg-elevated px-4 py-2 text-[13px] text-ink hover:border-line-strong"
        >
          Retry this view
        </button>
      </div>
    </div>
  )
}
