# Aegis v1.0.0 — Route Authorization Matrix

This document defines the authoritative access control matrix and operational boundary mappings for the Aegis v1.0.0 Stable Control Plane interface surface.

| Route | Method | Handler | Auth | Mode | Mutation? | Security Notes |
|---|---|---|---|---|---|---|
| `/health` | GET | `health` | Public | Active/Passive | No | Liveness only; avoids I/O blockages or thread starvation. |
| `/ready` | GET | `ready` | Public | Active/Passive | No | SQLite `SELECT 1` loop; drops node out of service pools on failure. |
| `/attestation/challenge/:node_id` | POST | `issue_challenge` | Node/Public | Active only | Yes | Generates CSPRNG nonce entry bound by strict `CHALLENGE_TTL_MS`. |
| `/attestation/verify` | POST | `verify_attestation` | Node/Public | Active only | Yes | Atomic verification loop; strictly pops nonces prior to signature analysis. |
| `/attestation/register` | POST | `register_node` | Admin token | Active only | Yes | Registers platform identity parameters using a write-through pipeline. |
| `/fleet/dependencies` | POST | `register_dependencies` | Admin token | Active only | Yes | Modifies topological DAG; guarded by White/Gray/Black cycle detection. |
| `/fleet/posture` | GET | `get_fleet_posture` | Public/read-only | Active/Passive | No | Returns un-mutated evaluation of system graph topology caches. |
| `/fleet/posture/:node_id` | GET | `get_node_posture` | Public/read-only | Active/Passive | No | Yields targeted graph evaluations for single specific node identities. |
| `/fleet/history/:node_id` | GET | `get_node_history` | Public/read-only | Active/Passive | No | Pulls time-series event listings from `posture_events` ledger. |
| `/fleet/flapping/:node_id` | GET | `get_node_flap_status` | Public/read-only | Active/Passive | No | Non-mutating stability loop assessing lookback frequency bounds. |
| `/system/backup/export` | POST | `export_backup` | Admin token | Active/Passive | No | Pulls comprehensive state summary directly into a formatted JSON schema. |
| `/system/backup/import` | POST | `import_backup` | Admin token | Active only | Yes | Triggers a destructive database format wipe wrapped in a transaction block. |

---

## Forbidden Regressions

- **Do not remove `require_admin_token` from admin routes.** Configuration change points must remain guarded under all compile variants.
- **Do not expose backup export/import publicly.** Snapshots hold comprehensive cluster schemas and cryptographic state indices; exposure breaks privacy boundaries.
- **Do not allow mutation routes in PassiveStandby.** Replicas must fail-closed immediately using an explicit `503 Service Unavailable` block.
- **Do not allow `/verify` without nonce consumption.** Signature validation passes must destructively consume their associated challenge entry beforehand to guarantee absolute replay immunity.
- **Do not replace cryptographic verification with a Trusted mock.** Overriding verification routines with an unconditional static trust assignment creates an open vector for unvalidated client intrusion.
