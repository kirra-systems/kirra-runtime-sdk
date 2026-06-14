'use client'

import { Area, AreaChart, ResponsiveContainer, Tooltip, XAxis, YAxis, CartesianGrid, RadialBar, RadialBarChart, PolarAngleAxis } from 'recharts'
import type { SeriesPoint } from '@/lib/types'

const COLORS = { safe: '#2fe6a6', warn: '#ffb020', crit: '#ff5468', ice: '#5cc6ff' } as const
type ChartColor = keyof typeof COLORS

export function TrendArea({ data, color = 'ice', height = 200 }: { data: SeriesPoint[]; color?: ChartColor; height?: number }) {
  const c = COLORS[color]
  return (
    <ResponsiveContainer width="100%" height={height}>
      <AreaChart data={data} margin={{ top: 6, right: 8, left: -16, bottom: 0 }}>
        <defs>
          <linearGradient id={`grad-${color}`} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor={c} stopOpacity={0.35} />
            <stop offset="100%" stopColor={c} stopOpacity={0} />
          </linearGradient>
        </defs>
        <CartesianGrid stroke="rgba(150,166,198,0.08)" vertical={false} />
        <XAxis dataKey="t" tick={{ fill: '#69728a', fontSize: 10, fontFamily: 'monospace' }} axisLine={false} tickLine={false} minTickGap={28} />
        <YAxis tick={{ fill: '#69728a', fontSize: 10, fontFamily: 'monospace' }} axisLine={false} tickLine={false} width={42} />
        <Tooltip
          contentStyle={{ background: '#10141e', border: '1px solid rgba(150,166,198,0.22)', borderRadius: 10, fontFamily: 'monospace', fontSize: 12 }}
          labelStyle={{ color: '#9aa6bd' }}
          itemStyle={{ color: c }}
        />
        <Area type="monotone" dataKey="v" stroke={c} strokeWidth={1.6} fill={`url(#grad-${color})`} dot={false} />
      </AreaChart>
    </ResponsiveContainer>
  )
}

export function Spark({ data, color = 'ice' }: { data: SeriesPoint[]; color?: ChartColor }) {
  const c = COLORS[color]
  return (
    <ResponsiveContainer width="100%" height={38}>
      <AreaChart data={data} margin={{ top: 2, right: 0, left: 0, bottom: 0 }}>
        <defs>
          <linearGradient id={`spark-${color}`} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor={c} stopOpacity={0.3} />
            <stop offset="100%" stopColor={c} stopOpacity={0} />
          </linearGradient>
        </defs>
        <Area type="monotone" dataKey="v" stroke={c} strokeWidth={1.4} fill={`url(#spark-${color})`} dot={false} />
      </AreaChart>
    </ResponsiveContainer>
  )
}

export function ScoreRing({ value, color = 'safe', label }: { value: number; color?: ChartColor; label?: string }) {
  const c = COLORS[color]
  return (
    <div className="relative">
      <ResponsiveContainer width="100%" height={150}>
        <RadialBarChart innerRadius="78%" outerRadius="100%" data={[{ value }]} startAngle={220} endAngle={-40}>
          <PolarAngleAxis type="number" domain={[0, 100]} tick={false} />
          <RadialBar dataKey="value" cornerRadius={8} fill={c} background={{ fill: 'rgba(255,255,255,0.05)' }} />
        </RadialBarChart>
      </ResponsiveContainer>
      <div className="pointer-events-none absolute inset-0 flex flex-col items-center justify-center">
        <span className="font-display text-3xl font-semibold text-ink">{value}</span>
        {label && <span className="font-mono text-[10px] uppercase tracking-wider text-faint">{label}</span>}
      </div>
    </div>
  )
}
