# Aegis LLM Integration Guide

Practical guide for connecting AI agents and language model pipelines to the Aegis Action Filter safety enforcement layer.

---

## Quick Start (5-Minute Integration)

The fastest path to LLM-safe actuator control: intercept every AI-generated action with a single HTTP call before sending it to hardware.

**Step 1.** Start Aegis with an admin token:
```bash
export AEGIS_ADMIN_TOKEN="your-secret-token"
export AEGIS_SUPERVISOR_RESET_KEY="your-reset-key"
./aegis_verifier_service
# Listening on 0.0.0.0:8090
```

**Step 2.** Wrap every AI-generated action with a safety check:
```bash
curl -s -X POST http://localhost:8090/action_filter/evaluate \
  -H "Authorization: Bearer your-secret-token" \
  -H "x-aegis-client-id: my-llm-agent" \
  -H "Content-Type: application/json" \
  -d '{
    "action_type": "cmd_vel",
    "target_node": "robot_base",
    "risk_class": "kinetic_write",
    "payload": {
      "linear_x": 0.3,
      "linear_y": 0.0,
      "linear_z": 0.0,
      "angular_x": 0.0,
      "angular_y": 0.0,
      "angular_z": 0.2
    }
  }'
```

Expected response when the fleet is healthy:
```json
{
  "allowed": true,
  "reason": "NOMINAL_VALID_KINEMATICS",
  "posture_at_evaluation": "Nominal",
  "request_id": "1748022345123"
}
```

**Step 3.** In your agent loop, only proceed if `allowed` is `true`. Never execute hardware commands based on model output alone.

That's the entire integration. The rest of this guide covers authentication, handling all response cases, posture stream subscriptions, and production recommendations.

---

## Authentication

Aegis uses two authentication mechanisms for the action filter endpoint:

### Bearer Token (Required on all protected routes)

All calls to `/action_filter/evaluate` require an `Authorization` header with the admin bearer token:

```
Authorization: Bearer <AEGIS_ADMIN_TOKEN>
```

The token is read from the `AEGIS_ADMIN_TOKEN` environment variable at startup. If the variable is absent or empty, the service returns `503 Service Unavailable` on all protected routes (fail-closed, not fail-open). There are no hardcoded fallback tokens.

Token comparison uses constant-time comparison to prevent timing attacks. Never use standard string equality for token comparison in your own code that wraps this API.

### Client Identity Header (Required for identity-gated routes)

The `/action_filter/evaluate` endpoint is in the identity-gated tier, which requires a client identity header in addition to the bearer token:

```
x-aegis-client-id: <your-agent-identifier>
```

The header name defaults to `x-aegis-client-id` and can be configured via the `AEGIS_CLIENT_ID_HEADER` environment variable. The value is a free-form string identifying the AI agent or orchestration system making the request (e.g., `"openai-agent-001"`, `"langchain-robot-controller"`, `"my-custom-planner"`).

This header is required when `AEGIS_TRUSTED_INGRESS_MODE=true`. In default configuration (`AEGIS_TRUSTED_INGRESS_MODE=false`), the header is accepted but not strictly required. For production deployments, enable trusted ingress mode to enforce strict client identity.

**Complete authenticated request headers:**
```
Authorization: Bearer <AEGIS_ADMIN_TOKEN>
x-aegis-client-id: <client-identifier>
Content-Type: application/json
```

### Token Security Recommendations

- Store `AEGIS_ADMIN_TOKEN` in a secrets manager (AWS Secrets Manager, HashiCorp Vault, Kubernetes Secrets). Never put it in code or version control.
- Rotate tokens regularly. The `/system/audit/rotate-signing-key` endpoint supports key rotation for the audit chain signing key.
- Use separate client IDs per agent or deployment (e.g., `"gpt4-prod-001"`, `"claude-staging-002"`). This allows per-agent audit filtering.
- In Kubernetes deployments, inject the token as an environment variable from a Secret, not a ConfigMap.

---

## Request/Response Schema

### Request

```json
{
  "action_type": "string",
  "target_node": "string",
  "risk_class": "string",
  "payload": {}
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `action_type` | string | Yes | The action to perform. Supported values: `"cmd_vel"`, `"read_telemetry"`. Any other value is denied as `UNKNOWN_ACTION_TYPE`. Empty string is denied as `MISSING_ACTION_TYPE`. |
| `target_node` | string | Yes | The node ID of the fleet member receiving this command. Must not be empty. Used in audit logging. |
| `risk_class` | string | Yes | Risk classification. Use `"kinetic_write"` for motion commands, `"read"` for telemetry reads. Used in posture policy evaluation. |
| `payload` | object | Yes | Action-specific parameters. See per-action payload schemas below. |

**Payload schema for `cmd_vel`:**

```json
{
  "linear_x": 0.3,
  "linear_y": 0.0,
  "linear_z": 0.0,
  "angular_x": 0.0,
  "angular_y": 0.0,
  "angular_z": 0.2
}
```

| Field | Type | Limit | Description |
|-------|------|-------|-------------|
| `linear_x` | float | ±0.5 m/s | Forward/backward velocity |
| `linear_y` | float | 0.0 only | Lateral velocity (must be zero for ground robots) |
| `linear_z` | float | 0.0 only | Vertical velocity (must be zero for ground robots) |
| `angular_x` | float | 0.0 only | Roll rate (must be zero) |
| `angular_y` | float | 0.0 only | Pitch rate (must be zero) |
| `angular_z` | float | ±1.0 rad/s | Yaw rate |

All fields are required. NaN and Infinity values are rejected.

**Payload schema for `read_telemetry`:**

```json
{}
```

Empty object is sufficient. No payload validation is performed.

### Response

```json
{
  "allowed": true,
  "reason": "NOMINAL_VALID_KINEMATICS",
  "posture_at_evaluation": "Nominal",
  "request_id": "1748022345123"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `allowed` | boolean | Primary decision. `true` means the action is approved. `false` means it must not be executed. This is the only field your agent loop *must* check. |
| `reason` | string | Structured reason code. Stable identifier suitable for programmatic handling. See the reason code table in the [Action Filter document](action_filter.md#api-reference). |
| `posture_at_evaluation` | string | Fleet posture at the time of this evaluation: `"Nominal"`, `"Degraded"`, or `"LockedOut"`. Useful for diagnosing why an action was denied. |
| `request_id` | string | Unique identifier for this evaluation. Matches the corresponding record in the audit chain. |

**Error response (malformed JSON, HTTP 400):**

```json
{
  "error": "MALFORMED_REQUEST",
  "detail": "...",
  "allowed": false
}
```

The `allowed: false` field is always present in error responses so that agent loops that only check `allowed` will correctly block execution even when the request itself was malformed.

---

## Handling Allow / Mutate / Deny in Agent Loops

The action filter returns one of three effective outcomes:

### Allow (`allowed: true`)

The action is approved. Your agent loop should proceed with hardware execution.

```python
result = check_with_aegis(action)
if result["allowed"]:
    hardware_client.execute(action)
    log.info(f"Executed: {action['action_type']} on {action['target_node']}")
```

### Deny — Posture (`reason` contains `POSTURE`)

The action is valid but the fleet is in a restricted state. The model should be informed that physical commands are currently unavailable and can switch to read-only observation.

```python
if not result["allowed"]:
    posture = result["posture_at_evaluation"]
    reason = result["reason"]
    
    if "POSTURE" in reason or "LOCKEDOUT" in reason or "DEGRADED" in reason:
        # Fleet is restricted — switch agent to read-only mode
        agent.set_mode("observe_only")
        log.warning(f"Fleet is {posture}. Kinetic commands suspended.")
        
        # Optionally: subscribe to SSE posture stream to know when to resume
        subscribe_to_posture_stream()
```

### Deny — Kinematic / Unknown / Malformed

The action itself is invalid, regardless of posture. The model generated an out-of-envelope command, an unrecognized action type, or a malformed payload.

```python
    elif reason == "KINEMATIC_ENVELOPE_BREACH":
        # Model hallucinated an unsafe velocity — inform and retry with clamped value
        log.error(f"Kinematic envelope breach: {action['payload']}")
        clamped = clamp_to_safe_envelope(action["payload"])
        retry_with_aegis(action["action_type"], action["target_node"], clamped)
        
    elif reason == "UNKNOWN_ACTION_TYPE":
        # Model invented a non-existent action — this is a hallucination
        log.error(f"Unknown action type: {action['action_type']}. Possible hallucination.")
        # Do NOT retry with the same action type. Log for model fine-tuning.
        
    elif reason == "MALFORMED_REQUEST":
        # Model output could not be parsed — structured output enforcement needed
        log.error(f"Malformed model output: {result['detail']}")
        # Add retry with forced JSON schema validation at the model layer
```

The key principle: `allowed: false` means **stop**. Do not retry with the same parameters without understanding and addressing the reason code. Do not try to route around Aegis. The audit trail records every attempt.

---

## Registering the AI Model as a Fleet Node

An AI model or agent can be registered as a fleet node. This allows Aegis to track its trust state and, critically, mark it `Untrusted` if it begins exhibiting anomalous behavior (repeated hallucinated commands, out-of-envelope requests, unexpected action types).

**Step 1. Register the model as a node:**

```bash
curl -s -X POST http://localhost:8090/attestation/register \
  -H "Authorization: Bearer your-admin-token" \
  -H "Content-Type: application/json" \
  -d '{
    "node_id": "gpt4o-agent-001",
    "ak_public_pem": null,
    "expected_pcr16_digest_hex": null
  }'
```

**Step 2. Mark the model Untrusted if it hallucinates repeatedly:**

If your agent loop detects repeated `UNKNOWN_ACTION_TYPE` or `KINEMATIC_ENVELOPE_BREACH` responses, it can report a sensor fault to escalate the model's trust state:

```bash
curl -s -X POST http://localhost:8090/fleet/diagnostics/report \
  -H "Authorization: Bearer your-admin-token" \
  -H "Content-Type: application/json" \
  -d '{
    "source_node_id": "gpt4o-agent-001",
    "confidence_score": 0.3,
    "hardware_fault_detected": false
  }'
```

A confidence score below the floor (default 0.70) marks the node `Untrusted`. Once a model node is `Untrusted`, it can affect the fleet-wide posture calculation via the dependency graph — making downstream actuator nodes `Degraded` or `LockedOut` if they depend on a trusted AI model for commands.

**Step 3. Recovery requires 5 consecutive healthy reports:**

The recovery hysteresis prevents a briefly-corrected model from immediately regaining full trust. Five consecutive healthy reports within a 10-second window are required. This prevents oscillation and ensures genuine stability before resuming kinetic commands.

---

## SSE Posture Stream Subscription

The posture stream lets your agent proactively know when the fleet state changes, rather than discovering it only when an action is denied.

**Subscribe:**

```bash
curl -N http://localhost:8090/system/posture/stream \
  -H "Authorization: Bearer your-admin-token" \
  -H "x-aegis-client-id: my-llm-agent" \
  -H "Accept: text/event-stream"
```

The stream emits Server-Sent Events (SSE) with JSON payloads:

```
data: {"event_type":"NODE_STATUS_CHANGED","node_id":"sensor_01","emitted_at_ms":1748022345123,"posture":{"node_id":"sensor_01","posture":"Degraded","blocked_by":[]}}
```

**Python SSE client:**

```python
import sseclient
import requests

def subscribe_posture_stream(aegis_url, token, client_id):
    resp = requests.get(
        f"{aegis_url}/system/posture/stream",
        headers={
            "Authorization": f"Bearer {token}",
            "x-aegis-client-id": client_id,
            "Accept": "text/event-stream",
        },
        stream=True,
    )
    client = sseclient.SSEClient(resp)
    for event in client.events():
        data = json.loads(event.data)
        if data["event_type"] == "NODE_STATUS_CHANGED":
            # Proactively switch agent mode before the next action attempt
            if data.get("posture", {}).get("posture") == "Degraded":
                agent.set_mode("read_only")
            elif data.get("posture", {}).get("posture") == "Nominal":
                agent.set_mode("full_control")
```

**Event types:**

| Event Type | When Emitted |
|------------|-------------|
| `NODE_STATUS_CHANGED` | A node's trust state changed (fault detected or recovery confirmed) |
| `DEPENDENCY_GRAPH_MUTATED` | Dependency graph edges were updated |
| `NODE_IDENTITY_PROVISIONED` | A new hardware fingerprint was registered |

Subscribe to the posture stream in a background thread at agent startup. This eliminates the latency of discovering a posture change at the next action attempt — your agent can pre-emptively switch to read-only mode before the first denied action.

---

## Production Recommendations

**1. Subscribe to the SSE stream at startup.** Don't wait for a denied action to discover that the fleet is degraded. A background thread subscribed to `/system/posture/stream` gives you real-time visibility into fleet health changes and lets your agent switch modes proactively.

**2. Always check `allowed` before any hardware call.** There should be no code path in your agent loop that sends a command to hardware without first receiving `allowed: true` from Aegis. Treat the Aegis response as a gate, not a suggestion.

**3. Use structured reason codes for agent behavior.** The `reason` field is a stable, machine-readable code. Build your agent loop to branch on reason codes rather than parsing free-text messages. `KINEMATIC_ENVELOPE_BREACH` should trigger a different response than `UNKNOWN_ACTION_TYPE` — the first suggests a clamp-and-retry, the second suggests a hallucination that needs to be logged for model evaluation.

**4. Log every denied action with its `request_id`.** The `request_id` in the response correlates to the audit chain entry. If you need to investigate an incident, the `request_id` is the key to pull the exact record from `/system/audit/export`.

**5. Register your AI model as a fleet node.** This enables the dependency graph to propagate trust state from the model to its downstream actuators. A hallucinating model can be marked `Untrusted`, which cascades to `Degraded` posture for any actuator that depends on it for commands.

**6. Enable trusted ingress mode in production.** Set `AEGIS_TRUSTED_INGRESS_MODE=true` and ensure every client sends a unique `x-aegis-client-id`. This enables per-agent audit filtering and prevents unauthenticated clients from reaching the action filter.

**7. Set connection timeouts.** Aegis is a synchronous HTTP endpoint. Set a 5-second timeout on your HTTP client. If Aegis is unreachable, treat the failure as a deny and halt the action. Never default to executing hardware commands when the safety filter is unavailable.

**8. Use the audit export for compliance.** The `/system/audit/export` endpoint provides a paginated, hash-chained export of all action filter decisions. Export and archive this log periodically for incident investigation, regulatory compliance, and model evaluation data.

**9. Test with all three posture states.** Your agent loop must handle `Nominal`, `Degraded`, and `LockedOut` postures correctly. Write integration tests that artificially set the posture (by marking fleet nodes untrusted) and verify that your agent switches to the appropriate mode without attempting kinetic commands.

**10. Never retry `UNKNOWN_ACTION_TYPE` with the same action.** If the model emits an unrecognized action type, retrying will always fail. The correct response is to log the hallucination, report it to your model evaluation pipeline, and prompt the model with corrected tool descriptions or function schemas.
