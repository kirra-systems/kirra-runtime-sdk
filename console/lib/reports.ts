import type { Tone } from './types'

// Reports & Releases — generated evidence documents (compliance / safety /
// incident / executive) and the staged software-rollout (OTA) campaign view.

export interface Report {
  id: string
  title: string
  kind: 'Compliance' | 'Safety' | 'Incident' | 'Executive'
  period: string
  format: 'PDF' | 'CSV' | 'JSON'
  signed: boolean
  ts: string
  tone: Tone
}

export const reports: Report[] = [
  { id: 'RPT-0912', title: 'ISO 26262 functional-safety evidence pack', kind: 'Compliance', period: 'Q2 2026', format: 'PDF', signed: true, ts: '2026-06-12 08:00', tone: 'safe' },
  { id: 'RPT-0911', title: 'Fleet safety summary — governor verdicts', kind: 'Safety', period: 'May 2026', format: 'PDF', signed: true, ts: '2026-06-01 06:00', tone: 'safe' },
  { id: 'RPT-0910', title: 'Incident INC-2041 root-cause report', kind: 'Incident', period: '2026-06-14', format: 'PDF', signed: true, ts: '2026-06-14 12:40', tone: 'crit' },
  { id: 'RPT-0909', title: 'Executive operations review', kind: 'Executive', period: 'Q2 2026', format: 'PDF', signed: false, ts: '2026-06-13 17:20', tone: 'ice' },
  { id: 'RPT-0908', title: 'Audit-chain integrity export', kind: 'Compliance', period: '30d', format: 'JSON', signed: true, ts: '2026-06-10 00:00', tone: 'safe' },
  { id: 'RPT-0907', title: 'Verdict throughput dataset', kind: 'Safety', period: '7d', format: 'CSV', signed: false, ts: '2026-06-09 23:00', tone: 'muted' },
]

export interface Scheduled { id: string; name: string; cadence: string; next: string; tone: Tone }

export const scheduled: Scheduled[] = [
  { id: 's1', name: 'Daily safety digest', cadence: 'daily · 06:00', next: 'tomorrow 06:00', tone: 'safe' },
  { id: 's2', name: 'Weekly compliance pack', cadence: 'weekly · Mon', next: 'Mon 08:00', tone: 'safe' },
  { id: 's3', name: 'Monthly executive review', cadence: 'monthly · 1st', next: 'Jul 1 09:00', tone: 'ice' },
]

// ── Software Rollout / OTA (#11) ───────────────────────────────────────
// Each ring gates on the health of the previous ring before adoption advances.
export interface Rollout {
  id: string
  ring: string
  assets: number
  adoption: number
  status: 'rolling' | 'paused' | 'complete' | 'staged'
  tone: Tone
  note: string
}

export const rolloutVersion = { version: 'v2.4.1', channel: 'stable', signed: true, started: '2026-06-10 09:12' }

export const rollout: Rollout[] = [
  { id: 'ring0', ring: 'Ring 0 · canary', assets: 2, adoption: 100, status: 'complete', tone: 'safe', note: 'soak passed · 48h' },
  { id: 'ring1', ring: 'Ring 1 · early', assets: 12, adoption: 100, status: 'complete', tone: 'safe', note: 'no regressions' },
  { id: 'ring2', ring: 'Ring 2 · broad', assets: 86, adoption: 74, status: 'rolling', tone: 'ice', note: 'staged · health-gated' },
  { id: 'ring3', ring: 'Ring 3 · fleet', assets: 142, adoption: 0, status: 'staged', tone: 'muted', note: 'awaits Ring 2 health gate' },
]

export interface VersionShare { version: string; pct: number; tone: Tone }

export const versionShare: VersionShare[] = [
  { version: 'v2.4.1', pct: 71, tone: 'safe' },
  { version: 'v2.4.0', pct: 26, tone: 'ice' },
  { version: 'v2.3.6', pct: 3, tone: 'warn' },
]
