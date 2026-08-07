# Tier 1 completion, and the clause that decides Q1's trigger

**KIRRA-WM-TIER1-DONE-001** · drafted 2026-08-07 · **status: PROPOSED, not adopted**

## The finding

ADR-0040's Q1 revisit trigger is keyed to a precondition **that cannot be
satisfied**. Not "has not been satisfied yet" — cannot be, as `WM_SCOPE.md` is
currently written.

Tier 1's checklist has two unticked boxes. §7's is ordinary open work. §6's is
not: **everything listed as remaining under it is explicitly assigned to Tier
2**, by that entry's own text.

> **Still open:** identity *adjudication* — candidate clustering, merge/split
> events — **is Tier 2**. **`entity_id` generation belongs there too**, not
> here: minting an id is deciding that something is a distinct thing, which is
> adjudication, whereas the three fields above are arithmetic over evidence that
> already exists.

A Tier 1 box waiting on Tier 2 work cannot tick from inside Tier 1. So Tier 1
never "completes", the trigger never fires, and the question stays deferred by
an accident of bookkeeping rather than by anyone's decision.

Finishing §7 does not rescue it. `evidence_digest` / `prev_hash` as core types
would tick §7 and leave §6 exactly where it is.

## This is the same defect ADR-0040 already found once

Worth stating, because it is the argument for fixing this rather than routing
around it. ADR-0040 contains a section titled **"Open question 1 named the wrong
prerequisite"**:

> The prerequisite for deciding Q1 was never storage; it is the **domain core** —
> Tier 1 of `WM_SCOPE.md`. Building more store cannot answer it.

That correction replaced an unreachable prerequisite (*storage exists*, which
was already true and still decided nothing) with what looked like a reachable
one. The replacement turns out to be unreachable too, for a different reason.
The pattern is a trigger keyed to a **proxy** for the thing actually wanted,
where the proxy does not track it.

## The trigger already contains its own answer

This is the load-bearing observation, and it is why the fix may be smaller than
the finding suggests. The trigger has **two clauses**, and only one of them is
unreachable:

> **Revisit trigger:** *when the domain core carries real types and the store
> consumes them* — **i.e. on completion of `WM_SCOPE.md` Tier 1** — measure what
> the seam actually carries.

| Clause | Status |
|---|---|
| **Substantive** — "the domain core carries real types and the store consumes them" | **satisfied**, and measured |
| **Proxy** — "i.e. on completion of `WM_SCOPE.md` Tier 1" | **unsatisfiable** as the checklist stands |

The substantive clause is not a matter of opinion. `KIRRA-WM-Q1-BASELINE-001`
Measurement 3 records the store constructing core values at 10 sites, naming 26
distinct core paths, and binding core types into 7 public struct fields and 5
public signatures — against a baseline where every one of those was zero.

The trigger's own next sentence explains why the proxy was chosen, and the
explanation is entirely about the substantive clause:

> Not before: until then the measurement has no content, because the dependency
> runs store → core and an empty core means an empty seam.

The concern was an **empty core producing an empty seam**. That concern is
retired. The proxy was a convenient way to say "when the core is real", written
at a moment when Tier 1's checklist looked like it would track that.

## What needs deciding

Two separable things. Conflating them is how a bookkeeping fix turns into an
unnoticed ruling on Q1.

1. **Which clause governs the trigger** — the substantive one, or the proxy.
2. **Whether §6's box is a defect** independent of Q1. A Tier 1 checklist item
   that can only be ticked by Tier 2 work misreports the tier's state to anyone
   reading it, whatever Q1 does.

## Three constraints any ruling has to respect

**1 — The measurement must not be re-taken to suit the ruling.** Measurement 3
was recorded *before* this proposal was drafted, and its numbers are already
merged (`ed0a82e5`). Whatever is decided here, the evidence Q1 is answered on
was fixed before the question of when to answer it was reopened. Re-measuring
after adopting a completion criterion would destroy exactly that property.

**2 — Ticking §6 must not silently import Tier 2 scope.** If §6's box is ticked,
its residue has to *land somewhere*. Tier 2's list currently names entity
resolution and merge/split but **not** `entity_id` minting — so the residue
cannot simply be deleted, or a real work item disappears from the plan while
looking like it was completed.

**3 — Nothing here rules Q1.** Adoption fires a trigger. The trigger asks for a
measurement, and the measurement is already on record pointing at *retain*. The
disposition remains the owner's act, exactly as the original Q1 ruling was.

## Options

**Option A — the substantive clause governs.** Record that the trigger's
operative condition is "the core carries real types and the store consumes
them", satisfied and measured; the "i.e. Tier 1" phrase was descriptive of that
condition, not an independent gate. `WM_SCOPE.md` untouched.

**Option B — repair the checklist.** Tick §6 on the grounds that its Tier 1
content is complete, and move `entity_id` minting onto Tier 2's list where the
entry itself says it belongs. Tier 1 then completes on §7, and the proxy clause
becomes reachable and satisfied in the ordinary way.

**Option C — both.** A resolves the trigger now; B fixes the checklist defect,
which exists whatever is decided about Q1.

**Option D — leave it.** Accept that the trigger never fires and the seam is
retained by default. Honest only if nobody ever cites "the trigger has not
fired" as evidence the question was left open deliberately.

## Recommendation

**Option C.**

A and B are not alternatives; they answer the two separable questions above. A
alone resolves Q1's trigger but leaves a Tier 1 box that misreports the tier to
every future reader. B alone works, but it reaches the answer by asserting a
scope judgement (*"§6 is done"*) when the trigger's own substantive clause
already gives a more direct route that requires asserting nothing.

Doing both means the trigger fires on the clause that states its actual intent,
**and** the checklist stops describing a state it cannot reach.

Option D is listed because it is a real choice, not to be dismissed — but it has
a specific cost. "The trigger has not fired" currently reads as *not yet*. Left
unfixed, it means *never*, while continuing to look like *not yet*. That gap
between what a record says and what it means is the thing every other ruling in
this directory was written to close.

## What adoption would license

Firing the trigger. `KIRRA-WM-Q1-BASELINE-001` Measurement 3 then becomes the
evidence in front of the owner, and Q1 is ruled — **retained** or **collapsed**,
both already pre-authorized by ADR-0040.

It does **not** license collapsing or retaining anything by itself, editing
Measurement 3, or treating "retain" as decided because the measurement points
there.

## What is deliberately not proposed

**Redefining Tier 1.** The tier's contents are not being renegotiated — only the
question of whether a box whose entire residue is Tier 2 work belongs on Tier
1's list.

**Rewording the trigger in ADR-0040.** Option A asks which of the trigger's two
existing clauses governs. Editing an Accepted ADR's ruling text is a larger act
and is not needed to answer that.

**Deciding Q1.** Stated twice on purpose.

## Provenance, including my own stake in it

Drafted by the same person holding every role, as with every other World Model
document here.

One thing is worth naming rather than left for a reader to notice. **I took the
measurement this proposal would put in front of a ruling, and it points at
retain.** Proposing the criterion that fires the trigger is therefore not a
neutral act on my part.

Two things bound that, and neither is a reason to skip the disclosure. Retain is
the outcome requiring *no action*, so the interest being served is a tidy record
rather than a change to the system. And the measurement was recorded and merged
before this document existed, so it cannot have been shaped to suit a criterion
that had not been written.

---

Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
SIL 3 requirements. Independent third-party assessment has not yet been
performed.
