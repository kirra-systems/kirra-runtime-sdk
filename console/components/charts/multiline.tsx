'use client'

import { Line, LineChart, ResponsiveContainer, Tooltip, XAxis, YAxis, CartesianGrid, Legend } from 'recharts'

type Row = { t: string; [k: string]: string | number }

export function MultiLine({ data, series, height = 260 }: { data: Row[]; series: { key: string; label: string; color: string }[]; height?: number }) {
  return (
    <ResponsiveContainer width="100%" height={height}>
      <LineChart data={data} margin={{ top: 6, right: 8, left: -16, bottom: 0 }}>
        <CartesianGrid stroke="rgba(150,166,198,0.08)" vertical={false} />
        <XAxis dataKey="t" tick={{ fill: '#69728a', fontSize: 10, fontFamily: 'monospace' }} axisLine={false} tickLine={false} minTickGap={32} />
        <YAxis domain={[0, 100]} tick={{ fill: '#69728a', fontSize: 10, fontFamily: 'monospace' }} axisLine={false} tickLine={false} width={42} unit="%" />
        <Tooltip contentStyle={{ background: '#10141e', border: '1px solid rgba(150,166,198,0.22)', borderRadius: 10, fontFamily: 'monospace', fontSize: 12 }} labelStyle={{ color: '#9aa6bd' }} />
        <Legend wrapperStyle={{ fontFamily: 'monospace', fontSize: 10 }} iconType="plainline" />
        {series.map((s) => (
          <Line key={s.key} type="monotone" dataKey={s.key} name={s.label} stroke={s.color} strokeWidth={1.6} dot={false} />
        ))}
      </LineChart>
    </ResponsiveContainer>
  )
}
