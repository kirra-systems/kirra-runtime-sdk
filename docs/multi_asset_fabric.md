# Multi-Asset Safety Fabric

## What the Multi-Asset Safety Fabric Is

### The Problem

The original Aegis runtime was designed around a single-fleet model: one DAG of trust nodes, one posture state, one set of kinematic limits. This works well when all devices operate under a unified trust boundary, but modern deployments involve heterogeneous physical systems — autonomous vehicles, warehouse robots, drones, industrial controllers, and infrastructure nodes — each with radically different safety envelopes, operational roles, and failure modes.

A single `FleetPosture` cannot meaningfully distinguish between a 35 m/s autonomous highway vehicle and a 0.5 m/s warehouse robot. Applying the same lockout or degraded state to both simultaneously ignores the operational reality that a slow-moving robot has very different consequences from a high-speed AV going LockedOut.

### The Solution

The Multi-Asset Safety Fabric introduces per-asset governors with type-specific kinematic contracts, an in-memory routing plane for command governance, cross-asset trust propagation rules, distributed telemetry aggregation, and a tamper-evident causal log linking events across assets.

Each registered asset gets its own `AssetGovernor` which selects the correct `VehicleKinematicsContract` based on the asset's current posture — Nominal uses the full envelope, Degraded applies MRC (Minimum Risk Condition) limits at 30% speed, and LockedOut denies all commands outright.

### Value Proposition

- **Per-asset kinematic contracts**: A drone in Degraded mode is governed by drone-specific MRC limits, not AV limits
- **Cross-asset trust propagation**: Structural dependencies between assets are enforced automatically (convoy followers degrade with leader lockout, drones degrade when ground control is locked)
- **Causal audit trail**: Every cross-asset state change is traceable to its root cause via the causal log
- **Zero-compromise safety semantics**: All fabric commands are fail-closed — unknown posture defaults to LockedOut

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     HTTP API Layer                               │
│   /fabric/assets/register   /fabric/command/{id}                │
│   /fabric/state             /fabric/causal-log                  │
└────────────────────────┬────────────────────────────────────────┘
                         │
           ┌─────────────▼──────────────┐
           │       FabricRouter         │
           │  DashMap<id, AssetGovernor>│
           │  DashMap<id, AssetPosture> │
           │  AtomicU64 fabric_gen      │
           └──┬──────────┬─────────────┘
              │          │
   ┌──────────▼──┐  ┌────▼─────────────────┐
   │AssetGovernor│  │ Cross-Asset Trust     │
   │  profile +  │  │ Propagation Rules     │
   │  contract   │  │ (4 structural rules)  │
   └──────────┬──┘  └────────────────────┬─┘
              │                          │
   ┌──────────▼──┐        ┌─────────────▼──────────┐
   │EnforceAction│        │   FabricCausalLog       │
   │Allow/Clamp/ │        │  Arc<Mutex<Vec<Entry>>> │
   │DenyBreach   │        │  Ed25519 signed         │
   └─────────────┘        └────────────────────────┘
              │
   ┌──────────▼──────────┐
   │   FabricTelemetry   │
   │  DashMap<id, Snap>  │
   │  per-asset CPM,     │
   │  denial rates       │
   └─────────────────────┘
```

Key design invariants:
- `FabricRouter` is behind `Arc` and uses `DashMap` — all operations are lock-free and thread-safe
- `FabricCausalLog` uses `Arc<Mutex<Vec>>` — entries are append-only
- All fabric state is fail-closed: unknown asset posture defaults to `LockedOut`
- `ServiceState` carries `Arc<FabricRouter>`, `Arc<FabricTelemetry>`, `Arc<FabricCausalLog>`

---

## Asset Types and Kinematic Profiles

### Asset Types

| Asset Type | Description |
|---|---|
| `autonomous_vehicle` | Highway/road AV with high-speed envelope |
| `robot` | Ground robot with pedestrian-space envelope |
| `drone` | Aerial vehicle with 3D flight envelope |
| `industrial_controller` | PLC/DCS — ground control station for drones |
| `warehouse` | Warehouse zone controller — governs robot fleet |
| `infrastructure` | Network or power infrastructure node |
| `unknown` | Unclassified asset |

### Kinematic Profiles and Limits

| Profile | Max Speed | Max Accel | Max Steering | Wheelbase | Use Case |
|---|---|---|---|---|---|
| `automotive_nominal` | 35.0 m/s | 3.0 m/s² | 45° | 2.7 m | Highway AV |
| `automotive_mrc` | 10.5 m/s | 1.2 m/s² | 22.5° | 2.7 m | AV Degraded (MRC) |
| `robot_nominal` | 1.8 m/s | 1.5 m/s² | 45° | 0.2 m | Ground robot |
| `drone_nominal` | 15.0 m/s | 3.0 m/s² | 180° | 0.4 m | Aerial drone |
| `industrial_nominal` | 0.5 m/s | 0.3 m/s² | 360° | 0.5 m | Factory robot |
| `custom` | (automotive nominal) | | | | Custom-profiled asset |

MRC contracts are derived by scaling the nominal contract:
- `max_speed_mps`: 30% of nominal
- `max_accel_mps2`: 40% of nominal
- `max_brake_mps2`: 100% of nominal (unchanged)
- `max_steering_deg`: 50% of nominal
- `max_steering_rate_deg_s`: 50% of nominal
- `min_follow_distance_m`: 200% of nominal (doubled)
- `max_lateral_accel_mps2`: 40% of nominal

---

## Cross-Asset Trust Propagation Rules

When `GET /fabric/state` is called, the fabric router evaluates four structural dependency rules before returning state. Any asset whose posture must change is updated with generation bump and `blocked_by: ["cross_asset_propagation"]`.

### Rule 1: Drone Depends on Ground Control Station

If any `IndustrialController` asset is `LockedOut`, all `Drone` assets currently in `Nominal` are downgraded to `Degraded`.

**Rationale**: Drones operating beyond visual line of sight depend on ground control communication. A locked-out GCS means the drone has lost its safety anchor.

**Example**:
```
gcs01 (IndustrialController) → LockedOut
drone01 (Drone, Nominal) → Degraded   ← propagated
drone02 (Drone, Nominal) → Degraded   ← propagated
```

### Rule 2: Convoy Follower Degrades with Leader Lockout

If any asset with `metadata.convoy_role = "leader"` is `LockedOut`, all assets with `metadata.convoy_role = "follower"` in `Nominal` are downgraded to `Degraded`.

**Rationale**: Platooning AVs maintain safe following distance by trusting the leader's telemetry. A locked-out leader means the follower's distance-keeping algorithm has lost its reference.

**Example**:
```
leader01 (AV, convoy_role=leader) → LockedOut
follower01 (AV, convoy_role=follower, Nominal) → Degraded
follower02 (AV, convoy_role=follower, Nominal) → Degraded
```

### Rule 3: Infrastructure Lockout Degrades Dependents

If any `Infrastructure` asset is `LockedOut`, all assets with `metadata.depends_on_infrastructure = "true"` in `Nominal` are downgraded to `Degraded`.

**Rationale**: Assets relying on infrastructure (network switches, power systems, GPS repeaters) cannot safely operate at full capacity when that infrastructure is compromised.

### Rule 4: Warehouse Lockout Degrades Registered Robots

If any `Warehouse` asset is `LockedOut`, all `Robot` assets with `metadata.warehouse_id = <locked_warehouse_id>` in `Nominal` are downgraded to `Degraded`.

**Rationale**: Warehouse lockout (e.g., fire suppression activation, unauthorized access, sensor failure) requires all operating robots in that zone to reduce to MRC mode.

**Example**:
```
wh01 (Warehouse) → LockedOut
robot01 (Robot, warehouse_id=wh01, Nominal) → Degraded
robot02 (Robot, warehouse_id=wh01, Nominal) → Degraded
robot03 (Robot, warehouse_id=wh02, Nominal) → unchanged
```

---

## The Causal Log

The `FabricCausalLog` records every significant fabric event with structured causality linkage. Each log entry stores:
- `entry_id`: SHA-256-derived hex identifier (16 bytes)
- `asset_id`: the asset that generated the event
- `event_type`: structured event name (e.g., `COMMAND_DENIED`, `GCS_FAULT`)
- `payload`: JSON event body
- `caused_by`: list of predecessor `entry_id`s
- `affects_assets`: list of asset IDs impacted by this event
- `fabric_generation`: fabric generation counter at time of event
- `signature_b64`: optional Ed25519 signature (same key as audit chain)

### Why It Matters for Incident Investigation

The causal chain API (`GET /fabric/causal-log/{entry_id}`) traverses the `caused_by` graph to reconstruct the full causal history of any event. This lets an operator answer: "Why is this drone in Degraded mode?" by tracing back to the GCS fault that triggered the propagation.

Example causal chain:
```
[1] gcs01: GCS_FAULT        (root cause, caused_by=[])
    → affects: [drone01, drone02]
[2] drone01: DRONE_DEGRADED  (caused_by=[entry_1])
[3] drone01: COMMAND_DENIED  (caused_by=[entry_2])
```

The log is append-only and all entries are kept in memory during runtime. For durable persistence, `save_causal_log_entry` writes to the `fabric_causal_log` SQLite table with full indexing on `(asset_id, timestamp_ms)`.

---

## API Reference

All fabric endpoints require Bearer token authentication (`AEGIS_ADMIN_TOKEN`).

### POST /fabric/assets/register

Register a new asset with the fabric.

**Request**:
```json
{
  "asset_id": "drone01",
  "asset_type": "drone",
  "display_name": "Survey Drone 01",
  "kinematic_profile": "drone_nominal",
  "metadata": {
    "operator": "site_alpha"
  }
}
```

**Response** (201):
```json
{ "asset_id": "drone01", "registered": true }
```

### GET /fabric/assets

List all registered fabric assets.

**Response**:
```json
{
  "assets": [
    {
      "asset_id": "drone01",
      "asset_type": "drone",
      "display_name": "Survey Drone 01",
      "kinematic_profile": "drone_nominal",
      "registered_at_ms": 1700000000000,
      "last_seen_ms": 1700000000000,
      "metadata": {}
    }
  ],
  "total": 1
}
```

### GET /fabric/state

Get current fabric posture with cross-asset trust propagation applied.

**Response**:
```json
{
  "total_assets": 3,
  "nominal_count": 2,
  "degraded_count": 1,
  "locked_out_count": 0,
  "assets": [
    {
      "asset_id": "drone01",
      "posture": "Degraded",
      "generation": 5,
      "computed_at_ms": 1700000000000,
      "contributing_nodes": [],
      "blocked_by": ["cross_asset_propagation"]
    }
  ],
  "fabric_generation": 8,
  "computed_at_ms": 1700000000000
}
```

### GET /fabric/telemetry

Get aggregated fabric telemetry summary.

**Response**:
```json
{
  "total_assets": 3,
  "active_assets": 3,
  "total_commands_per_minute": 180.0,
  "fabric_denial_rate": 0.12,
  "assets_by_type": { "Drone": 2, "Robot": 1 },
  "assets_by_posture": { "Nominal": 2, "Degraded": 1 },
  "highest_denial_asset": "drone01",
  "computed_at_ms": 1700000000000
}
```

### GET /fabric/telemetry/{asset_id}

Get telemetry snapshot for a specific asset.

**Response** (200 or 404):
```json
{
  "asset_id": "drone01",
  "asset_type": "Drone",
  "posture": "Nominal",
  "node_count": 4,
  "trusted_nodes": 4,
  "untrusted_nodes": 0,
  "last_command_ms": 1700000000000,
  "last_command_action": "Allow",
  "commands_per_minute": 60.0,
  "denial_rate": 0.05,
  "updated_at_ms": 1700000000000
}
```

### POST /fabric/command/{asset_id}

Submit a vehicle command for governance against the asset's current posture and kinematic contract.

**Request**:
```json
{
  "linear_velocity_mps": 10.0,
  "current_velocity_mps": 9.5,
  "delta_time_s": 0.1,
  "steering_angle_deg": 5.0,
  "current_steering_angle_deg": 4.8
}
```

**Response**:
```json
{
  "asset_id": "drone01",
  "action": "Allow",
  "allowed": true
}
```

On denial:
```json
{
  "asset_id": "robot01",
  "action": "DenyBreach(\"ASSET_LOCKED_OUT\")",
  "allowed": false
}
```

### GET /fabric/causal-log

Export causal log entries within a time window.

**Query params**: `from_ms` (optional), `to_ms` (optional)

**Response**:
```json
{
  "entries": [
    {
      "entry_id": "a1b2c3d4e5f6a7b8",
      "timestamp_ms": 1700000000000,
      "asset_id": "gcs01",
      "event_type": "GCS_FAULT",
      "payload": "{}",
      "caused_by": [],
      "affects_assets": ["drone01", "drone02"],
      "fabric_generation": 3,
      "signature_b64": null
    }
  ],
  "total": 1
}
```

### GET /fabric/causal-log/{entry_id}

Traverse the full causal chain for a specific entry.

**Response** (200 or 404):
```json
{
  "entry_id": "b2c3d4e5f6a7b8c9",
  "chain": [
    { "entry_id": "a1b2c3...", "event_type": "GCS_FAULT", ... },
    { "entry_id": "b2c3d4...", "event_type": "COMMAND_DENIED", ... }
  ],
  "depth": 2
}
```

---

## Example: Warehouse Robot Fleet

A deployment with 10 robots across 2 warehouses plus 1 infrastructure controller.

**Registration**:
```bash
# Register infrastructure
curl -X POST /fabric/assets/register \
  -d '{"asset_id":"infra01","asset_type":"infrastructure","display_name":"Network Switch","kinematic_profile":"industrial_nominal"}'

# Register warehouse zones
curl -X POST /fabric/assets/register \
  -d '{"asset_id":"wh_alpha","asset_type":"warehouse","display_name":"Alpha Warehouse","kinematic_profile":"industrial_nominal"}'

# Register robots with warehouse affiliation
for i in {1..5}; do
  curl -X POST /fabric/assets/register \
    -d "{\"asset_id\":\"robot_a$i\",\"asset_type\":\"robot\",\"display_name\":\"Robot A$i\",\"kinematic_profile\":\"robot_nominal\",\"metadata\":{\"warehouse_id\":\"wh_alpha\",\"depends_on_infrastructure\":\"true\"}}"
done
```

**Cascade scenario**: Infrastructure lockout

When `infra01` is set to `LockedOut`, the next `GET /fabric/state` call applies Rule 3 and Rule 4:

1. Rule 3: `infra01` is Infrastructure + LockedOut → all 5 robots in `wh_alpha` with `depends_on_infrastructure=true` degrade to `Degraded`
2. Causal log records `INFRA_LOCKOUT` with `affects_assets: [robot_a1...robot_a5]`
3. Subsequent commands to `robot_a1` at full speed are clamped by MRC limits (0.54 m/s max vs 1.8 m/s nominal)

---

## Example: Drone Swarm

A survey operation with 4 drones and 1 ground control station.

**Dependency structure**:
- `gcs01` (IndustrialController) — ground control
- `drone01..drone04` (Drone) — all depend on GCS per Rule 1

**Normal state**:
- `gcs01` Nominal → all drones Nominal
- Drones can command up to 15.0 m/s

**GCS failure cascade**:
1. External event sets `gcs01` → LockedOut
2. `GET /fabric/state` applies Rule 1
3. All 4 drones → Degraded
4. MRC limits: max 4.5 m/s (15.0 × 0.3), max accel 1.2 m/s²
5. Causal log entry: `GCS_FAULT` → `affects_assets: [drone01, drone02, drone03, drone04]`

**Resuming operations**:
1. `gcs01` returns to Nominal posture
2. Propagation rule no longer fires
3. Drones return to Nominal on next posture cycle

---

## Example: AV Convoy

A 3-vehicle highway convoy with leader-follower trust propagation.

**Asset registration with convoy metadata**:
```bash
curl -X POST /fabric/assets/register \
  -d '{"asset_id":"av_leader","asset_type":"autonomous_vehicle","display_name":"Convoy Lead","kinematic_profile":"automotive_nominal","metadata":{"convoy_role":"leader"}}'

curl -X POST /fabric/assets/register \
  -d '{"asset_id":"av_follow_1","asset_type":"autonomous_vehicle","display_name":"Follow 1","kinematic_profile":"automotive_nominal","metadata":{"convoy_role":"follower"}}'

curl -X POST /fabric/assets/register \
  -d '{"asset_id":"av_follow_2","asset_type":"autonomous_vehicle","display_name":"Follow 2","kinematic_profile":"automotive_nominal","metadata":{"convoy_role":"follower"}}'
```

**Leader lockout scenario**:
1. LIDAR fault on `av_leader` → node goes Untrusted → `av_leader` → LockedOut
2. `GET /fabric/state` applies Rule 2
3. `av_follow_1`, `av_follow_2` → Degraded
4. Follower MRC: max 10.5 m/s (35.0 × 0.3), minimum following distance doubled
5. Causal log: `LEADER_LOCKOUT` → `affects_assets: [av_follow_1, av_follow_2]`

**Safety invariant**: A follower at highway speed that suddenly loses leader telemetry trust immediately enters MRC mode — it cannot maintain the same speed and following distance as in Nominal mode, ensuring safe deceleration to increased gaps.
