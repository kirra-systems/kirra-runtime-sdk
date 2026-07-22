# Occy / KIRRA — ASIL Decomposition + Dependent Failure Analysis

**Issue:** S2 (#114) — ASIL decomposition design + DFA (independence).
**Doc ID (proposed):** KIRRA-OCCY-DFA-001.
**Status:** Working analysis for review. The decomposition validity and the
independence claims are safety-assessor judgments; this is a methodologically
sound draft to be confirmed. It establishes *what must be true* for the ASIL-D
claim to hold — and surfaces one finding that changes the build plan.

---

## 1. The decomposition

The architecture is a **safety-monitor (simplex) pattern**, expressed as an
ISO 26262-9 Cl.5 decomposition of each ASIL-D safety goal:

    ASIL D  =  ASIL D(D) [KirraGovernor]  +  QM(D) [Occy planner]

The Governor carries the full ASIL-D integrity; the ML planner is QM with
respect to the safety goals, because any hazardous trajectory it emits is
detected and mitigated by the Governor. This is the only tractable path —
developing an ML planner to ASIL D (or even B) is infeasible, so all safety
integrity is concentrated in the simpler, deterministic, verifiable Governor.

**This is valid only if BOTH proof obligations hold:**

- **PO-1 — Diagnostic coverage (§2):** the Governor detects *every* hazardous
  trajectory class. Any class it cannot catch is uncovered — the planner's QM
  faults in that class are NOT mitigated, and the decomposition fails there.
- **PO-2 — Independence (§3, the DFA):** the QM planner cannot defeat or corrupt
  the ASIL-D Governor, and no common cause disables both. Without independence,
  ISO 26262-9 voids the decomposition and *each* element would have to meet the
  full ASIL D.

  > **Scope note (updated 2026-07-22; original 2026-07-09 recorded the gap,
  > #887).** PO-2's "cannot defeat" is enforced on BOTH deployment topologies:
  > - **SHM/inline path** — EP-01 `ActuatorStation` verify-before-release;
  >   FDIT matrix + WP-21b carrier evidence (unchanged).
  > - **`ros2_ws`/R2 topology** — the ADR-0033 motor-boundary chokepoint: the
  >   verifying consumer is the sole releaser (Ed25519 verify-before-release
  >   at the last hop, so a rogue bus publisher's bytes carry no valid token
  >   and are refused regardless of topic remaps), with **Tier-1** CI-blocking
  >   cross-process evidence (`crates/kirra-actuation-consumer/tests/
  >   tier1_chokepoint.rs`: exactly one serial write, and it is the signed
  >   enforced bytes — unsigned / replay / token-over-different-bytes /
  >   deny-never-mints all refused) and **Tier-3** serial authority BELOW the
  >   bus (`robot/serial_exclusivity.py`: the consumer refuses to start unless
  >   it owns the port at mode 0600 with no other holder; TIOCEXCL for the
  >   session; the tightened `99-kirra-serial-exclusivity.rules` replaces the
  >   vendor's world-writable 0777; `AOU-ACTUATION-SERIAL-001` discharge).
  >
  > **Residuals (tracked, not silent):** the Tier-2 per-release rogue-flood
  > launch drill and the sros2 transport perimeter (defense-in-depth — the bus
  > itself remains open; harmless to actuation given the chokepoint, but a
  > rogue can still flood/delay, converted to a safe stop by the liveness
  > window per ADR-0033 "Does not enforce"). Consumer-process compromise and
  > DoS remain owned by ADR-0013 (hardwired E-stop) / ADR-0032 (QNX
  > partition). A bring-up run acknowledged via `KIRRA_ALLOW_SHARED_SERIAL=1`
  > carries NO serial-authority claim for that run (loudly self-labeled).

**Decision — planner rigor. SETTLED: DISCIPLINED-QM.**

The planner is QM (no ASIL claim), but developed with **elevated process
discipline as defense-in-depth**.

*Rationale.* PO-1 coverage is never perfect (G1 / occlusion is a known gap), so
a less fault-prone planner reduces residual risk in imperfectly-covered areas;
it makes the D(D) + QM(D) decomposition far easier to defend to an assessor; it
improves availability (fewer Governor vetoes / MRCs); and it is low-cost,
riding on the Rust discipline + scenario testing already planned.

*Disciplined-QM IS:*

- Documented requirements for intended behavior, traceable to tests.
- Coding standards + static analysis.
- Systematic regression + scenario testing.
- Controlled change management.
- Deterministic / replayable behavior.

*Disciplined-QM IS NOT:*

- An ASIL claim on the planner.
- MC/DC coverage on the planner.
- A qualified toolchain for the planner.

That integrity-grade rigor stays on the Governor (ASIL-D element).

**All three #114 decisions are now resolved:**
1. **Compute separation** → D3 / ADR-0003 (Governor + D1 on independent compute; separate SoC preferred).
2. **(ii) detection channel scope** → #124 / ARCH-001 (D1 add-on, Tier-2).
3. **Planner rigor** → disciplined-QM (this section).

---

## 2. PO-1 — Diagnostic coverage

The Governor must catch every hazardous-trajectory class implied by SG1–SG9:

| Hazard class | Governor check | Covered? |
|---|---|---|
| Longitudinal collision (SG1) | RSS over horizon | yes (lat. pending Ph3) |
| Road/lane departure (SG2) | per-step kinematics + drivable space | yes |
| Dynamic envelope (SG3) | per-step kinematics contract | yes |
| Untraversable water (SG4) | WATER_UNTRAVERSABLE | yes |
| Commit zone (SG5) | map-anchored block | yes (depends on localization — see C6) |
| Post-collision motion (SG6) | impact latch + veto | yes |
| Teleop unsafe command (SG7) | doer-agnostic check | yes |
| MRC reachability (SG8) | standing-MRC + commit-on-fail | yes |
| Fail-closed (SG9) | WCET / NaN / timeout | yes (bound via S3) |
| **Occlusion / limited visibility** | — | **GAP → G1 (#122)** |

**Coverage gaps = uncovered hazards.** G1 (occlusion-aware caution) is a known
hole: until closed, a planner that drives too fast for the available sightline
is NOT caught. PO-1 is only as complete as this list; new hazards (S4 catalog)
must each get a Governor check or be excluded from the ODD.

---

## 3. PO-2 — Dependent Failure Analysis

Coupling factors between doer (Occy) and checker (Governor), per ISO 26262-9 Cl.7:

| # | Coupling factor | Failure if coupled | Required mitigation | Residual |
|---|---|---|---|---|
| C1 | Shared compute (SoC/core) | HW fault / resource exhaustion downs both | Spatial+temporal FFI: MPU-isolated partition on a separate core (min); separate SoC (strong) | low (sep. SoC) / med (partition) |
| C2 | Shared power | common power fault disables both | independent/monitored power to Governor; safe-state on loss | low |
| C3 | Shared memory/state | planner corrupts Governor state | spatial FFI; immutable validated inputs; no shared mutable state | low |
| C4 | Shared scheduling | planner starves Governor → missed deadline | temporal FFI / separate compute; WCET bound (S3) | low (fail-closed) |
| **C5** | **Shared perception / world model (iii)** | **common-mode: a perception error corrupts BOTH plan and check → unsafe trajectory approved** | tier-dependent — base: conservative re-derivation covers UNCERTAINTY only, OMISSION delegated to integrator perception (assumption-of-use, envelope-bounded); + D1 add-on: closes OMISSION unilaterally via independent catch-and-veto (ARCH-001 / ADR-0003) | **HIGH — see §4** |
| **C6** | **Shared localization (G2)** | localization error misplaces map-anchored checks + plan | localization-confidence gating; combine perception+map; degrade on low confidence (#123) | med until G2 |
| C7 | Shared sensors | sensor fault/spoof hits both | tier-dependent — base: shared with integrator sensors (residual = assumption-of-use); + D1 add-on: dedicated diverse sensing (radar + thermal + optical), True-Redundancy analog, closes C7 unilaterally (ARCH-001) | base: high; + D1: low |
| C8 | Shared software/libraries | bug in shared code (RSS/math/parser) defeats both | minimize shared code on the safety path; develop Governor path independently to ASIL D; diverse impl where feasible | med |
| C9 | Shared systematic/design (same team/assumptions) | same wrong assumption in both | design/process diversity; independent review; **KIRRA vendor-independence** | low w/ independence |
| C10 | Shared egress/comms | unchecked command bypasses Governor | Governor in-line on actuation egress; no bypass; verify (teleop lesson) | low |
| C11 | Cascading (planner crashes checker) | malformed output crashes Governor | input validation; bounded processing; fail-closed (done: body-bound, NaN-trap) | low |
| C12 | Environmental | common temp/EMI/vibration stress | automotive qual; separate placement if co-located | low |

---

## 4. Central finding — the shared world model (C5/C7)

**This is the finding that changes the plan.** The (iii) conservative-shared
world view is a common-cause input: the Governor re-derives RSS at worst-case
bounds, but it re-derives from *the same perception the planner used*.

The critical distinction: **conservative bounds mitigate UNCERTAINTY, not
OMISSION.**
- *Uncertainty* (a detected-but-imprecise object): widen its bounds — the
  conservative Governor handles this. ✔
- *Omission* (an object/water/VRU never detected at all): you cannot be
  conservative about something you cannot see. The Governor shares the
  planner's blind spot and approves the unsafe trajectory. ✘

Omission is the highest-severity failure mode (the unseen pedestrian, the
undetected water edge), and it is **exactly the class the shared world view does
not cover.** Therefore the independent **(ii) world-state / detection channel
must be pulled forward** from Phase 4 — at least a focused independent detector
for the omission-critical classes (large obstacle, VRU, water boundary, commit
zone) — or the ASIL-D claim has a common-cause hole.

This does not require a *full* diverse world model immediately. The pragmatic
first step is an independent **detection** channel (diverse sensor/algorithm,
True-Redundancy style) for the few omission-critical classes, feeding the
Governor's veto path only. Full (ii) world-state diversity can still mature
later.

### 4.1 Tier-dependent disposition (see ARCH-001 / ADR-0003)

The omission gap above has **two valid dispositions**, captured by the two-tier
architecture (KIRRA-OCCY-ARCH-001 / ADR-0003):

- **Base (downstream Governor, no D1):** omission common-cause mitigated by
  conservative checking + the Perception Input Contract (ARCH-001 §4) +
  envelope-bounding to the integrator's delivered coverage. **Residual =
  delegated** to the perception provider as an explicit assumption-of-use.
  Standard SEooC disposition; ASIL-D claim is conditional on the contract.
- **+ D1 add-on (Tier 2):** omission common-cause **closed unilaterally** by
  KIRRA's own dedicated diverse sensing (radar + thermal + optical-for-water)
  on the Governor's independent compute. Residual reduces to D1's own
  characterized FP/FN, validated by S8 (#120). ASIL-D omission claim is
  unilateral.

The pull-forward narrative above ("independent (ii) detection channel … now")
remains the **prescription for the D1 add-on**; ADR-0003 re-frames it from
core-mandatory to optional Tier-2. Base-tier integrators who bring competent
diverse perception keep the SEooC claim; D1 buys the unilateral close.

---

## 5. Freedom from interference (FFI)

For the Governor to be a valid ASIL-D element it needs:
- **Spatial** — memory protection / partitioning; Governor state not writable by
  the planner; inputs copied and validated.
- **Temporal** — guaranteed execution budget; the planner cannot starve the
  Governor; missed budget → fail-closed (SG9). Strongest via separate compute.
- **Communication** — the Governor sits in-line on the actuation egress; there
  is no path from planner (or teleop) to actuators that bypasses it.

**Decision — compute separation.** Partition-on-shared-SoC (MPU/hypervisor
isolation) is the minimum; **separate SoC** is the strong form and the only one
that fully clears C1/C2/C12. KIRRA's vendor-neutral, independent-runtime thesis
argues for separate compute.

---

## 6. Decisions surfaced (need owner sign-off)

1. **Pull the independent (ii) detection channel forward** — required to close
   C5/C7 for omission failures. Recommended: focused independent detector for
   omission-critical classes now; full (ii) later. *(The central finding.)*
2. **Compute separation level** — isolated partition vs. separate SoC.
3. **Planner rigor** — strict QM vs. disciplined-QM (defense-in-depth).
4. **Close G1 (#122)** to complete PO-1 coverage; **close G2 (#123)** to clear C6.

---

## 7. The independence differentiator

C9 (shared systematic/design faults) is where most architectures are weakest —
The Tier-1 ADAS benchmark vendor and NVIDIA build checker and planner in-house, same team, same
assumptions. KIRRA's **vendor-independent checker** is the strongest possible
mitigation for C9, and a clean independence argument for C1/C8 as well. The DFA
is precisely the artifact where that independence becomes evidence rather than
a claim — the part competitors hand-wave.

Cross-refs: OCCY_SAFETY_GOALS.md (#113), OCCY_SOTIF.md (#116), S3 WCET (#115),
S8 validation (#120), G1 (#122), G2 (#123). Register as KIRRA-OCCY-DFA-001.
