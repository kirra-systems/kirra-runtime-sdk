'use client'

import { useEffect, useState, type ReactNode } from 'react'
import { X, Copy, Check } from 'lucide-react'

// Tap-to-expand raw-entry modal — for "quick raw log extraction" from forensic
// tables (audit ledger, governor decisions, incidents). Shows the full entry as
// pretty-printed JSON (or raw text) with copy-to-clipboard; closes on Escape or
// backdrop click. Self-contained client component.
export function RawModal({ title, subtitle, data, onClose }: {
  title: string
  subtitle?: string
  data: unknown
  onClose: () => void
}) {
  const [copied, setCopied] = useState(false)
  const text = typeof data === 'string' ? data : JSON.stringify(data, null, 2)

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose() }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [onClose])

  const copy = async () => {
    try { await navigator.clipboard.writeText(text); setCopied(true); setTimeout(() => setCopied(false), 1500) } catch { /* clipboard blocked */ }
  }

  return (
    <div className="fixed inset-0 z-50 flex items-end justify-center p-0 sm:items-center sm:p-4" role="dialog" aria-modal="true" aria-label={title}>
      <div className="absolute inset-0 bg-black/70 backdrop-blur-sm" onClick={onClose} />
      <div className="relative z-10 flex max-h-[80vh] w-full max-w-[640px] flex-col rounded-t-2xl border border-line bg-panel shadow-panel sm:rounded-xl">
        <div className="flex items-start justify-between gap-4 border-b border-line px-4 py-3">
          <div className="min-w-0">
            <div className="truncate font-display text-[15px] font-semibold text-ink">{title}</div>
            {subtitle && <div className="truncate font-mono text-[10px] text-faint">{subtitle}</div>}
          </div>
          <div className="flex shrink-0 items-center gap-1.5">
            <button
              onClick={copy}
              className="flex items-center gap-1.5 rounded-lg border border-line px-2.5 py-1 font-mono text-[10px] uppercase tracking-wider text-faint hover:bg-ink/[0.04] hover:text-ink"
            >
              {copied ? <Check className="h-3 w-3 text-safe" /> : <Copy className="h-3 w-3" />}
              {copied ? 'copied' : 'copy'}
            </button>
            <button onClick={onClose} aria-label="close" className="flex h-7 w-7 items-center justify-center rounded-lg border border-line text-faint hover:bg-ink/[0.04] hover:text-ink">
              <X className="h-3.5 w-3.5" />
            </button>
          </div>
        </div>
        <pre className="scrollbar-thin overflow-auto px-4 py-3 font-mono text-[11px] leading-relaxed text-muted">{text}</pre>
      </div>
    </div>
  )
}

// Manages a single raw-modal selection for a page. Rows call `open({...})`;
// render `{modal}` once near the page root.
export function useRawModal(): {
  open: (entry: { title: string; subtitle?: string; data: unknown }) => void
  modal: ReactNode
} {
  const [entry, setEntry] = useState<{ title: string; subtitle?: string; data: unknown } | null>(null)
  return {
    open: setEntry,
    modal: entry ? <RawModal title={entry.title} subtitle={entry.subtitle} data={entry.data} onClose={() => setEntry(null)} /> : null,
  }
}
