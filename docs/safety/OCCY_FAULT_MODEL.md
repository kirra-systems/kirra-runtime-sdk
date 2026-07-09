# Occy / KIRRA — Governor Fault Model + Degraded-Mode Availability (S7)

**Issue:** S7 (#119).
**Doc ID (proposed):** KIRRA-OCCY-FAULT-001.
**Status:** Working fault model for review. Qualitative (FMEA-style) fault →
reaction analysis + the sensor-availability degraded-mode design. The
*quantitative* hardware metrics (SPFM/LFM/PMHF for ASIL-D) are documented in
**KIRRA-OCCY-QUANT-001** (`docs/safety/OCCY_QUANTITATIVE_METRICS.md`) — S8 Item
D / #120. Surfaces one load-bearing implicit assumption (actuator safe-stop).

---

## 1. Posture

KIRRA is **fail-safe**: any fault it cannot mask resolves to fail-closed → MRC.
HA standby promotion provides *partial fail-operational recovery* (restores
normal operation after a fault), but the **safe state is reached immediately**
on a fault, not after recovery. The design prioritizes safety over availability,
and this document characterizes the availability cost so it is understood rather
than surprising.

**HA durability boundary (#74).** The HA **epoch fence** — the mechanism behind
"at most one effective writer across partition / skew / restart" — is now backed
against hard power-loss: the epoch claim (`try_claim_epoch`) commits on a
`synchronous=FULL` connection (fsync per commit), so a claimed epoch cannot
regress on recovery and the split-brain window is closed even across an OS crash.
(Before #74 the claim was persisted but only `synchronous=NORMAL`-durable, so a
power-loss at commit time could drop it — the "across restart" property had a
power-loss hole, now closed.) The federation **nonce burn** (anti-replay) is
likewise `synchronous=FULL`. The **audit ledger** tail is durable to the last
checkpoint (fsync'd on graceful safe-stop/shutdown + SQLite auto-checkpoint); the
final rows before an *ungraceful* power cut may be lost — a forensic gap, not a
safety-state gap (the verdict path is store-free). See
`docs/safety/CODING_GUIDELINES.md` INV-12 for the precise statement.

---

## 2. Governor fault model

| Fault | Detection | Response | Resulting state | Availability impact |
|---|---|---|---|---|
| Process death (panic=abort, crash) | no valid verdict at actuator; HA heartbeat loss | fail-closed: no Accept emitted → actuator safe-stops *(assumption of use, §6)*; HA promotes standby | **MRC immediately**; normal ops resume on promotion | up to ~10 s **in MRC** before normal-ops recovery (PROMOTION_TIMEOUT_MS) — not 10 s uncontrolled |
| Verdict WCET / cycle timeout | SG9 in-process timeout (bound = host-indicative p99.9, S3 — not certified WCET; re-measured on target under S8) | fail-closed: Reject → MRC for that cycle | MRC | per-cycle, transient |
| Compute / SoC fault (D3 element) | HA heartbeat loss / health monitor | fail-closed + HA failover | MRC then recovery | ~10 s as above |
| Mutex poisoned | detected at lock acquisition | fail-closed, logged *distinctly* from writer-gone | MRC / degraded | operator-visible |
| Audit write-queue full | try_send → Full | drop deny-audit record + loud log; verdict still returns | **no safety impact** (verdict is already Deny); forensic gap, sequence-detectable | none (forensic only) |
| Stale / missing world-model input | freshness/staleness traps (is_stale, posture staleness) | fail-closed: deny / derate | MRC / derate | depends on staleness duration |
| Posture Unknown | posture-cache Unknown (SG-006/SG9) | fail-closed: no-route | MRC | transient |
| Safety-critical sensor degraded/lost | independent health assessment (§3) | envelope contraction or MRC (§3) | reduced envelope or MRC | derate or MRC by sensor |

Key clarification: **process death does not mean ~10 s of no control.** The
actuator fail-closes to MRC immediately; the ~10 s is the *recovery-to-normal*
latency while the standby promotes. The vehicle is in a safe state throughout.

---

## 3. Input/sensor fault model — sensor-availability → permitted envelope

The degraded-mode design (the sensor-config-agnostic behavior):

- **Health = present + fresh + plausible**, failing toward *unhealthy*. The
  Governor assesses health **independently** — from the #126 Perception Input
  Contract health/freshness signals plus its own plausibility / physical-bounds /
  staleness checks — **not** from the planner's assertion.
- **Required-sensor-set per envelope.** Each (sub-ODD, speed band, condition) has
  a required set of healthy coverage. The full envelope needs the full set;
  missing a safety-critical source → that envelope is not permitted → contract to
  the largest envelope the *healthy* set supports. Formally:
  `cap = min(sub-ODD nominal, weather derate, coverage_supported(healthy set))`.
- **Two-tier fallback.** Lose the D1 add-on → fall back to the base-tier envelope
  (bounded by integrator coverage). Lose enough base coverage → MRC.
- **Split of duties.** The planner adapts its *style* to available perception;
  the Governor enforces the *envelope* by independently-assessed health.
  **Default-deny** — the bigger envelope is earned by confirmed health, never
  assumed.
- **Asymmetric.** Contract instantly on health loss; restore only on *sustained
  confirmed* recovery.
- **Phasing (day-only-without-thermal).** If thermal is absent/unhealthy, the
  night-VRU coverage requirement is unmet → restrict to a day / lower-risk
  envelope. This is the D1 add-on phasing in fault-model form.
- **MRC at the floor.** Health below the minimum for *any* safe envelope → MRC
  (standing-MRC controlled stop).

---

## 4. Availability characterization

Fail-safe with HA partial recovery. The availability events, none of which are
unsafe:
- **Failover dwell** — up to ~10 s in MRC after a Governor/compute death before
  normal ops resume.
- **Per-cycle SG9 rejects** — transient single-cycle MRC on a timeout.
- **Sensor-degradation derates** — reduced envelope while a source is unhealthy.
- **MRC floor** — controlled stop when no safe envelope is supported.

---

## 5. FTTI mapping

Distinguish **time-to-safe-state** (must be ≤ FTTI) from **time-to-normal-
recovery** (an availability latency, not an FTTI):
- Per-cycle faults (SG9, posture Unknown, staleness) reach the safe state within
  the per-cycle FTTI — bounded by the measured verdict WCET (S3).
- Process/compute death reaches the safe state within the **actuator safe-stop
  time** (assumption of use, §6), *not* the ~10 s HA window — the ~10 s is
  recovery-to-normal, during which the vehicle is already safe.

---

## 6. Decisions / residuals surfaced

1. **Actuator safe-stop is a load-bearing ASSUMPTION OF USE** — the entire
   fail-closed story depends on the integrator's actuation safe-stopping within a
   bounded T_safe-stop on loss of a valid verdict. This is the *output*
   counterpart to the #126 perception input contract and is currently implicit.
   **Recommend documenting it as an explicit assumption of use (new issue / part
   of the SEooC contract).**
2. **HA failover latency** — ~10 s recovery (in MRC). Acceptable given the vehicle
   is safe throughout; faster failover (hot standby / lower timeout) is an
   availability optimization for later, with a false-failover-risk tradeoff.
   *Recommend: accept now, revisit if MRC-dwell matters operationally.*
3. **Sensor-health plausibility depth** — trust the contract's health flags vs.
   add cross-source plausibility + physical-bounds checks. *Recommend at least
   freshness + physical-bounds + the existing staleness traps; cross-source where
   multiple sources exist.*

Cross-refs: OCCY_SAFETY_GOALS.md (SG8/SG9), OCCY_DFA.md, OCCY_ARCHITECTURE_TIERS.md
(two-tier / D1), ADR-0002 (condition-dependent cap), GOVERNOR_INTEGRITY_EVIDENCE.md
(WCET/SG9), #120 (S8 quantitative metrics), #126 (input contract). Register as
KIRRA-OCCY-FAULT-001.
