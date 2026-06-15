import type { Tone } from '@/lib/types'

// Wire types mirroring the verifier service's serde shapes (src/verifier.rs).
//   FleetPosture       -> "Nominal" | "Degraded" | "LockedOut"
//   NodeTrustState     -> "Trusted" | "Unknown" | { Untrusted: "<reason>" }
//   FleetNodePosture   -> { node_id, local_status, propagated_status, blocked_by }

export type FleetPostureState = 'Nominal' | 'Degraded' | 'LockedOut'
export type NodeTrustState = 'Trusted' | 'Unknown' | { Untrusted: string }

export interface FleetNodePosture {
  node_id: string
  local_status: NodeTrustState
  propagated_status: FleetPostureState
  blocked_by: string[]
}

export interface PostureStreamEvent {
  event_type: string
  node_id: string | null
  emitted_at_ms: number
  posture: FleetNodePosture | null
}

export interface HealthResponse { status: string }

export function trustLabel(s: NodeTrustState): string {
  return typeof s === 'string' ? s : 'Untrusted'
}
export function trustReason(s: NodeTrustState): string | null {
  return typeof s === 'string' ? null : s.Untrusted
}

export function postureTone(p: FleetPostureState): Tone {
  return p === 'Nominal' ? 'safe' : p === 'Degraded' ? 'warn' : 'crit'
}
export function trustTone(s: NodeTrustState): Tone {
  return typeof s === 'string' ? (s === 'Trusted' ? 'safe' : 'muted') : 'crit'
}
