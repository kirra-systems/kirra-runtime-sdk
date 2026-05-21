# Aegis v1.0.0 — Gateway Policy Matrix

This document defines the formal behavioral specifications and security invariants for the Aegis v1.0.0 edge gateway proxy interceptor. It serves as the definitive reference for proving how and why network payloads are validated, classified, and either routed or dropped at the cluster boundary.

---

## 1. Command Classification Engine

The gateway proxy applies a strict zero-trust model to incoming message payloads. To eliminate parameter-tampering, request-smuggling, and out-of-band injection vectors, policy decisions are derived exclusively from immutable transport metadata.

> **Core Invariant**: Command classification is derived solely from the HTTP Method and the URI Path. Caller-supplied payload headers, query variables, or inner body structures are never trusted for classification.

The core parsing routine maps incoming traffic to a specific command classification using the following path-matching table:

| HTTP Pattern | Target Path Scope | Command Class (`OperationalCommand`) |
| :--- | :--- | :--- |
| `GET` | `/metrics*` | `ReadTelemetry` |
| `GET` | `/telemetry*` | `ReadTelemetry` |
| `GET` | `/health*` | `ReadTelemetry` |
| `POST` | `/actuator/*` | `WriteState` |
| `PUT` | `/control/*` | `WriteState` |
| `POST` | `/cmd_vel` | `WriteState` |
| `POST` | `/firmware/*` | `SystemMutation` |
| `POST` | `/config/*` | `SystemMutation` |
| `DELETE` | `*` (Any path matched to verb) | `SystemMutation` |
| *Any* | Unmapped / Variant Path Pattern | `Unknown` |

> **Implementation note**: The v1.0.0 codebase maps unmapped HTTP methods to `OperationalCommand::SystemMutation` rather than a dedicated `Unknown` variant. This means unknown methods are denied in `Degraded` and `LockedOut` postures but are **permitted in `Nominal` posture**, diverging from the full-deny policy in Section 2 below. A future hardening pass should introduce a distinct `Unknown` variant that evaluates to deny across all posture states, including `Nominal`.

---

## 2. FleetPosture Authorization Matrix

Once a request has been classified, the proxy matches the command class against the cluster's current cryptographic legitimacy state machine.

| FleetPosture State | ReadTelemetry | WriteState | SystemMutation | Unknown |
| :--- | :---: | :---: | :---: | :---: |
| **Nominal** | **Allow** | **Allow** | **Allow** | Deny |
| **Degraded** | **Allow** | Deny | Deny | Deny |
| **LockedOut** | Deny | Deny | Deny | Deny |

### Invariant Constraint Enforcement:
* `OperationalCommand::Unknown` always evaluates to an immediate, hard **Deny**.
* This rejection is absolute and applies across all posture states—including **Nominal**—eliminating implicit parsing fallbacks and securing unmapped route variations.

---

## 3. Cache Freshness Enforcement

The edge proxy operates asynchronously from the core verifier to protect system latency. To prevent authorization inheritance across cluster state changes, the proxy must continuously assert temporal validity.

Authorization decisions are instantly invalidated if the lookback threshold is breached:

```
now_ms() - updated_at_epoch_ms > ttl_ms
```

### Staleness Policy:
* A stale posture cache is treated identically to a missing posture cache.
* If the cache fails the threshold check, permissions are instantly dropped, and the gateway issues an immediate rejection without querying downstream actuators:
  ```text
  HTTP/1.1 403 Forbidden
  Content-Type: text/plain
  Body: AEGIS_POLICY_DENY
  ```

---

## 4. Middleware Execution Flow

The gateway proxy executes interceptor logic sequentially inside a synchronized, low-overhead asynchronous wrapper.

### Step-by-Step Interception Sequence:
 1. **Receive HTTP Request**: Capture incoming raw request stream before socket dispatch.
 2. **Extract Metadata**: Pull the true HTTP Method and URI Path fields.
 3. **Invoke Classification**: Execute `classify_http_command()` to assign the runtime command class.
 4. **Acquire Posture Lock**: Request a shared read-lock (`RwLockReadGuard`) on the `SharedPostureCache`.
 5. **Assert Freshness**: Compare system clock offsets against the cached record epoch timestamp.
 6. **Evaluate Matrix Rules**: Assert the classification against the current `FleetPosture` enumeration.
 7. **Drop Lock and Dispatch**: **The cache read lock is explicitly dropped before downstream service execution.** This keeps the lock allocation window bound to a tight, microsecond-scale block, ensuring heavy data streams can never cause thread-starvation across proxy worker threads.
 8. **Forward or Reject**: Route the stream forward to the physical hardware or drop it with a `403 Forbidden`.

---

## 5. Forbidden Gateway Regressions

> **CRITICAL GATEWAY ENFORCEMENT PROTECTION RULES**

 * **Do not trust caller-supplied action headers.** Action mapping must remain tightly bound to the true HTTP verb and target path to prevent header-injection bypasses.
 * **Do not allow Unknown command classes.** Any payload pattern that cannot be explicitly identified must be rejected immediately, regardless of current cluster health.
 * **Do not bypass TTL freshness checks.** Interceptor routines must never accept cached data blocks without validating their age against active system clocks.
 * **Do not allow stale caches to inherit permissions.** If the verifier communication path breaks, the proxy must default to an unvalidated state, drop mutations, and fail-closed.
 * **Do not downgrade LockedOut into Degraded fallback behavior.** If the posture graph registers a hard isolation event, all telemetry streams and state writes must be blocked instantly.
 * **Do not permit WriteState actions during Degraded posture.** When dependent topology nodes drop trust, mutation capabilities must be sheared away immediately to prevent broken control feedback loops.
 * **Do not move policy evaluation downstream of service execution.** The gateway must validate and clear requests before a single packet hits internal cluster endpoints. Evaluation must always happen at the outermost entry boundary.
