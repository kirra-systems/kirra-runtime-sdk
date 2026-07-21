'use client'

import { Line, LineChart, ResponsiveContainer, Tooltip, XAxis, YAxis, CartesianGrid, Legend } from 'recharts'

type Row = { t: string; [key: string]: string | number }

export function DualLine({ data, keys, colors, height = 180 }: { data: Row[]; keys: [string, string]; colors: [string, string]; height?: number }) {
  return (
    <ResponsiveContainer width="100%" height={height}>
      <LineChart data={data} margin={{ top: 6, right: 8, left: -16, bottom: 0 }}>
        <CartesianGrid stroke="rgba(150,166,198,0.08)" vertical={false} />
        <XAxis dataKey="t" tick={{ fill: 'var(--c-faint)', fontSize: 10, fontFamily: 'monospace' }} axisLine={false} tickLine={false} minTickGap={28} />
        <YAxis tick={{ fill: 'var(--c-faint)', fontSize: 10, fontFamily: 'monospace' }} axisLine={false} tickLine={false} width={42} />
        <Tooltip contentStyle={{ background: 'var(--c-panel)', border: '1px solid rgba(150,166,198,0.22)', borderRadius: 10, fontFamily: 'monospace', fontSize: 12 }} labelStyle={{ color: 'var(--c-faint)' }} />
        <Legend wrapperStyle={{ fontFamily: 'monospace', fontSize: 10 }} iconType="plainline" />
        <Line type="monotone" dataKey={keys[0]} stroke={colors[0]} strokeWidth={1.6} dot={false} />
        <Line type="monotone" dataKey={keys[1]} stroke={colors[1]} strokeWidth={1.6} dot={false} />
      </LineChart>
    </ResponsiveContainer>
  )
}
