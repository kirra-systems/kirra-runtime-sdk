'use client'

import Link from 'next/link'
import { usePathname } from 'next/navigation'
import { cn } from '@/lib/utils'
import { navGroups } from './nav-items'

export function Sidebar() {
  const path = usePathname()
  return (
    <aside className="hidden h-full w-[244px] shrink-0 flex-col border-r border-line bg-surface lg:flex">
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
                    <Link href={it.href} className={cn('group flex items-center gap-3 rounded-lg px-3 py-2 text-[13px] transition-colors', active ? 'bg-white/[0.06] text-ink' : 'text-muted hover:bg-white/[0.03] hover:text-ink')}>
                      <Icon className={cn('h-4 w-4', active ? 'text-safe' : 'text-faint group-hover:text-muted')} strokeWidth={1.75} />
                      <span>{it.label}</span>
                      {active ? (
                        <span className="ml-auto h-1.5 w-1.5 rounded-full bg-safe" />
                      ) : it.preview ? (
                        <span className="ml-auto rounded-sm bg-warn/10 px-1 py-0.5 font-mono text-[8px] uppercase tracking-wider text-warn/80">preview</span>
                      ) : null}
                    </Link>
                  </li>
                )
              })}
            </ul>
          </div>
        ))}
      </nav>
      <div className="border-t border-line p-3">
        <div className="rounded-lg border border-line bg-panel p-3">
          <div className="flex items-center gap-2">
            <span className="h-2 w-2 rounded-full bg-safe" />
            <span className="font-mono text-[11px] text-muted">All systems nominal</span>
          </div>
          <p className="mt-1 font-mono text-[10px] text-faint">v1.2.0 · region us-fleet-1</p>
        </div>
      </div>
    </aside>
  )
}
