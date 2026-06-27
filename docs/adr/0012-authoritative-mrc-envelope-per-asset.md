# ADR-0012: One authoritative MRC (Degraded) envelope per asset

| Field | Value |
|---|---|
| Status | **Accepted — item 1 + item 3 implemented (2026-06-27)**; item 2 dispositioned (see Update). Owner direction (2026-06-21) ratified on merge. |
| Date | 2026-06-21 (decision) · 2026-06-27 (implementation) |
| Deciders | Project / safety-case owner |
| Issues | #406 (this), #405 / ADR-0011 (sequenced ahead — HTTP-path reachability), #70 (Degraded decel-to-stop), `SAFE_STATE_SPECIFICATION` SS-002 |
| Code | `src/fabric/governor.rs` (`KinematicProfileType::mrc_contract`), `src/gateway/kinematics_contract.rs` (`mrc_fallback_profile`), `src/gateway/policy_layer.rs` (Degraded branch) |

## Context

Two MRC (Minimal Risk Condition / Degraded) envelope definitions disagree for the **same
asset + posture**:

- **Fabric** — `KinematicProfileType::mrc_contract()` derives the MRC by a uniform
  **0.3× speed / 0.4× accel / 0.5× steering** derate of the *nominal* profile. For an
  automotive asset (nominal `max_speed_mps = 35.0`): `35.0 × 0.3 = ` **10.5 m/s**.
- **Canonical / gateway** — `mrc_fallback_profile()`, a hand-tuned profile used by
  `enforce_actuator_safety_envelope` / the policy layer: **5.0 m/s**.

The **5.0 m/s** figure is load-bearing in the safety case (Cruise SF Oct-2023 ~3 m/s post-stop
pullover-drag, *"under a 5 m/s crawl ceiling"*; CLAUDE.md / `SAFE_STATE_SPECIFICATION` SS-002).
The fabric's **10.5 m/s** is **2× looser** than the validated number — not a rounding gap.

Separately, the gateway's Degraded branch applies `mrc_fallback_profile()` (the automotive
5.0) **unconditionally to every platform**. So a `RobotNominal` asset (nominal 1.8 m/s) gets a
5.0 m/s Degraded ceiling — nonsensically *looser* than its platform-aware `1.8 × 0.3 = 0.54`.
There is no single authoritative answer to *"what is the MRC envelope for asset X in
Degraded"*, and which definition is looser depends on the platform.

## Decision (resolved 2026-06-21) — owner direction

1. **Authoritative MRC = the validated profile, not the computed derate (Option A).** Map
   automotive fabric assets to `mrc_fallback_profile()` so fabric and gateway agree at the
   safety-case **5.0** for automotive. Keep the per-platform `0.3× / 0.4× / 0.5×` derate only
   for non-automotive platforms, with each platform's derate factors **derived from its HARA /
   safety case**, documented, and not chosen for convenience. Aligning to the validated 5.0
   over a 2×-looser computed number is the conservative call; trusting a generic derate to
   land near the safety-case figure is not a safety argument.
2. **The "gateway applies automotive 5.0 to all platforms" defect is SEPARATE and
   UNCONDITIONAL.** Make the gateway Degraded branch profile-aware — resolve the asset's
   `KinematicProfileType` and use *its* `mrc_contract()` rather than the automotive fallback
   for every platform. This is wrong **regardless of #405 / Option A** and is not gated behind
   the reachability decision; it can land independently.
3. **Cross-point invariant + test.** The same asset + Degraded posture must yield the **same
   effective MRC speed ceiling** at every enforcement point (gateway HTTP envelope, fabric
   `AssetGovernor`, ros2-adapter, parko-kirra). Add a test asserting this so a future
   enforcement point cannot silently ship a drifted copy.

## Sequencing

The fabric Degraded branch (and this divergence) is **latent on the HTTP path** — 503'd by the
same outer `enforce_posture_routing` gate as #405 (see ADR-0011) — so the divergence becomes
*live* only under #405 Option A or via a non-HTTP caller of `route_command`. The
source-of-truth reconciliation (item 1) is therefore **sequenced behind the #405 resolution**.
The all-platforms gateway defect (item 2) is **unconditional** and may land independently.

All numbers come from the safety case (SS-002), never convenience. Implementation lands from a
laptop (large governor sources); this ADR records the decision, which does not.

## Update (2026-06-27) — implemented (#406 / #429)

ADR-0011 (the #405 reachability decision this was sequenced behind) is **Accepted (Option A)**,
so item 1 is unblocked. Landed in `src/fabric/governor.rs`:

- **Item 1 (authoritative automotive MRC) — DONE.** `KinematicProfileType::mrc_contract()` now
  returns the validated `mrc_fallback_profile()` (the safety-case **5.0 m/s**) for
  `AutomotiveNominal`, instead of the generic `0.3×` derate of the 35 m/s nominal (**10.5 m/s**,
  2× the validated figure). Non-automotive platforms keep their per-platform `0.3×/0.4×/0.5×`
  derate unchanged. No new number was introduced — the change adopts the *existing* canonical
  5.0 and is strictly more conservative (the fabric automotive ceiling tightens 10.5 → 5.0).
- **Item 3 (cross-point conformance test) — DONE.**
  `mrc_ceiling_is_authoritative_across_enforcement_points` pins
  `AutomotiveNominal.mrc_contract() == VehicleKinematicsContract::mrc_fallback_profile()`
  (full envelope, not just the speed scalar) with a `< 10.5` regression guard; companion tests
  pin the non-automotive derates and a behavioural over-ceiling clamp.
- **Item 2 (gateway profile-awareness) — DISPOSITIONED, no code change.** The gateway HTTP
  actuator path (`enforce_actuator_safety_envelope` / `policy_layer`) is **automotive-only**:
  it threads **no** per-asset `KinematicProfileType` and hard-codes
  `nominal_reference_profile()` / `mrc_fallback_profile()`, so the automotive 5.0 is the
  *correct* envelope there — a non-automotive asset reaches its MRC via the fabric
  `AssetGovernor` (item 1), not this path. The "applies 5.0 to all platforms" defect is
  therefore latent only if the gateway envelope is later extended to carry non-automotive
  assets; **if/when it is, it must resolve the profile through the same `mrc_contract()`**.
  Tracked as a follow-up rather than forced now (threading a profile through the single-AV HTTP
  path is a larger design change with no current caller that needs it).
