'use client'

import { useEffect, useMemo, useRef, useState } from 'react'
import { useRouter } from 'next/navigation'
import Link from 'next/link'
import { Search, Bell, Sun, Moon } from 'lucide-react'
import { MobileNav } from '@/components/shell/mobile-nav'
import { ConnectionStatus } from '@/components/shell/connection-status'
import { navGroups } from '@/components/shell/nav-items'
import { useHealth } from '@/lib/api/hooks'

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
          <stop offset="0%" stopColor="var(--c-bright)" stopOpacity="0.9" />
          <stop offset="45%" stopColor="var(--c-ice)" stopOpacity="0.5" />
          <stop offset="100%" stopColor="var(--c-ice)" stopOpacity="0" />
        </radialGradient>
        <filter id="k-blur" x="-60%" y="-60%" width="220%" height="220%">
          <feGaussianBlur stdDeviation="1.4" />
        </filter>
      </defs>
      <g stroke="var(--c-ice)" strokeWidth="1.3" strokeLinecap="round" opacity="0.6">
        {edges.map(([a, b], i) => {
          const [x1, y1] = nodes[a]
          const [x2, y2] = nodes[b]
          return <line key={i} x1={x1} y1={y1} x2={x2} y2={y2} />
        })}
      </g>
      <g fill="var(--c-ice)">
        {dim.map((k) => {
          const [x, y] = nodes[k]
          return <circle key={k} cx={x} cy={y} r="1.9" />
        })}
      </g>
      <circle cx={nodes.c[0]} cy={nodes.c[1]} r="7" fill="url(#k-ctr)" filter="url(#k-blur)" />
      <circle cx={nodes.c[0]} cy={nodes.c[1]} r="3.4" fill="none" stroke="var(--c-ice)" strokeOpacity="0.6" strokeWidth="0.8" />
      <circle cx={nodes.c[0]} cy={nodes.c[1]} r="2.6" fill="var(--c-bright)" />
    </svg>
  )
}

/** Environment chip — derived from the real backend binding, never hardcoded.
    A demo console must say demo; only a reachable verifier earns "LIVE". */
function EnvChip() {
  const { status } = useHealth()
  const label =
    status === 'ok' ? 'LIVE · verifier' : status === 'demo' ? 'DEMO · simulated fleet' : status === 'connecting' ? 'CONNECTING…' : 'OFFLINE'
  return (
    <span className="ml-1 hidden rounded-lg border border-line bg-panel px-3 py-1.5 font-mono text-[11px] text-muted xl:block">
      {label}
    </span>
  )
}

/** Quick-nav search: filters console destinations + fleet units. A real,
    keyboard-first affordance — not a decorative input. */
function QuickNav() {
  const router = useRouter()
  const [q, setQ] = useState('')
  const [open, setOpen] = useState(false)
  const [sel, setSel] = useState(0)
  const boxRef = useRef<HTMLDivElement>(null)

  const targets = useMemo(() => {
    const routes = navGroups.flatMap((g) => g.items.map((it) => ({ href: it.href, label: it.label, kind: g.label })))
    const units = ['r1', 'r2', 'r3', 'r4', 'r5', 'r6', 'r7', 'r8'].map((id) => ({
      href: `/fleet/${id}`,
      label: `Unit ${id.toUpperCase()}`,
      kind: 'Fleet unit',
    }))
    return [...routes, ...units]
  }, [])

  const hits = useMemo(() => {
    const needle = q.trim().toLowerCase()
    if (!needle) return []
    return targets.filter((t) => t.label.toLowerCase().includes(needle) || t.href.includes(needle)).slice(0, 8)
  }, [q, targets])

  useEffect(() => {
    const onDoc = (e: MouseEvent) => {
      if (!boxRef.current?.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', onDoc)
    return () => document.removeEventListener('mousedown', onDoc)
  }, [])

  const go = (href: string) => {
    setOpen(false)
    setQ('')
    router.push(href)
  }

  return (
    <div ref={boxRef} className="relative ml-auto hidden w-72 items-center md:flex">
      <Search className="absolute left-3 h-4 w-4 text-faint" aria-hidden="true" />
      <input
        role="combobox"
        aria-expanded={open && hits.length > 0}
        aria-controls="quicknav-list"
        aria-label="Go to page or fleet unit"
        placeholder="Go to page or unit…"
        value={q}
        onChange={(e) => {
          setQ(e.target.value)
          setOpen(true)
          setSel(0)
        }}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown') { e.preventDefault(); setSel((s) => Math.min(s + 1, hits.length - 1)) }
          if (e.key === 'ArrowUp') { e.preventDefault(); setSel((s) => Math.max(s - 1, 0)) }
          if (e.key === 'Enter' && hits[sel]) go(hits[sel].href)
          if (e.key === 'Escape') { setOpen(false); setQ('') }
        }}
        className="w-full rounded-lg border border-line bg-panel py-2 pl-9 pr-3 text-[13px] text-ink outline-none placeholder:text-faint focus:border-line-strong"
      />
      {open && hits.length > 0 && (
        <ul
          id="quicknav-list"
          role="listbox"
          className="absolute left-0 top-full z-40 mt-1 w-full overflow-hidden rounded-lg border border-line bg-panel shadow-panel"
        >
          {hits.map((h, i) => (
            <li key={h.href} role="option" aria-selected={i === sel}>
              <button
                onMouseEnter={() => setSel(i)}
                onClick={() => go(h.href)}
                className={`flex w-full items-center justify-between px-3 py-2 text-left text-[13px] ${i === sel ? 'bg-ink/[0.06] text-ink' : 'text-muted'}`}
              >
                <span>{h.label}</span>
                <span className="font-mono text-[10px] uppercase tracking-wider text-faint">{h.kind}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

function ThemeToggle() {
  const [light, setLight] = useState(false)
  useEffect(() => {
    setLight(document.documentElement.classList.contains('light'))
  }, [])
  const toggle = () => {
    const next = !light
    setLight(next)
    document.documentElement.classList.toggle('light', next)
    try {
      localStorage.setItem('kirra-console-theme', next ? 'light' : 'dark')
    } catch {}
  }
  return (
    <button
      onClick={toggle}
      aria-label="Toggle color theme"
      aria-pressed={light}
      className="rounded-lg border border-line bg-panel p-2 text-muted hover:text-ink"
    >
      {light ? <Moon className="h-4 w-4" /> : <Sun className="h-4 w-4" />}
    </button>
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
      <div className="ml-2 hidden sm:block">
        <ConnectionStatus />
      </div>
      <EnvChip />
      <QuickNav />
      <Link
        href="/events"
        aria-label="Open the event feed"
        className="ml-auto rounded-lg border border-line bg-panel p-2 text-muted hover:text-ink md:ml-0"
      >
        <Bell className="h-4 w-4" />
      </Link>
      <ThemeToggle />
      <div className="flex items-center gap-2 rounded-lg border border-line bg-panel py-1 pl-1 pr-2.5">
        <div className="grid h-7 w-7 place-items-center rounded-md bg-gradient-to-br from-ice/30 to-ice/10 font-display text-xs font-semibold text-ink">
          OP
        </div>
        <div className="hidden leading-none sm:block">
          <div className="text-[12px] text-ink">Operator</div>
          <div className="font-mono text-[10px] text-faint">read-only session</div>
        </div>
      </div>
    </header>
  )
}
