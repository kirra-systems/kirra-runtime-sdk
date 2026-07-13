# ADR-0035 — Decomposing the `kirra-verifier` monolith into layered crates

| | |
|---|---|
| **Status** | Proposed |
| **Date** | 2026-07-13 |
| **Supersedes** | — (extends the ongoing de-monolith program: Stage 1 → `kirra-core`, Stage 7 → perception/kinematics/capture, plus `kirra-fleet-types`, `kirra-trajectory`) |
| **Safety goals touched** | None *directly* — this is a structural refactor. Every stage MUST be behaviour-preserving (byte-identical verdicts, same audit chain, same fail-closed semantics). The checker cores (`kirra-core`, `kirra-trajectory`) and the frozen kinematics talisman are already extracted and are NOT re-cut here. |

## Context

The root `kirra-verifier` crate is an integration monolith: it is simultaneously
the **cargo workspace root**, a **`cdylib`** (the C FFI surface), the home of **6
binaries**, and ~30 library modules spanning HTTP control-plane, SQLite
persistence, HA/epoch fencing, the posture engine + gray/black DAG, attestation,
audit chaining, industrial protocol adapters, fleet/fabric state, and
observability. The runtime engineering review recommends splitting it into
**control-plane / safety-authority / persistence / industrial / observability**
crates.

This ADR fixes the crate boundaries, the dependency DAG, and — critically — the
**sequence and prerequisites**, because the naïve reading ("just move the
industrial adapters out first") does not hold against the actual coupling.

### Evidence (measured against the tree at time of writing)

- **`AppState` is referenced in 39 files.** It is the god-object every layer
  hangs off (store handle, posture cache, federation, audit writer, epoch
  atomics, industrial replay sequence store, HA flags, metrics). The
  *safety-authority / persistence / control-plane* boundaries are **all blocked
  on decomposing `AppState`** — that is the hard core of this work, not a leaf.
- **The industrial layer is not a clean leaf.** `protocol_adapter.rs` +
  `adapters/{canopen,dnp3,ethernet_ip}.rs` depend on:
  - `OperationalCommand` (the command-classification enum, defined in the
    `posture_cache` / `gateway::policy` cluster — `gateway::policy` is 447 LOC and
    itself depends on `posture_cache`);
  - `action_filter` (266 LOC), which depends on `gateway::cmd_vel` and the
    `verifier` posture types;
  - `FleetPosture` (already in `kirra-core`, re-exported — the one clean dep).
  So extracting `kirra-industrial` first requires relocating a shared
  **command/policy-types foundation** — otherwise a new leaf crate would have to
  depend *back* on the root crate, which is a dependency cycle.
- Neither `action_filter` nor `gateway::policy` touches `AppState`/`VerifierStore`
  — they are pure decision logic. That makes a **policy-types foundation crate**
  tractable and the right *first* extraction (Stage 0 below), ahead of industrial.

### Constraints that shape the plan

1. **The root crate stays the workspace root and the `cdylib`.** Moving the
   `[workspace]` table or the FFI surface is out of scope; the root remains
   `kirra-verifier` and becomes the thin **control-plane** layer.
2. **Shim compatibility is mandatory, per the established pattern.** Each
   extraction leaves a `pub use` re-export shim so existing `crate::X` /
   `kirra_verifier::X` paths (including sibling crates and the 6 bins) keep
   resolving unchanged. No stage may force a same-PR edit of an unrelated
   consumer.
3. **Behaviour preservation is the acceptance bar.** Verdicts, audit-chain bytes,
   fail-closed ordering, and WCET-critical paths must be byte-identical. The
   Kani/loom/fuzz/power-loss/replay suites are the regression net.
4. **CI-gate implications** each stage must satisfy:
   - the quality-guardrails ratchet (`ci/quality_guardrails_baseline.json`:
     `max_lines`, `panic_budget`, `ownership_scope`) — moving code between crates
     changes which files are tracked; update the baseline with justification;
   - **MSRV on both lockfiles** (`cargo +1.88.0 check --workspace --locked`) — a
     new crate adds workspace members and must build on 1.88;
   - the orphan-pure-core / wire-or-delete guard (`ci/check_orphan_cores.py`);
   - `cargo-deny` supply-chain (a new crate must add no new advisory surface).

## Decision

### Target dependency DAG (leaf → root; must stay acyclic + strictly layered)

```
            kirra-core / kirra-fleet-types / kirra-trajectory        [DONE — checker cores]
                                   ▲
        kirra-policy-types   (OperationalCommand, action_filter, cmd_vel,     [Stage 0 — NEW foundation]
                              the pure classify/route decision logic)
                                   ▲
        kirra-industrial     (protocol_adapter + adapters/*)                  [Stage 1]
                                   ▲
        kirra-persistence    (verifier_store/*, StoreHandle, EpochFence/       [Stage 2]
                              NodeStore seams — already trait-shaped)
                                   ▲
        kirra-safety-authority  (posture engine + gray/black DAG, actuator     [Stage 3 — the hard core]
                                 gate, attestation, audit chain; OWNS the
                                 AppState decomposition into a SafetyAuthority
                                 handle)
                                   ▲
        kirra-observability  (metrics, health, request-observability,          [Stage 4]
                              audit export/verify surfaces)
                                   ▲
        kirra-verifier  (control-plane: the axum bins, route handlers, auth,   [Stage 5 — residual root]
                         config wiring, FFI cdylib, [workspace] root)
```

Rule: a crate may depend only on crates strictly *below* it. `AppState` is
replaced by a `SafetyAuthority` handle owned by Stage 3; the control-plane holds
it behind the same `ServiceState` the handlers already use.

### Staging (each stage = one shippable, shim-compatible, behaviour-preserving PR)

- **Stage 0 — `kirra-policy-types` (prerequisite, do FIRST).** Extract
  `OperationalCommand`, `action_filter`, `cmd_vel`, and the pure
  `classify_http_command` / `route_command_verdict` decision logic into a lean
  crate depending only on `kirra-core`. Root re-exports them. Unblocks Stage 1
  and de-risks the whole program by proving the shim+guardrail mechanics on
  low-safety-impact code.
- **Stage 1 — `kirra-industrial`.** Move `protocol_adapter` + `adapters/*` onto
  `kirra-policy-types`. Now a clean leaf. The unified/dedicated replay-key
  normalization (PR #918) already lives here conceptually.
- **Stage 2 — `kirra-persistence`.** Move `verifier_store/*` + `StoreHandle`
  behind the existing `EpochFence` / `NodeStore` traits (already defined — this
  is the least-invasive of the big three because the seams exist).
- **Stage 3 — `kirra-safety-authority` (mini-project).** Decompose `AppState`:
  split its fields by owner (persistence handle, posture state, attestation
  state, audit writer, HA epoch), introduce a `SafetyAuthority` aggregate, and
  move the posture engine + DAG + actuator gate + attestation. This is where the
  39-file `AppState` coupling is paid down; expect several sub-PRs.
- **Stage 4 — `kirra-observability`.** Metrics/health/observability middleware +
  audit export/verify read surfaces.
- **Stage 5 — residual `kirra-verifier`.** What remains is the control-plane:
  the bins, handlers, auth, config, FFI, workspace root.

### Non-goals

- Re-cutting the already-extracted checker cores or the frozen talisman.
- Moving the workspace root or the FFI `cdylib` out of `kirra-verifier`.
- Any behaviour change. If a stage tempts a semantic change, that is a *separate*
  ADR/PR.
- Splitting `parko/` (its own sub-workspace) or the doer-side crates.

## Consequences

**Positive:** enforceable layering (the compiler forbids control-plane code from
reaching into persistence internals); independent testability and MSRV surface
per layer; smaller certification units (the safety-authority crate becomes the
reviewable safety boundary, separate from HTTP plumbing); parallel CI.

**Cost / risk:** Stage 3 is genuinely hard (the `AppState` decomposition) and
must be sub-staged. Every stage carries guardrail/MSRV/lockfile bookkeeping.
Mitigation: the shim discipline keeps each PR small and reversible, and the
existing fault-injection suites (Kani, loom, fuzz, power-loss, deterministic
replay) gate behaviour preservation.

## Execution note

Stages land as independent PRs in order (0 → 5); Stage 0 is the gating
prerequisite and the proof-of-mechanics. Do not begin Stage 1 until Stage 0 is
merged, and do not begin Stage 3 until Stages 0–2 are merged.
