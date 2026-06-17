import { LayoutDashboard, Globe, Boxes, ShieldCheck, Activity, Radio, Route, Rss, Brain, Bell, AlertTriangle, FileCheck2, BarChart3, Database, FileText, Settings } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'

// `preview: true` marks a route whose screen renders bundled mock data (no live
// verifier endpoint behind it). The sidebar renders a small "preview" tag so the
// live-backed routes stand out — the nav-level complement to the per-page
// DemoBadge ("SIMULATED DATA"). Live-backed routes (live/fleet/oversight/events/
// incidents/compliance) carry no flag.
export interface NavItem { href: string; label: string; icon: LucideIcon; preview?: boolean }
export interface NavGroup { label: string; items: NavItem[] }

// Shared navigation model used by both the desktop sidebar and the mobile drawer.
export const navGroups: NavGroup[] = [
  {
    label: 'Operations',
    items: [
      { href: '/', label: 'Overview', icon: LayoutDashboard, preview: true },
      { href: '/live', label: 'Live Fleet', icon: Rss },
      { href: '/global', label: 'Global Operations', icon: Globe, preview: true },
      { href: '/fleet', label: 'Fleet Operations', icon: Boxes },
      { href: '/safety', label: 'Safety Governor', icon: ShieldCheck, preview: true },
      { href: '/runtime', label: 'Runtime Health', icon: Activity, preview: true },
      { href: '/telemetry', label: 'Telemetry', icon: Radio, preview: true },
      { href: '/missions', label: 'Mission Timeline', icon: Route, preview: true },
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
      { href: '/analytics', label: 'Analytics', icon: BarChart3, preview: true },
      { href: '/explorer', label: 'Telemetry Explorer', icon: Database, preview: true },
      { href: '/reports', label: 'Reports', icon: FileText, preview: true },
      { href: '/settings', label: 'Settings', icon: Settings, preview: true },
    ],
  },
]
