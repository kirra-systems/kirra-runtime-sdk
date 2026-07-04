# Learning-Loop Architecture — fixed checker (Kirra) + adaptive doer (Linux)

Status: DESIGN (2026-06-05). Owner: Justin. Scope: how Kirra's interventions feed a
continuous-improvement loop for the planner/perception **without** Kirra ever learning.

> **Repo placement / status.** Design record. Cross-refs: ADR-0004 (independent safety
> channel — the doer/checker boundary this loop is built on), the fixed verdict path
> (`src/gateway/kinematics_contract.rs`, blob `ed00f4da…`), KIRRA-OCCY-DEPLOY-001 (bench →
> vehicle topology), and #189 (QNX 8.0 / nto80, see §6). Open decisions are tracked in §8;
> **§3 capture location is CONFIRMED — hybrid (3)** (owner 2026-07-04), and is the shape the
> repo already implements (Phase 1 #191 / Phase 1.5 #192, default-OFF). See the as-built
> grounding appendix (§9) for the existing repo mechanisms this design builds on.

## 0. The one principle everything follows
- **Checker (Kirra)** — fixed, certified, has final authority on the command path,
  and *produces the training data*. **Never learns. Never co-adapts.**
- **Doer (Occy / Autoware perception + prediction + planning)** — adaptive. *Proposes*
  commands; never has final authority. This is what improves.
- **Parko** — the governed inference runtime that *serves* the doer's learnable models
  deterministically. Doesn't train.

The fixed checker is precisely what makes letting the doer learn *safe*: the doer can be
imperfect and still never execute a catastrophic command, because Kirra bounds the
consequences. The governor is the seatbelt that lets the doer experiment.

## 1. Two domains, one minimal interface

```
   SAFETY DOMAIN  (certified, fixed)              AUTONOMY + LEARNING DOMAIN  (Linux)
   ┌─────────────────────────────┐                ┌──────────────────────────────────────┐
   │ Kirra governor (fixed)      │◄── proposal ───│ Occy / Autoware  (DOER — learns)       │
   │  verdict path ed00f4da…     │                │   perception → prediction → planning   │
   │  pass / clamp / MRC / veto  │── governed ───►│   models served by Parko (TensorRT)    │
   │        │  verdict record     │   command      │   → actuator iface / Dataspeed bridge  │
   └────────┼────────────────────┘ (non-blocking) └───────────────┬────────────────────────┘
            │                                                      │ telemetry (bus tap)
            └──────────────► Linux collector ◄─────────────────────┘
                                   │
                                   ▼
                    corrective-supervision dataset (versioned)
                                   │ train (GPU box / cloud)
                                   ▼
                    candidate model ── validate in AWSIM vs FIXED Kirra
                                   │
                                   ▼  human release gate (per-model safety case)
                    deploy new model → Parko ─────────────► (back to "drive")
```

The interface is deliberately thin: the doer **proposes**, Kirra **disposes**, and Kirra
**emits a small verdict record**. The safety domain's correctness does NOT depend on the
learning domain — Kirra fails closed on silence/garbage. That independence is exactly what
licenses the Linux side to be experimental.

## 2. The data: the corrective-supervision triple
Every Kirra decision is captured as:
1. **State / inputs** — what the doer saw at decision time (perception output, ego state,
   relevant context).
2. **Proposal** — the command/trajectory the doer wanted to execute.
3. **Verdict + correction** — Kirra's decision (pass / clamp / MRC / veto), the deny
   code/reason, and the *safe command Kirra substituted*.

Plus metadata: timestamp, **doer model version**, scenario tags, pass-vs-intervention flag.

This is richer than negative examples: a clamp gives you *(state, bad proposal, safe
correction)* — the same shape as learning from an expert who only steps in when you're
about to err. High-quality supervision, and you already generate it for free.

## 3. Capture — where it happens  [CONFIRMED — hybrid (3), owner 2026-07-04]
The certified checker must NOT take on heavy logging/IO that could touch its determinism or
WCET. Three options:

- **(1) Kirra logs the full triple.** Authoritative, but loads the certified checker with a
  non-safety responsibility. Risky for the verdict path's timing budget.
- **(2) Linux observer reconstructs from the bus.** Keeps the checker minimal, but if
  Kirra's deny-code/reason isn't on the wire, you lose the richest part of the signal.
- **(3) HYBRID — recommended.** Kirra emits a *small, fixed-size, fire-and-forget* verdict
  record (deny code + the substitute it applied) on a **non-safety telemetry path** that the
  verdict path never waits on (ring buffer / shared mem). A **Linux-side collector** joins
  that with bus-observed proposal + perception to assemble the full triple and store it.
  → certified checker's extra job stays tiny and non-blocking; all heavy data engineering
  lives on Linux.

**Decision: (3) — CONFIRMED (owner, 2026-07-04).** The hybrid is also what the repo already
implements: the fire-and-forget emit is live at both seams (fast-loop command gateway,
Phase 1 #191; slow-loop trajectory, Phase 1.5 #192), default-OFF behind
`KIRRA_CAPTURE_ENABLED`, wait-free `try_send` (drop-on-full), verdict path byte-identical
(`ed00f4da…`). The Linux-side collector (`crates/kirra-collector`, `COLLECTOR_DESIGN.md`
D1–D6) is the offline joiner. The emit records **every** verdict arm (Allow/Clamp/Deny —
not Deny-only), deliberately, to avoid downstream selection bias
(`src/gateway/policy_layer.rs`). (§9 grounds the off-hot-path emit primitive this generalized.)

## 4. The closed loop (with the human gate)
1. **Drive** — doer proposes → Kirra governs → governed command executes; Kirra emits
   verdict records.
2. **Capture** — Linux collector joins verdict records + bus telemetry → versioned dataset.
3. **Mine** — pull corrections (clamp/MRC/veto) **plus** normal-driving samples **plus**
   sim data. (Never clamp-only — see §5.)
4. **Train** — improve the Autoware perception/prediction/planning model(s) offline on the
   GPU box / cloud.
5. **Validate** — run the candidate in AWSIM **against the FIXED Kirra**; measure driving
   quality, intervention rate, and absence of new unsafe behaviors, plus existing gates.
6. **Gate (human)** — deliberate release decision; candidate must pass sim + against-fixed-
   governor first. Tag the model with its dataset + validation lineage.
7. **Deploy** — new model artifact → Parko. Kirra **unchanged**.
8. **Repeat** — new model generates fresh verdict records → step 1.

"Continuous improvement" = a steady cadence of **validated releases**, NOT live weight
updates on a moving vehicle.

## 5. Safety invariants (architectural constraints, not nice-to-haves)
1. **Kirra never trains / never co-adapts.** It's the fixed reference the doer is validated
   against. Enforced by: separate, hash-pinned, certified binary; the training pipeline has
   no write path to it.
2. **Safety-domain independence.** Kirra's correctness can't depend on the learning domain;
   it fails closed on silence/garbage. The interface is one-way-authority.
3. **Objective ≠ "minimize clamps."** Optimize *good and safe driving*; clamp-rate is a
   diagnostic, not the loss. (Goodhart: minimizing clamps alone teaches timid or
   governor-gaming behavior.)
4. **Never train on clamps alone.** Mix corrections + normal driving + sim. (Selection
   bias: corrections are *safe*, not *optimal*.)
5. **Human-gated, sim-validated deploys.** No online learning on the live vehicle; every
   model proves itself in sim against the fixed governor.
6. **Versioned lineage.** Every deployed model ties to its dataset + validation evidence —
   a per-model mini safety case, consistent with the existing safety-case discipline.

## 6. Where each piece runs — and the bench-vs-production reality
| Component | Bench (now) | Production vehicle (later) |
|---|---|---|
| Kirra (checker, fixed) | **Linux** (Orin/JetPack) | **QNX** (certified RTOS, dedicated/partitioned) |
| Occy / Autoware (doer) | Linux | Linux compute node |
| Parko (serves models) | Linux (Orin, TensorRT) | Linux / DRIVE OS |
| Collector + training + AWSIM + gate | Linux (GPU box / cloud) | Linux (offline) |

Key honest point: **the learning loop is OS-agnostic for Kirra.** The checker's job —
propose → govern → emit verdict record — is identical whether it runs on Linux (bench) or
QNX (production). So you build and run the *entire* loop on Linux now; Kirra later moves to
QNX for the certified vehicle with **zero change to the loop**. (QNX 8.0 is currently
blocked on upstream libc `nto80`; tracked separately in **#189**. Bench Kirra stays Linux
regardless.)

## 7. Staged build plan
1. **Capture pipeline** (next; buildable pre-hardware) — Kirra emits the non-blocking
   verdict record; Linux collector assembles the versioned corrective-supervision dataset.
2. **Sim validation harness** — candidate model in AWSIM vs fixed Kirra (uses the GPU box).
3. **Training loop** — dataset → offline model improvement.
4. **Release gate + lineage** — human-gated deploy with per-model safety case.
5. **(Later) QNX production deployment** of the unchanged checker.

## 8. Open decisions to confirm
- ~~§3 capture location~~: **CONFIRMED — hybrid (3)** (owner 2026-07-04); as-built at both
  seams (#191/#192). The `CAPTURE_PIPELINE_SPEC.md §6` sub-decisions are likewise resolved
  by `COLLECTOR_DESIGN.md` D1–D6 (owner 2026-06-06).
- Which model(s) learn first (perception/prediction vs planning) — pick the one whose
  failures dominate Kirra's interventions once the capture data exists. **Still open**
  (needs capture data from a bench run).
- ~~Dataset schema + storage~~: **CONFIRMED** — Parquet + bulk-ref, partitioned by
  `doer_version` (`COLLECTOR_DESIGN.md` D4).

## 9. As-built grounding (repo, verified 2026-06-05)
Editorial appendix (not part of the original design; records how the design lands on the
current codebase so a future implementer starts from what exists):

- **The "fixed checker" anchor is real.** `kirra_core::kinematics_contract`
  (`validate_vehicle_command`, the verdict path; re-exported via
  `src/gateway/kinematics_contract.rs`) is the hash-pinned reference. It is amended ONLY
  under explicit review + a re-pin: the stop-gate review H1/M1 amendment (ClampBoth +
  direction-aware accel/brake) re-pinned it to logic blob
  `ed00f4da30afe8f3f83ff10a0d31103737526622` (superseding the historical
  `997fb7ae…`; see `CAPTURE_PIPELINE_SPEC.md` §0). §5 invariant #1 ("hash-pinned, no
  write path") is the discipline enforced.
- **§3(3) "fire-and-forget verdict record" is a generalization of an existing primitive.**
  The Deny arm of `enforce_actuator_safety_envelope` (`src/gateway/policy_layer.rs`) already
  emits a structured `KinematicViolationPayload` (deny code + values) via
  `audit_writer_tx.try_send(...)` — wait-free, drop-on-full, **off the verdict path** (the
  WCET-gate boundedness argument depends on that try_send never blocking). And
  `EnforcementOutcome { action: Allow | ClampLinear | ClampSteering | Deny, + enforced
  values + ProposedCommandPayload }` is already produced for **every** arm (threaded as a
  request extension today). So §3(3) ≈ "extend that emit from Deny-only to all verdicts,
  onto a telemetry path a Linux collector reads" — not new machinery on the certified path.
- **Non-safety telemetry-path precedent exists.** `PostureStreamEvent` is broadcast over a
  bounded ring buffer (`broadcast::channel(POSTURE_BROADCAST_CAPACITY)`, SSE) — the same
  "publish, never block the verdict" shape §3 calls for.
- **Load-bearing constraint for whoever builds §7.1:** the verdict-record emit must stay a
  bounded, droppable enqueue (try_send "Full = Err, never block"), and
  `kinematics_contract.rs` must remain byte-identical. The capture pipeline adds a
  *non-safety* emit + a Linux collector; it must not become a verdict-path dependency.
- **QNX move is tracked.** §6's "Kirra later moves to QNX, zero loop change" depends on the
  QNX-8.0 toolchain, blocked on upstream libc `nto80` — issue **#189**.
