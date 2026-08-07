# What becomes of an entity that was split — a ruling request

**KIRRA-WM-SPLIT-SURVIVAL-001** · drafted 2026-08-07 · **status: PROPOSED — not
a ruling and not an authorization**

> Kirra is designed in alignment with ISO 26262 ASIL-D requirements and IEC 61508
> SIL 3 requirements. Independent third-party assessment has not yet been
> performed.

| | |
|---|---|
| **Question** | Does the entity named as a split's `source` survive the split? |
| **Blocks** | Persisting `SplitEntity` (`WM_SCOPE.md` §5); entity resolution's redirect model |
| **Raised by** | `crates/kirra-world/src/entity.rs`, carried since the lifecycle landed |
| **Made load-bearing by** | `crates/kirra-world/src/adjudication.rs` (#1391) |
| **Decides** | Whether `Lifecycle` needs a state it does not have |

---

## 1. The finding — the code already answered this, while saying it had not

`adjudication.rs` states, in its module docs and in
`IdentityAdjudication::unresolved_consequence`, that the source's fate is
**deliberately not decided**. That claim is not true of the constructor.

`SplitEntity::new` enforces two rules together:

```rust
if into.len() < 2 { return Err(SplitTooNarrow { found: into.len() }); }
// ...
if into.contains(&source) { return Err(SplitIntoSelf { entity: source }); }
```

Take the ordinary case for a surviving original: **you believed there was one
pallet; evidence shows a pallet with a box sitting on it.** The pallet did not
stop existing. Expressing that split needs either

* `into = [box]` — one destination, **refused** by `SplitTooNarrow`; or
* `into = [pallet, box]` — naming the survivor, **refused** by `SplitIntoSelf`.

**Both spellings of "the original survives" are unrepresentable.** The type
already implements the reading in which a split *replaces* its source with two or
more successors. The neutrality is in the prose only.

This is recorded first because it is the more useful half of the finding: an
open question that the implementation has quietly closed is worse than one still
open, since the next reader takes the constraint as considered.

## 2. What the blueprint says, and the asymmetry in what it does not

Everything `KIRRA-WM-ARCH-001` states about the mechanics of a split is one
sentence, §6.3:

> Splitting emits `EntitySplit { from, into[], evidence, at }`.

§6.1's lifecycle row adds `Split(from)` beside `Merged(into)`, and nothing else.

**The asymmetry is the evidence.** Two sentences earlier, the same section
settles the survivorship question for *merge* explicitly:

> Merging two entities emits an `EntityMerged { from, into, evidence, at }`
> event. **Both original IDs remain resolvable forever and answer with a
> redirect.**

The author addressed what happens to the absorbed ids in a merge and wrote no
equivalent for the source of a split. That is a gap in the specification, not a
statement that nothing happens — the same shape as `Evidence` being an
unelaborated parameter name in three verb signatures (#1391), and as the
observation kinds before `KIRRA-WM-OBSKIND-001`.

## 3. The two readings

### A — the original survives

`Split(from)` is a marker on each *product*, pointing back at where it came
from. The source is untouched and stays live, keeping its id.

**For it:**

* The blueprint's lifecycle list names a state for the products and none for the
  source. Silence can mean "unchanged".
* **The strongest argument, and it is a real one:** a split is often a
  *subtraction*. The pallet was always there; what changed is that we noticed a
  box on it. Retiring the pallet and minting a new pallet id would lose
  continuity for a thing that never stopped existing — which is precisely the
  identity-loss failure §6.3 exists to prevent.

**Against it:**

* Under A, the original judgement is never actually revised. The system holds
  "this is one pallet" and "this is a pallet and a box" **at the same time**,
  both live, both answerable. P5 says identity is *revisable*; A revises nothing,
  it accretes.
* When the source genuinely does not survive — one blob that was always two
  boxes, neither of which is "the blob" — A leaves a live entity corresponding to
  nothing. A query for it answers confidently about a thing that does not exist.

### B — the original is superseded, and stays resolvable

The source becomes terminal, like `Merged`, and remains resolvable forever —
but its redirect names *N* successors rather than one.

**For it:**

* **§14.2 gives `WhereIs` a third return value that nothing else explains:**
  `Located | Unknown | **Ambiguous**`. Asking where the old blob is, after it was
  adjudicated to be two boxes, is exactly that answer: it was one thing, it is
  now several, and the honest reply names them. Under A there is no source of
  `Ambiguous` at all — the original simply answers.
* **Symmetry.** Merge makes the absorbed ids terminal-but-resolvable. Under A the
  two identity-revision verbs have opposite shapes for no stated reason; under B
  they are the same shape with a different arity.
* It is what the constructor already enforces (§1).

**Against it:**

* `Merged { into: EntityId }` cannot express *N* targets, so **B requires a
  `Lifecycle` state that does not exist**. That is a change to a Tier 1 type.
* It does not answer the subtraction case on its own. See §4.

## 4. The case that decides it, and why neither reading covers it alone

The pallet-and-box case is not a rhetorical flourish; it is the common one, and
it is what makes this a genuine question rather than a tidy-up.

The two readings are answering **different questions**:

| | the original corresponds to nothing afterwards | the original is one of the pieces |
|---|---|---|
| **A** | leaves a phantom | correct, and cheap |
| **B** | correct | destroys continuity |

So the honest position is that **a split has two distinct shapes**, and the
current type conflates them by refusing one:

* **Partition** — the source was never a coherent thing; it becomes *N* pieces,
  none of which is it. `SplitTooNarrow(≥2)` and `SplitIntoSelf` are exactly
  right here.
* **Subtraction** — the source survives as one of the pieces; the others are
  carved out of it. Both current rules are wrong here.

That reframing is what §1's finding actually exposes. It is offered as the
recommendation below rather than smuggled in, because it is a *third* option
neither the blueprint nor `entity.rs`'s note anticipated.

## 5. Recommendation

**Adopt B for partition, admit subtraction as a distinct shape, and refuse the
ambiguity rather than defaulting.**

Concretely, and stated so the cost is visible:

1. **Add a terminal `Lifecycle` state for a partitioned source**, carrying its
   successors — `Superseded { by: Vec<EntityId> }` or similar. Terminal, and
   resolvable forever, exactly as `Merged` is. `Entity::redirects_to` returns
   one id and would need a sibling that returns many; **collapsing them into one
   accessor would make "redirects to one thing" and "was several things"
   indistinguishable at the call site**, which is the distinction the whole
   ruling is about.
2. **Let `SplitEntity` carry which shape it is**, so partition and subtraction
   are separate constructors with separate rules rather than one constructor
   with a flag. A flag invites a caller to pass the wrong one; two constructors
   make the wrong call not compile.
3. **Keep `unresolved_consequence` until step 1 lands**, then delete it. It is
   an honest placeholder now and becomes a hiding place the moment a real answer
   exists — and the longer it sits, the more likely callers grow around it.

**Not recommended: reusing `Retired` for a partitioned source.** `Retired` is
what `ForgetEntity` produces and means an operator retired this. Overloading it
would make "an operator retired this" and "this turned out to be several things"
the same state, losing the *why* — which is the entire reason this module records
events rather than editing lifecycle fields, and the same
proxy-cannot-answer-what-the-type-was-for failure the store's `WriterClass` and
`subject` columns already exhibit.

## 6. What adoption would and would not authorize

**Would**: changing `Lifecycle` to carry a partitioned-source state; splitting
`SplitEntity::new` into shape-specific constructors; deleting
`unresolved_consequence` once the state exists; designing the storage redirect
model against a one-to-many mapping.

**Would not**: authorize persisting `SplitEntity` (that needs the schema slice,
and `subject` is inside the canonically-hashed bytes); authorize entity
resolution; or settle whether `Split { from }` should be renamed for symmetry
with a new source-side state. That rename is a breaking change to a Tier 1 type
and deserves its own decision rather than riding along.

## 7. Constraints on adoption

1. **The §1 finding is not deleted by ticking a box.** Whatever is ruled, the
   record should keep that the constructor had already chosen — a proposal whose
   own finding vanishes on adoption is the failure `KIRRA-WM-TIER1-DONE-001`'s
   second constraint names.
2. **No new `Lifecycle` state without a negative control.** The existing
   terminality tests pass against an `advance_to` that permits everything; any
   new terminal state needs the same anchor before it is claimed to be terminal.
3. **One approver.** As with every World Model ruling, this would be recorded by
   the same person holding every role. Stated so nobody infers independence from
   a decision being written down.

---

## Appendix — where each claim was checked

| Claim | Checked against |
|---|---|
| Both spellings of a surviving original are refused | `adjudication.rs`, `SplitEntity::new` — read, not recalled |
| Blueprint says one sentence about split mechanics | `WORLD_MODEL_ARCHITECTURE.md:345`, and a grep for `split` across the file |
| Merge's survivorship IS stated | `WORLD_MODEL_ARCHITECTURE.md:342-344` |
| `WhereIs` returns `Ambiguous` | `WORLD_MODEL_ARCHITECTURE.md:750` (§14.2). Cited as 752 on the first pass, which is `Resolve` — checked and corrected |
| `redirects_to` handles only `Merged` | `entity.rs`, `Entity::redirects_to` |
| `Retired` is what `ForgetEntity` produces | `adjudication.rs`, `resulting_lifecycles` |
