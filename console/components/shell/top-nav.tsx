'use client'

import { Search, Bell, ChevronDown } from 'lucide-react'
import { Pill } from '@/components/ui/primitives'
import { MobileNav } from '@/components/shell/mobile-nav'

function KMark() {
  return (
    <svg viewBox="0 0 64 64" className="h-7 w-7" aria-hidden="true">
      <g stroke="#2fe6a6" strokeWidth="3" strokeLinecap="round" opacity="0.9">
        <line x1="20" y1="12" x2="20" y2="52" />
        <line x1="20" y1="32" x2="46" y2="12" />
        <line x1="20" y1="32" x2="46" y2="52" />
      </g>
      <g fill="#2fe6a6">
        <circle cx="20" cy="12" r="3" />
        <circle cx="20" cy="52" r="3" />
        <circle cx="46" cy="12" r="3" />
        <circle cx="46" cy="52" r="3" />
      </g>
      <circle cx="20" cy="32" r="5" fill="#5cc6ff" />
    </svg>
  )
}

export function TopNav() {
  return (
    <header className="sticky top-0 z-30 flex h-14 items-center gap-3 border-b border-line bg-bg/70 px-4 backdrop-blur-xl">
      <MobileNav />
      <div className="flex items-center gap-2.5">
        <KMark />
        <div className="leading-none">
          <div className="font-display text-[15px] font-semibold tracking-[3px] text-ink">KIRRA</div>
          <div className="font-mono text-[9px] uppercase tracking-[2px] text-faint">Mission Console</div>
        </div>
      </div>
      <div className="ml-2 hidden sm:block"><Pill tone="safe">Governor Online</Pill></div>
      <button className="ml-1 hidden items-center gap-2 rounded-lg border border-line bg-panel px-3 py-1.5 font-mono text-[11px] text-muted hover:text-ink xl:flex">
        PRODUCTION · us-fleet-1 <ChevronDown className="h-3.5 w-3.5 text-faint" />
      </button>
      <div className="relative ml-auto hidden w-72 items-center md:flex">
        <Search className="absolute left-3 h-4 w-4 text-faint" />
        <input placeholder="Search robots, missions, events…" className="w-full rounded-lg border border-line bg-panel py-2 pl-9 pr-3 text-[13px] text-ink outline-none placeholder:text-faint focus:border-line-strong" />
      </div>
      <button className="relative ml-auto rounded-lg border border-line bg-panel p-2 text-muted hover:text-ink md:ml-0">
        <Bell className="h-4 w-4" />
        <span className="absolute right-1.5 top-1.5 h-1.5 w-1.5 rounded-full bg-warn" />
      </button>
      <div className="flex items-center gap-2 rounded-lg border border-line bg-panel py-1 pl-1 pr-2.5">
        <div className="grid h-7 w-7 place-items-center rounded-md bg-gradient-to-br from-safe/30 to-ice/20 font-display text-xs font-semibold text-ink">JL</div>
        <div className="hidden leading-none sm:block">
          <div className="text-[12px] text-ink">J. Looney</div>
          <div className="font-mono text-[10px] text-faint">Operator · Admin</div>
        </div>
      </div>
    </header>
  )
}
