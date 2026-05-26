# Aegis Action Filter — LLM to Actuator Safety Enforcement

## What It Is

The Aegis Action Filter is the enforcement layer between AI model output and physical actuators. When a large language model generates a command — whether via OpenAI function calling, a LangChain tool invocation, or raw JSON output from a custom agent — the Action Filter intercepts that command before it reaches any hardware and evaluates it against the live fleet posture.

The filter operates on the principle that AI models are not trusted to make physical safety decisions. They can reason about goals, choose strategies, and select appropriate actions — but the final determination of whether an action is physically safe to execute at this exact moment, given the current state of the fleet, belongs to Aegis. This separation of concerns is not optional. A model that hallucinates a velocity of 999 m/s must be stopped at the software layer with the same certainty that a hardware interlock would stop it.

The Action Filter combines three independent enforcement mechanisms: posture-aware policy (what is permitted in the current system state), kinematic contract validation (are the physical parameters within the safe operating envelope), and action-type whitelisting (is this a recognized, permitted action at all). All three must pass for a command to be allowed. Any single failure results in an unambiguous denial with a structured reason code that is logged to the SHA-256 hash-chained audit ledger.

---

## The Pipeline

```
┌─────────────────────────────────────────────────────────────────────┐
│                    AI Model / Agent Loop                            │
│   GPT-4o, Claude, Gemini, LangChain, custom agent                  │
└─────────────────────────┬───────────────────────────────────────────┘
                          │  JSON action intent
                          ▼
┌─────────────────────────────────────────────────────────────────────┐
│              POST /action_filter/evaluate                           │
│         (Bearer token + x-aegis-client-id required)                │
│                                                                     │
│  Stage 1: JSON Schema Validation                                    │
│    Malformed JSON → 400 BAD_REQUEST (logged, never 422 or 500)      │
│                                                                     │
│  Stage 2: Action Type Whitelist                                     │
│    Unknown or empty type → DENY (UNKNOWN_ACTION_TYPE)               │
│                                                                     │
│  Stage 3: Posture Gate                                              │
│    Read live FleetPosture from posture cache                        │
│    LockedOut → DENY all                                             │
│    Degraded → DENY kinetic_write, ALLOW read_telemetry only         │
│    Nominal → proceed to Stage 4                                     │
│                                                                     │
│  Stage 4: Kinematic Contract Validation (cmd_vel only)              │
│    Parse payload into CmdVel struct                                 │
│    Validate against DEFAULT_CMD_VEL_LIMITS envelope                 │
│    Breach → DENY (KINEMATIC_ENVELOPE_BREACH)                        │
│                                                                     │
│  Stage 5: Audit Chain Write                                         │
│    Every decision (Allow or Deny) logged to SHA-256 chain           │
│    request_id + posture_at_evaluation included in every record      │
└─────────────────────────┬───────────────────────────────────────────┘
                          │  JSON decision
                          ▼
┌─────────────────────────────────────────────────────────────────────┐
│                      Agent Loop                                     │
│   Reads: allowed, reason, posture_at_evaluation, request_id         │
│   If allowed: sends command to actuator hardware                    │
│   If denied: logs, retries, escalates, or aborts — never bypasses   │
└─────────────────────────┬───────────────────────────────────────────┘
                          │  (only if allowed=true)
                          ▼
┌─────────────────────────────────────────────────────────────────────┐
│                    Physical Actuator                                 │
│   Robot base, drone, industrial PLC, vehicle drivetrain             │
└─────────────────────────────────────────────────────────────────────┘
```

Each stage is independent and runs in sequence. A failure at any stage short-circuits the pipeline and returns an immediate denial. The posture check (Stage 3) reads from an in-memory cache updated by the posture engine worker, so it does not block on a SQLite read at request time. The audit write (Stage 5) is the only database operation on the critical path.

---

## Why This Matters for AI Companies

**The Hallucination Problem.** Language models generate text that is statistically plausible, not physically safe. A model instructed to "move the robot forward as fast as possible" may output a velocity command that exceeds the physical design limits of the platform. A model that has seen robotics tutorials in its training data may confidently emit a `deploy_payload_at_max_velocity` command that does not correspond to any registered action type on the actual fleet. Without a hard enforcement layer, these hallucinations become actuator commands.

Aegis eliminates this failure mode at the transport layer. The model's output is treated as an *intent claim* — a request to perform an action — rather than an authorized instruction. The claim is evaluated against ground truth (live fleet posture, kinematic contracts, registered action whitelist) and either approved or denied. The model never touches hardware directly.

**Posture-Aware Restriction.** An action that would be safe under nominal conditions may be catastrophic when the fleet is in a degraded state. If a sensor fusion node has reported a hardware fault, the fleet posture transitions to `Degraded`. In Degraded posture, all kinetic writes are denied regardless of the model's confidence in its command. The model does not know — and cannot be expected to know — the real-time health state of every fleet component. Aegis enforces posture-appropriate restrictions automatically and transparently.

**The Audit Trail.** Every action evaluated by the filter is logged to a SHA-256 hash-chained audit ledger. The chain links each record to its predecessor via a cryptographic hash, making retroactive modification detectable. This means that every command an AI model attempted — whether allowed or denied — is permanently recorded with the fleet posture at evaluation time, the request identifier, and the reason for the decision. For regulated deployments (aerospace, medical robotics, autonomous vehicles), this ledger provides the evidence trail required for incident investigation and certification.

---

## Posture-Action Matrix

| Action Type | Nominal | Degraded | LockedOut |
|-------------|---------|----------|-----------|
| `cmd_vel` (kinetic write) | ✓ with kinematics validation | ✗ `DEGRADED_POSTURE_KINETIC_DENIED` | ✗ `LOCKEDOUT_POSTURE_ABSOLUTE_DENIAL` |
| `read_telemetry` | ✓ `DEGRADED_READ_ONLY_PERMITTED`* | ✓ `DEGRADED_READ_ONLY_PERMITTED` | ✗ `LOCKEDOUT_POSTURE_ABSOLUTE_DENIAL` |
| Unknown / unrecognized | ✗ `UNKNOWN_ACTION_TYPE` | ✗ `UNKNOWN_ACTION_TYPE` | ✗ `LOCKEDOUT_POSTURE_ABSOLUTE_DENIAL` |
| Empty `action_type` | ✗ `MISSING_ACTION_TYPE` | ✗ `MISSING_ACTION_TYPE` | ✗ `LOCKEDOUT_POSTURE_ABSOLUTE_DENIAL` |
| `cmd_vel` with envelope breach | ✗ `KINEMATIC_ENVELOPE_BREACH` | ✗ `DEGRADED_POSTURE_KINETIC_DENIED` | ✗ `LOCKEDOUT_POSTURE_ABSOLUTE_DENIAL` |
| `cmd_vel` with malformed payload | ✗ `MALFORMED_CMD_VEL_PAYLOAD` | ✗ `DEGRADED_POSTURE_KINETIC_DENIED` | ✗ `LOCKEDOUT_POSTURE_ABSOLUTE_DENIAL` |

\* In Nominal posture, `read_telemetry` is permitted. The reason code shown above is returned in Degraded posture to distinguish the policy basis.

The design principle is explicit: anything not on the whitelist is denied. There is no fallthrough, no "try it and see", no default allow. The whitelist is enforced at the code level — `cmd_vel` and `read_telemetry` are the only registered action types; every other string, regardless of how plausible it sounds, returns `UNKNOWN_ACTION_TYPE`.

---

## API Reference

### POST /action_filter/evaluate

**Authentication:** `Authorization: Bearer <AEGIS_ADMIN_TOKEN>` and `x-aegis-client-id: <client-id>` headers required.

**Request Schema:**

```json
{
  "action_type": "string",      // required — e.g. "cmd_vel", "read_telemetry"
  "target_node": "string",      // required — fleet node ID receiving the command
  "risk_class": "string",       // required — e.g. "kinetic_write", "read"
  "payload": { ... }            // required — action-specific parameters
}
```

Field descriptions:
- `action_type`: The type of action the AI model wants to execute. Must match a registered action type exactly. Unknown strings are denied.
- `target_node`: The node ID of the robot, sensor, or actuator that would receive this command. Used for audit logging and future per-node policy enforcement.
- `risk_class`: A classification of the action's risk profile. `"kinetic_write"` indicates the action would cause physical movement. `"read"` indicates a non-mutating observation.
- `payload`: Action-specific parameters. For `cmd_vel`, must be a JSON object with `linear_x`, `linear_y`, `linear_z`, `angular_x`, `angular_y`, `angular_z` fields (all floats). For `read_telemetry`, may be an empty object.

**Response Schema:**

```json
{
  "allowed": true,
  "reason": "NOMINAL_VALID_KINEMATICS",
  "posture_at_evaluation": "Nominal",
  "request_id": "1748022345123"
}
```

Response fields:
- `allowed` (boolean): Whether the action is approved for execution. The agent loop must check this field before sending any command to hardware.
- `reason` (string): A structured reason code explaining the decision. Always present for both allowed and denied responses. Reason codes are stable identifiers suitable for logging, alerting, and programmatic handling.
- `posture_at_evaluation` (string): The fleet posture at the moment of evaluation — `"Nominal"`, `"Degraded"`, or `"LockedOut"`. Useful for debugging and understanding why a normally-valid action was denied.
- `request_id` (string): A unique identifier for this evaluation. Corresponds to the audit chain entry. Use this when correlating API responses with the `/system/audit/export` log.

**Reason Codes:**

| Reason Code | Meaning |
|-------------|---------|
| `NOMINAL_VALID_KINEMATICS` | Allowed — valid cmd_vel in Nominal posture |
| `DEGRADED_READ_ONLY_PERMITTED` | Allowed — read_telemetry in Degraded posture |
| `UNKNOWN_ACTION_TYPE` | Denied — action_type not on whitelist |
| `MISSING_ACTION_TYPE` | Denied — action_type field is empty |
| `MISSING_TARGET_NODE` | Denied — target_node field is empty |
| `KINEMATIC_ENVELOPE_BREACH` | Denied — velocity/angular rate exceeds limits |
| `MALFORMED_CMD_VEL_PAYLOAD` | Denied — payload cannot be parsed as CmdVel |
| `DEGRADED_POSTURE_KINETIC_DENIED` | Denied — kinetic_write blocked in Degraded posture |
| `DEGRADED_UNSUPPORTED_CLAIM_TYPE` | Denied — action not supported in Degraded posture |
| `LOCKEDOUT_POSTURE_ABSOLUTE_DENIAL` | Denied — all actions blocked in LockedOut posture |

**curl Examples:**

Allow (nominal, valid kinematics):
```bash
curl -s -X POST http://localhost:8090/action_filter/evaluate \
  -H "Authorization: Bearer your-admin-token" \
  -H "x-aegis-client-id: my-agent" \
  -H "Content-Type: application/json" \
  -d '{
    "action_type": "cmd_vel",
    "target_node": "robot_base",
    "risk_class": "kinetic_write",
    "payload": {"linear_x": 0.3, "linear_y": 0.0, "linear_z": 0.0,
                 "angular_x": 0.0, "angular_y": 0.0, "angular_z": 0.2}
  }'
# Response: {"allowed":true,"reason":"NOMINAL_VALID_KINEMATICS","posture_at_evaluation":"Nominal","request_id":"..."}
```

Deny (hallucinated velocity):
```bash
curl -s -X POST http://localhost:8090/action_filter/evaluate \
  -H "Authorization: Bearer your-admin-token" \
  -H "x-aegis-client-id: my-agent" \
  -H "Content-Type: application/json" \
  -d '{
    "action_type": "cmd_vel",
    "target_node": "robot_base",
    "risk_class": "kinetic_write",
    "payload": {"linear_x": 999.0, "linear_y": 0.0, "linear_z": 0.0,
                 "angular_x": 0.0, "angular_y": 0.0, "angular_z": 0.0}
  }'
# Response: {"allowed":false,"reason":"KINEMATIC_ENVELOPE_BREACH","posture_at_evaluation":"Nominal","request_id":"..."}
```

Deny (degraded posture):
```bash
curl -s -X POST http://localhost:8090/action_filter/evaluate \
  -H "Authorization: Bearer your-admin-token" \
  -H "x-aegis-client-id: my-agent" \
  -H "Content-Type: application/json" \
  -d '{
    "action_type": "cmd_vel",
    "target_node": "robot_base",
    "risk_class": "kinetic_write",
    "payload": {"linear_x": 0.3, "linear_y": 0.0, "linear_z": 0.0,
                 "angular_x": 0.0, "angular_y": 0.0, "angular_z": 0.2}
  }'
# Response (when fleet is Degraded): {"allowed":false,"reason":"DEGRADED_POSTURE_KINETIC_DENIED","posture_at_evaluation":"Degraded","request_id":"..."}
```

Deny (malformed JSON):
```bash
curl -s -X POST http://localhost:8090/action_filter/evaluate \
  -H "Authorization: Bearer your-admin-token" \
  -H "x-aegis-client-id: my-agent" \
  -H "Content-Type: application/json" \
  -d 'not valid json'
# Response: {"error":"MALFORMED_REQUEST","detail":"...","allowed":false}  HTTP 400
```

---

## Hallucination Containment

The Action Filter is specifically designed to contain three categories of AI model output failure:

**Unrecognized action types.** When a model has been trained on robotics literature or tool descriptions from other systems, it may emit action types that sound plausible but do not exist in the registered fleet vocabulary. Examples include `deploy_payload`, `emergency_override`, `maintenance_mode`, `navigate_to_waypoint`, or any other string not explicitly on the whitelist. All of these return `UNKNOWN_ACTION_TYPE` and are logged to the audit chain with the event type `ACTION_FILTER_UNKNOWN_TYPE`. This makes hallucinated command attempts visible in the audit log even though they are never executed.

**Out-of-envelope parameters.** A model may emit a syntactically valid `cmd_vel` command with a velocity value that exceeds the physical limits of the robot platform. The kinematic contract enforces hard limits: `linear_x` cannot exceed ±0.5 m/s, `linear_y` must be 0.0, `linear_z` must be 0.0, and `angular_z` cannot exceed ±1.0 rad/s. Any value outside these limits — whether 999.0 from a hallucination or -0.501 from floating-point imprecision — is rejected with `KINEMATIC_ENVELOPE_BREACH`. The limits are constants defined in `gateway/cmd_vel.rs` and are enforced at parse time without executing any motion.

**Malformed JSON.** AI models occasionally produce malformed JSON output, especially when under token pressure or when a function call schema is not enforced at the model layer. The Action Filter handler uses axum's `JsonRejection` to catch all malformed request bodies and returns a structured `400 BAD_REQUEST` with an `error: MALFORMED_REQUEST` field and a `detail` field containing the parse error. This is also logged to the audit chain. The response is never a `422 Unprocessable Entity` (which would imply the JSON was valid but semantically wrong) and never a `500 Internal Server Error`.

---

## Integration Examples

**OpenAI Function Calling:**

```python
# Define the action as an OpenAI function
check_action = {
    "name": "check_action_with_aegis",
    "description": "Validate a robot action through Aegis safety filter before execution",
    "parameters": {
        "type": "object",
        "properties": {
            "action_type": {"type": "string"},
            "target_node": {"type": "string"},
            "risk_class": {"type": "string"},
            "payload": {"type": "object"}
        },
        "required": ["action_type", "target_node", "risk_class", "payload"]
    }
}

# In the agent loop: always check before executing
def execute_if_safe(action_type, target_node, risk_class, payload):
    result = requests.post(
        "http://aegis:8090/action_filter/evaluate",
        headers={"Authorization": "Bearer $TOKEN", "x-aegis-client-id": "gpt-agent"},
        json={"action_type": action_type, "target_node": target_node,
              "risk_class": risk_class, "payload": payload}
    ).json()
    
    if result["allowed"]:
        send_to_hardware(action_type, target_node, payload)
    else:
        log.warning(f"Action denied: {result['reason']} (posture: {result['posture_at_evaluation']})")
        # The agent can react to the reason code — e.g., switch to read-only mode
        # when posture is Degraded
```

**LangChain Tool Decorator:**

```python
from langchain.tools import tool

@tool
def move_robot(target_node: str, linear_x: float, angular_z: float) -> str:
    """Move a robot at the specified velocity. Always filtered by Aegis."""
    result = aegis_check("cmd_vel", target_node, "kinetic_write",
                         {"linear_x": linear_x, "linear_y": 0.0, "linear_z": 0.0,
                          "angular_x": 0.0, "angular_y": 0.0, "angular_z": angular_z})
    if result["allowed"]:
        hardware_send(target_node, linear_x, angular_z)
        return f"Moving at {linear_x} m/s"
    else:
        return f"Denied: {result['reason']} — posture is {result['posture_at_evaluation']}"
```

**Direct JSON (any agent framework):**

```python
# Universal pattern: every AI-generated action becomes an HTTP call to Aegis
# before touching any hardware interface.
action = ai_model.generate_action(current_state)  # returns dict
aegis_result = requests.post("http://aegis:8090/action_filter/evaluate",
                             headers=auth_headers, json=action).json()
if aegis_result["allowed"]:
    robot_interface.execute(action)
```

The integration pattern is identical across frameworks: the AI model decides *what* to do; Aegis decides *whether* it is safe to do it right now.

---

## Audit Trail

Every call to `POST /action_filter/evaluate` results in a write to the SHA-256 hash-chained audit ledger, regardless of the decision. The audit record includes:

- `request_id`: Monotonic millisecond timestamp used as a unique identifier
- `target_node`: The fleet node the action targeted
- `action_type`: The action type string from the request
- `risk_class`: The risk classification
- `allowed`: Boolean decision
- `reason`: The structured reason code
- `posture`: The fleet posture at evaluation time

Records are written with one of three event type tags: `ACTION_FILTER_ALLOWED`, `ACTION_FILTER_DENIED`, or `ACTION_FILTER_UNKNOWN_TYPE` (for unrecognized action types specifically). Malformed request bodies are logged as `ACTION_FILTER_MALFORMED_REQUEST`.

Each record in the audit chain includes the SHA-256 hash of the previous record. This hash chain means that any retroactive modification — deleting a denied action from the log, changing an `allowed: false` to `allowed: true`, or altering a reason code — would break the chain integrity, which is detectable via `GET /system/audit/verify`. The chain can be exported in paginated form via `GET /system/audit/export` for offline analysis or archival.
