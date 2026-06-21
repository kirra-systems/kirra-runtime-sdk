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

// Audit chain — /system/audit/verify (admin) and /console/audit (public).
export interface AuditVerify {
  chain_intact: boolean
  total_entries: number
  latest_hash: string
  signing_enabled: boolean
  signed_entries: number
  unsigned_entries: number
  signature_valid: boolean
  public_key_b64: string | null
  head_verified: boolean
  head_status: string
  verified: boolean
}
export interface AuditEntry {
  id: number
  timestamp_ms: number
  event_type: string
  source: string
  payload: string
  prev_hash: string
  entry_hash: string
  signature_b64: string | null
  signature_status: string
}
export interface AuditPage {
  entries: AuditEntry[]
  total: number
  public_key_b64: string | null
  chain_intact: boolean
}

// Fabric governance telemetry — GET /fabric/telemetry (admin).
export interface FabricTelemetry {
  total_assets: number
  active_assets: number
  total_commands_per_minute: number
  fabric_denial_rate: number
  assets_by_type: Record<string, number>
  assets_by_posture: Record<string, number>
  highest_denial_asset: string | null
  computed_at_ms: number
}

// Per-node posture history — GET /fleet/history/{node_id} (public).
export interface NodeHistoryEntry {
  event_type: string
  posture: FleetNodePosture | null
  reason: string | null
  created_at_ms: number
}
export interface NodeHistory {
  node_id: string
  history: NodeHistoryEntry[]
}

// Cross-controller federated trust reports — GET /federation/reports/{asset_id}
// (public). `posture` is the JSON-encoded FleetPosture (e.g. "\"Nominal\"") — the
// store round-trips serde_json::to_string(&FleetPosture), so normalize on read.
export interface FederatedReport {
  source_controller_id: string
  asset_id: string
  posture: string
  issued_at_ms: number
  expires_at_ms: number
}
export interface FederationReports {
  asset_id: string
  reports: FederatedReport[]
}

// Fabric governance state — GET /fabric/state (admin). Per-asset posture in the
// cross-asset fabric DAG plus fleet-wide counts. `posture` is a plain
// FleetPosture (serde struct field — not double-encoded like federation).
export interface AssetPosture {
  asset_id: string
  posture: FleetPostureState
  generation: number
  computed_at_ms: number
  contributing_nodes: string[]
  blocked_by: string[]
}
export interface FabricState {
  total_assets: number
  nominal_count: number
  degraded_count: number
  locked_out_count: number
  assets: AssetPosture[]
  fabric_generation: number
  computed_at_ms: number
}

// Console runtime health — GET /console/runtime (#395). Single-snapshot view of
// the verifier's operational state: mode, uptime, posture-engine generation /
// cache, fabric denial rate, audit size, SSE subscribers, HA heartbeat age.
export interface ConsoleRuntime {
  mode: 'Active' | 'PassiveStandby'
  uptime_ms: number
  posture_generation: number
  last_recalc_ms: number
  posture_cache_ttl_ms: number
  total_nodes: number
  fabric_assets: number
  fabric_denial_rate: number
  audit_entries: number
  broadcast_subscribers: number
  ha_heartbeat_age_ms: number | null
}

// Console analytics — GET /console/analytics?window_ms= (#396). Historical
// (NOT forecast) trend series over the requested window: bucketed posture
// transitions, denial-rate series, per-asset interventions, and flapping nodes.
export interface PostureTransitionBucket {
  bucket_start_ms: number
  to_degraded: number
  to_lockedout: number
  to_nominal: number
}
export interface DenialRatePoint {
  bucket_start_ms: number
  denial_rate: number
}
export interface InterventionByAsset {
  asset_id: string
  clamps: number
  denies: number
}
export interface FlappingNode {
  node_id: string
  transitions: number
}
export interface ConsoleAnalytics {
  window_ms: number
  posture_transitions: PostureTransitionBucket[]
  denial_rate_series: DenialRatePoint[]
  interventions_by_asset: InterventionByAsset[]
  flapping_top: FlappingNode[]
}

// Console site distribution — GET /console/sites (#397). Fleet posture rolled up
// per site, plus the count of nodes with no site assignment.
export interface SiteDistribution {
  site: string
  total: number
  nominal: number
  degraded: number
  lockedout: number
}
export interface ConsoleSites {
  sites: SiteDistribution[]
  unassigned: number
}

// Console version adoption — GET /console/versions (#398). Software-version
// share across the fleet, plus the nodes reporting an unknown version.
export interface VersionShareEntry {
  version: string
  count: number
  pct: number
}
export interface ConsoleVersions {
  versions: VersionShareEntry[]
  total: number
  unknown: number
}

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
