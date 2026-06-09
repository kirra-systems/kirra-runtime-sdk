# RTM Gap Report

**Generated:** 2026-05-29
**Source RTM:** `docs/safety/REQUIREMENTS_TRACEABILITY.md` (AEGIS-RTM-001, v1.0.0)
**Source goals:** `docs/safety/SAFETY_GOALS.md` (AEGIS-SG-001, v1.0.0)
**Tracking:** CERT-003

---

## Summary

| Metric | Initial baseline (2026-05-29) | After CERT-004 (2026-05-29) |
|---|---|---|
| Total safety goals | 16 | 16 |
| Test functions named in RTM | 40 | 40 |
| Tests found in codebase | 5 | 8 (+3 from `tests/fault_injection.rs`) |
| Tests missing from codebase | 35 | 32 |
| Goals with **any** test coverage | 5 | **8** |
| Goals with **zero** test coverage | 11 | **8** |
| Goal-level coverage | 31.25% (5 / 16) | **50.00%** (8 / 16) |
| Test-level coverage | 12.5% (5 / 40) | **20.0%** (8 / 40) |
| Code references to safety goal IDs | 0 | 0 (deferred — recommendation pending) |

Newly covered by CERT-004 (`tests/fault_injection.rs`):
- SG-006 ✓ `test_safety_goal_sg_006_unknown_command_denial`
- SG-014 ✓ `test_safety_goal_sg_014_federation_report_replay_prevention`
- SG-016 ✓ `test_safety_goal_sg_016_dds_actuator_volatile_durability`

The RTM's own self-reported coverage (`All 16 safety goals are covered`, `Total Test Suite Size: 306 passing`) was aspirational — it names the tests that *should* exist per technical requirement but was never reconciled against actual test functions in the source tree. **No ASIL-relevant test exists for 11 of 16 safety goals.**

---

## Update — CERT-003 ASIL-D increment (2026-06-07)

The three remaining **ASIL-D** zero-coverage goals now have real, deterministic
test evidence:

| Goal | ASIL | Status after this increment | Tests (actual fn names / locations) |
|---|---|---|---|
| SG-003 | D | **CLOSED** | `src/telemetry_watchdog.rs` mod `sg_003_cert_tests`: `test_watchdog_marks_node_untrusted_after_timeout`, `test_watchdog_detection_latency_within_bound`, `test_watchdog_triggers_posture_recalculation` (RTM-named; in-crate because `watchdog_sweep_once` is `pub(crate)`) |
| SG-007 | D | **CLOSED** (propagation + causal-log) | `tests/fault_injection.rs::test_safety_goal_sg_007_cross_asset_lockout_propagation` (leader LockedOut → followers Degraded in one synchronous fabric pass, + precondition cross-check) AND `tests/cert_003_rtm_gap_stubs.rs::test_safety_goal_sg_007_causal_log_records_propagation_event` (the causal-log sub-gap, now closed): `FabricRouter::propagate_and_record` records a `cross_asset_trust_degrade` event per rule-firing to the `FabricCausalLog` while the propagation decisions stay byte-identical to `propagate_cross_asset_trust`. **Conscious deferral:** fan-in rules (an "any LockedOut source of type X degrades dependents" rule — drone↔ground-station, infrastructure) record a single *deterministic representative* trigger (the lexicographically-smallest LockedOut source); the degrade *decision* is unchanged. Multi-trigger fan-out (one event per trigger→follower, or all sources in `caused_by`) is the eventual refinement for forensic completeness on multi-source lockouts — not yet required. The convoy and warehouse rules already attribute the exact trigger. |
| SG-008 | D | **CLOSED** | `src/bin/kirra_verifier_service.rs`: pure `check_startup_invariants` predicate + mod `sg_008_cert_tests` (admin-token / WAL / watchdog / posture-engine violations, all-present Ok, Active-vs-PassiveStandby distinction, check-order stability). `main` evaluates it immediately before `TcpListener::bind` and aborts on `Err` (fail-closed; bind never reached on violation). |

Resulting goal-level coverage: **11 / 16 (68.75%)**. Every **ASIL-D** goal
(SG-001, SG-002, SG-003, SG-005, SG-006, SG-007, SG-008) now has at least one
real test. The remaining zero-coverage goals are all ASIL-B/C — **SG-009,
SG-010, SG-012, SG-013, SG-015**. (The SG-007 causal-log sub-gap noted above is
now CLOSED — see the SG-007 row.)

Code-side traceability: `grep -rn "SG-0" src/` now returns **68** hits
(mechanisms carry `// Verifies: SG-NNN` tags), up from ~0 in the canonical form.

**Test-count reconciliation:** the long-stale "Total Test Suite Size: 306
passing" figure is superseded — the core crate (`src/` + `tests/`) currently
defines **582** `#[test]`/`#[tokio::test]` functions. (The 306 figure predates
substantial test growth and should be treated as historical; downstream docs
still quoting it — REQUIREMENTS_TRACEABILITY.md, SAFETY_CASE_INDEX.md,
ROADMAP_TO_ASIL_D.md, IEC_61508_MAPPING.md, ASTM_F3269_MAPPING.md — are a
separate reconciliation pass.)

## Update — CERT-003 ASIL-B increment (2026-06-08)

The ASIL-B zero-coverage goals were worked next. Real, deterministic test
evidence was added where a real mechanism exists; two are reported as honest
mechanism gaps rather than faked.

| Goal | ASIL | Status after this increment | Tests / finding |
|---|---|---|---|
| SG-009 | B | **CLOSED** | `src/standby_monitor.rs` mod `sg_009_promotion_act_tests`: the per-poll gate is extracted into `pub(crate) promotion_decision` (the real decision the `spawn_promotion_monitor` loop calls; runtime behavior unchanged) — stale→promote / fresh→hold / inclusive boundary / clock-skew-safe; and the ACT, `perform_promotion`, is driven directly — `mode_active` false→true, durable promotion record + audit event, epoch claim, posture recalc populates the cache. In-crate because `perform_promotion` is async + module-private. |
| SG-010 | B | **tamper-detection CLOSED**; startup-verify sub-gap OPEN | `src/verifier_store.rs` mod `sg_010_audit_tamper_tests` (file-backed tempfile DB + `#[cfg(test)] pub(crate) raw_conn` seam): a second connection back-dates a written row and `verify_audit_chain_full` reports `chain_intact == false` with `first_invalid_signature_index == Some(<first tampered index>)`; hash linkage catches tampering even unsigned. **OPEN sub-gap:** SG-010 also requires audit verification to run automatically at startup before the listener binds — that mechanism does not exist (the bin runs only `check_startup_invariants` before bind; the chain is verified on demand via `/system/audit/verify` + a shutdown checkpoint). Wiring verify-and-abort into startup is a behavior change, out of scope for a test-only increment. |
| SG-012 | B | **MECHANISM GAP (OPEN)** | Investigated: `src/adapters/dnp3.rs::Dnp3Adapter::evaluate` is a pure classifier — no audit-chain write, no control-output application, hence no audit-before-control ordering and no fail-closed "block control on audit-write failure" path to assert. Closing SG-012 requires ADDING the mandatory-audit-before-control mechanism (a behavior change), not a test. Reported, not faked. |
| SG-013 | B | **CLOSED** | `tests/cert_003_rtm_gap_stubs.rs::test_safety_goal_sg_013_recovery_hysteresis_streak_and_window` (external — the whole closure is public): 4 healthy reports → `StreakBuilding{4}`; 5th in-window → `RecoveryConfirmed{5}`; >10s gap → `WindowExpired` then rebuild-to-4 (no confirm); injected unhealthy report (the production `reset_recovery_streak` path) → reset, rebuild-to-4 (no confirm). Time injected via explicit `now_ms`. |
| SG-015 | B | **CLOSED** | `src/security.rs` mod `sg_015_admin_token_tests`: the env-check is factored into pure `admin_token_ok(provided, configured)` (uses `constant_time_compare`, never `==`); `require_admin_token` calls it while preserving the 503 (configured absent/empty) vs 401 (provided absent/mismatch) mapping. Truth table tested in-crate without env-var mutation (INVARIANT #13). |

Net: SG-009, SG-013, SG-015 fully CLOSED; SG-010 tamper-detection CLOSED with an
explicit startup-verification sub-gap; SG-012 is an open mechanism gap. Each
non-pointer mechanism carries a `// Verifies: SG-NNN` tag. The two open items
are mechanism (behavior) changes, deliberately excluded from this test-only,
no-behavior-change increment.

## Gaps — goals without any test coverage

After CERT-004 these **8** safety goals (down from 11) still have no real test in the codebase. SG-006, SG-014, and SG-016 were closed and now live in `tests/fault_injection.rs`. The remaining 8 stubs are in `tests/cert_003_rtm_gap_stubs.rs`, with infrastructure-required notes added for SG-010, SG-013, and SG-015.

| Goal | ASIL | Description (one line) | Missing RTM-named tests |
|---|---|---|---|
| SG-003 | D | Sensor timeout fault detection within `AV_TELEMETRY_TIMEOUT_MS` (2 s) + sweep (100 ms) | `test_watchdog_marks_node_untrusted_after_timeout`, `test_watchdog_detection_latency_within_bound`, `test_watchdog_triggers_posture_recalculation` |
| SG-006 | D | `OperationalCommand::Unknown` denied in **all** posture states before posture eval | `test_unknown_command_denied_in_all_posture_states`, `test_unrecognized_path_classified_as_unknown`, `test_action_filter_denies_unknown_action_type` |
| SG-007 | D | Leader `LockedOut` → all followers `Degraded` within one fabric tick (≤ 500 ms) | `test_convoy_leader_lockout_degrades_followers`, `test_convoy_leader_degraded_degrades_nominal_followers`, `test_causal_log_records_propagation_event` |
| SG-008 | D | `startup_sentinel` aborts before listener bind on any invariant failure | `test_startup_aborts_without_admin_token` (plus integration tests for bind-ordering / middleware coverage) |
| SG-009 | B | HA standby promotes within `PROMOTION_TIMEOUT_MS` (10 s); promoted instance resumes heartbeat | `test_standby_promotes_after_primary_timeout`, `test_heartbeat_written_within_interval`, `test_promoted_instance_begins_heartbeat_and_recalculation` |
| SG-010 | B | `AuditChainLinker` detects any tampered entry via `prev_hash` mismatch; `/system/audit/verify` returns first bad index | `test_audit_chain_tamper_detection`, `test_audit_verify_endpoint_detects_corruption` |
| SG-012 | B | DNP3 broadcast → audit chain entry written **before** control output (fail-closed ordering) | `test_dnp3_broadcast_always_audited`, `test_dnp3_broadcast_blocked_on_audit_write_failure`, `test_dnp3_unicast_audit_failure_non_fatal` |
| SG-013 | B | Recovery requires exactly 5 healthy reports inside a single 10 s window; gap or unhealthy report resets streak to 0 | `test_recovery_requires_full_streak`, `test_streak_resets_on_gap`, `test_unhealthy_report_resets_streak` |
| SG-014 | B | `reconcile_reports` rejects replayed `FederatedTrustReportV2`; Ed25519 signatures verified; nonces burned | `test_federation_replay_rejected`, `test_federation_nonce_burn_prevents_replay`, `test_federation_invalid_signature_rejected` |
| SG-015 | B | `require_admin_token` returns HTTP 503 when `KIRRA_ADMIN_TOKEN` absent/empty; comparison uses `constant_time_compare` | `test_admin_token_absent_returns_503` (plus inspection of all mutation handlers and the token comparison call site) |
| SG-016 | C | DDS actuator topics use `DurabilityPolicy::Volatile`; `startup_sentinel` aborts on `TransientLocal` | `test_startup_aborts_on_transient_local_durability` (plus code inspection for any `TransientLocal` usage) |

---

## Single-coverage goals (at risk — only 1 of 3 RTM-named tests exist)

These 5 goals have one named test in the codebase but the other two RTM-required tests are missing. Each represents a single point of failure — if the one existing test were ever deleted or made `#[ignore]`'d, coverage drops to zero.

| Goal | ASIL | Test that exists | Tests still missing |
|---|---|---|---|
| SG-001 | D | `test_speed_above_ceiling_triggers_clamp_linear` | `test_zero_velocity_fence_enforced`; plus the proptest priority-ordering check |
| SG-002 | D | `test_nominal_highway_speed_high_steering_clamps_steering` | `test_yaw_rate_above_ceiling_triggers_clamp_angular`, `test_forward_sim_confirms_lateral_accel_constraint` |
| SG-004 | C | `test_inf_linear_velocity_is_denied` | `test_nan_linear_velocity_is_denied`, `test_ros2_adapter_rejects_nan_command`, `proptest_nan_inf_always_denied` |
| SG-005 | D | `test_stale_cache_fails_closed_after_virtual_clock_advance` | `test_stale_cache_blocks_all_commands` |
| SG-011 | C | `test_canopen_nmt_stop_triggers_posture_recalculation` | `test_canopen_nmt_start_does_not_trigger_recalculation`, `test_canopen_recalc_trigger_processed_within_cycle` |

---

## Recommendation

The RTM in its current state is **not defensible for an ASIL-D certification assessment.** Two distinct deficiencies that must be closed in this order:

1. **Implement the 11 missing zero-coverage tests first** (highest priority — most are ASIL B/C/D safety properties with no automated check today). Test stubs are landed in `tests/cert_003_rtm_gap_stubs.rs`; replace each `todo!()` body with the verification logic per the RTM's TR-N description for that goal. ASIL-D goals (SG-003, 006, 007, 008) should be implemented before ASIL B/C ones.
2. **Implement the 2nd and 3rd test for each single-coverage goal** (SG-001, 002, 004, 005, 011). The existing one-test-per-goal pattern provides no defense against test mutation or deletion.

Two structural changes that should accompany the test work:

- **Add code-side traceability** — every safety-critical function should carry a doc comment `// Verifies: SG-NNN` so the codebase grep (`grep -rn "SG-0"`) returns useful results. Today it returns zero.
- **Reconcile RTM "Test Suite Size: 306 passing" claim** — the actual test inventory should be re-counted and the RTM updated to reflect what's truly there, not what was originally planned.

Additional follow-up work outside CERT-003 scope:

- The safety docs themselves are still branded `AEGIS-*` (per CLAUDE.md naming rules, all new safety case material should be `KIRRA-*`). This is a separate rename pass.
- Untraced items in RTM §4 (ROS2 interlock node, EtherNet/IP adapter, Modbus / OPC-UA adapters, CARLA integration) need HARA-side assessment before traceability lines can be drawn.

---

## How this report was produced

```bash
# Test functions named in the RTM
grep -oE "test_[a-z_0-9]+" docs/safety/REQUIREMENTS_TRACEABILITY.md | sort -u

# For each, check existence as `fn <name>` in the source tree
grep -rln "fn <name>\b" src/ tests/ parko/ --include="*.rs"

# Cross-map: from each missing test name, walk back to which SG it covers
awk '/^### SG-/ { current=$2; next } $0 ~ name { print current; exit }' \
  docs/safety/REQUIREMENTS_TRACEABILITY.md
```

Re-running the same procedure after closing gaps will show the coverage delta.
