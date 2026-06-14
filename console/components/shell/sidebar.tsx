'use client'

import Link from 'next/link'
import { usePathname } from 'next/navigation'
import { cn } from '@/lib/utils'
import { LayoutDashboard, Boxes, ShieldCheck, Activity, Radio, Route, Brain, Bell, AlertTriangle, FileCheck2, BarChart3, Database, FileText, Settings } from 'lucide-react'

const groups = [
  {
    label: 'Operations',
    items: [
      { href: '/', label: 'Overview', icon: LayoutDashboard },
      { href: '/fleet', label: 'Fleet Operations', icon: Boxes },
      { href: '/safety', label: 'Safety Governor', icon: ShieldCheck },
      { href: '/runtime', label: 'Runtime Health', icon: Activity },
      { href: '/telemetry', label: 'Telemetry', icon: Radio },
      { href: '/missions', label: 'Mission Timeline', icon: Route },
    ],
  },
  {
    label: 'Governance',
    items: [
      { href: '/oversight', label: 'AI Oversight', icon: Brain },
      { href: '/events', label: 'Events', icon: Bell },
      { href: '/incidents', label: 'Incident Review', icon: AlertTriangle },
      { href: '/compliance', label: 'Compliance', icon: FileCheck2 },
    ],
  },
  {
    label: 'Insight',
    items: [
      { href: '/analytics', label: 'Analytics', icon: BarChart3 },
      { href: '/explorer', label: 'Telemetry Explorer', icon: Database },
      { href: '/reports', label: 'Reports', icon: FileText },
      { href: '/settings', label: 'Settings', icon: Settings },
    ],
  },
]

export function Sidebar() {
  const path = usePathname()
  return (
    <aside className="hidden h-full w-[244px] shrink-0 flex-col border-r border-line bg-surface lg:flex">
      <nav className="scrollbar-thin flex-1 overflow-y-auto px-3 py-4">
        {groups.map((g) => (
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
