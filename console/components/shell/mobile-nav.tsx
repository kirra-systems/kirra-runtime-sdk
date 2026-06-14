'use client'

import { useEffect, useState } from 'react'
import Link from 'next/link'
import { usePathname } from 'next/navigation'
import { Menu, X } from 'lucide-react'
import { cn } from '@/lib/utils'
import { navGroups } from './nav-items'

// Mobile navigation: a hamburger button + slide-in drawer, shown only below the
// `lg` breakpoint (where the desktop sidebar is hidden). Closes on navigation,
// on backdrop tap, and on Escape.
export function MobileNav() {
  const [open, setOpen] = useState(false)
  const path = usePathname()

  useEffect(() => { setOpen(false) }, [path])

  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') setOpen(false) }
    document.addEventListener('keydown', onKey)
    document.body.style.overflow = 'hidden'
    return () => { document.removeEventListener('keydown', onKey); document.body.style.overflow = '' }
  }, [open])

  return (
    <>
      <button
        onClick={() => setOpen(true)}
        aria-label="Open navigation menu"
        className="rounded-lg border border-line bg-panel p-2 text-muted hover:text-ink lg:hidden"
      >
        <Menu className="h-4 w-4" />
      </button>

      {open && (
        <div className="fixed inset-0 z-50 lg:hidden" role="dialog" aria-modal="true" aria-label="Navigation">
          <div className="absolute inset-0 bg-black/60 backdrop-blur-sm" onClick={() => setOpen(false)} />
          <aside className="absolute left-0 top-0 flex h-full w-[270px] max-w-[82%] flex-col border-r border-line bg-surface shadow-panel">
            <div className="flex h-14 shrink-0 items-center justify-between border-b border-line px-4">
              <div className="leading-none">
                <div className="font-display text-[15px] font-semibold tracking-[3px] text-ink">KIRRA</div>
                <div className="font-mono text-[9px] uppercase tracking-[2px] text-faint">Mission Console</div>
              </div>
              <button onClick={() => setOpen(false)} aria-label="Close navigation menu" className="rounded-lg border border-line bg-panel p-2 text-muted hover:text-ink">
                <X className="h-4 w-4" />
              </button>
            </div>

            <nav className="scrollbar-thin flex-1 overflow-y-auto px-3 py-4">
              {navGroups.map((g) => (
                <div key={g.label} className="mb-5">
                  <p className="px-3 pb-2 font-mono text-[10px] uppercase tracking-[0.2em] text-faint">{g.label}</p>
                  <ul className="space-y-0.5">
                    {g.items.map((it) => {
                      const active = path === it.href
                      const Icon = it.icon
                      return (
                        <li key={it.href}>
                          <Link
                            href={it.href}
                            onClick={() => setOpen(false)}
                            className={cn('group flex items-center gap-3 rounded-lg px-3 py-2.5 text-[13px] transition-colors', active ? 'bg-white/[0.06] text-ink' : 'text-muted hover:bg-white/[0.03] hover:text-ink')}
                          >
                            <Icon className={cn('h-4 w-4', active ? 'text-safe' : 'text-faint group-hover:text-muted')} strokeWidth={1.75} />
                            <span>{it.label}</span>
                            {active && <span className="ml-auto h-1.5 w-1.5 rounded-full bg-safe" />}
                          </Link>
                        </li>
                      )
                    })}
                  </ul>
                </div>
              ))}
            </nav>

            <div className="border-t border-line p-3">
              <div className="flex items-center gap-2">
                <span className="h-2 w-2 rounded-full bg-safe" />
                <span className="font-mono text-[11px] text-muted">All systems nominal</span>
              </div>
            </div>
          </aside>
        </div>
      )}
    </>
  )
}
