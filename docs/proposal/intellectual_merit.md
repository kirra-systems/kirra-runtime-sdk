<!--
  docs/proposal/intellectual_merit.md — the CANONICAL Intellectual Merit text
  (NSF SBIR Phase I). Landed in-repo so the source of truth is versioned and
  diffable (the prior canonical was a Notion reconstruction; the original PDFs
  are lost). Every factual claim below is verified against this repository —
  see the "Provenance & changelog" section at the bottom for the edit history
  and the claim-verification discipline. The repo wins on any conflict.
-->

# KIRRA — Intellectual Merit (NSF SBIR Phase I)

## The gap

Autonomous driving and edge-AI systems now make actuation decisions using
machine-learned planners and perception. Those components are statistical:
their behavior cannot be exhaustively verified, and no ML planner can itself be
certified to the highest automotive functional-safety integrity level
(ISO 26262 ASIL-D). The field's response has been proprietary, vertically
integrated safety logic bolted onto single-vendor stacks — non-portable,
self-attested, and impossible for a third party to verify after an incident.
There is today no independent, certifiable, vendor-neutral runtime element that
sits between an arbitrary planner and the physical actuator and can prove, in
bounded time, that each command is safe. KIRRA closes that gap.

## The core idea: a checker, not a planner

KIRRA's verdict component — the **Kirra checker** — is classical and
deterministic, not a planner and not a watchdog. It does not propose
trajectories. It re-derives a conservative world view from the same inputs and
evaluates each candidate command from the upstream planner (**Occy**, the
project's classical planner, or a learned planner such as **TCP**) against
explicit kinematic and safety-envelope constraints — RSS separation, corridor
containment, kinematic clamp, speed cap — returning an accept / clamp / reject
verdict.

Because the verdict path is classical by mandate — allocation-free, O(1),
**deterministic** — it is amenable to the very verification the ML planner is
not. The machine learning remains upstream, in the planner and perception that
**Kirra** guards, separated from the checker by a frozen interface contract
(`kinematics_contract.rs`).

KIRRA is organized in tiers: the **Kirra Governor** (a Safety Element out of
Context, SEooC) → the verdict line → an optional D1 independent detection
channel.

## Why this is the right safety architecture

The doer/checker split is not an engineering convenience; it is the ISO 26262
ASIL-D decomposition itself. A high-integrity safety claim is achieved by
decomposing it across a complex, un-certifiable doer (the ML planner) and a
simple, independently verifiable checker, where the checker carries the
integrity. This is the central intellectual claim of the project. Phase I
exists to establish — with evidence, not assertion — that the claim holds: that
the checker's timing is bounded, that its independence from the doer is real
and defensible, that one checker generalizes across methodologically distinct
doers, and that the whole is certifiable.

The four research tasks below each retire one of those four open questions.

## Task 1 — Bounded-time verdict (WCET)

**Objective.** Establish a proven WCET bound for the **Kirra verdict path** on
a real-time QNX target — **QNX SDP 8.0** (the Phase I measurement target), with
**NVIDIA DRIVE + QNX OS for Safety** as the Phase II cert-grade target — with
demonstrated margin to the system Fault-Tolerant Time Interval (FTTI). The
verdict path is allocation-free and O(1); its contract-verdict core already
ships as a `no_std`, `core`-only artifact independent of the Linux/Jetson
doer-side compute, and folding the full kinematic clamp path into that
artifact — its steering clamp uses libm-class float operations that do not
cross a `core`-only boundary as-is — is part of this task's scope.

**The open question.** A runtime governor is only a safety element if its
verdict is guaranteed to land inside the FTTI. A checker that is merely fast on
average, or whose worst case is unbounded, provides no safety guarantee at all.

**Approach.** The verdict path is classical by mandate — allocation-free, O(1),
no unbounded loops — which is precisely what makes a provable bound, not just
an observed latency, achievable. We combine static WCET analysis of the verdict
path with high-watermark instrumentation measured on the QNX SDP 8.0 target.
Phase-I feasibility evidence already exists: the harness's `no_std`
contract-verdict judge has executed on a QNX SDP 8.0 x86_64 VM under KVM with
`SCHED_FIFO` — all 9 FDIT rows passing verdict-correctness, with a measured max
of 19.96 µs (p99.9 = 79 ns, n = 10⁶), recorded under the project's provenance
discipline as `INDICATIVE-NOT-WCET`: a VM figure is corroboration of the
structural bound, never the bound itself, and the toolchain refuses to mint a
target-measured label from a VM. The cert-grade measurement on NVIDIA DRIVE +
QNX OS for Safety is a Phase II deliverable.

**Success criterion (locked).** A proven WCET bound on a QNX SDP 8.0 target
under SCHED_FIFO with demonstrated FTTI margin — prove a bound, not a
pre-committed millisecond figure; the cert-grade measurement on NVIDIA DRIVE is
a Phase II deliverable.

## Task 2 — Independence and dependent-failure analysis (DFA)

**Objective.** Establish that the checker is independent of the doer to the
degree ISO 26262 requires for an ASIL-D decomposition, and analyze and close
the dependent-failure (common-cause) modes.

**The open question.** A decomposition only carries integrity if the checker
can fail independently of the doer. Two failure classes threaten this:
uncertainty (perception gaps shared with the planner) and omission (a common
cause that blinds doer and checker at once). Homogeneous deep-learning
redundancy — a second neural net — does not satisfy ISO 26262 independence,
because it shares failure modes with the first.

**Approach and claim.** The base tier closes the uncertainty class by
envelope-bounding perception gaps as a declared assumption-of-use: the checker
treats unresolved perception as worst-case within a bounded envelope. The
omission common cause is closed by the classical D1 independent detection
channel — diverse by modality (4D-radar CFAR, thermal, lidar anomaly) and
diverse by algorithm class (classical signal processing over ML, not another DL
model). D1 is specified as an optional Tier-2 channel (ADR-0003); its
implementation and the accompanying DFA are Phase I work.

**Success criterion.** A documented DFA showing the doer/checker independence
argument and the diverse D1 channel closing the residual common cause,
consistent with ISO 26262 decomposition requirements.

## Task 3 — Vendor neutrality across methodologically distinct stacks

**Objective.** Demonstrate that one checker, behind one frozen contract,
governs two upstream autonomy stacks of fundamentally different construction —
identically, with zero unsafe passes.

**The open question.** A vendor-neutral safety layer's central claim is that it
is indifferent to how the planner was built. That claim is only credible if the
same checker, unchanged, governs a classical planner and a neural end-to-end
agent with equal effect. If the checker had to be re-tuned per stack, it would
not be neutral.

**Approach.** Govern (1) **Occy, the project's geometric classical planner
(Autoware-interface-compatible)**, and (2) TCP, an end-to-end neural CARLA
agent — classical doer vs. neural doer — through the identical **Kirra
checker** and frozen `kinematics_contract.rs`, and show that zero unsafe
commands reach the actuator across the scenario battery. TCP is cited as the
credible ML stack. The dominant schedule risk lives in upstream bring-up (the
CARLA↔Autoware bridge), for which the CARLA BehaviorAgent is a lower-friction
classical fallback that preserves the classical-vs-neural contrast.

**Success criterion.** The identical checker governs both stacks; zero unsafe
passes across the scenario set under both doers.

## Task 4 — Certifiability and verifiable evidence

**Objective.** Produce the artifacts that make the safety claim auditable and
the element certifiable as a reusable ASIL-D building block.

**The open question.** Certifiability is not a property added at the end; an
element is certifiable only if its evidence is architected in from the start.
And that evidence must be verifiable by a third party who does not trust the
operator.

**Approach.** A requirements traceability matrix (RTM) linking safety
requirements to implementation and tests; structural coverage to ≥95% MC/DC
with documented exceptions — the accepted ASIL-D practice, not a 100% claim
that would misrepresent real ASIL-D programs; the Ed25519-signed,
tamper-evident attestation chain as a first-class safety-case artifact, with
every verdict, override, minimal-risk commit, and human clearance independently
verifiable; and a UL 4600 case skeleton for the autonomous-operation safety
argument.

**Success criterion.** RTM + ≥95% MC/DC (documented exceptions) + attestation
chain as safety-case evidence + UL 4600 skeleton.

## Preliminary work and readiness (TRL 3–4)

KIRRA is not a paper concept. A working governor prototype renders
**deterministic** verdicts today; the Ed25519 attestation chain is implemented
and tested; the safety-goal decomposition (**SG-001–SG-016**) is defined, and
an FDIT/RTM verification harness exercises **9 rows — 8 fault classes plus a
clean-accept control (SG-00–SG-08)** — every row passing verdict-correctness,
host-side in CI and on a QNX SDP 8.0 x86_64 VM (the #274 Phase-I feasibility
run; all timing to date is indicative — host CI regression gates and the
KVM-VM figure above — with cert-grade timing a Phase II deliverable); and the
verdict core is implemented in **Rust (upstream rustc today, Ferrocene-ready;
productization tracked in #132)** with a **C++→Rust FFI exercised host-side in
CI and validated on the QNX SDP 8.0 VM** (on-hardware cert-target validation is
Phase II). This places the work at TRL 3–4 entering Phase I and substantially
de-risks the 12-month plan.

## Schedule

A 12-month Phase I with milestones M1–M4 at months 3, 6, 9, and 12, mapping to
the four tasks: bounded-time verdict (M1), independence/DFA (M2), dual-stack
neutrality (M3), and certifiability evidence package (M4).

## Why it hasn't been done

The hard part is not any single component; it is holding all four properties at
once — bounded-time, independent, neutral, and certifiable — in one element.
Single-vendor stacks optimize their own planner and self-attest; they have no
incentive to build a neutral, third-party-verifiable checker, and their safety
logic is not architected as a reusable decomposition element. KIRRA's
contribution is the element itself, and the Phase I evidence that the four-way
claim holds.

---

## Provenance & changelog

Reconstructed from the author's full IM text (his own words), with corrections
applied in two passes. The repository is the source of truth for every factual
claim; on conflict, the repo wins.

**Pass 1 — the four edits (applied in the Notion canonical, 2026-07-09):**

1. **Core idea** — Occy → **Kirra** as the checker (3 spots); Occy/TCP named as
   planners; "bounded-time on QNX SDP 8.0 / Jetson Orin" → "deterministic"
   (removes the Jetson≠QNX category error and the premature bounded-time claim
   Task 1 exists to prove).
2. **Task 3 Approach** — "identical **Occy** checker" → "identical **Kirra**
   checker"; Occy named the **geometric classical planner
   (Autoware-interface-compatible)** — not "Autoware-based."
3. **Preliminary Work** — "certified-toolchain Rust" → "upstream rustc today,
   Ferrocene-ready (#132)"; SG-001–SG-016 safety goals distinguished from the
   harness rows; "renders bounded-time verdicts" → "deterministic";
   "FFI validated on QNX" → "exercised host-side" (as of that pass).
4. **Task 1** — Jetson≠QNX decoupled (SDP 8.0 = Phase I; DRIVE + QNX OS for
   Safety = Phase II cert-grade); "Occy verdict path" → "Kirra verdict path";
   Approach "measured on Orin/QNX" → "on the QNX SDP 8.0 target."

**Pass 2 — this repo landing (2026-07-10), all values quoted from repository
artifacts:**

5. **Placeholders filled** (Preliminary Work): the harness row count is **9
   rows — 8 fault classes plus a clean-accept control, SG-00–SG-08** — counted
   from the on-target artifact
   (`tools/qnx-rtm-harness/results/qnx800-x86_64-vm-kvm.txt`, FDIT matrix rows,
   `GATE: PASS`, `FDIT_EXIT=0`).
6. **Task 1 corroboration folded in** with the artifact's actual values:
   measured **max = 19.96 µs (19,962 ns), p99.9 = 79 ns, n = 10⁶**, QNX SDP 8.0
   x86_64 VM, KVM (`-enable-kvm -cpu host`), `SCHED_FIFO`, label
   **`INDICATIVE-NOT-WCET`** (`results/qnx800-x86_64-vm-kvm.txt:36-38`; the
   #734 provenance gate refuses a target-measured label from a VM). Framed as
   corroboration of the structural O(1) bound (`src/wcet_gate.rs`), never as
   "WCET = …".
7. **Task 1 Objective precision** (repo-wins correction): "the verdict path
   runs as a `no_std` … artifact" narrowed to what the code supports — the
   **contract-verdict core** is the `no_std`/`core`-only artifact today
   (`tools/qnx-rtm-harness/kirra_judge.rs`); the full kinematic clamp path uses
   libm-class float operations (`crates/kirra-core/src/kinematics_contract.rs`
   P6 steering clamp) and folding it into the `no_std` artifact is declared
   Task 1 scope.
8. **Preliminary Work freshness** (repo-wins, favorable direction): "on-target
   validation tracked in #274" was stale — the #274 Phase-I run has since
   executed (FDIT 9/9 on the QNX 8.0 VM, TCG and KVM); the FFI sentence now
   says host-side CI **and** QNX-VM validated, with on-hardware cert-target
   validation Phase II.
