import { API_BASE } from './config'
import type { FleetNodePosture, HealthResponse } from './types'

// Thin typed client over the verifier's public read endpoints. All calls are
// no-store and abortable. CORS note: when the console and verifier are on
// different origins, the verifier must send CORS headers (or sit behind the
// same origin / a reverse proxy) for these browser fetches to succeed.

async function getJSON<T>(path: string, signal?: AbortSignal): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, {
    method: 'GET',
    headers: { accept: 'application/json' },
    cache: 'no-store',
    signal,
  })
  if (!res.ok) throw new Error(`GET ${path} → ${res.status}`)
  return (await res.json()) as T
}

export const kirra = {
  health: (signal?: AbortSignal) => getJSON<HealthResponse>('/health', signal),
  ready: (signal?: AbortSignal) => getJSON<HealthResponse>('/ready', signal),
  fleetPosture: (signal?: AbortSignal) => getJSON<{ fleet: FleetNodePosture[] }>('/fleet/posture', signal),
  nodePosture: (id: string, signal?: AbortSignal) =>
    getJSON<FleetNodePosture>(`/fleet/posture/${encodeURIComponent(id)}`, signal),
}
