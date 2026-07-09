# Deterministic Replay — Incident Reconstruction (EP-19)

**Doc ID:** KIRRA-REPLAY-001.
**Tool:** `kirra-replay` (`crates/kirra-replay`).
**Claim:** a captured session fed back through the REAL gateway checker
reproduces BIT-IDENTICAL verdicts — so an incident's recorded decisions can be
independently re-derived from their recorded inputs, and any mismatch is a
loud, per-record alarm rather than a shrug.

---

## 1. What replay is (and is not)

`kirra-replay` re-runs each captured `CommandGateway` record through the SAME
functions the deployed gateway ran — `validate_vehicle_command` at Nominal,
`enforce_degraded_decel_to_stop` at Degraded, over the SAME per-class contract
profiles — and maps the recomputed verdict back through the SAME
`record_from_verdict` emit mapping. Nothing is reimplemented; the comparison
cannot drift from the deployment.

Equality is **bitwise**: `f64::to_bits` on the substituted safe value, exact
equality on outcome / deny-code / MRC flag. There is no epsilon.

Replay is NOT a simulator: it re-derives per-record verdicts from per-record
inputs. Time never enters (the planning `dt` rides inside each command), so no
clock — virtual or otherwise — is involved in the fast-loop verdict itself.
For time-DEPENDENT flows (posture transitions, watchdog timeouts, recovery
hysteresis) use the existing deterministic temporal harness
(`VirtualClock` + `ScenarioRunner`, `src/scenario_runner.rs`); replay and the
scenario harness compose — replay pins the per-command verdicts, the harness
pins the posture timeline that selected each verdict's arm.

## 2. Operator workflow

```text
1. Pull the session capture (the KIRRA_CAPTURE_PATH JSONL the capture writer
   appended; one CaptureRecord per line).
2. Identify the deployment's vehicle class (KIRRA_VEHICLE_CLASS of the boxed
   build — it is in the boot EffectiveConfig digest audit event).
3. Run:    kirra-replay --class robotaxi session.jsonl
4. Read the classification:
     identical        — the record re-derives bit-for-bit. The verdict the
                        fleet acted on is exactly what this build's checker
                        produces for those inputs.
     DIVERGENT        — THE ALARM. Same inputs, same checker, different
                        verdict. Either the capture was tampered with, the
                        replaying build differs from the deployed one, or the
                        class is wrong. Exit code 1.
     not-replayable   — the record does not carry its complete checker inputs;
                        the reason names exactly what is missing (see §3).
5. Cross-check any record of interest against its tamper-evident anchors:
     - the EP-17 verdict artifact (GET /verdicts/{id}) for a denial — the
       chained, signed record of the same decision;
     - the audit chain (GET /system/audit/verify) for overall ledger
       integrity.
```

## 3. Determinism guarantees and their honest boundaries

| Guarantee | Boundary |
|---|---|
| Bit-identical re-derivation of `CommandGateway` records | On the SAME build (same binary/toolchain/libm). Clamp values that traverse the P6 bicycle-model path (`tan`/`atan`) are transcendental-library results: replaying on a DIFFERENT libm/platform may differ in the last ulp on exactly those records. Same-build replay — the incident-reconstruction case — is exact. |
| Exact JSON round-trip of every f64 | Requires serde_json's `float_roundtrip` feature (kirra-replay enables it). The DEFAULT serde_json parse is approximate and was measured one ulp off on real clamp values — any OTHER consumer parsing capture JSONL for exact comparison must enable the same feature. |
| Slow-loop trajectory records | NOT replayable by design: the capture carries a bounded O(1) summary (endpoint poses + counts), never the full trajectory/objects (a WCET constraint on the emit site). Classified `not-replayable`, never guessed. |
| Perception-derate records (`derate_enabled = true`) | NOT replayable: the composed cap value is not in the schema, so the Nominal contract cannot be reconstructed. Classified, never guessed. |
| NaN/Inf-input denials | The denial is captured, but JSON cannot carry the non-finite input (`serde_json` writes `null`) — the record surfaces as a loud parse error on replay. The NaN fail-closed guarantee itself is machine-checked for EVERY f64 bit pattern by the EP-15 Kani proof K1, which is stronger than replay could ever be. |
| `LOCKED_OUT` records | Cannot originate from the gateway emit site (the posture gate short-circuits first); such a record is foreign/corrupt — classified. |

## 4. CI

The EP-19 DoD test (`crates/kirra-replay/src/lib.rs::tests`) runs in the
normal workspace test lane on every PR: a synthetic-but-faithful captured
session (REAL checker, REAL emit mapping, REAL JSONL) replays bit-identically
across all three vehicle classes, a tampered record and a one-ulp-mutated
clamp value DIVERGE (non-vacuity), incomplete-context records classify, and
the class parse is fail-closed.
