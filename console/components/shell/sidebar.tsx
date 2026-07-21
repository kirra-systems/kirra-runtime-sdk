'use client'

import Link from 'next/link'
import { usePathname } from 'next/navigation'
import { cn } from '@/lib/utils'
import { navGroups } from './nav-items'
import { useHealth } from '@/lib/api/hooks'

// Honest footer: derived from the actual verifier binding, never a hardcoded
// "all systems nominal". Demo mode says so; live mode reports reachability.
function SidebarStatus() {
  const { status } = useHealth()
  const view =
    status === 'ok'
      ? { dot: 'bg-safe', label: 'Verifier connected', sub: 'live control plane' }
      : status === 'demo'
        ? { dot: 'bg-warn', label: 'Demo mode', sub: 'simulated fleet — not evidence' }
        : status === 'connecting'
          ? { dot: 'bg-warn', label: 'Connecting…', sub: 'reaching verifier' }
          : { dot: 'bg-crit', label: 'Backend offline', sub: 'verifier unreachable' }
  return (
    <div className="border-t border-line p-3">
      <div className="rounded-lg border border-line bg-panel p-3">
        <div className="flex items-center gap-2">
          <span className={cn('h-2 w-2 rounded-full', view.dot)} />
          <span className="font-mono text-[11px] text-muted">{view.label}</span>
        </div>
        <p className="mt-1 font-mono text-[10px] text-faint">{view.sub} · console v0.1.0</p>
      </div>
    </div>
  )
}

export function Sidebar() {
  const path = usePathname()
  return (
    <aside className="hidden h-full w-[244px] shrink-0 flex-col border-r border-line bg-surface lg:flex">
      <nav aria-label="Console sections" className="scrollbar-thin flex-1 overflow-y-auto px-3 py-4">
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
                      aria-current={active ? 'page' : undefined}
                      className={cn(
                        'group flex items-center gap-3 rounded-lg px-3 py-2 text-[13px] transition-colors',
                        active ? 'bg-ink/[0.06] text-ink' : 'text-muted hover:bg-ink/[0.03] hover:text-ink',
                      )}
                    >
                      <Icon
                        className={cn('h-4 w-4', active ? 'text-ice' : 'text-faint group-hover:text-muted')}
                        strokeWidth={1.75}
                      />
                      <span>{it.label}</span>
                      {active ? (
                        <span className="ml-auto h-1.5 w-1.5 rounded-full bg-ice" />
                      ) : it.preview ? (
                        <span className="ml-auto rounded-sm bg-warn/10 px-1 py-0.5 font-mono text-[8px] uppercase tracking-wider text-warn/80">
                          preview
                        </span>
                      ) : null}
                    </Link>
                  </li>
                )
              })}
            </ul>
          </div>
        ))}
      </nav>
      <SidebarStatus />
    </aside>
  )
}
