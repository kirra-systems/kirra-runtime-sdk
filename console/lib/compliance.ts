import type { Tone } from './types'

// Compliance & Certification Center — the standing evidence posture for the
// functional-safety case: certification status, verification coverage, the
// software bill of materials, signed-firmware provenance, and policy enforcement.

export interface Cert {
  id: string
  standard: string
  scope: string
  level: string
  status: 'Certified' | 'In Audit' | 'Self-Assessed'
  tone: Tone
  expires: string
}

export const certs: Cert[] = [
  { id: 'iso26262', standard: 'ISO 26262', scope: 'Road-vehicle functional safety', level: 'ASIL D', status: 'Certified', tone: 'safe', expires: '2027-03' },
  { id: 'iec61508', standard: 'IEC 61508', scope: 'E/E/PE safety-related systems', level: 'SIL 3', status: 'Certified', tone: 'safe', expires: '2027-01' },
  { id: 'iso13849', standard: 'ISO 13849', scope: 'Safety of machinery — control', level: 'PL e / Cat 4', status: 'In Audit', tone: 'warn', expires: '2026-09' },
  { id: 'ul4600', standard: 'UL 4600', scope: 'Autonomous product safety case', level: 'Conformant', status: 'Self-Assessed', tone: 'ice', expires: '2026-12' },
]

export interface TestGroup { id: string; name: string; passed: number; total: number; tone: Tone }

export const tests: TestGroup[] = [
  { id: 't1', name: 'Safety-goal verification (SG1–SG4)', passed: 412, total: 412, tone: 'safe' },
  { id: 't2', name: 'Fail-closed fault injection (FDIT/RTM)', passed: 188, total: 192, tone: 'warn' },
  { id: 't3', name: 'Kinematic envelope property tests', passed: 2048, total: 2048, tone: 'safe' },
  { id: 't4', name: 'WCET timing-evidence (QNX target)', passed: 64, total: 64, tone: 'safe' },
  { id: 't5', name: 'Attestation & replay-prevention', passed: 96, total: 96, tone: 'safe' },
]

export interface SbomEntry { id: string; component: string; version: string; license: string; risk: Tone; note: string }

export const sbom: SbomEntry[] = [
  { id: 'sb1', component: 'kirra-runtime-sdk', version: '2.4.1', license: 'Proprietary', risk: 'safe', note: 'signed' },
  { id: 'sb2', component: 'ed25519-dalek', version: '2.1.1', license: 'BSD-3', risk: 'safe', note: 'pinned' },
  { id: 'sb3', component: 'rusqlite (bundled)', version: '0.31.0', license: 'MIT', risk: 'safe', note: 'pinned' },
  { id: 'sb4', component: 'tokio', version: '1.40.0', license: 'MIT', risk: 'safe', note: 'pinned' },
  { id: 'sb5', component: 'axum', version: '0.8.1', license: 'MIT', risk: 'safe', note: 'pinned' },
  { id: 'sb6', component: 'iceoryx2', version: '0.4.0', license: 'Apache-2.0', risk: 'warn', note: 'spike — not in cert scope' },
]

export interface Firmware { id: string; target: string; version: string; digest: string; signer: string; ts: string; tone: Tone }

export const firmware: Firmware[] = [
  { id: 'fw1', target: 'Governor partition (QNX)', version: '2.4.1', digest: '0x7c1a…9e', signer: 'release-ed25519', ts: '2026-06-10 09:12', tone: 'safe' },
  { id: 'fw2', target: 'ROS2 adapter guest', version: '2.4.1', digest: '0x33b8…04', signer: 'release-ed25519', ts: '2026-06-10 09:12', tone: 'safe' },
  { id: 'fw3', target: 'Fleet verifier service', version: '2.4.0', digest: '0xa9f0…71', signer: 'release-ed25519', ts: '2026-05-28 16:40', tone: 'ice' },
]

export interface PolicyLog { id: string; ts: string; actor: string; policy: string; outcome: 'ENFORCED' | 'BLOCKED'; tone: Tone }

export const policyLogs: PolicyLog[] = [
  { id: 'p1', ts: '12:00:04', actor: 'orchestrator-east', policy: 'admin-token required on mutation route', outcome: 'ENFORCED', tone: 'safe' },
  { id: 'p2', ts: '11:52:18', actor: 'unknown-client', policy: 'identity-gated · x-kirra-client-id missing', outcome: 'BLOCKED', tone: 'crit' },
  { id: 'p3', ts: '11:40:51', actor: 'peer-controller-west', policy: 'Ed25519 federated report signature', outcome: 'ENFORCED', tone: 'safe' },
  { id: 'p4', ts: '11:31:09', actor: 'peer-controller-west', policy: 'replay nonce already burned', outcome: 'BLOCKED', tone: 'warn' },
  { id: 'p5', ts: '11:05:33', actor: 'release-ed25519', policy: 'signed-firmware provenance', outcome: 'ENFORCED', tone: 'safe' },
]
