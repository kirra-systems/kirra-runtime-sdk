# RTM Gap Report

**Generated:** 2026-05-29
**Source RTM:** `docs/safety/REQUIREMENTS_TRACEABILITY.md` (AEGIS-RTM-001, v1.0.0)
**Source goals:** `docs/safety/SAFETY_GOALS.md` (AEGIS-SG-001, v1.0.0)
**Tracking:** CERT-003

---

## Summary

| Metric | Value |
|---|---|
| Total safety goals | 16 |
| Test functions named in RTM | 40 |
| Tests found in codebase | 5 |
| Tests missing from codebase | 35 |
| Goals with **any** test coverage | 5 |
| Goals with **zero** test coverage | 11 |
| Goal-level coverage | **31.25%** (5 / 16) |
| Test-level coverage | **12.5%** (5 / 40) |
| Code references to safety goal IDs | 0 |

The RTM's own self-reported coverage (`All 16 safety goals are covered`, `Total Test Suite Size: 306 passing`) was aspirational — it names the tests that *should* exist per technical requirement but was never reconciled against actual test functions in the source tree. **No ASIL-relevant test exists for 11 of 16 safety goals.**

---

## Gaps — goals without any test coverage

These 11 safety goals have **zero** corresponding test in the codebase. Stubs added in `tests/cert_003_rtm_gap_stubs.rs`.

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
