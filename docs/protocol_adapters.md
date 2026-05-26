# Aegis Protocol Adapters — Industrial Safety Integration

## Overview

Aegis functions as a universal safety kernel across the industrial automation stack. Protocol adapters translate protocol-specific messages into Aegis `OperationalCommand` types, which are then evaluated against the current fleet posture to determine whether a command is safe to execute.

The adapter layer is intentionally thin — no external protocol libraries are required. Each adapter implements the message classification logic directly in pure Rust, keeping the safety-critical path free of third-party dependencies.

## Industry Coverage

| Protocol | Automotive | Factory | Utilities | Oil & Gas | Medical | Building |
|----------|-----------|---------|-----------|-----------|---------|---------|
| Modbus | | ✓ | ✓ | ✓ | | ✓ |
| OPC-UA | | ✓ | ✓ | ✓ | ✓ | |
| EtherNet/IP | ✓ | ✓ | | | ✓ | |
| CANOpen | ✓ | ✓ | | | ✓ | |
| DNP3 | | | ✓ | ✓ | | |

## Command Classification

All adapters map protocol operations to one of four `OperationalCommand` variants:

| Command | Meaning | Posture Behavior |
|---------|---------|-----------------|
| `ReadTelemetry` | Sensor reads, telemetry queries | Allowed in all postures |
| `WriteState` | Actuator writes, setpoint changes | Blocked in LockedOut |
| `SystemMutation` | State machine changes, resets | Allowed in Nominal only |
| `Unknown` | Unrecognized operation | Denied in ALL postures |

## Posture-Command Decision Matrix

| Command | Nominal | Degraded | LockedOut |
|---------|---------|----------|-----------|
| ReadTelemetry | ✓ | ✓ | ✗ |
| WriteState | ✓ | ✗ | ✗ |
| SystemMutation | ✓ | ✗ | ✗ |
| Unknown | ✗ | ✗ | ✗ |

---

## Protocol: Modbus

**Used in:** Factory automation, power distribution, HVAC, water treatment, oil and gas, building management.

Modbus is the oldest and most widely deployed industrial protocol. It uses a simple master-slave model with register-based addressing.

### Command Mapping

| Operation | Command |
|-----------|---------|
| `read_register` | `ReadTelemetry` |
| `write_register` | `WriteState` (maps to `cmd_vel`) |
| `coil_write` | `WriteState` (maps to `cmd_vel`) |

### Example Request

```bash
curl -X POST http://localhost:8090/industrial/evaluate \
  -H "Authorization: Bearer $AEGIS_ADMIN_TOKEN" \
  -H "x-aegis-client-id: modbus-gateway-01" \
  -H "Content-Type: application/json" \
  -d '{
    "protocol": "modbus",
    "message": {
      "asset_id": "pump_station_01",
      "operation": "read_register",
      "address": "40001",
      "value": 0,
      "risk_class": "read"
    }
  }'
```

---

## Protocol: OPC-UA

**Used in:** Smart manufacturing, process automation, pharmaceutical, utilities, oil and gas.

OPC Unified Architecture is the modern successor to OPC Classic. It provides a platform-independent, service-oriented architecture for industrial data exchange with built-in security.

### Command Mapping

| Operation | Command |
|-----------|---------|
| `read_node` | `ReadTelemetry` |
| `write_node` | `WriteState` |
| `call_method` | `WriteState` |

---

## Protocol: EtherNet/IP

**Used in:** Automotive assembly lines, factory robots, packaging machinery, medical devices, semiconductor manufacturing.

EtherNet/IP (Ethernet Industrial Protocol) is the dominant protocol in North American industrial automation, widely deployed in Rockwell Automation / Allen-Bradley PLC installations. It runs CIP (Common Industrial Protocol) over standard Ethernet.

### CIP Service Code Mapping

| Service Code | Name | Command |
|-------------|------|---------|
| `0x0E` | Get_Attribute_Single | `ReadTelemetry` |
| `0x10` | Set_Attribute_Single | `WriteState` |
| `0x4B` | Execute_Service | `SystemMutation` |
| `0x4C` | Read_Tag | `ReadTelemetry` |
| `0x4D` | Write_Tag | `WriteState` |
| Unknown | — | `Unknown` |

### Safety-Relevant CIP Classes

The following CIP classes are flagged as `safety_relevant=true` and receive elevated scrutiny in the audit log:

| Class | Name |
|-------|------|
| `0x29` | Safety Supervisor |
| `0x2A` | Safety Validator |
| `0x3B` | Safety Analog Input Point |
| `0x3C` | Safety Analog Output Point |

### Example Requests

**Read a tag (allowed in any posture):**
```bash
curl -X POST http://localhost:8090/industrial/ethernet-ip/evaluate \
  -H "Authorization: Bearer $AEGIS_ADMIN_TOKEN" \
  -H "x-aegis-client-id: eip-gateway-01" \
  -H "Content-Type: application/json" \
  -d '{
    "command_code": 101,
    "session_handle": 100,
    "status": 0,
    "service_code": 76,
    "class_id": 1,
    "instance_id": 1,
    "attribute_id": 3,
    "data": [],
    "source_node": "assembly_robot_01"
  }'
```

**Response (Nominal posture):**
```json
{
  "protocol": "ethernet_ip",
  "command": "ReadTelemetry",
  "allowed": true,
  "denial_reason": null,
  "posture_at_evaluation": "Nominal",
  "adapter_details": {
    "service_name": "Read_Tag",
    "is_write": false,
    "target_description": "class=0x01 instance=0x01 attr=0x03",
    "safety_relevant": false
  },
  "audit_ref": "1716480000000"
}
```

**Unified endpoint:**
```bash
curl -X POST http://localhost:8090/industrial/evaluate \
  -H "Authorization: Bearer $AEGIS_ADMIN_TOKEN" \
  -H "x-aegis-client-id: eip-gateway-01" \
  -d '{
    "protocol": "ethernet_ip",
    "message": {
      "command_code": 101,
      "session_handle": 100,
      "status": 0,
      "service_code": 16,
      "class_id": 41,
      "instance_id": 1,
      "attribute_id": 3,
      "data": [10, 0],
      "source_node": "conveyor_01"
    }
  }'
```

---

## Protocol: CANOpen

**Used in:** Automotive electronics, industrial robots, medical devices, elevators, building automation, agricultural machinery.

CANOpen is an application layer protocol running over CAN bus. It is the dominant protocol in European automotive and robotics systems. CAN frames are limited to 8 bytes of data, making it optimized for real-time control loops.

### Message Type Mapping

Message types are determined by the upper 4 bits of the CAN Object ID (COB-ID):

| Function Code | Message Type | Command |
|-------------|-------------|---------|
| `0x0` | NMT (Network Management) | `SystemMutation` |
| `0x1` | SYNC | `ReadTelemetry` |
| `0x2` | Timestamp | `ReadTelemetry` |
| `0x3` | Emergency | `ReadTelemetry` |
| `0x4-0x5` | PDO Transmit | `ReadTelemetry` |
| `0x6-0x7` | PDO Receive | `WriteState` |
| `0x8-0x9` | SDO Transmit | `ReadTelemetry` |
| `0xA-0xB` | SDO Receive | `WriteState` |
| `0xE` | Heartbeat | `ReadTelemetry` |

### Emergency Message Handling

Emergency messages (function code `0x3`) set `is_emergency=true` and parse the emergency error code from bytes 0-1 (little-endian):

| Emergency Code Range | Meaning |
|--------------------|---------|
| `0x1000-0x1FFF` | Generic error — logged to audit chain |
| `0x4000-0x4FFF` | Drive/motor error — triggers posture evaluation |
| `0x8000-0x8FFF` | Device-specific — logged with full payload |

### NMT State Transitions

NMT commands that take a node offline set `triggers_recalculation=true`, signaling the caller to trigger a posture recalculation:

| NMT Command Byte | State | Triggers Recalculation |
|-----------------|-------|----------------------|
| `0x01` | Start (Operational) | No |
| `0x02` | Stop | Yes |
| `0x80` | Pre-Operational | Yes |
| `0x81` | Reset Node | Yes |
| `0x82` | Reset Communication | Yes |

### Example Request

```bash
curl -X POST http://localhost:8090/industrial/canopen/evaluate \
  -H "Authorization: Bearer $AEGIS_ADMIN_TOKEN" \
  -H "x-aegis-client-id: can-gateway-01" \
  -d '{
    "node_id": 5,
    "function_code": 3,
    "data": [0, 16, 0, 0, 0, 0, 0, 0],
    "source_node": "robot_joint_01"
  }'
```

**Response (Emergency, generic error 0x1000):**
```json
{
  "protocol": "canopen",
  "command": "ReadTelemetry",
  "allowed": true,
  "denial_reason": null,
  "posture_at_evaluation": "Nominal",
  "adapter_details": {
    "message_type": "Emergency",
    "node_id": 5,
    "is_emergency": true,
    "emergency_code": 4096,
    "triggers_recalculation": false
  },
  "audit_ref": "1716480000001"
}
```

---

## Protocol: DNP3

**Used in:** Power grids, substations, water treatment plants, wastewater systems, oil and gas pipelines, natural gas distribution.

DNP3 (Distributed Network Protocol 3) is the dominant protocol for SCADA systems controlling critical infrastructure. It was designed for high-reliability, low-bandwidth environments with inherent support for data integrity and time synchronization.

**Security Note:** DNP3 was designed before cybersecurity was a concern. Broadcast commands (destination address `0xFFFF`) are a significant attack vector — a single malicious packet can command every device on the network simultaneously. Aegis always creates an audit entry for broadcast commands regardless of posture.

### Function Code Mapping

| Function Code | Name | Command |
|-------------|------|---------|
| `0x01` | Read | `ReadTelemetry` |
| `0x02` | Write | `WriteState` |
| `0x03` | Select | `WriteState` |
| `0x04` | Operate | `SystemMutation` |
| `0x05` | Direct_Operate | `SystemMutation` |
| `0x06` | Direct_Operate_NR | `SystemMutation` |
| `0x07` | Freeze | `WriteState` |
| `0x08` | Freeze_NR | `WriteState` |
| `0x81` | Response | `ReadTelemetry` |
| `0x82` | Unsolicited_Response | `ReadTelemetry` |

### Critical Infrastructure Object Groups

| Group | Name | Risk |
|-------|------|------|
| 12 | CROB (Control Relay Output Block) | Direct actuator control |
| 41 | Analog Output Block | Setpoint control |
| 42 | Analog Output Event | |
| 110-113 | Octet String | Proprietary control |

### Broadcast Detection

Any message with `dest_address == 0xFFFF` is flagged as a broadcast. Aegis always creates an audit chain entry for broadcast commands with event type `DNP3_BROADCAST_COMMAND`, regardless of whether the command is allowed.

### Example Requests

**Read (allowed in any posture):**
```bash
curl -X POST http://localhost:8090/industrial/dnp3/evaluate \
  -H "Authorization: Bearer $AEGIS_ADMIN_TOKEN" \
  -H "x-aegis-client-id: dnp3-gateway-01" \
  -d '{
    "source_address": 1,
    "dest_address": 10,
    "function_code": 1,
    "data_link_control": 0,
    "objects": [{"group": 30, "variation": 1, "data": []}],
    "source_node": "substation_01"
  }'
```

**Direct Operate with CROB (denied in Degraded posture):**
```json
{
  "protocol": "dnp3",
  "command": "SystemMutation",
  "allowed": false,
  "denial_reason": "FLEET_DEGRADED_WRITE_BLOCKED",
  "posture_at_evaluation": "Degraded",
  "adapter_details": {
    "function_name": "Direct_Operate",
    "is_control": true,
    "is_broadcast": false,
    "critical_infrastructure_relevant": true
  },
  "audit_ref": "1716480000002"
}
```

---

## Unified Evaluation Endpoint

All five protocols can be evaluated through a single endpoint using the `protocol` discriminator:

```
POST /industrial/evaluate
Authorization: Bearer <AEGIS_ADMIN_TOKEN>
x-aegis-client-id: <client-id>
Content-Type: application/json

{
  "protocol": "modbus" | "opcua" | "ethernet_ip" | "canopen" | "dnp3",
  "message": { ... protocol-specific fields ... }
}
```

The `message` field schema varies by protocol. See the per-protocol sections above for field definitions.

### Unified Response Schema

```json
{
  "protocol": "string",
  "command": "ReadTelemetry | WriteState | SystemMutation | Unknown",
  "allowed": true,
  "denial_reason": null,
  "posture_at_evaluation": "Nominal | Degraded | LockedOut",
  "adapter_details": { ... },
  "audit_ref": "string",
  "triggers_recalculation": false
}
```

## Audit Trail

Every industrial evaluation is subject to the Aegis audit chain:

- **Denials** always create an `INDUSTRIAL_ACTION_DENIED` entry
- **DNP3 broadcast commands** always create a `DNP3_BROADCAST_COMMAND` entry
- **CANOpen NMT node-offline transitions** create a `CANOPEN_NMT_NODE_OFFLINE` entry
- **EtherNet/IP allowed broadcasts** on safety-relevant classes create an `INDUSTRIAL_ACTION_ALLOWED_BROADCAST` entry

All entries are SHA-256 hash-chained and optionally Ed25519-signed (when `AEGIS_LOG_SIGNING_KEY` is set).
