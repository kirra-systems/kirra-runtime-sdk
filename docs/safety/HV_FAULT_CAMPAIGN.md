# KIRRA-OCCY-HVFAULT-001 — The #279 hypervisor-layer fault-injection campaign (Phase-II execution spec)

| Field | Value |
|---|---|
| Status | **Specified** — executable when the Phase-II preconditions (below) are met |
| Date | 2026-07-02 |
| Scope | The **hypervisor-owned rows** of the HVCHAN §4 failure-semantics table, plus re-validation of the discipline/judge rows over the real `HvRegion` carrier. Maps **1:1** onto §4's "owning barrier" column (#279's attribution taxonomy) |
| Cross-refs | `HYPERVISOR_CONTRACT_CHANNEL.md` (HVCHAN-001 §4/§5); ADR-0030 (Clause B `HvRegion`; the *Update 2026-07-02* Phase-I evidence); ADR-0031 (budget split the latency rows verify); `AOU-HV-CLOCK-001` / `AOU-HV-ROMAP-001` / `AOU-HV-SCHED-001` (the register entries these rows discharge); `crates/kirra-l3-e2e` (the harness the campaign extends); #279, #274, #278 |

## 1. Purpose and the attribution rule

HVCHAN §4 assigns every fault a **detection point**, a **fail-closed verdict**, and an
**owning barrier** (hypervisor / contract-discipline / judge). Phase-I (PR #766) proved
the *contract-discipline* and *judge* rows on the QNX target OS. This campaign is the
remaining half: the rows only a real hypervisor deployment can exercise — and the
**attribution rule is normative**: *a fault absorbed by the wrong layer is a FINDING,
not a pass.* A guest flood survived only because the judge is fast means the hypervisor
scheduling config failed, even though nothing unsafe was actuated.

## 2. Preconditions (the honest gate — recorded 2026-07-02)

1. **QNX Hypervisor 8.0 product.** Probed on the dev laptop: the SDP 8.0 install
   carries hypervisor *documentation* and the `mkqnximage/qvm` image **template**, but
   **no `qvm` binary / vdev modules** — the hypervisor package is not present under the
   current (non-commercial) license. QNX Hypervisor for Safety is a commercial product
   (see `KIRRA_PLATFORM_DEPLOYMENT_STRATEGY.md`). **The campaign is blocked on
   obtaining the hypervisor package**; the `mkqnximage --type=qvm` path means the
   tooling flow exists the day the package installs.
2. **A hypervisor image** with two partitions: a guest (publisher; eventually the ROS
   adapter's host) and the governor partition, sharing one `shmem` vdev region of
   exactly `CANONICAL_IMAGE_LEN` bytes — guest RW, governor **RO** (R-HV-1).
3. **The `HvRegion` binding** (ADR-0030 Clause B): `ContractReader`/`ContractWriter`
   over the vdev region — the same traits, trust chain, and freeze assertions as
   `PosixShmRegion`; only the map primitive differs. The `kirra-l3-e2e` harness then
   runs UNCHANGED over it (the carrier is behind the seam).
4. **The boundary-clock primitive** identified and readable from both partitions
   (AOU-HV-CLOCK-001) — required by the HV-C1 row; the campaign can start without it
   (rows HV-R1/HV-S1/HV-S2 do not need it) but cannot **complete**.

## 3. Row matrix

**Already discharged (Phase-I, PR #766, QNX OS)** — re-run over `HvRegion` as
regression, expecting byte-identical verdicts:

| Row | §4 fault | Phase-I evidence |
|---|---|---|
| CD-1..5 | layout-version / magic / retry-exhaustion / CRC / bounds | `kirra-l3-e2e` L3-05 (cold start) + the carrier/consumer test suites |
| J-1..2 | judge rejects (deadline, kinematic, replay/regress) | `kirra-l3-e2e` L3-02/03/04 |

**Hypervisor-owned rows (this campaign — need the real hypervisor):**

| Row | Fault injected | Injection method | Observation point | PASS criterion (attribution-correct) |
|---|---|---|---|---|
| **HV-R1** | Governor-side write attempt on the contract region (R-HV-1 violation probe) | A deliberate test-only write through the governor partition's mapping (page poke at a known offset) | Hypervisor fault handling; region bytes | The write **faults at the hypervisor** (partition exception / denied), region bytes unchanged. Absence-of-write-in-software is NOT a pass — the mapping must make it impossible (`AOU-HV-ROMAP-001`) |
| **HV-S1** | Guest CPU **flood**: guest spins all its vCPUs at max while publishing at max rate | Guest-side busy loops (N threads) + tight publish loop | Governor-path latency percentiles (`kirra-l3-e2e` timing section) under load | Governor `decide_cycle` percentiles stay within the FTTI allocation and within a stated factor of the Phase-I unloaded baseline (~1.1 µs p99.9 FIFO). Degradation traced to the scheduler = **hypervisor-config FINDING** even if verdicts stay correct (`AOU-HV-SCHED-001`) |
| **HV-S2** | Guest **starve-then-burst**: guest goes silent past the liveness window, then floods | Guest publisher pause (> watchdog window) followed by max-rate burst | (a) liveness posture during silence; (b) governor latency + watermark behavior during the burst | (a) `publisher silent` row fires → the SG-003 Degraded/MRC posture (never a stale-data accept); (b) the burst neither starves the governor nor replays past the watermark |
| **HV-C1** | **Clock skew beyond bound** between partitions (R-HV-3) | Skew the guest's view of the boundary clock beyond the AOU-HV-CLOCK-001 bound (test hook or vdev manipulation) | Governor deadline verdicts | Deadlines mis-signed by skew are **rejected** (the §4 skew row) — never a stale command admitted as fresh. Requires the measured skew bound; completes AOU-HV-CLOCK-001 |
| **HV-T1** | Torn/churning writer across the **real** partition boundary | Guest publishes at max rate while the governor snapshots continuously (the Phase-I seqlock test, now over the vdev + hypervisor memory model) | Snapshot loop outcomes | Zero torn snapshots accepted; persistent churn → `RetryExhausted` reject. Re-proves the Clause-A atomics assumption on the hypervisor's actual shared-memory semantics |

**Timing evidence taken alongside** (not pass/fail rows): the `kirra-l3-e2e` timing
section on the governor partition — unloaded, under HV-S1, and under HV-S2 — labelled
per the #274 discipline (only genuine hypervisor-hardware-under-FIFO numbers may feed a
WCET/FTTI claim; `KIRRA_WCET_CERTIFIED` stays reserved for certified hardware).

## 4. Instruments

- `kirra-l3-e2e` (PR #766) — the verdict matrix + timing, unchanged over `HvRegion`.
- A small guest-side **load generator** (flood threads + publish-rate control + the
  silence/burst sequencer) — to be added to the harness crate when the campaign
  unblocks (`--load` role beside `--guest`).
- The HV-R1 **write probe** — a test-only governor-partition poke, kept OUT of the
  carrier crate (it violates the discipline on purpose; it lives with the campaign).

## 5. Exit criteria

The campaign is complete when: every HV-* row passes **with correct attribution**;
the CD/J regression rows are byte-identical over `HvRegion`; the three `AOU-HV-*`
register entries move from **AoU-GAP** to discharged-with-evidence (config review +
row results + the measured skew bound); and the timing set (unloaded / HV-S1 / HV-S2)
is recorded with honest labels in `crates/kirra-l3-e2e/results/`.
