# WM-2 `synchronous` policy — proposal

| | |
|---|---|
| **Identifier** | KIRRA-WM2-SYNC-001 |
| **Status** | **ADOPTED 2026-08-05.** P-1 through P-4 were ruled on by the World Model owner and are now the rule. Recorded in ADR-0041 *Open questions* 1. |
| **Addresses** | ADR-0041 **open question 1** — "`synchronous` policy per source class" |
| **Evidence** | ADR-0041 **D-17**, `docs/evidence/wm2-oq1-20260804/` · **D-19**, `docs/evidence/wm2-postrepair-20260804/` — the third observation, cited in §1 (both `JETSON-TARGET-MEASURED`) |
| **Date** | 2026-08-04 |

> Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
> SIL 3 requirements. Independent third-party assessment has not yet been
> performed.

---

## 1. Summary

**ADOPTED: `synchronous=FULL` for the evidence log, for every class, with
per-class differentiation moved to commit grouping instead.**

> **Ruled 2026-08-05.** P-1 through P-4 were adopted as written. The document is
> retained in its proposing voice below — the argument is the record of why, and
> rewriting it into the imperative would lose the alternatives that were
> weighed. §7's falsifiers remain the conditions for revisiting.
>
> One thing strengthened after adoption: **D-19** supplies a third observation
> of the reproducible property this rests on, agreeing with D-17 within 0.2 %.

Three findings drive it. The first says the question as posed cannot be
answered; the second says it does not need to be; the third says the answer it
implies would cost evidence the ADR has already banked.

---

## 2. "Per source class" is implementable — but it does not buy a per-class guarantee

ADR-0041 fixes `world_events` as **"the append-only evidence log (the only
writable table)"**, hash-chained, with a total replay order that projections
fold over.

`PRAGMA synchronous` is a **per-connection** setting. An earlier draft of this
document concluded from that that a per-class policy "requires splitting the
log". **That was wrong, and review caught it.** Nothing stops several writer
connections addressing the *same* database with different `synchronous` values
— safety events through a `FULL` connection, telemetry through a `NORMAL` one,
both appending to one `world_events` and one chain. No split is needed.

**The objection is different, and it is the one that decides the question.**

In WAL mode all writers append frames to one shared `-wal` file, and **`fsync`
flushes that file, not a transaction.** So a `FULL` commit makes every
preceding frame durable, including frames some other connection wrote under
`NORMAL`. Two consequences follow, and both are bad:

| Consequence | Why it disqualifies the design |
|---|---|
| A `NORMAL` class's durability depends on **other classes' traffic** | Its events become durable when some *other* connection next fsyncs. Its loss window is therefore set by a stream it does not control and cannot observe. That is not a per-class guarantee; it is a guarantee about the fleet's aggregate write pattern |
| The intended relationship inverts | The strict class ends up **subsidising** the lax one — every safety fsync silently hardens whatever telemetry preceded it — while the lax class's exposure varies with unrelated load. The knob does not do what its name implies |

There is one thing this arrangement is *not*: chain-breaking. Because `fsync`
flushes the whole WAL prefix, a mixed-setting log cannot end up with a durable
entry after a lost one. **No holes, and the chain still verifies** — the
earlier draft implied otherwise and that was also wrong.

**Genuinely independent per-class durability does require splitting the log**,
and that is where the architectural costs live:

| Split cost | Why it matters |
|---|---|
| One hash chain becomes several | Tamper evidence (blueprint P3) is a property of *one* chain over *one* order. N chains prove N things and say nothing about their interleaving |
| The total replay order is lost | Projections are a pure fold over that order. Across two logs, "rebuild from zero equals the incremental state" stops being well-defined without a merge rule |
| Compaction-with-citation fragments | §11.3 citations reference a contiguous generation span; spans in different logs are not comparable |

So the shape of the answer is: **per-class `synchronous` is available and
cheap, but meaningless on a shared WAL; it becomes meaningful only at a cost
the architecture should not pay for a 1.16× effect (§3).**

**Also undefined: "source class" itself.** The phrase appears once in the ADR,
in OQ1. What exists is **retention class** (§11.3: safety, incident,
calibration, adjudication, operator, plus the `raw` default), which is a
different axis — how long an event is kept, not who produced it. Whether the
two taxonomies coincide is an owner decision and is coupled to open question 2.
A policy cannot enumerate classes that have not been defined; this proposal
therefore supplies a **rule**, not a class table.

---

## 3. And it does not need to be — the setting is the small lever

From D-17, target, `JETSON-TARGET-MEASURED`, events/second:

| | batch=1 | batch=64 | batching gain |
|---|---:|---:|---:|
| FULL | 3 246 | 31 665 | **9.8×** |
| NORMAL | 9 924 | 36 870 | 3.7× |
| OFF | 15 089 | 56 406 | 3.7× |

**Batching is a ~10× lever. `synchronous` is a 1.16× lever once you batch.**

At batch=64, choosing NORMAL over FULL buys **16 %** throughput
(36 870 / 31 665) and costs **32 %** at p99:

| NORMAL / FULL, batch=64 | p50 | p99 | max |
|---|---:|---:|---:|
| target | 0.65× | **1.32×** | 1.40× |
| host (indicative) | 0.47× | **1.51×** | 4.87× |

That is the trade in one line: **NORMAL buys median throughput by paying tail
latency.** Reproducible on two machines and both batch sizes (D-17).

For a store whose consumers care about *worst-case* behaviour — a governor
deciding whether it may act, an incident reconstruction needing the record to
be there — paying tail latency for 16 % median throughput is the wrong
direction. At batch=1 NORMAL does buy 3.1×, but the answer there is to batch,
not to weaken durability.

---

## 4. The durability gate was measured at FULL only

ADR-0041's tier C closed on **five physical power cuts with no acknowledged
write lost** (D-11). Those trials ran at `Durability::Full` —
`crash.rs::powercut_arm` opens the store with it, and there is no path to arm
at another setting.

So `synchronous=NORMAL` for the evidence log would mean **the closed durability
gate does not cover the shipped configuration.** Re-opening tier C is five more
physical power cuts. That cost belongs in the decision, and it is a cost the
16 % does not come close to justifying.

This is not an argument that NORMAL is unsafe. It is an argument that its
safety here is **unevidenced**, which for a gated ADR is the operative
distinction.

---

## 5. The proposal

**P-1. `synchronous=FULL` on the evidence log, universally.** One log, one
chain, one setting — matching the architecture rather than fighting it, and
matching the configuration tier C was closed under.

**P-2. Per-class differentiation moves to commit grouping.** How many events a
class accumulates before a commit, or how long it may wait, is settable per
class *without* touching the chain, the order or the connection: it is a
property of the writer, not the store. It is also the 9.8× lever rather than
the 1.16× one.

**P-3. Commit grouping is the class-visible durability knob, and it should be
stated as a loss window.** A class committing every N events or every T
milliseconds risks at most **its uncommitted tail** — up to N events, or up to
T milliseconds' worth — on power loss. *Committed* events are not at risk:
under P-1 every commit fsyncs, so acknowledgement means durable. The loss
window bounds what has not yet been acknowledged, which is the only thing it
could bound.

That is a far more legible per-class contract than a `synchronous` value,
because it is expressed in the units the class owner already reasons about —
events and milliseconds, not fsync semantics — and because, unlike a
per-connection `synchronous` on a shared WAL (§2), it is genuinely the class's
own: it depends on that class's commit cadence and nothing else's.

**P-4. `synchronous=OFF` is never proposed for the log.** D-17 measures it only
to bound the fsync term. It is 1.78× FULL at batch=64 and gives up the property
the log exists to provide.

---

## 6. What this proposal does NOT decide

- **The class taxonomy.** Undefined in the ADR (§2). Owner's, coupled to OQ2.
- **The per-class grouping budgets.** P-2 and P-3 say the knob is grouping and
  the contract is a loss window; the numbers per class are an owner decision
  requiring inputs this measurement cannot supply — how much of each stream may
  be lost on power loss.
- **Whether any class justifies its own store.** If some class genuinely needs
  a different durability posture, the honest route is a separate store with its
  own chain, accepting §2's costs deliberately. Not proposed here; nothing in
  the evidence argues for it.
- **Anything about Kirra World domain semantics.** This is a substrate setting.

---

## 7. What would change this proposal

Stated so it is falsifiable rather than merely reasonable:

- **A class with a measured throughput requirement above ~31 700 ev/s at
  batch=64 that cannot be met by larger batching.** Then the 16 % matters and
  the trade reopens.
- **Tier C re-run at NORMAL, passing.** That removes §4 entirely, though not §2
  or §3.
- **A per-class durability requirement that grouping cannot express** — a class
  needing a *stronger* guarantee than FULL provides, which grouping cannot
  supply because it only ever widens the loss window.
- **Splitting the log for an unrelated reason.** If §2's costs are paid anyway,
  per-store `synchronous` becomes available as a side effect and should be
  reconsidered on its merits.
- **A strict class whose fsync cadence is dense and guaranteed.** §2 rejects
  mixed `synchronous` on a shared WAL because a lax class's durability then
  depends on other classes' traffic. If some class is known to commit under
  `FULL` at a bounded minimum rate, that dependence becomes a *bounded* one,
  and mixing stops being unprincipled. It would still need stating as a
  cross-class coupling rather than a per-class setting — but the objection in
  §2 is about unboundedness, not about mixing as such.

---

## 8. Effect on open question 1

OQ1 was **narrowed** by D-17: its premise — an unexplained `NORMAL` < `FULL`
inversion — does not reproduce, so it no longer blocks a decision.

This document proposes the decision. **It does not close OQ1**, for two
reasons: the ruling is the owner's, and the residual anomaly D-17 records — why
D-15's `NORMAL` figure was low — is unexplained and independent of the policy
choice. A policy decision does not retire an open measurement question; it just
stops waiting on it.
