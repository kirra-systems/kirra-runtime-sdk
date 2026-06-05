# Kirra — Curated Autoware Interface: Version-Sync SRAC

**Document ID:** KIRRA-OCCY-MSGSYNC-001.
**Status:** Draft. Safety-related application condition (SRAC).
**Classification:** ISO 26262 Part 9 (SRAC) / Part 10 (SEooC integration condition).
**Cross-refs:** `SAFETY_CASE_INDEX.md` (AEGIS-SC-000), `ASSUMPTIONS_OF_USE.md`
(AOU-MSG-TOOLCHAIN-001), `OCCY_PERCEPTION_DERATE_VALIDATION_GATE.md` (KIRRA-OCCY-PMON-004 §8),
`PACIFICA_PILOT_ARCHITECTURE.md` (KIRRA-OCCY-DEPLOY-001), the curated packages under
`ros2_ws/src/autoware_{perception,planning}_msgs/` + `scripts/curated_interface/`.

---

## 1. What this records

The Kirra governor is built against a **curated subset** of the Autoware ROS 2 message
interface: the real package names (`autoware_perception_msgs`, `autoware_planning_msgs`)
carrying **only** the verbatim message closures the governor consumes
(`PredictedObjects`, `Trajectory`). This replaces the un-versioned, un-verified bench
**trim** (the hand-replaced `autoware_planning_msgs` overlay recorded in
KIRRA-OCCY-PMON-004 §8 / AOU-MSG-TOOLCHAIN-001) with a sanctioned, version-controlled,
hash-verified interface. This document is the **version-sync SRAC** that keeps the curated
subset wire-compatible with the deployed Autoware.

## 2. Pinned reference

| Field | Value |
|-------|-------|
| Source | ROS 2 **Jazzy** apt — Autoware messages |
| Pinned reference version | **1.11.0-1noble.20260412** (the bench reference) |
| Reference share path | `/opt/ros/jazzy/share` |
| Curated seeds | `autoware_perception_msgs/PredictedObjects`, `autoware_planning_msgs/Trajectory` |

> The pinned version is the apt package version the curated `.msg` were extracted from.
> RIHS/DDS wire compatibility is determined by the **message-closure structure** (the
> verbatim `.msg` + the base-message versions from ros-base), not by the curated package's
> own `package.xml` `<version>` field (which is metadata only).

## 3. The SRAC (the condition)

**SRAC-MSGSYNC-1 — byte-identical closure.** The curated packages' `.msg` files **MUST
remain byte-identical** to the corresponding `.msg` of the **deployed** Autoware version.
This is enforced by `scripts/curated_interface/verify_hashes.sh` (byte-diff each curated
`.msg` against the reference; non-zero exit on any mismatch). Byte-identical closure +
identical base-message versions ⇒ identical RIHS type hash ⇒ DDS delivers genuine Autoware
messages to the governor.

**SRAC-MSGSYNC-2 — re-verify on every Autoware version change.** Any change to the deployed
Autoware version (per target) **requires re-running `verify_hashes.sh` against that target's
Autoware reference before deployment**, and — if the `.msg` changed — re-running
`extract_closures.sh` and bumping §2 above. The curated subset is valid **only** for an
Autoware version whose `.msg` closure it byte-matches.

**SRAC-MSGSYNC-3 — no hand-edits.** A curated `.msg` is **only** ever produced by
`extract_closures.sh` from a reference install. Hand-editing a `.msg` silently changes the
RIHS type hash and breaks wire compatibility — it is prohibited.

## 4. Topology precondition (carried honestly, not assumed away)

**TOPO-1 — interface isolation.** This curated-interface resolution holds **only where the
governor's build + runtime host does NOT carry the full Autoware message set** — i.e. a
dedicated compute node, or an isolated container / overlay. If the full Autoware messages
are present on the same host, the r2r binding generator sees the full set again and the
codegen panic (AOU-MSG-TOOLCHAIN-001 / PMON-004 §8) returns. The deployment-topology
commitment that satisfies this is recorded in KIRRA-OCCY-DEPLOY-001 (container-isolation on
the single-Orin bench; dedicated / container on the Pacifica) — referenced here as a
**precondition**, not re-decided.

**TOPO-2 — per-target re-verification.** The byte-identical closure (SRAC-MSGSYNC-1) must be
**re-verified against whatever Autoware version each deployment target actually runs** — the
bench reference does not transfer to a target running a different Autoware version. (This is
the per-target instance of SRAC-MSGSYNC-2, stated as a topology condition because each target
is a distinct integration.)

## 5. Verification status — bench PASS recorded; per-target re-verify OPEN

- The **scaffold** (packages, scripts, this SRAC) is in the repo (Phase 1).
- **Phase 2 — DONE on the bench (2026-06-05, ROS 2 Jazzy):**
  - `scripts/curated_interface/verify_hashes.sh` = **PASS** — all **8** curated `.msg`
    byte-identical to the apt reference `ros-jazzy-autoware-{perception,planning,common}-msgs`
    **1.11.0-1noble.20260412**. Wire compatibility (RIHS type hash) holds by construction.
  - `cargo build/test -p kirra-ros2-adapter --features ros2` = **GREEN** against the curated
    overlay with **NO full Autoware present** (no apt Autoware, no trim). Verdict path unchanged.
- **Still OPEN: per-target re-verification (TOPO-2).** SRAC-MSGSYNC-1/2 must be re-passed
  against whatever Autoware version each deployment target runs; the bench PASS does not
  transfer to a target on a different Autoware version. `KIRRA_PERCEPTION_DERATE_ENABLED` is
  unaffected by this item and stays default-OFF on its own gates.

## 6. Relationship to AOU-MSG-TOOLCHAIN-001 (SUPERSEDES — owner decision 2026-06-05)

This SRAC **supersedes the relevant scope of AOU-MSG-TOOLCHAIN-001** (owner decision
2026-06-05, **option C**; see `ASSUMPTIONS_OF_USE.md` → AOU-MSG-TOOLCHAIN-001 "Resolution").
AOU-MSG-TOOLCHAIN-001 is marked **SUPERSEDED — discharged for the isolated-governor topology
(TOPO-1)** via this curated interface, with the §5 bench PASS as the dated discharge evidence
under the TOPO-1/TOPO-2 conditions. The precise **residual** — a co-resident-with-full-Autoware
r2r codegen topology, which the curated package avoids by topology but does not fix — is carried
by the new **AOU-MSG-TOOLCHAIN-002 (OPEN)**, cross-referenced to the r2r-codegen track.
