import { PROXY_BASE } from './config'
import type { AuditPage, AuditVerify, FabricTelemetry, FleetNodePosture, HealthResponse, NodeHistory } from './types'

// Sentinel thrown when the proxy reports demo mode (no backend configured).
// Distinguishes "intentionally offline" from "backend down" for labeling.
export class DemoMode extends Error {
  constructor() { super('demo'); this.name = 'DemoMode' }
}

async function getJSON<T>(path: string, signal?: AbortSignal): Promise<T> {
  const res = await fetch(`${PROXY_BASE}${path}`, {
    method: 'GET',
    headers: { accept: 'application/json' },
    cache: 'no-store',
    signal,
  })
  if (res.headers.get('x-kirra-mode') === 'demo') throw new DemoMode()
  if (!res.ok) throw new Error(`GET ${path} → ${res.status}`)
  return (await res.json()) as T
}

export const kirra = {
  health: (signal?: AbortSignal) => getJSON<HealthResponse>('/health', signal),
  fleetPosture: (signal?: AbortSignal) => getJSON<{ fleet: FleetNodePosture[] }>('/fleet/posture', signal),
  nodePosture: (id: string, signal?: AbortSignal) =>
    getJSON<FleetNodePosture>(`/fleet/posture/${encodeURIComponent(id)}`, signal),
  auditVerify: (signal?: AbortSignal) => getJSON<AuditVerify>('/system/audit/verify', signal),
  auditPage: (limit = 12, signal?: AbortSignal) => getJSON<AuditPage>(`/console/audit?limit=${limit}`, signal),
  fabricTelemetry: (signal?: AbortSignal) => getJSON<FabricTelemetry>('/fabric/telemetry', signal),
  nodeHistory: (id: string, signal?: AbortSignal) =>
    getJSON<NodeHistory>(`/fleet/history/${encodeURIComponent(id)}`, signal),
}
