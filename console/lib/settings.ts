import type { Tone } from './types'

// System & Operator Control — runtime configuration, feature flags, the
// human-in-the-loop operator tools, and access control. Operator actions are
// always re-adjudicated by the fail-closed Governor; nothing here bypasses it.

export interface ConfigItem { key: string; value: string; scope: 'env' | 'runtime'; note: string; tone: Tone }

export const config: ConfigItem[] = [
  { key: 'KIRRA_ADMIN_TOKEN', value: '•••••••• set', scope: 'env', note: 'mutation routes · absent → 503 fail-closed', tone: 'safe' },
  { key: 'KIRRA_SUPERVISOR_RESET_KEY', value: '•••••••• set', scope: 'env', note: 'reset ops · ≤ 64 bytes', tone: 'safe' },
  { key: 'KIRRA_VERIFIER_MODE', value: 'active', scope: 'runtime', note: 'active / passive_standby', tone: 'safe' },
  { key: 'KIRRA_TRUSTED_INGRESS_MODE', value: 'true', scope: 'env', note: 'client-id header enforcement', tone: 'safe' },
  { key: 'KIRRA_VERIFIER_ADDR', value: '0.0.0.0:8090', scope: 'env', note: 'listen address', tone: 'muted' },
  { key: 'CHALLENGE_TTL_MS', value: '30000', scope: 'runtime', note: 'nonce expiry', tone: 'muted' },
  { key: 'POSTURE_CACHE_TTL_MS', value: '5000', scope: 'runtime', note: 'cache staleness TTL', tone: 'muted' },
]

export interface FeatureFlag { name: string; enabled: boolean; note: string }

export const flags: FeatureFlag[] = [
  { name: 'Federated trust reconciliation v2', enabled: true, note: 'generation-ordered cross-controller reports' },
  { name: 'AV recovery hysteresis', enabled: true, note: '5 healthy reports / 10s window' },
  { name: 'Telemetry watchdog', enabled: true, note: '2s fault threshold' },
  { name: 'iceoryx2 transport spike', enabled: false, note: 'host-side spike — not in cert scope' },
  { name: 'TPM measured-boot quote', enabled: false, note: 'PCR16 follow-up (tracked)' },
]

// ── Operator Tools (#12) ──────────────────────────────────────────────
export interface OperatorAction { id: string; name: string; desc: string; tone: Tone; critical?: boolean }

// Each entry is a governed *request* the operator sends to the fail-closed
// Governor — never a direct console→actuator command. The console holds no
// actuator authority; the Governor adjudicates and acts under its own. The
// authenticated operator→governor request path is tracked in #412; these are
// display-only in this build (no wired request channel yet).
export const operatorActions: OperatorAction[] = [
  { id: 'estop', name: 'Fleet E-STOP', desc: 'Request governor-commanded MRC controlled-stop + HOLD (all assets)', tone: 'crit', critical: true },
  { id: 'hold', name: 'Hold Selected', desc: 'Request a single-asset pause at the next safe waypoint', tone: 'warn' },
  { id: 'teleop', name: 'Teleop Handoff', desc: 'Request supervised manual control (governor still gates envelope)', tone: 'ice' },
  { id: 'reset', name: 'Lockout Reset', desc: 'Request human reset of a LockedOut asset — requires supervisor key', tone: 'warn' },
]

export interface OperatorSession { asset: string; operator: string; mode: string; since: string; tone: Tone }

export const operatorSessions: OperatorSession[] = [
  { asset: 'KIRRA-13', operator: 'J. Looney', mode: 'Lockout review', since: '11:58', tone: 'crit' },
  { asset: 'KIRRA-10', operator: 'A. Reyes', mode: 'Supervised teleop', since: '12:01', tone: 'ice' },
]

export interface Member { name: string; role: string; access: string; status: 'active' | 'idle'; tone: Tone }

export const roster: Member[] = [
  { name: 'J. Looney', role: 'Fleet Supervisor', access: 'admin · reset key', status: 'active', tone: 'safe' },
  { name: 'A. Reyes', role: 'Operator', access: 'teleop · hold', status: 'active', tone: 'safe' },
  { name: 'M. Chen', role: 'Safety Engineer', access: 'read · compliance', status: 'idle', tone: 'muted' },
  { name: 'svc-orchestrator', role: 'Service account', access: 'mutation · token', status: 'active', tone: 'ice' },
]
