# Kirra Safe State Specification

Document ID: KIRRA-SSS-001
Version: 1.1
Status: Active
Standard: ISO 26262 ASIL-D
Date: 2026-06-03

Change log:
  - v1.1 (2026-06-03, Issue #70): SS-002 Degraded redefined from "MRC
    reduced-speed crawl" to "controlled decel-to-stop-and-HOLD with no
    autonomous re-initiation of motion"; added the Cruise Oct-2023
    pullover-drag rationale; disambiguated Degraded MRC envelope vs LockedOut
    MRC fallback; folded in the Issue #49 closeout (§7). Behavior change, not
    docs-only — enforced by `enforce_degraded_decel_to_stop`.

## 1. Overview

A safe state is a system state in which no unreasonable risk exists.
When Kirra detects a fault condition, it transitions to the appropriate
safe state based on fault severity. This document specifies each safe
state, its trigger conditions, behavior, and recovery path.

This document covers all 16 safety goals (SG-001 through SG-016) as
defined in `docs/safety/SAFETY_GOALS.md`. For the 11 goals without test
coverage, see `docs/safety/RTM_GAP_REPORT.md` and
`tests/cert_003_rtm_gap_stubs.rs`.

## 2. Safe States

### SS-001: Normal Operation (PostureState::Nominal)

Behavior:
  Full kinematic envelope. 35.0 m/s velocity ceiling. Stricter
  acceleration rate-limit applied by `KirraGovernor` nominal profile.
  All commands forwarded to actuators subject to kinematic limits.

Entry conditions:
  - All nodes trusted
  - No RSS violation (gap ≥ `longitudinal_safe_distance`)
  - Governor reachable
  - All startup invariants satisfied (`startup_sentinel` passed)
  - No active telemetry timeout

Exit conditions: Any fault trigger in SS-002, SS-003, or SS-004.

Safety goals covered: SG-001, SG-002, SG-004, SG-005, SG-011

---

### SS-002: Minimum Risk Condition — Controlled Decel-to-Stop and Hold (PostureState::Degraded)

Behavior (Issue #70):
  Degraded is a **controlled decel-to-stop-and-HOLD**, NOT a sustained
  reduced-speed crawl. The Governor permits a command in Degraded **only if
  all** of the following hold; otherwise it is denied and the actuator falls
  to the MRC controlled stop:

  - **(a) within the MRC kinematic envelope** — the MRC profile
    (`MRC_VELOCITY_CEILING_MPS` = 5.0 m/s speed cap, plus the MRC
    accel/brake/steering/lateral limits) acts as the **decel-trajectory
    bound** for a *still-moving, decelerating* command — it is the upper
    bound on a command that is already converging toward zero, NOT a crawl
    set-point the vehicle is driven up to;
  - **(b) non-increasing speed** — `|proposed| ≤ |current|`. Any speed
    increase is denied (`DenyCode::DegradedSpeedIncreaseDenied`);
  - **(c) no autonomous re-initiation of motion** — if the vehicle is
    stopped (`|current| ≤ STOP_EPSILON_MPS`, 0.05 m/s), any command above
    the stop floor is denied (`DenyCode::DegradedReinitiationDenied`); the
    vehicle HOLDS at zero. A direction reversal through a stop is likewise
    treated as re-initiation and denied.

  Enforced by `enforce_degraded_decel_to_stop` at every Degraded enforcement
  point (gateway `enforce_actuator_safety_envelope`, fabric
  `AssetGovernor::evaluate_command`, ros2-adapter `validate_trajectory_slow`,
  and parko-kirra `KirraGovernor::apply_mrc_profile` — the last also gates an
  *independent angular-velocity* channel for differential-drive platforms).
  The net effect: a vehicle that enters Degraded while moving bleeds speed to
  a standstill under the MRC bound and then holds; a stopped vehicle stays
  stopped. **The Governor never authors re-acceleration.**

Rationale — **Cruise, San Francisco, October 2023.** After an initial
  collision, a robotaxi executed an automated pullover **from a stop** and
  dragged a pedestrian who was pinned under the vehicle roughly 20 ft at
  ~3 m/s. A 3 m/s pullover-from-stop sits *below* a 5 m/s reduced-speed crawl
  ceiling — so the prior "Degraded = MRC crawl" behavior would have
  **permitted** exactly that maneuver. SS-002 is therefore defined as
  decel-to-stop-and-HOLD with no autonomous re-initiation: under a degraded
  safety posture the safe action is to stop and stay stopped, not to perform
  a discretionary low-speed maneuver. (A *legitimate* pullover requires full
  situational competence and belongs to SS-001 Nominal — by design it is out
  of scope for Degraded.)

"MRC" disambiguation: the **Degraded MRC** here is the decel-to-stop
  *envelope* (the bound on a converging command). It is distinct from the
  **LockedOut MRC fallback** (SS-003), which is the safe-stop *maneuver* the
  actuator performs when every command is denied. Both drive toward a
  standstill; Degraded additionally permits a cooperative decelerating
  command, LockedOut permits none.

Entry conditions (any of):
  - SG-003: Sensor telemetry timeout (`AV_TELEMETRY_TIMEOUT_MS` exceeded)
  - SG-005: RSS violation (gap < `longitudinal_safe_distance`)
  - Governor unreachable (timeout or network partition)
  - Node trust state `Untrusted` with non-critical dependency impact
  - `Degraded` posture propagated from dependency graph

Recovery (AUTOMATIC):
  `AV_RECOVERY_STREAK_THRESHOLD` (5) consecutive clean ticks within
  `AV_RECOVERY_WINDOW_MS` (10,000 ms) → transitions to SS-001 Nominal, and
  only then may motion be (re-)initiated. A single unhealthy report or gap in
  the window resets streak to 0. (SG-013) This is the key contrast with
  SS-003: Degraded recovery is automatic on return to Nominal, whereas
  LockedOut requires an explicit human reset.

Implements: ISO 26262 safe state for recoverable faults.

Issue-#49 closeout: #49 ("Degraded must converge to 0") is realized by this
  decel-to-stop-and-HOLD behavior — convergence to zero is the (b)+(c)
  invariant, and "hold at zero" is the no-re-initiation rule. See the
  loose-ends note in §7.

Safety goals covered: SG-003, SG-005, SG-013

---

### SS-003: Lockout / Hard Stop (PostureState::LockedOut)

Behavior:
  0.0 m/s hard stop. No commands forwarded to actuators under any
  circumstance — every command is denied and the actuator performs its
  **MRC fallback** safe-stop maneuver. Human intervention required to clear.
  `LockedOut` dominates all other posture states.

  "MRC" note: the LockedOut **MRC fallback** is the safe-stop *maneuver*
  executed when all commands are denied (e.g. `mrc_command` / MRC fallback
  profile → zero velocity, max-decel brake ramp). It is distinct from the
  SS-002 Degraded MRC *envelope*, which still admits a cooperative
  decelerating command. LockedOut admits none.

Entry conditions (any of):
  - DAG cycle detected in dependency graph
  - Multiple critical nodes `Untrusted` simultaneously (DAG propagation)
  - `MAX_DEPENDENCY_DEPTH` (10) exceeded in recursive DAG traversal
  - `GovernorComparator` divergence detected (CERT-006)
  - Leader `LockedOut` → followers `Degraded` within one fabric tick
    ≤ 500 ms; propagation recorded in fabric causal log (SG-007)

Recovery:
  Explicit human-initiated reset via `KIRRA_SUPERVISOR_RESET_KEY`
  endpoint. Automatic recovery from `LockedOut` is NOT permitted
  under any circumstances. Human must verify system state before
  issuing reset.

Liveness/observability exemption (path-class distinction):
  "LockedOut admits no commands" (and fail-closed posture gating generally)
  governs the **command** and **mutation** path classes — actuator/control
  routes and state-mutating routes. It does NOT govern read-only
  liveness/observability probes, which carry NO command, mutation, or actuator
  authority. The two statements describe **different path classes** and are not
  in tension.

  The following read-only paths are DELIBERATELY exempt from posture gating
  AND from the HA epoch fence, and remain reachable under `LockedOut` and on a
  self-demoted / fenced node — exactly the set enforced by `is_posture_exempt`
  (`src/gateway/policy_layer.rs`):

      "/health" | "/health/live" | "/ready" | "/metrics"

  Rationale — keeping liveness reachable under `LockedOut` is itself a safety
  property: an operator / orchestrator MUST be able to observe that a node is
  `LockedOut`. A node that goes dark is indistinguishable from a crash and can
  trigger incorrect failover (e.g. a standby promoting against a node that is
  in fact alive and correctly `LockedOut`).

  Source of truth + enforcing test: the authoritative allowlist is
  `is_posture_exempt` in `src/gateway/policy_layer.rs`; the real-router test
  `health_exempt_under_lockedout_on_real_router`
  (`src/bin/kirra_verifier_service.rs`, issue #72 / PR #226) proves `/health`
  stays HTTP 200 under `LockedOut` on the assembled production router. This doc
  list and `is_posture_exempt` MUST be kept in sync — a change to one requires
  updating the other.

Implements: ISO 26262 safe state for non-recoverable faults.

Safety goals covered: SG-007, SG-011 (partial)

---

### SS-004: Process Fail-Closed (`startup_sentinel` abort)

Behavior:
  Process does not start. TCP listener never binds. No commands
  accepted. System remains completely offline until invariant
  violation is corrected and process restarted.

Entry conditions (checked by `startup_sentinel` before bind):
  - `KIRRA_ADMIN_TOKEN` absent or empty (SG-008, SG-015)
  - `KIRRA_SUPERVISOR_RESET_KEY` absent, empty, or > 64 bytes
  - Watchdog thread fails to start
  - Posture engine fails to initialize
  - SQLite WAL mode fails to activate
  - DDS actuator topic configured with `TransientLocal` (SG-016)
  - Any startup invariant listed in CRITICAL SECURITY INVARIANTS

Recovery:
  Fix the invariant violation. Restart the process.
  `startup_sentinel` re-runs all checks on every startup.

Implements: ISO 26262 fail-safe startup behavior. Ensures the system
never enters a partially-initialized state that could accept commands.

Safety goals covered: SG-008, SG-015, SG-016

---

## 3. Fault to Safe State Mapping

| Fault Mode | Safety Goal | ASIL | Safe State | Recovery | Test Status |
|------------|------------|------|-----------|---------|-------------|
| Linear velocity command exceeds `max_speed_mps` of active kinematic contract | SG-001 | D | SS-001 (clamp via `validate_vehicle_command` Priority 2; command continues post-clamp) | Automatic — clamp applied per command, no state transition | ✓ `test_speed_above_ceiling_triggers_clamp_linear` |
| Vehicle command implies lateral acceleration above `max_lateral_accel_mps2` (bicycle model) | SG-002 | D | SS-001 (clamp steering via `validate_vehicle_command` Priority 6) | Automatic — steering clamped per command | ✓ `test_nominal_highway_speed_high_steering_clamps_steering` |
| AV sensor node silent ≥ `AV_TELEMETRY_TIMEOUT_MS` (2,000 ms) | SG-003 | D | SS-002 — node marked `Untrusted`, posture recalculated within `AV_WATCHDOG_SWEEP_MS` (100 ms) | SS-002 → SS-001 via SG-013 recovery hysteresis | PENDING — stub: `test_safety_goal_sg_003_sensor_timeout_fault_detection` |
| Non-finite (NaN / Inf) value in any f64 field of vehicle command | SG-004 | C | SS-001 (command rejected at Priority 0; arithmetic never executed; posture unaffected) | Automatic — single-command rejection, system continues | ✓ `test_inf_linear_velocity_is_denied` |
| Posture cache age ≥ `POSTURE_CACHE_TTL_MS` (5,000 ms) at command-evaluation time | SG-005 | D | SS-003 — `resolve_posture_with_reason` returns `LockedOut(PostureCacheStale)`; all commands fail-closed | Cache refresh by next successful recalculation cycle returns posture to SS-001 / SS-002 | ✓ `test_stale_cache_fails_closed_after_virtual_clock_advance` |
| `OperationalCommand::Unknown` received (unrecognized path + method) | SG-006 | D | SS-001 (single request denied unconditionally before posture eval; fleet posture unchanged) | Automatic — per-request denial, no state transition | PENDING — stub: `test_safety_goal_sg_006_unknown_command_denial` |
| Leader asset enters `LockedOut` in multi-asset fabric | SG-007 | D | SS-003 (leader) + SS-002 (all followers, within one fabric tick ≤ 500 ms); propagation logged in fabric causal log | Leader recovery via human reset → fabric tick restores followers to SS-001 | PENDING — stub: `test_safety_goal_sg_007_cross_asset_lockout_propagation` |
| `startup_sentinel` invariant failure at boot (token, watchdog, posture engine, WAL, DDS durability) | SG-008 | D | SS-004 — process aborts before TCP listener binds; no command surface exposed | Fix invariant violation; restart process; `startup_sentinel` re-runs | PENDING — stub: `test_safety_goal_sg_008_process_fail_closed_on_crash` |
| Primary heartbeat silent ≥ `PROMOTION_TIMEOUT_MS` (10,000 ms) in HA deployment | SG-009 | B | SS-001 maintained via standby promotion (`mode_active.compare_exchange`); enforcement coverage gap bounded by promotion timeout | Promoted standby begins heartbeat + posture recalculation immediately on promotion | PENDING — stub: `test_safety_goal_sg_009_ha_standby_promotion_within_timeout` |
| Audit chain entry `prev_hash` mismatch detected during verification | SG-010 | B | SS-001 maintained, integrity-failure flag raised for operator review; chain logged; service continues | Operator-driven forensic review; no automatic recovery of tampered entries | PENDING — stub: `test_safety_goal_sg_010_audit_chain_tamper_detection` |
| CANOpen NMT command with `data[0]` in `{0x02, 0x80, 0x81, 0x82}` | SG-011 | C | Posture engine triggered to recalculate; result places system in SS-001 / SS-002 / SS-003 based on resulting fleet posture | Per recovery rules of resulting safe state | ✓ `test_canopen_nmt_stop_triggers_posture_recalculation` (1 of 3 — partial coverage) |
| DNP3 message to `DNP3_BROADCAST_ADDRESS` received | SG-012 | B | SS-001 normally — audit chain entry written before control output; if audit write fails, control blocked (SS-001 with denied command) | Automatic — audit-before-action ordering enforced per request | PENDING — stub: `test_safety_goal_sg_012_dnp3_broadcast_mandatory_audit` |
| Recovery hysteresis evaluation for recently-faulted node | SG-013 | B | SS-002 → SS-001 transition only when 5 consecutive healthy reports arrive inside a 10 s window; otherwise streak resets | Per the streak / window rule itself; no other recovery path | PENDING — stub: `test_safety_goal_sg_013_recovery_hysteresis_streak_and_window` |
| `FederatedTrustReportV2` with `generation ≤ last_accepted_generation` from same peer, or replayed nonce | SG-014 | B | SS-001 maintained — report rejected; rejection logged to audit chain; posture unchanged | Automatic — replay rejection per request, no state transition | PENDING — stub: `test_safety_goal_sg_014_federation_report_replay_prevention` |
| `KIRRA_ADMIN_TOKEN` absent or empty at mutation route invocation | SG-015 | B | SS-004 if absent at startup (process never binds); HTTP 503 if absent at request time (SS-001 with denied request) | Provide token via environment; restart if previously missing at startup | PENDING — stub: `test_safety_goal_sg_015_admin_token_absent_fail_closed` |
| DDS actuator topic detected with `DurabilityPolicy::TransientLocal` at startup | SG-016 | C | SS-004 — `startup_sentinel` aborts; process never binds | Reconfigure topic to `DurabilityPolicy::Volatile`; restart | PENDING — stub: `test_safety_goal_sg_016_dds_actuator_volatile_durability` |

---

## 4. Safe State Transition Invariants

The following invariants are enforced in code and must never be
violated regardless of what upstream AI systems instruct:

1.  `LockedOut` can only be cleared by human reset — never automatic
2.  `Degraded` recovery requires N consecutive clean ticks — not immediate;
    recovery is AUTOMATIC on return to Nominal (contrast invariant 1)
3.  Governor unreachable → `Degraded` semantics (NOT `LockedOut`)
4.  RSS unsafe → `Degraded` semantics (NOT `LockedOut`)
4a. `Degraded` is decel-to-stop-and-HOLD: a command is admitted only if it is
    non-increasing in speed AND does not re-initiate motion from a stop
    (`enforce_degraded_decel_to_stop`); a stopped vehicle HOLDS, and the
    Governor never authors re-acceleration. Motion may be (re-)initiated only
    after recovery to SS-001 Nominal. (Issue #70 — Cruise 2023 pullover-drag)
5.  NaN / Inf model output → safe floor applied before governor runs
6.  DAG `LockedOut` propagates upward — never downgraded by RSS recovery
7.  `LockedOut` dominates `Degraded` — if both conditions present,
    `LockedOut` wins
8.  `startup_sentinel` abort → no commands accepted under any circumstance
9.  `OperationalCommand::Unknown` denied in ALL posture states (SG-006)
    — before posture check, before governor, unconditionally
10. DDS actuator topics must use `DurabilityPolicy::Volatile` only —
    `TransientLocal` triggers `startup_sentinel` abort (SG-016)
11. SQLite writes go to disk before memory — `persist_and_insert_node`
    calls `save_node` then `nodes.insert`, never reversed
12. `KIRRA_ADMIN_TOKEN` compared with `constant_time_compare` only —
    standard `==` forbidden on security-critical byte sequences
13. Liveness/observability probes are EXEMPT from posture gating and the HA
    epoch fence, and stay reachable under `LockedOut` and on a fenced /
    self-demoted node — `is_posture_exempt` allowlists exactly
    `/health`, `/health/live`, `/ready`, `/metrics`. This does NOT contradict
    invariants 7–9: "`LockedOut` admits no commands" governs the COMMAND and
    MUTATION path classes; read-only liveness probes carry no command or
    actuator authority and are a different path class. Keeping liveness
    observable under `LockedOut` is itself a safety property (a dark node is
    indistinguishable from a crash and can trigger incorrect failover). The
    doc list (SS-003) and `is_posture_exempt`
    (`src/gateway/policy_layer.rs`) must be kept in sync. (Issue #70)

---

## 5. Open Items (CERT-003 Gaps)

The following safety goals have documented stubs but no implemented
test coverage. Each is tracked in `tests/cert_003_rtm_gap_stubs.rs`.
This section shrinks as CERT-004 implements each test.

| Goal | ASIL | Property to verify | Stub function | Mapped safe state |
|---|---|---|---|---|
| SG-003 | D | Watchdog marks node `Untrusted` within `AV_TELEMETRY_TIMEOUT_MS + AV_WATCHDOG_SWEEP_MS` of last telemetry; `PostureRecalcTrigger` fired same cycle | `test_safety_goal_sg_003_sensor_timeout_fault_detection` | SS-002 |
| SG-006 | D | `should_route_command` denies `Unknown` unconditionally before posture eval in all three posture states | `test_safety_goal_sg_006_unknown_command_denial` | SS-001 (per-request denial) |
| SG-007 | D | Leader `LockedOut` → followers `Degraded` within one fabric tick ≤ 500 ms; event logged in fabric causal log | `test_safety_goal_sg_007_cross_asset_lockout_propagation` | SS-003 (leader) + SS-002 (followers) |
| SG-008 | D | `startup_sentinel` aborts before listener bind on any invariant failure (token, watchdog, posture engine, WAL, DDS durability) | `test_safety_goal_sg_008_process_fail_closed_on_crash` | SS-004 |
| SG-009 | B | Standby promotes (`mode_active.compare_exchange`) within `PROMOTION_TIMEOUT_MS` (10 s) of last primary heartbeat; promoted instance resumes heartbeat + recalculation | `test_safety_goal_sg_009_ha_standby_promotion_within_timeout` | SS-001 (maintained via standby) |
| SG-010 | B | `AuditChainLinker` detects any tampered entry via `prev_hash` mismatch; `/system/audit/verify` returns first bad index; verification runs at startup | `test_safety_goal_sg_010_audit_chain_tamper_detection` | SS-001 with integrity flag |
| SG-012 | B | DNP3 broadcast → audit chain entry written before control output; audit write failure on broadcast blocks control (fail-closed ordering) | `test_safety_goal_sg_012_dnp3_broadcast_mandatory_audit` | SS-001 (per-request) |
| SG-013 | B | `evaluate_recovery_report` requires exactly 5 healthy reports inside a 10 s window; gap or unhealthy report resets streak to 0 | `test_safety_goal_sg_013_recovery_hysteresis_streak_and_window` | SS-002 → SS-001 transition |
| SG-014 | B | `reconcile_reports` rejects replayed `FederatedTrustReportV2`; Ed25519 signature verified; nonces burned in `federation_report_nonces` | `test_safety_goal_sg_014_federation_report_replay_prevention` | SS-001 (per-request rejection) |
| SG-015 | B | `require_admin_token` returns HTTP 503 when `KIRRA_ADMIN_TOKEN` absent / empty; comparison uses `constant_time_compare`; reaches every mutation handler | `test_safety_goal_sg_015_admin_token_absent_fail_closed` | SS-004 at startup / SS-001 at request time |
| SG-016 | C | Every DDS actuator topic uses `DurabilityPolicy::Volatile`; `startup_sentinel` aborts on `TransientLocal` | `test_safety_goal_sg_016_dds_actuator_volatile_durability` | SS-004 |

---

## 6. Issue #49 Closeout and Loose Ends

Issue #49 (PARK-037) — "Integrate Parko + KirraGovernor with ROS2 cmd_vel
topics; Governor clamps observable on `filtered_cmd_vel`; closed-loop on
Hiwonder" — has a **safety-behavior** portion and a **wiring/hardware**
portion. This change (Issue #70) closes the former; the latter remains.

CLOSED by this change (the Degraded-converge-to-zero safety semantics):
  - The Degraded posture now provably converges the vehicle to a standstill
    and holds it there (the SS-002 (b)+(c) invariant), realized uniformly at
    all four enforcement points via `enforce_degraded_decel_to_stop`. The
    "Degraded → 0" intent #49 carried is now a tested, fail-closed behavior
    (decel-to-stop-and-HOLD, no autonomous re-initiation).

LOOSE ENDS — split to the follow-up issue #171; the topic-naming half is now
RESOLVED (below), the Hiwonder half remains open:
  1. **ROS2 topic naming / observability — RESOLVED (#171).** The phantom
     `/filtered_cmd_vel` from the original PARK-037 planning text was **never
     implemented** in either component; it existed only in planning docs. The
     canonical, implemented safe-output topics are:
       - **`/cmd_vel_safe`** — the robotics stack (`kirra_safety`
         `cmd_vel_interceptor` → enforced command; `posture_subscriber` zero-Twist
         on LockedOut; the C++ bridge `/kirra/cmd_vel_safe`; `kirra_params.yaml`);
       - **`~/output/cmd_vel`** — the AV adapter
         (`crates/kirra-ros2-adapter`, `parko/crates/parko-ros2/src/config.rs`),
         which has published on it all along.
     The dedicated-filtered-topic question is resolved as **no new topic**: the
     raw-vs-gated observability a `/filtered_cmd_vel` would have provided already
     exists via the **input/output split** — raw = the input topic (`/cmd_vel`),
     gated = the output topic (`/cmd_vel_safe` / `~/output/cmd_vel`) — and the
     governor's clamp is independently observable on the enforcement/action topic
     and the Ed25519 audit chain. A dedicated topic would be redundant. The stale
     `/filtered_cmd_vel` references in the planning docs are retired in favor of
     these real names; do not rename working code to match a stale spec.
     OPTIONAL consistency follow-up (flagged, NOT done here): unify the AV
     adapter's `~/output/cmd_vel` to `/cmd_vel_safe` for one cross-component name
     — an integration nicety, not a safety change.
  2. **Hiwonder closed-loop hardware verification — OPEN (#171).** #49's
     acceptance includes on-hardware closed-loop validation on the Hiwonder
     platform. This is verified in-process today (unit + the
     `governor_closes_loop_proof` axis); hardware bring-up is separate and
     remains blocked on the hardware.
  3. **#49 ↔ #92 scope overlap.** Triage (`docs/ISSUE_TRIAGE_2026-06-01.md`)
     flags overlap between #49 (ROS2 cmd_vel wiring) and #92 (Occy Governor
     trajectory check). The overlap is unaffected by — and orthogonal to —
     the Degraded behavior change here.

Status: the Degraded-converge-to-0 sub-goal of #49 closed against the Issue #70
change, and the remainder was re-homed to follow-up issue **#171**. Of that
remainder, the **topic-naming half is now resolved** (loose-end 1 above);
**only the Hiwonder closed-loop hardware validation remains open** under #171.

---

## 7. Implementation References

- `PostureState` enum: `src/posture_engine.rs` (or `src/verifier.rs`)
- Degraded decel-to-stop-and-hold gate (Issue #70):
  `enforce_degraded_decel_to_stop` + `STOP_EPSILON_MPS` in
  `src/gateway/kinematics_contract.rs`; wired at
  `src/gateway/policy_layer.rs`, `src/fabric/governor.rs`,
  `crates/kirra-ros2-adapter/src/validation.rs`, and
  `parko/crates/parko-kirra/src/lib.rs` (`apply_mrc_profile`,
  + `STOP_EPSILON_RAD_S` angular gate)
- `KirraGovernor` authority model: `parko/parko-kirra/src/lib.rs`
- `startup_sentinel`: `src/bin/kirra_verifier_service.rs`
- `should_route_command`: `src/posture_cache.rs`
- DDS bridge: `src/dds_bridge.rs`
- ADL-001 (governor authority model): `work/decisions.md`
- Safety goals: `docs/safety/SAFETY_GOALS.md`
- RTM gap report: `docs/safety/RTM_GAP_REPORT.md`
- Test stubs: `tests/cert_003_rtm_gap_stubs.rs`
