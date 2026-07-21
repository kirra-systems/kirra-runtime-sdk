'use client'

import { Line, LineChart, ResponsiveContainer, Tooltip, XAxis, YAxis, CartesianGrid, Legend } from 'recharts'

export function LatencyLines({ data, height = 200 }: { data: { t: string; p50: number; p99: number }[]; height?: number }) {
  return (
    <ResponsiveContainer width="100%" height={height}>
      <LineChart data={data} margin={{ top: 6, right: 8, left: -16, bottom: 0 }}>
        <CartesianGrid stroke="rgba(150,166,198,0.08)" vertical={false} />
        <XAxis dataKey="t" tick={{ fill: 'var(--c-faint)', fontSize: 10, fontFamily: 'monospace' }} axisLine={false} tickLine={false} minTickGap={28} />
        <YAxis tick={{ fill: 'var(--c-faint)', fontSize: 10, fontFamily: 'monospace' }} axisLine={false} tickLine={false} width={42} unit="ms" />
        <Tooltip contentStyle={{ background: 'var(--c-panel)', border: '1px solid rgba(150,166,198,0.22)', borderRadius: 10, fontFamily: 'monospace', fontSize: 12 }} labelStyle={{ color: 'var(--c-faint)' }} />
        <Legend wrapperStyle={{ fontFamily: 'monospace', fontSize: 10, color: 'var(--c-faint)' }} iconType="plainline" />
        <Line type="monotone" dataKey="p50" stroke="var(--c-ice)" strokeWidth={1.6} dot={false} name="p50" />
        <Line type="monotone" dataKey="p99" stroke="var(--c-warn)" strokeWidth={1.6} dot={false} name="p99" />
      </LineChart>
    </ResponsiveContainer>
  )
}
