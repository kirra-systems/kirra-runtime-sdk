'use client'

import { Search, Bell, ChevronDown } from 'lucide-react'
import { MobileNav } from '@/components/shell/mobile-nav'
import { ConnectionStatus } from '@/components/shell/connection-status'

function KMark() {
  // Trust-lattice "K" — mirrors the marketing-site logo: a constellation of
  // nodes meshed into the letter with a glowing central vertex.
  const nodes: Record<string, [number, number]> = {
    s0: [20, 12], s1: [20, 23], c: [23, 33], s3: [20, 43], s4: [20, 53],
    a1: [35, 22], a0: [47, 11], m: [40, 30], l1: [35, 42], l0: [49, 54],
  }
  const edges: [string, string][] = [
    ['s0', 's1'], ['s1', 'c'], ['c', 's3'], ['s3', 's4'],
    ['c', 'a1'], ['a1', 'a0'], ['s1', 'a1'], ['s0', 'a1'], ['c', 'a0'],
    ['a1', 'm'], ['c', 'm'], ['m', 'l1'],
    ['c', 'l1'], ['l1', 'l0'], ['m', 'l0'], ['s3', 'l1'],
  ]
  const dim = ['s0', 's1', 's3', 's4', 'a1', 'a0', 'm', 'l1', 'l0']
  return (
    <svg viewBox="0 0 64 64" className="h-7 w-7" aria-hidden="true">
      <defs>
        <radialGradient id="k-ctr" cx="50%" cy="50%" r="50%">
          <stop offset="0%" stopColor="#cdf6ff" stopOpacity="0.9" />
          <stop offset="45%" stopColor="#5cc6ff" stopOpacity="0.5" />
          <stop offset="100%" stopColor="#5cc6ff" stopOpacity="0" />
        </radialGradient>
        <filter id="k-blur" x="-60%" y="-60%" width="220%" height="220%">
          <feGaussianBlur stdDeviation="1.4" />
        </filter>
      </defs>
      <g stroke="#46d6cf" strokeWidth="1.3" strokeLinecap="round" opacity="0.6">
        {edges.map(([a, b], i) => {
          const [x1, y1] = nodes[a]
          const [x2, y2] = nodes[b]
          return <line key={i} x1={x1} y1={y1} x2={x2} y2={y2} />
        })}
      </g>
      <g fill="#5fd9d2">
        {dim.map((k) => {
          const [x, y] = nodes[k]
          return <circle key={k} cx={x} cy={y} r="1.9" />
        })}
      </g>
      <circle cx={nodes.c[0]} cy={nodes.c[1]} r="7" fill="url(#k-ctr)" filter="url(#k-blur)" />
      <circle cx={nodes.c[0]} cy={nodes.c[1]} r="3.4" fill="none" stroke="#5cc6ff" strokeOpacity="0.6" strokeWidth="0.8" />
      <circle cx={nodes.c[0]} cy={nodes.c[1]} r="2.6" fill="#d6f6ff" />
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
      <div className="ml-2 hidden sm:block"><ConnectionStatus /></div>
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
