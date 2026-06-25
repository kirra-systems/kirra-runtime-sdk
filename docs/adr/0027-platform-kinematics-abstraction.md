# ADR-0027: Platform-kinematics abstraction (one platform-parameterized governor surface)

| Field | Value |
|---|---|
| Status | **Proposed (design note)** — ratified on merge. |
| Date | 2026-06-25 |
| Deciders | Project / safety-case owner |
| Safety goals | **SG2** (drivable-space containment — generalized to any platform's footprint); **SG8/SG9** (kinematic envelope + non-finite/integrity, per platform). The Ackermann path is the existing enforced talisman, unchanged. |
| Cross-refs | `docs/safety/STAGE_S-PK1_PLATFORM_KINEMATICS.md` (the staged spec; supersedes its "ADR-0017" placeholder — that number was taken by the predictive-RSS arc); code: `crates/kirra-core/src/platform_kinematics.rs` (`PlatformKinematics`, `PlatformVerdict`, `AckermannPlatform`, `validate_platform_containment`), `parko/crates/parko-kirra/src/platform.rs` (`DiffDrivePlatform`, `DiffDriveVerdict`); the frozen talisman `crates/kirra-core/src/kinematics_contract.rs` (`validate_vehicle_command`) |

## Context

The governor's safety surface was **Ackermann-shaped** and scattered across three
unparameterized expressions: the bicycle-model talisman (`validate_vehicle_command` /
`VehicleKinematicsContract`; the angular channel "intentionally NOT gated here"), a *separate*
differential-drive angular channel in `parko-kirra` (`angular_bound`, #407), and the generic scalar
`KirraKernelGovernor`. SG2 containment was 2D but consumed only via the Ackermann `VehicleConfig`.

The governor can only **bound** a robot whose physics it can **express**. Until a
platform-parameterized contract existed, every doer (nav, SLAM, planning, behavior) on a
non-Ackermann platform would be **unbounded**. This is the Track-3 keystone — the prerequisite for
the doer–checker thesis to hold on a new platform.

## Decision

Introduce one **platform-parameterized** abstraction in `kirra-core`, keeping each platform's
*shape* in its impl and the shared surface **minimal and behavioral**:

- **D1 — associated `Verdict`, not a generalized verdict.** `trait PlatformKinematics { type Verdict:
  PlatformVerdict; … }`. `Ackermann::Verdict = EnforceAction` **byte-identical** (the frozen
  talisman / audit reason strings / QNX `deny_code_num` demand it). A generalized angular verdict is
  the *second* platform's shape masquerading as the abstraction (already fails omni / aerial). The
  tiny `PlatformVerdict` bound (`is_admitted` + `deny_reason`) gives audit / posture /
  consumer-safe-stop a uniform view. (`deny_reason` returns `&str`, not `&'static str` — the
  differential-drive sibling's runtime `Deny { reason: String }` surfaced that generalization.)
- **D2 — `evaluate` + `footprint()` + the few cross-check primitives** (`max_speed_mps`,
  `max_brake_mps2`, `stop_epsilon_mps`). Mechanism (`wheelbase_m`, steering geometry, ICR) stays
  **private** to the impl. The moment a mechanism field is on the trait, that platform has leaked
  into the abstraction.
- **D3 — the scalar `KirraKernelGovernor` stays the composable PRIMITIVE**, never a platform: a
  scalar channel has no footprint / no spatial containment, so a degenerate `footprint()` would be a
  silent-degenerate on the safety surface. It keeps the lean ASIL-D / C-FFI surface (#404); platform
  impls *compose* it.

**Non-negotiable constraint — additive around the frozen talisman.** `validate_vehicle_command` is
INV-3-adjacent frozen: the Ackermann impl is a **verbatim adapter** (its `evaluate` literally calls
the talisman), so the existing AV safety case is preserved exactly, and the existing talisman tests
(+ parko's angular tests) are the proof that the abstraction changed nothing.

**Enforcement generalization (SG2).** `validate_platform_containment<P: PlatformKinematics>` runs the
existing `validate_trajectory_containment` against `platform.footprint()` — the same SG2 checker
now bounds **any** platform, drive-agnostically (footprint-driven, no mechanism), purely additive
(the per-pose path and the Ackermann slow loop are untouched), fail-closed by construction.

**Tiers.** Tier A (ground holonomy — diff-drive / omni; AMR/AGV) first — done (S-PK1a/b/c). Tier B
(aerial = 3D containment + a new envelope) and the unified slow loop are **gated** (need a named
driver). Tier C (manipulator — a different safety surface: joint-space reachable-set / self-collision)
is **cut** unless a customer requires arms.

## Consequences

- The safety architecture (envelope, containment, RSS, decel-to-stop, frame-integrity) extends to a
  new platform through the trait, not a per-platform reimplementation — demonstrated end-to-end:
  the Ackermann AV and the real `DiffDrivePlatform` are bounded by the **same** SG2 seam.
- **Scope honesty (claimable status).** The Ackermann path is **ENFORCED** (unchanged). The
  abstraction + seam are **IMPLEMENTED and dual-platform-PROVEN**. A non-Ackermann **deployment**
  (e.g. a live diff-drive ROS node consuming the seam, with the platform's verified geometry/limits)
  is **DEPLOYMENT-PENDING** — an integrator obligation, recorded as **AOU-PLATFORM-GEOMETRY-001**.
  No new-platform safety goal is marked ENFORCED on the basis of the seam alone.
- RSS and the per-command verdict stay platform-specific (Ackermann: pure-kinematic `evaluate` +
  separate decel gate; diff-drive: `evaluate` wraps the RSS-inclusive governor `gate`). The seam
  unifies *containment* only — a deliberate, low-risk scope.
