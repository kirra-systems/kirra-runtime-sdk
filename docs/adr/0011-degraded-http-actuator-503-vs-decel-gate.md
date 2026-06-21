# ADR-0011: Degraded HTTP actuator path — 503 deny-all vs. the inner decel-to-stop gate

| Field | Value |
|---|---|
| Status | **Proposed resolution (2026-06-21)** — owner direction set; ratified on merge. Supersedes the prior "Open / deferred" status (see Decision). |
| Date | 2026-06-20 (finding) · 2026-06-21 (resolution) |
| Deciders | Project / safety-case owner |
| Issues | #405 (this), #406 (sequenced behind — MRC divergence, ADR-0012), #70 (Degraded decel-to-stop-and-HOLD design) |
| Code | `src/bin/kirra_verifier_service.rs` (router assembly), `src/gateway/policy_layer.rs`, `src/posture_cache.rs` (`should_route_command`), `src/bin/kirra_carla_client.rs` (the actuator consumer) |

## Context

On the assembled production router, the outermost layer is `enforce_posture_routing`.
A `POST /actuator/motion/command` classifies as `OperationalCommand::WriteState`, and
`should_route_command`'s Degraded arm permits `ReadTelemetry` **only** — so in Degraded the
command is **503'd by the outer gate before the inner `enforce_actuator_safety_envelope`
ever runs**. The inner envelope's Issue-#70 **controlled-decel-to-stop-and-HOLD** branch
(`enforce_degraded_decel_to_stop`) and its LockedOut short-circuit are therefore **dead code
on the HTTP path** (#405).

Two facts frame the decision:

1. **An invariant is in tension.** CLAUDE.md states *"`should_route_command` … Degraded →
   allows `ReadTelemetry` only."* Restoring #70's intent on the HTTP path (letting Degraded
   actuator `WriteState` reach the inner gate) **relaxes a documented fail-closed rule** — a
   HARA/DFA-gated change, never a quiet code edit.
2. **CLAUDE.md is inaccurate here.** It claims the decel-to-stop gate is *"wired at all four
   enforcement points,"* including the gateway `enforce_actuator_safety_envelope`. That holds
   for the fabric / parko-kirra / ros2-adapter call sites (which invoke
   `enforce_degraded_decel_to_stop` directly, off the HTTP posture-routing gate) but **not**
   for the HTTP actuator path, where the inner gate is unreachable.

## Finding (verified 2026-06-20) — the `503 → controlled-stop` mapping does NOT exist

The safety of "accept the 503 deny-all" (#405 Option B) rests entirely on the downstream
consumer mapping `503 → controlled stop`. **It does not.** The only in-repo consumer of
`/actuator/motion/command` is `kirra_carla_client::submit_motion_command`
(`src/bin/kirra_carla_client.rs`):

```
200 → enforced response
400 → DenyBreach        (enforced_linear = 0.0)   // stop
403 → DenyBreach        (enforced_linear = 0.0)   // stop  (LockedOut)
s   → Err("unexpected status {s}")                // ← 503 (Degraded) lands here
```

and the caller, on `Err`:

```rust
Err(e) => { eprintln!(...); state.elapsed_ms += DT_MS; continue; }  // hold last command; NO decel
```

So **403 (LockedOut) is mapped to a stop, but 503 (Degraded) is not** — it falls to the
catch-all `Err`, and the consumer holds the last command (no decel). The ROS interceptor
(`src/gateway/interceptor.rs`) does not consume the actuator HTTP response at all.

**Consequence:** the current Degraded HTTP behavior is *not* "merely more conservative." A
Degraded command is denied with 503 and **nothing converts that to a decel-to-stop** — the
vehicle holds its pre-Degraded speed until a separate watchdog fires. That is the **opposite**
of Issue #70's intent (Cruise SF Oct-2023 pullover-drag lesson) and a latent hazard. So this
is **not** a free choice between A and B — there is a **gap to close** regardless. The client
*creates* the no-command condition on 503 (it does not resolve it), whereas on 400/403 it
actively authors a zero command — an asymmetry in the consumer's deny handling.

## Options (owner decision)

- **A — relax the invariant.** Let Degraded actuator `WriteState` reach the inner gate so
  `enforce_degraded_decel_to_stop` runs and forwards a converging-decel command. Keep
  LockedOut deny-all. **Requires** a HARA/DFA rationale and this ADR's sign-off to amend the
  documented "Degraded = ReadTelemetry only" invariant.
- **B⁺ — keep the invariant AND close the gap.** Keep the 503 deny-all, but make the
  consumer fail-closed to a controlled stop on 503 (map 503 like 403 → `enforced 0.0` / decel,
  not hold-last-command), and record the **real-robot interceptor's `503 → controlled stop`
  as an Assumption of Use** (the production actuator is integrator-owned, so the kernel cannot
  guarantee it). Note this still does *not* forward a converging-decel command — it commands a
  stop — so it is more conservative than #70 but safe.
- **Either way:** add the missing **assembled-router (`build_app`) Degraded actuator test**
  asserting the chosen contract (its absence is the coverage gap that hid this — the existing
  Degraded *envelope* tests call `enforce_actuator_safety_envelope` directly, bypassing the
  outer gate), and **correct the CLAUDE.md "wired at all four points" claim**.

## #406 is sequenced behind this

The fabric-vs-gateway MRC divergence (#406: automotive Degraded ceiling **10.5 m/s** via
`KinematicProfileType::mrc_contract()` vs **5.0 m/s** via `mrc_fallback_profile()`) is **latent
behind the same 503** — the fabric Degraded branch is also unreachable on the HTTP path. It
becomes live under Option A (or any non-HTTP caller). Reconciling it needs (a) an
authoritative-MRC decision and (b) **per-platform derate factors derived from the HARA /
safety case** — the 5.0 m/s figure is load-bearing (CLAUDE.md / `SAFE_STATE_SPECIFICATION`
SS-002) and must not be chosen for convenience. #406 stays blocked behind #405 and its numbers
come from the safety case. The #406 decision is recorded in **ADR-0012**.

## Decision

> The prior status was **Open / deferred to the safety-case owner**. The owner direction below
> was set on 2026-06-21 following the verified egress trace; **merging this ADR ratifies it.**
> A deliberate fail-closed invariant is never relaxed silently — the decision lives here, in
> the open, for any assessor reading the trail.

**Resolution (2026-06-21) — owner direction:**

1. **The `503 → controlled-stop` consumer fix is the SAFETY FLOOR — do it first, independent
   of A vs B.** Map `503` in the actuator consumer to an `enforced 0.0 / 0.0` controlled stop,
   exactly as `400` / `403` already are (`src/bin/kirra_carla_client.rs`, and the production
   interceptor as its mirror). This removes the hazard **at the layer the kernel controls**,
   restores the consumer invariant *"every deny-shaped response authors a safe command,"* and
   does **not** depend on the integrator-owned DBW. It is correct under either A or B and is
   therefore not an A/B question — it is just correct.
2. **Option A becomes a follow-on DESIGN choice, not a safety requirement.** Once the consumer
   safe-stops on 503, A only upgrades a flat-zero stop to a richer *converging-decel* stop. If
   adopted it is still HARA/DFA-gated (it relaxes the documented "Degraded = ReadTelemetry
   only" invariant), but it is no longer load-bearing for the safety floor.
3. **Bare B (document the 503 contract + annotate dead branches, no consumer fix) is
   REJECTED.** It parks the hazard on the unconfirmed DBW no-fresh-command watchdog (Dataspeed
   command-freshness spec) — an integrator/hardware assumption, not a kernel guarantee.
4. **DBW watchdog = defense-in-depth, not the safety floor.** Confirm the Dataspeed
   command-freshness behavior and record it as an Assumption of Use, but the `503 → 0.0` fix
   is what lets the safety case stop depending on it.

**Evidence:** the verified in-repo egress trace (Finding above) — full four-way client trace
and the contrast with the fail-safe ROS2/parko path (which publishes an affirmative MRC
command on any deny, but bypasses the HTTP posture gate) is recorded on issue #405:
https://github.com/kirra-systems/kirra-runtime-sdk/issues/405#issuecomment-4761893645

**Still required either way:** the assembled-router (`build_app`) Degraded actuator test, and
the CLAUDE.md "wired at all four points" correction.

**Implementation** (the consumer `503 → 0.0` fix + the test) lands from a laptop (large
governor sources; web `git push` unavailable). This ADR records the decision, which does not.
