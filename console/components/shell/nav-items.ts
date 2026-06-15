import { LayoutDashboard, Globe, Boxes, ShieldCheck, Activity, Radio, Route, Rss, Brain, Bell, AlertTriangle, FileCheck2, BarChart3, Database, FileText, Settings } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'

export interface NavItem { href: string; label: string; icon: LucideIcon }
export interface NavGroup { label: string; items: NavItem[] }

// Shared navigation model used by both the desktop sidebar and the mobile drawer.
export const navGroups: NavGroup[] = [
  {
    label: 'Operations',
    items: [
      { href: '/', label: 'Overview', icon: LayoutDashboard },
      { href: '/live', label: 'Live Fleet', icon: Rss },
      { href: '/global', label: 'Global Operations', icon: Globe },
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
