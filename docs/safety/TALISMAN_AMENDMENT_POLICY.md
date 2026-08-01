# Talisman amendment policy — separation of duties on the frozen kernel

**Status:** normative. Applies to every change to the frozen kinematics talisman
(`crates/kirra-core/src/kinematics_contract.rs`) and to its blob-hash re-pin.

Scope note: this governs the **human control** around a talisman amendment. The
mechanical procedure — what to record, which four pin locations must agree,
which gates to re-run — is Step 6 of the change plan for the amendment in hand
(`TALISMAN_CHANGE_PLAN_1242.md` §6 is the worked example). This document
governs *who approves*, and what happens when nobody eligible exists.

---

## 1. Why this document exists

The previous control read: *"reviewer approval, named"*, with the #1242 re-pin
note requiring it to be recorded **before merge**. Both halves failed on first
contact. #1242 merged with the approval line still reading `PENDING`, and the
approval that was eventually recorded came from the same principal who authored
the change.

The tempting repair is to write "approval, with a caveat" and move on. That is
the one repair that must not be made. A control which is *always* satisfied —
because the caveat absorbs every failure — is not a control. It is a sentence.

So the control is split into a normal path and an explicitly-flagged fallback,
and a reviewer can now answer a **binary** question about any talisman
amendment:

> Does second-principal approval exist, **or** does a formally recorded
> exception exist?

There is no third answer. In particular, the author's own acknowledgement is
**not** an answer — it is the fallback path, and the fallback path has
mandatory content.

## 2. The control

> **Talisman blob changes require approval from a second named reviewer before
> merge. If no eligible independent reviewer is available, the change must not
> claim independent approval; instead, record the limitation, the attempted
> escalation, and the named authority accepting the residual procedural risk.**

### 2.1 Normal path — second-principal approval

A named reviewer who is **not** the author of the change approves the amendment,
and the approval is recorded in the re-pin note in
`docs/CAPTURE_PIPELINE_SPEC.md` **before** the merge, not after.

"Not the author" means a different human principal. An account that authored the
commits — including one operating an automated assistant — is the author, not a
reviewer, regardless of which of them typed the approval.

### 2.2 Fallback path — a recorded exception

If no eligible independent reviewer is available, the change proceeds **only** as
a formally recorded **exception to the control**. It is not approval. The
re-pin note must state, explicitly, that the control was not satisfied, and must
carry all five of the following:

| # | Field | Why it is mandatory |
|---|---|---|
| 1 | **Who authored the change** | Establishes the principal the reviewer would have had to be independent of. Without it "independent" is unverifiable. |
| 2 | **Why no eligible independent reviewer was available** | Distinguishes a structural constraint (single-maintainer repository) from an avoidable one (nobody was asked, or the deadline was tight). Only the first is a legitimate standing reason. |
| 3 | **Who accepted the exception** | A named authority owns the residual risk. An exception nobody owns is an excuse. |
| 4 | **Exactly which evidence was independently machine-checked** | This is what actually substitutes for the missing human. Name the specific gates and their results, not "CI was green". Machine checks are independent of the author in a way the author's own reading is not. |
| 5 | **What residual risk remains because human independence was unavailable** | Names what the machine checks cannot cover — typically judgement-shaped questions: is the property the right property, is an assumption sound, is the scope correctly drawn. |

### 2.3 The fallback must stay rare and auditable

Every exception is a debt against the safety case. Two consequences:

- **Auditable.** Because the required fields are fixed, an auditor can grep the
  pin history for exceptions and check each one has all five. A partially-filled
  exception is a finding.
- **Rare.** A repository where every amendment takes the fallback path has not
  implemented separation of duties; it has documented that it lacks one. That
  may be an acceptable state, but it should be visible as a standing gap rather
  than rediscovered one exception at a time.

## 3. What the exception explicitly does not license

- It does **not** license claiming independent approval anywhere else — commit
  messages, PR descriptions, release notes, or certification submissions.
- It does **not** convert into approval later by the passage of time or by the
  change proving uneventful in service.
- It does **not** lower the evidence bar. Every mechanical gate in the change
  plan's Step 6 still applies in full. The exception concerns *who signed*, not
  *what was checked* — and since the machine-checked evidence is what carries
  the weight when human independence is absent, weakening it under an exception
  would be exactly backwards.

## 4. Recorded exceptions

| Amendment | Blob | Path taken | Where recorded |
|---|---|---|---|
| #1243 — Priority 3/4 rate bound runs on the over-ceiling path. **Also an ASSURANCE re-pin: SG1 loses its per-PR symbolic proof** (K3 → weekly deep lane, concrete mirror becomes the standing per-PR gate). ⚠️ Amended 2026-07-31: the destination lane had never completed a run (#1260), so SG1's proof was SUSPENDED, not relocated. ✅ **Resolved 2026-08-01: lane repaired (#1262), K3 discharges in 67 m 13 s, SG1's symbolic proof RESTORED** (weekly; a per-PR symbolic proof is still not restored, 67 min > the 45-min budget). The reduction this row records is real but far smaller than the suspension described. K8 is now profiled and DEMOTED (#1260, same diagnosis as K7); R2 remains restricted — solver-bound, not diagnosed intractable; K7 separately demoted (#1268) | `6a61b74f…` → `851f3f44…` | **Exception** (§2.2) — single-maintainer repository, author and accepting authority the same principal; all five fields recorded, and recorded **before** merge. The acceptance covers the assurance reduction as well as the behaviour change. Note field 5(b)'s *reasoning* was later falsified — "no budget is known to suffice" was inferred from a 15-minute non-result; 67 minutes sufficed | `docs/CAPTURE_PIPELINE_SPEC.md`, re-pin note + its CORRECTION and RESOLVED blocks |
| #1242 — Priority 2 accumulates past the speed ceiling | `ed00f4da…` → `6a61b74f…` | **Exception** (§2.2) — single-maintainer repository, author and accepting authority the same principal; additionally recorded *after* merge rather than before | `docs/CAPTURE_PIPELINE_SPEC.md`, re-pin note |

This table is the audit entry point. Any future amendment taking the fallback
path is appended here as well as recorded in the pin note.
