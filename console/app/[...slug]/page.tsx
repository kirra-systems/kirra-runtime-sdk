import { Panel } from '@/components/ui/primitives'
import { DemoBadge } from '@/components/ui/demo-badge'

export default async function ModulePlaceholder({ params }: { params: Promise<{ slug: string[] }> }) {
  const { slug } = await params
  const name = (slug?.[0] ?? 'module').replace(/-/g, ' ')
  return (
    <div className="mx-auto max-w-[1500px] p-6">
      <div className="mb-6 flex items-center justify-between gap-4">
        <div>
          <h1 className="font-display text-xl font-semibold capitalize text-ink">{name}</h1>
          <p className="font-mono text-[11px] text-faint">module scaffold</p>
        </div>
        <DemoBadge live={false} />
      </div>
      <Panel>
        <div className="grid place-items-center gap-2 py-20 text-center">
          <span className="h-2 w-2 rounded-full bg-ice" />
          <p className="font-display text-lg text-ink">Module in development</p>
          <p className="max-w-sm text-sm text-muted">This screen is part of the Kirra Mission Console build-out. Operational widgets, engineering charts, and cross-linked drill-downs ship in the next increment.</p>
        </div>
      </Panel>
    </div>
  )
}
