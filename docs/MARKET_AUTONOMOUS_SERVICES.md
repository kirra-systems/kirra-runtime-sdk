# Market — Autonomous Services: the Vendor-Neutral Governance Layer

| Field | Value |
|---|---|
| Issue | feeds **#298** (README / public-positioning); board index **#302** (lane `domain:fleet`) |
| Status | **Market-positioning narrative — decides nothing.** Architecture claims are anchors over decided artifacts (the `ARCHITECTURE_STACK.md` rule); market claims are owner-supplied research filed per the **ADL-012 discipline**. |
| Scope | Why the converging autonomous-services market needs a vendor-neutral governance + attestation plane, and which already-built KIRRA artifact answers each part |
| As-of | **June 2026** (the market facts; see the sourcing note) |

> **Sourcing discipline (ADL-012).** The market facts in §2 are **owner-supplied
> research from live sources, June 2026**. They are filed here **with their as-of
> dates and caveats intact and are NOT independently re-verified in this repo** —
> exactly as `work/decisions.md` **ADL-012** files owner-supplied vendor research
> ("findings are filed with their caveats intact; on-target verification rides the
> eventual hardware day"). This document does **not** pad those facts from model
> training memory: a figure not attributed inline below is not asserted. Treat the
> numbers as the owner's research snapshot, not a KIRRA measurement.

> **Architecture discipline (the `ARCHITECTURE_STACK.md` rule).** Every architecture
> claim below **cites the artifact where the decision actually lives** (code / spec /
> ADR / issue) and asserts nothing on its own. Where a capability is described as
> "built", the anchor is the code; where it is a gap, §4 says so plainly.

---

## 1. Thesis

Autonomous **service** fleets — sidewalk couriers, road delivery AVs, and
robotaxi-class vehicles — have converged into **mixed fleets under single operators
and platforms**, run today on **improvised remote supervision with no common safety
plane**. Each vehicle class arrived with its own teleop console, its own incident
format, and its own (or no) attestation story; the operator stitching them together
inherits N safety models and zero shared evidence.

KIRRA is the layer that market is missing: **one governor, one posture vocabulary,
one signed evidence chain, one operator console — across heterogeneous vehicle
classes.** It is vendor-neutral by construction (it governs whatever silicon and
whatever planner the operator already bought) and the per-class difference is
confined to **one parameter family** (§3c), not a forked stack.

This is the *services-market* framing of the same vendor-neutral thesis the
`COMPETITIVE_ROADMAP.md` carries for the automotive OEM market: there, KIRRA is the
governance layer no chip vendor can offer without cannibalizing silicon; here, it is
the governance layer no single-vendor robot fleet ships and no aggregator owns.

---

## 2. Segment map (facts as-of June 2026 — owner-supplied; sources inline)

> Each bullet carries its source and as-of inline. Per the sourcing note above,
> these are the owner's June-2026 research snapshot, caveats intact.

### Sidewalk couriers (pedestrian-space, low-speed)
- **Serve Robotics** — **~2,000 robots across 44 cities / 14 states; ~1.8M
  deliveries; fleet up ~20× year-over-year; software services ~⅓ of revenue;
  embedded in the two largest US delivery apps.** *(Source: SEC filings, FY2026 —
  owner-supplied, June 2026.)*
- **Coco** — **1,000+ robots, targeting 10,000; "Coco 2" explicitly expanding from
  sidewalks → bike lanes → roads.** *(Source: owner-supplied market research, June
  2026.)*
- **Starship** — **campus / closed-environment model.** *(Source: owner-supplied,
  June 2026.)*

### Road delivery AVs (mixed-traffic, mid-speed)
- **Nuro** — **pivot toward licensing its "driver"** (the autonomy stack) rather than
  only operating its own vehicles; **grocery / retail integrations** (Kroger
  driverless trucking noted). *(Source: market reports — owner-supplied, June 2026.)*

### Robotaxi / courier-platform class (mixed-traffic, full-speed)
- **Waymo / Uber-style platform partnerships** — the high-end of the same operator
  consolidation. This class is the **home of the full ASIL-D + hypervisor reference
  architecture** (see `ARCHITECTURE_STACK.md`). *(Source: owner-supplied, June 2026.)*

### The convergence point (why "mixed fleet" is the unit, not "a robot")
- **Single operators now span classes** — Coco 2 explicitly (sidewalk → bike lane →
  road). *(Owner-supplied, June 2026.)*
- **Platforms integrate MULTIPLE robot vendors** — Uber Eats / DoorDash dispatch to
  several robot makers at once. *(Owner-supplied, June 2026.)*

> The operative fact for KIRRA is not any single number; it is that **the buyer who
> needs governance is increasingly the operator/platform spanning vehicle classes
> and vendors**, who has no common safety plane to span them with.

---

## 3. The three theses (the analytical core)

Each thesis names a market gap, then anchors the KIRRA artifact that answers it. The
anchors are load-bearing: where this says "built", the cited code is the proof.

### a. Remote supervision is load-bearing industry-wide — and ungoverned

Remote human supervision is not a transitional detail; it is the operational spine of
every class today (Coco's human-in-the-loop framing; the stated L4 trajectory of
*reducing* operators-per-robot — owner-supplied, June 2026). Yet it is **bespoke
teleop with no governed boundary between "assist" and "override"**: the same console
that nudges a stuck robot can, on most stacks, command it, with no attestable line
between the two.

KIRRA already draws that line, certifiably, and it is **built**:
- **SG6 escalation → console → signed grant → two-checkpoint delivery.** A
  post-collision impact latches the vehicle (SG6); the operator's clearance is a
  **well-formed, signed grant**, re-validated at *delivery* time by the node's own
  loop (the two-checkpoint design), consumed exactly once, never auto-retried.
- **Anchors:** `parko/crates/parko-kirra/src/clearance_delivery.rs`
  (`ClearanceDelivery::poll_and_deliver`, the one-shot consume + two-checkpoint
  validation); `parko/crates/parko-core/src/impact.rs` (the SG6 `ClearanceLoop` /
  `OperatorClearanceGrant`); the operator console (`static/console.html`, served at
  `/console`, posture-**exempt** because it is the recovery plane); **#310** (the
  node-owned delivery wiring) and **#311** (arming the loop from live detection);
  `docs/CONSOLE_RUNBOOK.md` (the end-to-end demo + the "On the vehicle" deploy path).

The market has the supervision; it lacks the *governed boundary*. That boundary is
the difference between teleop and a certifiable assist/override interlock.

### b. The platform wedge — aggregators have no common compliance plane

An aggregator dispatching to **N robot vendors** (Uber Eats / DoorDash — §2) has **no
common posture model, no common incident format, and no attestable cross-vendor
evidence**. Each vendor reports differently; none of it composes into something a
city or insurer can trust.

KIRRA federation is that cross-vendor compliance plane (the mechanism is **built**;
the multi-tenant scale-out is a named gap, §4):
- **One posture vocabulary** across vendors (`FleetPosture` —
  `Nominal`/`Degraded`/`LockedOut`).
- **Ed25519-signed, generation-ordered cross-controller trust reports** that
  reconcile between controllers.
- **A tamper-evident incident record** — the hash-chained, signed audit ledger — that
  is exactly the artifact a **city (permits) or insurer** can eventually demand as the
  demand-side for trustworthy incident evidence.
- **Anchors:** `src/verifier.rs` (`FleetPosture`, `FleetNodePosture`);
  `src/federation.rs` (`FederatedTrustReport`, Ed25519 verify) and
  `src/federation_reconciliation.rs` (`FederatedTrustReportV2`, reconciliation);
  `src/audit_chain.rs` + the `audit_log_chain` table (the SHA-256 hash-chained,
  Ed25519-signed ledger).

The wedge is that the aggregator's *compliance* obligation is cross-vendor, but no
single robot vendor can satisfy it. A vendor-neutral plane can.

### c. The architecture already parameterizes across classes

The same governor, chain, and console serve all three classes; the architecture is
explicitly a **three-domain** model (`ARCHITECTURE_STACK.md` §2: safety partition /
boundary / autonomy guest) that scales by *configuration*, not by fork:
- **Robotaxi end** — the **full hypervisor reference architecture** (QNX safety
  partition + frozen-layout boundary + isolated Autoware/Occy guest).
- **Courier end** — a **QNX-native / single-SoC variant** — the framing the PARK-026
  rules already carry (`parko/QNX_BACKEND_SELECTION.md`); pedestrian-space, low-speed,
  no separate planner box.

**The per-class delta is the KINEMATIC CONTRACT family** — the envelope, the ODD
speed caps, and the SG6 `ImpactCfg` thresholds — **not** the governor, the chain, or
the console:
- **Anchors:** `src/gateway/kinematics_contract.rs` —
  `VehicleKinematicsContract` already exposes profile *instances*
  (`nominal_reference_profile()`, `mrc_fallback_profile()`) and an `odd_speed_cap_mps`
  knob; `parko/crates/parko-core/src/impact.rs` `ImpactCfg::spike_threshold_mps2`
  (the per-class impact threshold, already a VALIDATION-PENDING parameter).
- **Frozen-talisman caveat (load-bearing):** the talisman file
  `src/gateway/kinematics_contract.rs` is the **frozen-ABI precedent** — its byte
  layout *is* the safety claim (`ARCHITECTURE_STACK.md` §2 / `HYPERVISOR_CONTRACT_CHANNEL.md`
  §2.4). Per-class profiles are **siblings within the family — additional instances /
  configs — never edits to the frozen instance.** A profile that required changing the
  talisman's layout would be a finding, not a feature.

---

## 4. What this market promotes on the roadmap

`COMPETITIVE_ROADMAP.md` stays the capability tracker; this section only says **which
already-named (or newly-named) capabilities this market moves up in priority, and why.**

> **Honest status: nothing in this section is built.** This is a map of named gaps,
> not a claim of capability. Each line points at the tracking issue.

| Capability | Market driver | Tracking |
|---|---|---|
| **Remote / wide-area transport** (Zenoh) | Cellular courier fleets make robot↔cloud transport **strategic now**, not "later" — fleet posture + federated reports must cross the wide-area hop. **Still STRICTLY QM, never the safety channel** (ADR-0006 Clause 2). | **#296** (promoted — see the comment there) |
| **Multi-tenant + scale** for console / federation | Operators run **thousands of nodes** with **per-operator identity**; the single supervisor key and single-tenant console do not scale to a platform. | *new issue* (multi-tenant console + fleet scale, `domain:fleet`) |
| **Per-class contract profiles** | A contract **family** (courier / delivery-AV / robotaxi) parameterizing envelope + ODD caps + `ImpactCfg`, talisman frozen, profiles as siblings (§3c). | *new issue* (per-class kinematic contract profiles, `domain:boundary`) |
| **VRU-dense low-speed ODD profile** | The sidewalk-courier class operates in **pedestrian space**; it needs its own ODD caps and SG6 thresholds. | *new issue* (low-speed VRU-dense ODD profile, `domain:autonomy-guest`) |

None of these change a decided artifact; they are configuration/scale work over the
built governor, chain, and console.

---

## 5. Cross-references

- **`docs/ARCHITECTURE_STACK.md`** — the three-domain model and the anchors-only rule
  this doc's architecture claims obey. (Issue **#295**, board **#302**.)
- **`docs/COMPETITIVE_ROADMAP.md`** — the capability tracker this doc promotes items on
  (it points back here in one line).
- **#298** — README + public-positioning pass: this doc is **positioning input** for
  the README repositioning (the services-market face of the vendor-neutral thesis).
- **#296** — Zenoh fleet transport: promoted by §4 (cellular courier fleets).
- **#302** — the three-domain board; this doc's issue trail is grouped under
  `domain:fleet`.
